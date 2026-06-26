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
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        let decoded = percent_decode(value);
        match key {
            "path" => path = Some(decoded),
            "token" => supplied_token = Some(decoded),
            "t" => t = Some(decoded),
            _ => {}
        }
    }

    if supplied_token.as_deref() != Some(token) {
        return respond_error(request, 403, "forbidden");
    }
    let Some(path) = path else {
        return respond_error(request, 400, "missing `path` query parameter");
    };

    // A positive `t` (seconds) means "open already positioned here": GStreamer
    // seeking is unreliable on this WebKitGTK build (issue #24), so the frontend
    // reloads the `<video>` at a `&t=` URL instead of writing `currentTime`. We
    // hand back a fragmented MP4 that begins at the keyframe ≤ `t`, so the webview
    // never issues a seek. `t == 0` (or absent) is the plain whole-file path.
    match t.and_then(|s| s.parse::<f64>().ok()).filter(|v| *v > 0.0) {
        Some(t_secs) => serve_stream_at(request, &path, t_secs),
        None => serve_file(request, &path),
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

/// Transcode the recording at `path` to a complete, seekable H.264/AAC file and
/// replace the original in place via the ffmpeg sidecar. The source codec is
/// discarded (ADR 0005). Writes to a hidden temp file in the same directory and
/// atomically renames over the original, so an interrupted run never leaves a
/// recording half-written. `+faststart` moves the moov atom to the front for
/// instant playback; the output is a normal (non-fragmented) mp4 with a real
/// duration. The path (and its extension) are preserved — the playback server
/// declares `video/mp4` regardless of extension. Keyframes are forced ~once per
/// second so arbitrary scrub seeks land near a keyframe and stay smooth (ADR 0005).
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

    let output = Command::new(sidecar_path("ffmpeg"))
        .args([
            "-v",
            "error",
            "-nostats",
            "-y",
            "-i",
            path,
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            // Force a keyframe roughly once per second (fps-independent), so seeking
            // to an arbitrary scrub point decodes forward from a nearby keyframe
            // rather than x264's sparse default GOP (~250 frames ≈ 8s). Trades a
            // modest file-size increase for smooth, frame-accurate scrubbing in the
            // seek-dominated review loop (ADR 0005).
            "-force_key_frames",
            "expr:gte(t,n_forced*1)",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-movflags",
            "+faststart",
            "-f",
            "mp4",
        ])
        .arg(&temp)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("ffmpeg could not run: {e}"))?;

    if !output.status.success() {
        let _ = std::fs::remove_file(&temp);
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ffmpeg failed to transcode: {stderr}"));
    }

    std::fs::rename(&temp, src).map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        format!("could not replace original with transcode: {e}")
    })?;
    Ok(())
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
        is_web_playable, keyframes_dense_enough, max_keyframe_gap, parse_fps, percent_decode,
        probe, start,
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
