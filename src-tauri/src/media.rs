//! Playback source for the in-app player: a loopback HTTP server.
//!
//! Recordings are now decoded and seeked by embedded libmpv (ADR 0008), which
//! handles any codec and sparse GOPs natively, so the import **transcode is
//! eliminated** (ADR 0005 superseded): originals are never modified and a
//! recording is playable immediately after import. Import is just probe (for
//! capture date and frame rate) + segmentation.
//!
//! The loopback HTTP server below is the legacy webview playback transport,
//! retained until the dead webview path is removed (#39); mpv opens recordings
//! straight from disk and does not use it.
//!
//! ffmpeg/ffprobe are the bundled sidecars from ADR 0004.

use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use sha2::{Digest, Sha256};
use tiny_http::{Header, Request, Response, Server, StatusCode};

/// Number of worker threads draining the request queue. A handful is plenty: at
/// most one recording plays at a time, plus the odd overlapping range request.
const WORKERS: usize = 4;

/// How the frontend reaches the playback server: the origin to build URLs from
/// and a per-launch token that must accompany every request, so other local
/// processes can't drive the server against arbitrary files.
#[derive(Clone, Serialize)]
pub struct PlaybackEndpoint {
    pub origin: String,
    pub token: String,
}

/// Start the playback HTTP server on a loopback ephemeral port and return the
/// endpoint the frontend needs. Worker threads outlive this call for the
/// lifetime of the process.
pub fn start() -> Result<PlaybackEndpoint, String> {
    let server = Server::http("127.0.0.1:0").map_err(|e| format!("playback server: {e}"))?;
    let port = server
        .server_addr()
        .to_ip()
        .map(|addr| addr.port())
        .ok_or("playback server bound to a non-IP address")?;
    let token = generate_token();

    let server = Arc::new(server);
    let shared_token = Arc::new(token.clone());
    for _ in 0..WORKERS {
        let server = Arc::clone(&server);
        let token = Arc::clone(&shared_token);
        std::thread::spawn(move || {
            while let Ok(request) = server.recv() {
                handle_request(request, &token);
            }
        });
    }

    Ok(PlaybackEndpoint {
        origin: format!("http://127.0.0.1:{port}"),
        token,
    })
}

/// An unguessable per-launch token derived from launch time and pid. Loopback
/// binding is the primary defense; this just stops other local processes from
/// guessing the URL and reading arbitrary files through us.
fn generate_token() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut hasher = Sha256::new();
    hasher.update(nanos.to_le_bytes());
    hasher.update(std::process::id().to_le_bytes());
    let digest = hasher.finalize();
    digest[..16].iter().map(|b| format!("{b:02x}")).collect()
}

/// Route one request: validate the token, then serve the recording. By playback
/// time every recording is already web-playable (ADR 0005), so the whole-file
/// path only passes bytes through. A `&t=` request instead remuxes a fragmented
/// MP4 starting at that offset (codec-copy, no re-encode) so the webview can
/// "seek" by reloading rather than via the unreliable GStreamer seek (issue #24).
fn handle_request(request: Request, token: &str) {
    let url = request.url().to_string();
    let (route, query) = url.split_once('?').unwrap_or((url.as_str(), ""));
    if route != "/play" {
        return respond_error(request, 404, "not found");
    }

    let mut path = None;
    let mut supplied_token = None;
    let mut t = None;
    let mut frame = false;
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        let decoded = percent_decode(value);
        match key {
            "path" => path = Some(decoded),
            "token" => supplied_token = Some(decoded),
            "t" => t = Some(decoded),
            "frame" => frame = decoded == "1",
            _ => {}
        }
    }

    if supplied_token.as_deref() != Some(token) {
        return respond_error(request, 403, "forbidden");
    }
    let Some(path) = path else {
        return respond_error(request, 400, "missing `path` query parameter");
    };

    let t_secs = t.and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);

    // `frame=1` asks for a single still decoded at exactly `t`: frame-step can't
    // nudge the `<video>` by writing `currentTime` (dropped on this WebKitGTK
    // build — issue #24 / ADR 0007), so the player overlays this exact JPEG over
    // the paused video instead. Checked before the stream/file split so the
    // first-frame case (`t == 0`) still decodes a frame rather than serving the
    // whole file.
    if frame {
        return serve_frame_at(request, &path, t_secs.max(0.0));
    }

    // A positive `t` (seconds) means "open already positioned here": GStreamer
    // seeking is unreliable on this WebKitGTK build (issue #24), so the frontend
    // reloads the `<video>` at a `&t=` URL instead of writing `currentTime`. We
    // hand back a fragmented MP4 that begins at the keyframe ≤ `t`, so the webview
    // never issues a seek. `t == 0` (or absent) is the plain whole-file path.
    if t_secs > 0.0 {
        serve_stream_at(request, &path, t_secs);
    } else {
        serve_file(request, &path);
    }
}

fn respond_error(request: Request, code: u16, message: &str) {
    let _ = request.respond(Response::from_string(message).with_status_code(StatusCode(code)));
}

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes())
        .expect("static header name/value are ASCII")
}

/// Serve a recording from disk, honoring a `Range` header so the webview's
/// native seek bar works, and always declaring `video/mp4` so WebKitGTK accepts
/// it regardless of the file's extension. The body streams lazily from the file
/// handle, so a multi-gigabyte recording is never buffered in memory.
fn serve_file(request: Request, path: &str) {
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => return respond_error(request, 500, &format!("could not open recording: {e}")),
    };
    let total = match file.metadata() {
        Ok(m) => m.len(),
        Err(e) => return respond_error(request, 500, &format!("could not stat recording: {e}")),
    };

    // Parse a single `bytes=start-end` range if present; otherwise serve whole.
    let range = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Range"))
        .and_then(|h| h.value.as_str().strip_prefix("bytes=").map(str::to_string))
        .and_then(|spec| {
            let (start, end) = spec.split_once('-')?;
            let start: u64 = start.parse().ok()?;
            let end: u64 = if end.is_empty() {
                total.saturating_sub(1)
            } else {
                end.parse().ok()?
            };
            if start > end || start >= total {
                return None;
            }
            Some((start, end.min(total.saturating_sub(1))))
        });

    let result = match range {
        Some((start, end)) => {
            let len = end - start + 1;
            if file.seek(SeekFrom::Start(start)).is_err() {
                return respond_error(request, 500, "could not seek recording");
            }
            let response = Response::new(
                StatusCode(206),
                vec![
                    header("Content-Type", "video/mp4"),
                    header("Accept-Ranges", "bytes"),
                    header("Content-Range", &format!("bytes {start}-{end}/{total}")),
                ],
                file.take(len),
                Some(len as usize),
                None,
            );
            request.respond(response)
        }
        None => {
            let response = Response::new(
                StatusCode(200),
                vec![
                    header("Content-Type", "video/mp4"),
                    header("Accept-Ranges", "bytes"),
                ],
                file,
                Some(total as usize),
                None,
            );
            request.respond(response)
        }
    };
    let _ = result;
}

/// Serve a recording as a fragmented MP4 that begins at the keyframe at or before
/// `t_secs`, so the webview opens already positioned there and never issues a
/// GStreamer seek (unreliable on this WebKitGTK build — issue #24). `-ss` before
/// `-i` does a fast keyframe seek and `-c copy` remuxes without re-encoding, so
/// the cut is cheap and uses almost no CPU — which matters because every seek in
/// the review loop reloads a fresh stream; `frag_keyframe+empty_moov` makes a
/// streamable fragmented MP4 (the moov is up front, no second pass — so it works
/// over a pipe, which `+faststart` would not). ffmpeg's stdout streams straight to
/// the response body with no `Content-Length` (the cut length isn't known ahead of
/// time); the body is sent chunked. [`ChildStream`] kills and reaps ffmpeg when
/// the webview drops the connection (a new seek reloads a fresh stream), so no
/// zombie lingers.
///
/// A `-c copy` cut cannot start mid-GOP — it snaps to the keyframe ≤ `t` and the
/// stream replays from there. That snap is only acceptable because **every**
/// recording is kept at ~1s keyframes at rest: incompatible ones via the transcode
/// (ADR 0005), and web-playable-but-sparse ones are transcoded at import for this
/// very reason (a recording's native GOP can be many seconds, which made short
/// arrow-key seeks land on the same keyframe and replay the identical scene —
/// issue #24). With dense keyframes the snap is ≤~1s, the tolerance scrubbing
/// already lived with — and output timestamps reset to ~0, so the frontend's
/// `seekBaseMs + currentTime` mapping (seekBaseMs = `t`) stays correct.
fn serve_stream_at(request: Request, path: &str, t_secs: f64) {
    let mut child = match Command::new(sidecar_path("ffmpeg"))
        .args([
            "-v", "error", "-nostats",
            "-ss", &format!("{t_secs}"),
            "-i", path,
            "-c", "copy", // remux only — no re-encode, so the cut is fast and cheap
            "-movflags", "frag_keyframe+empty_moov+default_base_moof",
            "-f", "mp4",
            "pipe:1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return respond_error(request, 500, &format!("could not start ffmpeg: {e}")),
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return respond_error(request, 500, "ffmpeg produced no output stream");
    };
    let stream = ChildStream { child, stdout };
    let response = Response::new(
        StatusCode(200),
        vec![
            header("Content-Type", "video/mp4"),
            // No byte-range seeking here: this is a one-way progressive stream
            // already positioned at the target, of unknown length.
            header("Accept-Ranges", "none"),
        ],
        stream,
        None,
        None,
    );
    let _ = request.respond(response);
}

/// Serve a single still frame as a JPEG, decoded at exactly `t_secs`. Frame-step
/// (issue #19) can't advance the `<video>` a frame by writing `currentTime` —
/// this WebKitGTK build silently drops those seeks (issue #24 / ADR 0007) — so
/// the player overlays this exact still over the paused video instead. `-ss`
/// before `-i` seeks to the keyframe ≤ `t` and decodes forward to the exact
/// frame, so the JPEG is frame-accurate (unlike the `-c copy` `&t=` stream, which
/// can only start on a keyframe). It is a single-frame decode of one GOP — small
/// because every recording is kept at ~1s keyframes at rest (ADR 0005) — so the
/// per-step cost is modest. Unlike the stream, the whole JPEG is buffered (it is
/// tiny and its length must be known) rather than piped.
fn serve_frame_at(request: Request, path: &str, t_secs: f64) {
    let output = Command::new(sidecar_path("ffmpeg"))
        .args([
            "-v", "error", "-nostats",
            "-ss", &format!("{t_secs}"),
            "-i", path,
            "-frames:v", "1",
            "-an", // no audio in a still
            "-c:v", "mjpeg",
            "-q:v", "3", // visually lossless enough for frame inspection, still small
            "-f", "mjpeg",
            "pipe:1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    let output = match output {
        Ok(o) => o,
        Err(e) => return respond_error(request, 500, &format!("could not start ffmpeg: {e}")),
    };
    if !output.status.success() || output.stdout.is_empty() {
        return respond_error(request, 500, "could not decode frame");
    }
    let len = output.stdout.len();
    let response = Response::new(
        StatusCode(200),
        vec![
            header("Content-Type", "image/jpeg"),
            // A stepped frame is never the same URL twice in a row anyway, but be
            // explicit: the webview must not serve a stale still for a position.
            header("Cache-Control", "no-store"),
        ],
        std::io::Cursor::new(output.stdout),
        Some(len),
        None,
    );
    let _ = request.respond(response);
}

/// An ffmpeg child whose stdout is streamed as a response body. Reads delegate to
/// stdout; dropping the stream (the webview closed the connection, or the body
/// finished) kills and reaps the child so a long recording's ffmpeg doesn't
/// linger after the seek that spawned it is superseded (issue #24).
struct ChildStream {
    child: std::process::Child,
    stdout: std::process::ChildStdout,
}

impl Read for ChildStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.stdout.read(buf)
    }
}

impl Drop for ChildStream {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Sample rate the segmenter analyzes at (ADR 0002). 16 kHz comfortably
/// resolves the rhythm of shuttle hits while keeping an hour of mono f32 audio
/// to a manageable ~230 MB in memory.
pub const SEGMENT_SAMPLE_RATE: u32 = 16_000;

/// Extract a recording's audio as mono little-endian f32 PCM at
/// [`SEGMENT_SAMPLE_RATE`] via the ffmpeg sidecar (ADR 0004), returning the
/// decoded samples in memory for the segmenter. ffmpeg downmixes to one channel
/// and resamples for us, so the caller does no audio work. Errors if the sidecar
/// cannot run, exits non-zero, or the recording carries no decodable audio
/// (nothing on stdout) — the worker then marks the recording rather than looping.
pub fn extract_pcm(path: &str) -> Result<Vec<f32>, String> {
    let rate = SEGMENT_SAMPLE_RATE.to_string();
    let output = Command::new(sidecar_path("ffmpeg"))
        .args([
            "-v", "error", "-nostats",
            "-i", path,
            "-vn", // drop video; we only want the audio track
            "-ac", "1", // downmix to mono
            "-ar", &rate, // resample
            "-f", "f32le", // raw little-endian float samples
            "-", // write to stdout
        ])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("ffmpeg could not run: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ffmpeg failed to extract audio: {stderr}"));
    }
    if output.stdout.is_empty() {
        return Err("recording has no decodable audio track".to_string());
    }

    // Reinterpret the raw byte stream as f32 samples. A trailing partial sample
    // (impossible from clean ffmpeg output, but cheap to guard) is dropped.
    let samples = output
        .stdout
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    Ok(samples)
}

/// Frames per second the motion track is sampled at (ADR 0006). A low rate is
/// plenty to see a rally's movement envelope rise and fall, and keeps an hour of
/// downscaled grayscale frames small.
pub const MOTION_FPS: u32 = 5;
/// Downscaled frame dimensions for motion analysis. Small enough to be cheap,
/// large enough that whole-court player movement still registers.
const MOTION_WIDTH: u32 = 96;
const MOTION_HEIGHT: u32 = 54;

/// Extract a per-frame **motion** track via the ffmpeg sidecar (ADR 0004): decode
/// the video as downscaled grayscale frames at [`MOTION_FPS`] and return the mean
/// absolute pixel difference between consecutive frames. High values mean lots of
/// movement (a rally); low values mean stillness (a gap). This is the primary
/// boundary signal for segmentation (ADR 0006) and assumes a roughly static
/// camera. Returns one value per frame transition (so `frames - 1` values), or an
/// error if the sidecar cannot run, fails, or the video yields no frames.
pub fn extract_motion(path: &str) -> Result<Vec<f64>, String> {
    let vf = format!("fps={MOTION_FPS},scale={MOTION_WIDTH}:{MOTION_HEIGHT},format=gray");
    let output = Command::new(sidecar_path("ffmpeg"))
        .args([
            "-v", "error", "-nostats",
            "-i", path,
            "-an", // drop audio; the motion track is video-only
            "-vf", &vf,
            "-f", "rawvideo",
            "-pix_fmt", "gray",
            "-",
        ])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("ffmpeg could not run: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ffmpeg failed to extract frames: {stderr}"));
    }

    let frame_size = (MOTION_WIDTH * MOTION_HEIGHT) as usize;
    if output.stdout.len() < frame_size * 2 {
        return Err("video yielded too few frames for motion analysis".to_string());
    }

    // Mean absolute difference between each frame and its predecessor.
    let mut energy = Vec::with_capacity(output.stdout.len() / frame_size);
    let mut prev: Option<&[u8]> = None;
    for frame in output.stdout.chunks_exact(frame_size) {
        if let Some(previous) = prev {
            let sum: u64 = frame
                .iter()
                .zip(previous)
                .map(|(a, b)| (i32::from(*a) - i32::from(*b)).unsigned_abs() as u64)
                .sum();
            energy.push(sum as f64 / frame_size as f64);
        }
        prev = Some(frame);
    }
    Ok(energy)
}

/// What a probe tells the importer about a recording. libmpv plays any codec and
/// seeks sparse GOPs (ADR 0008), so there is no longer a transcode decision — the
/// only thing probed here is the frame rate the player frame-steps by.
pub struct Probe {
    /// Frames per second of the video stream, parsed from ffprobe's
    /// `avg_frame_rate` rational (issue #19), so the player can frame-step
    /// exactly. `None` when ffprobe reports no usable rate; the player then
    /// defaults to 30 fps.
    pub fps: Option<f64>,
}

/// Resolve a bundled sidecar binary. Tauri places `externalBin` next to the
/// app executable (and copies them beside the dev binary), so we look there.
fn sidecar_path(name: &str) -> PathBuf {
    let file = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(&file)))
        .unwrap_or_else(|| PathBuf::from(file))
}

/// Probe a recording with the `ffprobe` sidecar for the frame rate the player
/// frame-steps by (issue #19). libmpv plays any codec (ADR 0008), so there is no
/// transcode decision left to make. Returns an error string if the probe fails
/// entirely (missing file, unreadable, sidecar not resolvable) so the caller can
/// mark the recording rather than silently retry forever.
pub fn probe(path: &str) -> Result<Probe, String> {
    let output = Command::new(sidecar_path("ffprobe"))
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type,avg_frame_rate",
            "-of",
            "default=noprint_wrappers=1:nokey=0",
            path,
        ])
        .output()
        .map_err(|e| format!("ffprobe could not run: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ffprobe could not read the file: {stderr}"));
    }

    let report = String::from_utf8_lossy(&output.stdout);
    Ok(Probe {
        fps: parse_fps(&report),
    })
}

/// Parse the video stream's frame rate from an ffprobe report (issue #19).
/// ffprobe emits `avg_frame_rate` as a rational such as `30000/1001` (29.97) or
/// `0/0` for a stream with no usable rate (e.g. an audio-only stream's line).
/// Returns the first positive rate found, or `None` when no video stream
/// reports one — the player then defaults to 30 fps.
fn parse_fps(report: &str) -> Option<f64> {
    for line in report.lines() {
        let Some(("avg_frame_rate", value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        let (num, den) = value.split_once('/').unwrap_or((value, "1"));
        let (Ok(num), Ok(den)) = (num.parse::<f64>(), den.parse::<f64>()) else {
            continue;
        };
        if num > 0.0 && den > 0.0 {
            return Some(num / den);
        }
    }
    None
}

/// The capture date a recording carries in its container metadata. Both fields
/// are the raw tag values (ISO-8601-ish strings) when present, so the caller
/// decides precedence and parsing. Empty when the recording carries neither tag.
#[derive(Debug, Default, PartialEq)]
pub struct CaptureDate {
    /// `com.apple.quicktime.creationdate` — local wall-clock time **with** the
    /// camera's UTC offset (e.g. `2024-03-15T21:30:00+0800`). iPhone footage. The
    /// best source for "which day did the player record on": its date portion is
    /// already the local day, so a late-evening session does not roll into the
    /// next day the way a UTC timestamp would.
    pub quicktime_creationdate: Option<String>,
    /// `creation_time` — ISO-8601 in **UTC** (e.g. `2024-03-15T13:30:00.000000Z`).
    /// Present on most cameras. Grouped by its UTC day when the offset-bearing tag
    /// above is absent.
    pub creation_time: Option<String>,
}

/// Read a recording's container capture-date tags with the `ffprobe` sidecar.
/// Errors only when ffprobe cannot run or read the file at all; a file that
/// simply carries no date tags yields an empty [`CaptureDate`], not an error, so
/// the caller falls back to the file's mtime rather than retrying forever.
pub fn probe_capture_date(path: &str) -> Result<CaptureDate, String> {
    let output = Command::new(sidecar_path("ffprobe"))
        .args([
            "-v",
            "error",
            "-show_entries",
            "format_tags=creation_time,com.apple.quicktime.creationdate",
            "-of",
            "default=noprint_wrappers=1:nokey=0",
            path,
        ])
        .output()
        .map_err(|e| format!("ffprobe could not run: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ffprobe could not read the file: {stderr}"));
    }

    Ok(parse_capture_date(&String::from_utf8_lossy(&output.stdout)))
}

/// Parse the capture-date tags out of an ffprobe `format_tags` report. ffprobe
/// prints each tag as `TAG:<key>=<value>`; we pick out the two date keys and
/// ignore the rest. Split out from [`probe_capture_date`] so the parsing is
/// unit-testable without invoking the sidecar.
fn parse_capture_date(report: &str) -> CaptureDate {
    let mut date = CaptureDate::default();
    for line in report.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.strip_prefix("TAG:").unwrap_or(key).trim();
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        match key {
            "com.apple.quicktime.creationdate" => {
                date.quicktime_creationdate = Some(value.to_string());
            }
            "creation_time" => date.creation_time = Some(value.to_string()),
            _ => {}
        }
    }
    date
}

/// Minimal percent-decoding for query values (avoids a url-crate dependency).
/// Handles `%XX` and `+`.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    out.push((hi * 16 + lo) as u8);
                    i += 3;
                    continue;
                }
                out.push(bytes[i]);
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::{
        parse_capture_date, parse_fps, percent_decode, start, CaptureDate,
    };
    use std::io::{Read, Write};
    use std::net::TcpStream;

    /// ffprobe prints date tags as `TAG:<key>=<value>`; the parser must pull both
    /// the offset-bearing Apple tag (as real iPhone footage carries it) and the
    /// generic UTC `creation_time`, ignoring unrelated tags.
    #[test]
    fn parses_both_capture_date_tags() {
        let report = "\
TAG:major_brand=qt
TAG:creation_time=2024-03-15T13:30:00.000000Z
TAG:com.apple.quicktime.creationdate=2024-03-15T21:30:00+0800
TAG:encoder=Lavf62.12.102";
        assert_eq!(
            parse_capture_date(report),
            CaptureDate {
                quicktime_creationdate: Some("2024-03-15T21:30:00+0800".to_string()),
                creation_time: Some("2024-03-15T13:30:00.000000Z".to_string()),
            }
        );
    }

    /// A recording with no date tags parses to an empty result (caller falls back
    /// to mtime), not an error.
    #[test]
    fn parses_missing_capture_date_as_empty() {
        let report = "TAG:major_brand=isom\nTAG:encoder=Lavf62.12.102";
        assert_eq!(parse_capture_date(report), CaptureDate::default());
    }

    #[test]
    fn parses_rational_frame_rate() {
        let report = "\
codec_name=h264
codec_type=video
avg_frame_rate=30000/1001
codec_name=aac
codec_type=audio
avg_frame_rate=0/0";
        let fps = parse_fps(report).unwrap();
        assert!((fps - 29.97).abs() < 0.01, "fps was {fps}");
    }

    #[test]
    fn missing_frame_rate_is_none() {
        let report = "\
codec_name=h264
codec_type=video
avg_frame_rate=0/0";
        assert!(parse_fps(report).is_none());
    }

    #[test]
    fn percent_decode_handles_paths_and_spaces() {
        assert_eq!(percent_decode("%2Fhome%2Fa+b.mov"), "/home/a b.mov");
    }


    /// End-to-end: the loopback server serves a ranged `video/mp4` body to a real
    /// TCP client (what WebKitGTK's GStreamer source does), and rejects a bad
    /// token. Guards the transport that actually makes playback work.
    #[test]
    fn server_serves_ranged_mp4_and_checks_token() {
        let dir = std::env::temp_dir();
        let path = dir.join("voloph-server-test.bin");
        let data: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
        std::fs::File::create(&path)
            .unwrap()
            .write_all(&data)
            .unwrap();

        let endpoint = start().unwrap();
        let host = endpoint.origin.strip_prefix("http://").unwrap().to_string();
        let encoded: String = path
            .to_string_lossy()
            .bytes()
            .map(|b| format!("%{b:02X}"))
            .collect();

        // Valid token + range → 206 with the exact slice and video/mp4.
        let ok = http_get(
            &host,
            &format!("/play?path={encoded}&token={}", endpoint.token),
            Some("bytes=10-19"),
        );
        assert!(ok.status_line.contains("206"), "status: {}", ok.status_line);
        assert!(ok.headers.to_lowercase().contains("content-type: video/mp4"));
        assert!(ok.headers.contains("Content-Range: bytes 10-19/1000"));
        assert_eq!(ok.body, data[10..=19]);

        // Wrong token → 403, no bytes served.
        let denied = http_get(
            &host,
            &format!("/play?path={encoded}&token=wrong"),
            Some("bytes=0-9"),
        );
        assert!(denied.status_line.contains("403"), "status: {}", denied.status_line);

        let _ = std::fs::remove_file(&path);
    }

    struct HttpResponse {
        status_line: String,
        headers: String,
        body: Vec<u8>,
    }

    /// Bare-bones HTTP/1.0 GET so the test needs no HTTP-client dependency.
    fn http_get(host: &str, target: &str, range: Option<&str>) -> HttpResponse {
        let mut stream = TcpStream::connect(host).unwrap();
        let mut req = format!("GET {target} HTTP/1.0\r\nHost: {host}\r\n");
        if let Some(r) = range {
            req.push_str(&format!("Range: {r}\r\n"));
        }
        req.push_str("\r\n");
        stream.write_all(req.as_bytes()).unwrap();

        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).unwrap();
        let split = raw
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("response has header/body separator");
        let head = String::from_utf8_lossy(&raw[..split]).into_owned();
        let body = raw[split + 4..].to_vec();
        let (status_line, headers) = head.split_once("\r\n").unwrap_or((head.as_str(), ""));
        HttpResponse {
            status_line: status_line.to_string(),
            headers: headers.to_string(),
            body,
        }
    }
}
