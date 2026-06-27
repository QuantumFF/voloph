//! Playback source for the in-app player: a loopback HTTP server + in-place
//! codec normalization.
//!
//! WebKitGTK on Linux cannot decode HEVC/H.265 (the iPhone default) in a
//! `<video>` element, even though the OS can. Each web-incompatible recording is
//! therefore transcoded **once, in place**, to a complete, seekable H.264/AAC
//! file that replaces the original at its path (ADR 0005). The transcode is
//! destructive — the source codec is discarded — which ADRs 0001 and 0003 permit
//! only as one-time import normalization.
//!
//! The playable file is served to the webview over a tiny HTTP server bound to
//! `127.0.0.1` on an ephemeral port — **not** the asset protocol or a custom URI
//! scheme. WebKitGTK plays HTML5 media through GStreamer, which loads from real
//! `http://` sources (with byte-range seeking via `souphttpsrc`) but does not
//! route through WebKit's custom-scheme handlers, so `asset://`/`stream://`
//! sources fail with `MediaError` code 4. Serving over loopback HTTP, with an
//! explicit `video/mp4` Content-Type and range support, is what actually plays.
//!
//! ffmpeg/ffprobe are the bundled sidecars from ADR 0004.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, OnceLock};
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

/// Longest tolerable gap between keyframes (seconds) for a recording to be played
/// without transcoding. Seeking reloads a `&t=` stream that `-c copy` snaps to the
/// keyframe ≤ `t` (issue #24); the snap is only acceptable while keyframes are
/// dense, so a web-playable file whose native GOP exceeds this is transcoded at
/// import to the forced ~1s keyframes (ADR 0005) like an incompatible one. Set a
/// little above the forced ~1s so an already-dense file (or one with the odd
/// scene-cut keyframe) is not needlessly re-encoded.
const MAX_KEYFRAME_GAP_SECS: f64 = 2.0;

/// How far into the video to read packets when measuring the keyframe gap. A short
/// window is enough to characterize a constant-GOP camera recording and keeps the
/// probe cheap; a file with a single keyframe in this window has a GOP at least
/// this long and is treated as sparse.
const KEYFRAME_PROBE_WINDOW_SECS: u32 = 12;

/// What a probe tells us about whether a source can play in the webview as-is.
pub struct Probe {
    /// True when the source can play directly: web-playable container + codecs
    /// (H.264/AAC in mp4/mov) **and** keyframes dense enough for copy-based seeking
    /// (issue #24). Otherwise it is transcoded in place (ADR 0005).
    pub passthrough: bool,
    /// Frames per second of the video stream, parsed from ffprobe's
    /// `avg_frame_rate` rational (issue #19), so the player can frame-step
    /// exactly. `None` when ffprobe reports no usable rate; the player then
    /// defaults to 30 fps. The in-place transcode does not resample, so this
    /// stays valid afterward.
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

/// Probe a recording with the `ffprobe` sidecar to decide transcode vs direct
/// play. Returns an error string if the probe fails entirely (missing file,
/// unreadable, sidecar not resolvable) so the caller can mark the recording
/// rather than silently retry forever.
pub fn probe(path: &str) -> Result<Probe, String> {
    let output = Command::new(sidecar_path("ffprobe"))
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type,codec_name,avg_frame_rate:format=format_name",
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
    // A file is only safe to play directly when it is web-playable *and* its
    // keyframes are dense enough for copy-based seeking (issue #24). Skip the
    // keyframe read for files already bound for transcode — the density is moot.
    let passthrough = is_web_playable(&report)
        && keyframes_dense_enough(probe_keyframe_gap(path));
    Ok(Probe {
        passthrough,
        fps: parse_fps(&report),
    })
}

/// Whether the largest keyframe gap (seconds) is within [`MAX_KEYFRAME_GAP_SECS`].
/// `None` (the gap could not be measured) is treated as dense: an unreadable
/// keyframe layout is not worth a destructive re-encode on a file that already
/// decodes.
fn keyframes_dense_enough(max_gap: Option<f64>) -> bool {
    max_gap.map(|g| g <= MAX_KEYFRAME_GAP_SECS).unwrap_or(true)
}

/// Measure the largest keyframe gap in the first [`KEYFRAME_PROBE_WINDOW_SECS`] of
/// the video via the `ffprobe` sidecar. Returns `None` if the probe cannot run or
/// reports no keyframe, so the caller falls back to "dense" rather than transcode
/// on uncertainty.
fn probe_keyframe_gap(path: &str) -> Option<f64> {
    let output = Command::new(sidecar_path("ffprobe"))
        .args([
            "-v", "error",
            "-read_intervals", &format!("%+{KEYFRAME_PROBE_WINDOW_SECS}"),
            "-select_streams", "v:0",
            "-show_entries", "packet=pts_time,flags",
            "-of", "csv=p=0",
            path,
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    max_keyframe_gap(&String::from_utf8_lossy(&output.stdout))
}

/// Largest gap between consecutive keyframes in an ffprobe `packet=pts_time,flags`
/// CSV (a keyframe is a packet whose flags contain `K`). The gap from time 0 to
/// the first keyframe is included. Fewer than two keyframes in the read window
/// means the GOP is at least the window long, reported as [`f64::INFINITY`] so it
/// reads as sparse; no keyframe at all is `None` (unmeasurable).
fn max_keyframe_gap(report: &str) -> Option<f64> {
    let mut times = Vec::new();
    for line in report.lines() {
        let mut fields = line.split(',');
        let Some(pts) = fields.next() else { continue };
        let is_key = fields.any(|f| f.contains('K'));
        if !is_key {
            continue;
        }
        if let Ok(t) = pts.trim().parse::<f64>() {
            times.push(t);
        }
    }
    if times.is_empty() {
        return None;
    }
    if times.len() < 2 {
        return Some(f64::INFINITY);
    }
    let mut max = times[0].max(0.0);
    for pair in times.windows(2) {
        max = max.max(pair[1] - pair[0]);
    }
    Some(max)
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

/// Decide whether an ffprobe report describes a source the webview can play
/// directly: an mp4/mov-family container carrying H.264 video and, if present,
/// AAC audio. Anything else (HEVC, AV1, mkv, AC-3 audio, …) needs transcoding.
fn is_web_playable(report: &str) -> bool {
    let mut web_container = false;
    let mut video_codecs = Vec::new();
    let mut audio_codecs = Vec::new();
    // ffprobe emits `codec_name` and `codec_type` for each stream in an order
    // we do not control, so we hold the most recent name until its type is
    // known and pair them then. A new name before a type means an unnamed
    // stream — drop the stale name.
    let mut pending_name: Option<String> = None;

    for line in report.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key {
            "format_name" => {
                // ffprobe lists comma-separated container names, e.g.
                // "mov,mp4,m4a,3gp,3g2,mj2".
                web_container = value
                    .split(',')
                    .any(|name| matches!(name, "mp4" | "mov" | "m4a" | "3gp" | "3g2"));
            }
            "codec_name" => pending_name = Some(value.to_string()),
            "codec_type" => {
                if let Some(name) = pending_name.take() {
                    match value {
                        "video" => video_codecs.push(name),
                        "audio" => audio_codecs.push(name),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    let video_ok = video_codecs.iter().all(|c| c == "h264") && !video_codecs.is_empty();
    let audio_ok = audio_codecs.iter().all(|c| c == "aac");
    web_container && video_ok && audio_ok
}

/// Which H.264 encoder the transcode drives, resolved once against the host's
/// actual hardware. The bundled ffmpeg (ADR 0004) is compiled with NVENC, VAAPI,
/// QSV and AMF, but "compiled in" is not "usable": a box may carry the codec yet
/// have no device that can open it (an iGPU with no H.264 encode entrypoint fails
/// at encoder-open, not at the encoder list). So each GPU candidate is confirmed
/// with a throwaway test encode before we trust it for real recordings; anything
/// that fails — or a machine with no GPU at all — falls back to software libx264,
/// which always works. A working GPU encoder cuts a transcode to roughly a third
/// of the software time on high-motion footage.
#[derive(Clone, Debug, PartialEq)]
enum Encoder {
    /// NVIDIA NVENC. Software-decode + GPU-encode (not a full `-hwaccel cuda`
    /// pipeline): marginally slower than keeping frames on the GPU, but it decodes
    /// anything ffmpeg can and lets us force 8-bit `yuv420p`, so 10-bit HEVC
    /// (iPhone HDR) still lands as web-playable 8-bit H.264.
    Nvenc,
    /// VAAPI on a specific render node (Intel/AMD iGPU). The `hwupload` filter
    /// also normalizes to 8-bit `nv12` for the same web-playability reason.
    Vaapi(String),
    /// CPU libx264 `veryfast`. The universal fallback — no hardware required.
    Software,
}

/// The fastest working H.264 encoder for this host, probed once and cached for
/// the process lifetime (the answer cannot change while the app runs). Probed
/// lazily on the first transcode, off the UI thread in the media worker.
fn encoder() -> &'static Encoder {
    static ENCODER: OnceLock<Encoder> = OnceLock::new();
    ENCODER.get_or_init(detect_encoder)
}

/// Find the fastest usable encoder: prefer NVENC, then VAAPI on whichever render
/// node opens, else software. Each GPU candidate is confirmed with a real (tiny)
/// test encode — listing `-encoders` only says what was compiled in, not what the
/// hardware will accept (VAAPI on a machine whose iGPU lacks an H.264 entrypoint
/// lists the codec but fails to open it).
fn detect_encoder() -> Encoder {
    if encoder_works(&Encoder::Nvenc) {
        log::info!("transcode: using NVENC hardware encoder");
        return Encoder::Nvenc;
    }
    for node in ["/dev/dri/renderD128", "/dev/dri/renderD129"] {
        if Path::new(node).exists() {
            let candidate = Encoder::Vaapi(node.to_string());
            if encoder_works(&candidate) {
                log::info!("transcode: using VAAPI hardware encoder ({node})");
                return candidate;
            }
        }
    }
    log::info!("transcode: no usable GPU encoder; using software libx264");
    Encoder::Software
}

/// Confirm `enc` can actually open and encode on this host by running a fraction
/// of a second of a generated source through it to the null muxer. Cheap and the
/// only reliable signal — it exercises the real encoder-open path. Failure (or an
/// unrunnable sidecar) reads as "not usable".
fn encoder_works(enc: &Encoder) -> bool {
    let mut cmd = Command::new(sidecar_path("ffmpeg"));
    cmd.args(["-v", "error", "-nostats", "-y"]);
    cmd.args(input_args(enc));
    cmd.args(["-f", "lavfi", "-i", "color=c=black:s=320x240:r=30:d=0.2"]);
    cmd.args(video_args(enc));
    cmd.args(["-f", "null", "-"]);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    matches!(cmd.status(), Ok(s) if s.success())
}

/// Transcode the recording at `path` to a complete, seekable H.264/AAC file and
/// replace the original in place via the ffmpeg sidecar. The source codec is
/// discarded (ADR 0005). Writes to a hidden temp file in the same directory and
/// atomically renames over the original, so an interrupted run never leaves a
/// recording half-written. `+faststart` moves the moov atom to the front for
/// instant playback; the output is a normal (non-fragmented) mp4 with a real
/// duration. The path (and its extension) are preserved — the playback server
/// declares `video/mp4` regardless of extension. Keyframes are forced ~once per
/// second so arbitrary scrub seeks land near a keyframe and stay smooth (ADR 0005).
///
/// Uses the host's fastest usable encoder ([`encoder`]). A GPU encoder that
/// passed the startup probe can still choke on a particular recording (a busy
/// device, an exotic input), so a GPU failure falls back to software libx264 for
/// that one file rather than marking it failed.
pub fn transcode_in_place(path: &str) -> Result<(), String> {
    let src = Path::new(path);
    let parent = src
        .parent()
        .ok_or_else(|| "recording has no parent directory".to_string())?;
    let file_name = src
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| "recording has no file name".to_string())?;
    // Hidden, same-directory temp so the rename is atomic (same filesystem) and
    // the in-progress file is neither user-visible nor picked up by a re-scan.
    let temp = parent.join(format!(".{file_name}.voloph-transcoding.tmp"));

    let enc = encoder();
    match run_transcode(enc, path, &temp) {
        Ok(()) => {}
        Err(e) if *enc != Encoder::Software => {
            log::warn!(
                "transcode: {enc:?} failed for {path} ({e}); retrying with software libx264"
            );
            run_transcode(&Encoder::Software, path, &temp)?;
        }
        Err(e) => return Err(e),
    }

    std::fs::rename(&temp, src).map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        format!("could not replace original with transcode: {e}")
    })?;
    Ok(())
}

/// Run one ffmpeg transcode of `path` into `temp` with `enc`. On failure the
/// partial temp is removed (so a fallback retry starts clean) and the sidecar's
/// stderr is surfaced.
fn run_transcode(enc: &Encoder, path: &str, temp: &Path) -> Result<(), String> {
    let output = Command::new(sidecar_path("ffmpeg"))
        .args(transcode_args(enc, path, temp))
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("ffmpeg could not run: {e}"))?;

    if !output.status.success() {
        let _ = std::fs::remove_file(temp);
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ffmpeg failed to transcode: {stderr}"));
    }
    Ok(())
}

/// Encoder-specific flags that must precede `-i` (input options). VAAPI opens its
/// device before the input; the others need nothing here.
fn input_args(enc: &Encoder) -> Vec<String> {
    match enc {
        Encoder::Vaapi(node) => vec!["-vaapi_device".to_string(), node.clone()],
        Encoder::Nvenc | Encoder::Software => vec![],
    }
}

/// The video-encode flags for `enc`: the codec, its speed/quality knob, and the
/// pixel-format handling that guarantees 8-bit, web-playable H.264 even from
/// 10-bit HEVC. Software uses `veryfast` at the default CRF; NVENC targets the
/// same nominal quality with `-cq`; VAAPI normalizes to `nv12` on upload.
fn video_args(enc: &Encoder) -> Vec<String> {
    match enc {
        Encoder::Nvenc => vec![
            "-c:v".into(),
            "h264_nvenc".into(),
            "-preset".into(),
            "p4".into(),
            "-cq".into(),
            "23".into(),
            "-pix_fmt".into(),
            "yuv420p".into(),
            // NVENC ignores the shared `-force_key_frames` unless forced keyframes
            // are emitted as IDR frames; without this it falls back to its sparse
            // default GOP and copy-based seeks snap a whole GOP, replaying the same
            // scene (issue #24 regression from the GPU-encoder change, #29). libx264
            // honors `-force_key_frames` on its own, so this is NVENC-only.
            "-forced-idr".into(),
            "1".into(),
        ],
        Encoder::Vaapi(_) => vec![
            "-vf".into(),
            "format=nv12,hwupload".into(),
            "-c:v".into(),
            "h264_vaapi".into(),
        ],
        Encoder::Software => vec![
            "-c:v".into(),
            "libx264".into(),
            "-preset".into(),
            "veryfast".into(),
            "-pix_fmt".into(),
            "yuv420p".into(),
        ],
    }
}

/// The full ffmpeg argument vector that transcodes `path` to `out` as
/// web-playable H.264/AAC with `enc`, **preserving the source's metadata** across
/// the rewrite.
///
/// A QuickTime recording (iPhone footage) carries a "Create Date", camera
/// make/model, GPS and similar tags. ffmpeg silently drops most of these on a
/// mov→mp4 rewrite unless told to keep them (issue #25), which matters because
/// the recording is replaced in place (ADR 0005) — once the transcode lands the
/// original metadata is gone for good, including the capture date the app means
/// to group sessions by. Two flags together preserve it:
///   * `-map_metadata 0` keeps recognized tags such as `creation_time`, which
///     the mp4 muxer otherwise blanks (writing `0000-00-00`) rather than copying.
///   * `+use_metadata_tags` keeps arbitrary/unrecognized tags (make, model, …)
///     that the muxer would otherwise discard as not part of its known set.
///
/// `+faststart` moves the moov atom to the front for instant playback. `-f mp4`
/// forces the muxer since the temp file's `.tmp` extension can't. The per-encoder
/// codec flags come from [`video_args`]; the metadata, container, audio and
/// forced-keyframe flags are shared so they hold no matter which encoder ran.
///
/// Keyframes are forced roughly once per second (`expr:gte(t,n_forced*1)`,
/// fps-independent) so an arbitrary scrub seek decodes forward from a nearby
/// keyframe rather than the encoder's sparse default GOP (~250 frames ≈ 8s).
/// This is a correctness requirement of the copy-based seek mechanism, not just
/// smoothness (ADR 0005, issue #24), so it must hold across every encoder.
fn transcode_args(enc: &Encoder, path: &str, out: &Path) -> Vec<String> {
    let mut args = vec!["-v".into(), "error".into(), "-nostats".into(), "-y".into()];
    args.extend(input_args(enc));
    args.push("-i".into());
    args.push(path.to_string());
    args.extend(["-map_metadata".into(), "0".into()]);
    args.extend(video_args(enc));
    args.extend(["-force_key_frames".into(), "expr:gte(t,n_forced*1)".into()]);
    args.extend(["-c:a".into(), "aac".into()]);
    args.extend(["-movflags".into(), "+faststart+use_metadata_tags".into()]);
    args.extend(["-f".into(), "mp4".into()]);
    args.push(out.to_string_lossy().into_owned());
    args
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
        detect_encoder, input_args, is_web_playable, keyframes_dense_enough, max_keyframe_gap,
        parse_capture_date, parse_fps, percent_decode, probe, sidecar_path, start, transcode_args,
        transcode_in_place, video_args, CaptureDate, Encoder, MAX_KEYFRAME_GAP_SECS,
    };
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::path::{Path, PathBuf};
    use std::process::Stdio;

    // codec_name precedes codec_type per stream in ffprobe's real output.
    const H264_AAC_MP4: &str = "\
codec_name=h264
codec_type=video
codec_name=aac
codec_type=audio
format_name=mov,mp4,m4a,3gp,3g2,mj2";

    // An iPhone HEVC recording: hevc video, aac audio, plus metadata `data`
    // tracks with unknown codec names.
    const HEVC_MOV: &str = "\
codec_name=hevc
codec_type=video
codec_name=aac
codec_type=audio
codec_name=unknown
codec_type=data
format_name=mov,mp4,m4a,3gp,3g2,mj2";

    #[test]
    fn h264_aac_mp4_passes_through() {
        assert!(is_web_playable(H264_AAC_MP4));
    }

    #[test]
    fn hevc_is_transcoded() {
        assert!(!is_web_playable(HEVC_MOV));
    }

    #[test]
    fn h264_in_mkv_is_transcoded() {
        let report = "\
codec_name=h264
codec_type=video
codec_name=aac
codec_type=audio
format_name=matroska,webm";
        assert!(!is_web_playable(report));
    }

    #[test]
    fn non_aac_audio_is_transcoded() {
        let report = "\
codec_name=h264
codec_type=video
codec_name=ac3
codec_type=audio
format_name=mov,mp4,m4a,3gp,3g2,mj2";
        assert!(!is_web_playable(report));
    }

    /// Whatever encoder is chosen, the transcode must carry the source's metadata
    /// across the in-place rewrite (issue #25): without `-map_metadata 0` ffmpeg
    /// blanks the "Create Date" on a mov→mp4 transcode, and without
    /// `use_metadata_tags` it drops camera make/model and similar tags. These
    /// shared flags — plus a forced mp4 muxer (the `.tmp` temp has no usable
    /// extension), AAC audio, and faststart — must hold across every encoder, so
    /// switching to a GPU encoder for speed never regresses playability or the
    /// capture date that sessions group by (ADR 0005/0007).
    #[test]
    fn transcode_preserves_source_metadata_for_every_encoder() {
        let out = Path::new("/tmp/.in.mov.voloph-transcoding.tmp");
        for enc in [
            Encoder::Software,
            Encoder::Nvenc,
            Encoder::Vaapi("/dev/dri/renderD128".to_string()),
        ] {
            let args = transcode_args(&enc, "/in.mov", out);
            let pos = |needle: &str| args.iter().position(|a| a == needle);

            let metadata_idx = pos("-map_metadata");
            assert_eq!(
                metadata_idx.and_then(|i| args.get(i + 1)).map(String::as_str),
                Some("0"),
                "{enc:?}: must pass `-map_metadata 0` to keep creation_time"
            );
            let movflags = pos("-movflags")
                .and_then(|i| args.get(i + 1))
                .map(String::as_str)
                .unwrap_or("");
            assert!(
                movflags.contains("use_metadata_tags"),
                "{enc:?}: must pass `use_metadata_tags` to keep make/model, got `{movflags}`"
            );
            assert!(movflags.contains("faststart"), "{enc:?}: lost +faststart");
            assert!(
                args.iter().any(|a| a == "aac"),
                "{enc:?}: lost AAC audio"
            );
            assert!(
                args.windows(2).any(|w| w == ["-f", "mp4"]),
                "{enc:?}: lost forced mp4 muxer (temp file has no usable extension)"
            );
            assert_eq!(
                args.last().map(String::as_str),
                out.to_str(),
                "{enc:?}: output path must be the final argument"
            );
        }
    }

    /// Each encoder selects the right H.264 codec and the pixel-format handling
    /// that keeps 10-bit HEVC (iPhone HDR) coming out as 8-bit, web-playable
    /// H.264: software/NVENC force `yuv420p`, VAAPI normalizes to `nv12` on
    /// upload. A regression here means either no speedup (wrong codec) or
    /// unplayable 10-bit output.
    #[test]
    fn each_encoder_picks_its_codec_and_keeps_8bit() {
        let sw = video_args(&Encoder::Software);
        assert!(sw.iter().any(|a| a == "libx264"));
        assert!(sw.windows(2).any(|w| w == ["-pix_fmt", "yuv420p"]));

        let nv = video_args(&Encoder::Nvenc);
        assert!(nv.iter().any(|a| a == "h264_nvenc"));
        assert!(
            nv.windows(2).any(|w| w == ["-pix_fmt", "yuv420p"]),
            "NVENC must force 8-bit yuv420p so 10-bit HEVC stays web-playable"
        );
        assert!(
            nv.windows(2).any(|w| w == ["-forced-idr", "1"]),
            "NVENC ignores the shared -force_key_frames without -forced-idr 1, so it \
             would emit a sparse default GOP and break copy-based seeking (issue #24)"
        );

        let va = video_args(&Encoder::Vaapi("/dev/dri/renderD128".to_string()));
        assert!(va.iter().any(|a| a == "h264_vaapi"));
        assert!(
            va.iter().any(|a| a.contains("nv12") && a.contains("hwupload")),
            "VAAPI must hwupload as nv12 (8-bit) for web playability"
        );
    }

    /// End-to-end against the real ffmpeg sidecar: a HEVC source is transcoded in
    /// place to seekable 8-bit H.264/AAC with its capture date and make/model
    /// intact, exercising the actual encoder detection, fallback wiring and atomic
    /// rename — not just the assembled args. Ignored by default (needs the bundled
    /// sidecar and is slower than a unit test); run with `--ignored`. Self-skips
    /// if the sidecar can't be located next to the test binary.
    #[test]
    #[ignore = "needs the ffmpeg sidecar; run with --ignored"]
    fn transcodes_a_real_hevc_file_in_place() {
        // The sidecar resolves next to current_exe; provision it there from the
        // committed repo binaries if missing (the test binary lives in deps/).
        let exe_dir = std::env::current_exe().unwrap().parent().unwrap().to_path_buf();
        for name in ["ffmpeg", "ffprobe"] {
            let dest = sidecar_path(name);
            if !dest.exists() {
                let repo = exe_dir
                    .ancestors()
                    .map(|a| a.join(format!("binaries/{name}-x86_64-unknown-linux-gnu")))
                    .find(|p| p.exists());
                let Some(src) = repo.or_else(|| {
                    // also try the workspace src-tauri/binaries relative to CARGO_MANIFEST_DIR
                    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join(format!("binaries/{name}-x86_64-unknown-linux-gnu"));
                    p.exists().then_some(p)
                }) else {
                    eprintln!("skipping: sidecar {name} not found");
                    return;
                };
                std::fs::copy(&src, &dest).unwrap();
            }
        }
        let ff = sidecar_path("ffmpeg");
        let ffprobe = sidecar_path("ffprobe");

        // A short HEVC/AAC clip carrying the tags sessions group by.
        let dir = std::env::temp_dir();
        let src = dir.join("voloph-transcode-it.mov");
        let _ = std::fs::remove_file(&src);
        let status = std::process::Command::new(&ff)
            .args([
                "-v", "error", "-y",
                // A sparse-GOP source (keyframes ~4s apart) so a transcode that fails
                // to force dense keyframes is visibly distinguishable from one that does.
                "-f", "lavfi", "-i", "testsrc2=s=640x360:r=30:d=4",
                "-f", "lavfi", "-i", "sine=frequency=440:duration=4",
                "-c:v", "libx265", "-tag:v", "hvc1", "-pix_fmt", "yuv420p10le",
                "-g", "120", "-keyint_min", "120", "-sc_threshold", "0",
                "-c:a", "aac",
                "-metadata", "creation_time=2024-03-15T13:30:00.000000Z",
                "-metadata", "make=Apple", "-metadata", "model=iPhone 15 Pro",
            ])
            .arg(&src)
            .status()
            .unwrap();
        assert!(status.success(), "could not synthesize HEVC fixture");

        // Detection must land on something usable (NVENC here, but any is fine).
        eprintln!("detected encoder: {:?}", detect_encoder());

        transcode_in_place(src.to_str().unwrap()).expect("transcode failed");

        // The file at the same path is now 8-bit H.264/AAC with metadata intact.
        let report = std::process::Command::new(&ffprobe)
            .args([
                "-v", "error",
                "-show_entries",
                "stream=codec_name,pix_fmt:format_tags=creation_time,make,model",
                "-of", "default=noprint_wrappers=1:nokey=0",
            ])
            .arg(&src)
            .output()
            .unwrap();
        let report = String::from_utf8_lossy(&report.stdout);
        assert!(report.contains("codec_name=h264"), "not H.264:\n{report}");
        assert!(report.contains("codec_name=aac"), "not AAC:\n{report}");
        assert!(report.contains("pix_fmt=yuv420p"), "not 8-bit yuv420p:\n{report}");
        assert!(
            report.contains("creation_time=2024-03-15"),
            "lost capture date:\n{report}"
        );
        assert!(report.contains("model=iPhone 15 Pro"), "lost model:\n{report}");

        // The transcode must force ~1s keyframes regardless of which encoder ran
        // (issue #24): without dense keyframes a copy-based `&t=` seek snaps back a
        // whole GOP and replays the same scene. NVENC in particular ignores
        // `-force_key_frames` unless `-forced-idr 1` is set (#29 regression), so a
        // sparse-GOP source coming out dense is the real signal here.
        let packets = std::process::Command::new(&ffprobe)
            .args([
                "-v", "error",
                "-select_streams", "v:0",
                "-show_entries", "packet=pts_time,flags",
                "-of", "csv=p=0",
            ])
            .arg(&src)
            .output()
            .unwrap();
        let gap = max_keyframe_gap(&String::from_utf8_lossy(&packets.stdout));
        assert!(
            matches!(gap, Some(g) if g <= MAX_KEYFRAME_GAP_SECS),
            "transcode left a sparse GOP (max keyframe gap {gap:?}s > {MAX_KEYFRAME_GAP_SECS}s); \
             copy-based seeking would replay the same scene (issue #24)"
        );

        let _ = std::fs::remove_file(&src);
    }

    /// VAAPI needs its device opened *before* the input; the other encoders take
    /// no pre-input flags. Wrong ordering means ffmpeg can't find the device.
    #[test]
    fn vaapi_opens_its_device_before_input() {
        let va = input_args(&Encoder::Vaapi("/dev/dri/renderD129".to_string()));
        assert_eq!(va, vec!["-vaapi_device", "/dev/dri/renderD129"]);
        assert!(input_args(&Encoder::Nvenc).is_empty());
        assert!(input_args(&Encoder::Software).is_empty());

        // And in the full command the device really precedes `-i`.
        let args = transcode_args(
            &Encoder::Vaapi("/dev/dri/renderD129".to_string()),
            "/in.mov",
            Path::new("/out.tmp"),
        );
        let dev = args.iter().position(|a| a == "-vaapi_device").unwrap();
        let input = args.iter().position(|a| a == "-i").unwrap();
        assert!(dev < input, "`-vaapi_device` must come before `-i`");
    }

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

    // ffprobe `packet=pts_time,flags` CSV: `K` in the flags marks a keyframe.
    #[test]
    fn keyframe_gap_reads_dense_and_sparse_layouts() {
        // ~1s keyframes (a transcoded file): largest gap ~1s.
        let dense = "0.000000,K__\n0.033333,__\n1.000000,K__\n2.000000,K__\n3.000000,K__";
        assert!((max_keyframe_gap(dense).unwrap() - 1.0).abs() < 1e-6);

        // ~8s keyframes (a passthrough camera recording) with a scene-cut keyframe:
        // the max gap is what matters, not the close pair.
        let sparse = "0.000000,K__\n1.566667,K__\n9.900000,K__";
        assert!((max_keyframe_gap(sparse).unwrap() - 8.333333).abs() < 1e-4);

        // A single keyframe in the window → GOP at least the window → sparse.
        assert_eq!(max_keyframe_gap("0.000000,K__\n0.033333,__"), Some(f64::INFINITY));

        // No keyframe line at all → unmeasurable.
        assert_eq!(max_keyframe_gap("0.033333,__\n0.066667,__"), None);
    }

    #[test]
    fn density_threshold_gates_passthrough() {
        assert!(keyframes_dense_enough(Some(1.0)));
        assert!(keyframes_dense_enough(Some(2.0))); // exactly at the bound
        assert!(!keyframes_dense_enough(Some(8.3))); // a sparse camera GOP
        assert!(!keyframes_dense_enough(Some(f64::INFINITY)));
        // Unmeasurable → assume dense (don't re-encode a file that already decodes).
        assert!(keyframes_dense_enough(None));
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

    /// Regression for issue #24: a web-playable recording with a **sparse GOP**
    /// (the camera's native keyframe spacing) must NOT pass through — it has to be
    /// transcoded so copy-based seeking lands within ~1s of the target instead of
    /// snapping back a whole GOP and replaying the same scene. A dense-GOP clip of
    /// the same codecs still passes through. Drives the real `probe()` end-to-end;
    /// skips cleanly when the ffmpeg/ffprobe sidecars aren't reachable (cargo-test's
    /// binary dir isn't the sidecar layout).
    #[test]
    fn sparse_keyframe_recording_is_not_passthrough() {
        let (Some(ffmpeg), true) = (ensure_sidecar("ffmpeg"), ensure_sidecar("ffprobe").is_some())
        else {
            eprintln!("skipping sparse_keyframe_recording_is_not_passthrough: sidecars unavailable");
            return;
        };

        // Same H.264/AAC codecs and container for both; only the keyframe spacing
        // differs — sparse (~10s GOP) vs dense (~1s forced keyframes).
        let dir = std::env::temp_dir();
        let sparse = dir.join("voloph-sparse-probe-test.mp4");
        let dense = dir.join("voloph-dense-probe-test.mp4");
        let encode = |out: &Path, key_args: &[&str]| {
            let mut cmd = std::process::Command::new(&ffmpeg);
            cmd.args([
                "-v", "error", "-y",
                "-f", "lavfi", "-i", "testsrc2=size=192x108:rate=30:duration=12",
                "-f", "lavfi", "-i", "sine=frequency=440:duration=12",
                "-c:v", "libx264", "-preset", "ultrafast",
            ])
            .args(key_args)
            .args(["-pix_fmt", "yuv420p", "-c:a", "aac", "-movflags", "+faststart", "-f", "mp4"])
            .arg(out)
            .stdin(Stdio::null());
            matches!(cmd.status(), Ok(s) if s.success())
        };
        let sparse_ok = encode(&sparse, &["-g", "300", "-keyint_min", "300", "-sc_threshold", "0"]);
        let dense_ok = encode(&dense, &["-force_key_frames", "expr:gte(t,n_forced*1)"]);
        if !sparse_ok || !dense_ok {
            let _ = std::fs::remove_file(&sparse);
            let _ = std::fs::remove_file(&dense);
            eprintln!("skipping sparse_keyframe_recording_is_not_passthrough: fixture encode failed");
            return;
        }

        let sparse_probe = probe(sparse.to_str().unwrap()).unwrap();
        let dense_probe = probe(dense.to_str().unwrap()).unwrap();
        let _ = std::fs::remove_file(&sparse);
        let _ = std::fs::remove_file(&dense);

        assert!(
            !sparse_probe.passthrough,
            "a ~10s-GOP web-playable recording must be transcoded for dense keyframes (issue #24)"
        );
        assert!(
            dense_probe.passthrough,
            "a ~1s-GOP web-playable recording should play directly"
        );
    }

    /// True if `bin` runs (`-version` exits 0).
    fn sidecar_runs(bin: &Path) -> bool {
        std::process::Command::new(bin)
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Ensure the named sidecar (`ffmpeg`/`ffprobe`) is reachable where the
    /// server's `sidecar_path` looks (beside the running binary) and return that
    /// path. Under `cargo test` the binary's dir has no sidecar, so locate a real
    /// one among the binary's ancestor dirs (the dev/Tauri layout copies them to
    /// `target/debug`) and copy it into the sidecar slot. Returns `None` — so tests
    /// skip — when none is found, rather than failing in a bare environment.
    fn ensure_sidecar(name: &str) -> Option<PathBuf> {
        let sidecar = std::env::current_exe().ok()?.parent()?.join(name);
        if sidecar_runs(&sidecar) {
            return Some(sidecar);
        }
        let exe = std::env::current_exe().ok()?;
        let source = exe.ancestors().find_map(|dir| {
            for candidate_name in [name.to_string(), format!("{name}-x86_64-unknown-linux-gnu")] {
                let cand = dir.join(&candidate_name);
                if cand != sidecar && sidecar_runs(&cand) {
                    return Some(cand);
                }
            }
            None
        })?;
        std::fs::copy(&source, &sidecar).ok()?;
        sidecar_runs(&sidecar).then_some(sidecar)
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
