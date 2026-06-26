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

/// Route one request: validate the token, then serve the file with range
/// support. By playback time every recording is already web-playable (ADR
/// 0005), so this only ever passes bytes through — it never transcodes.
fn handle_request(request: Request, token: &str) {
    let url = request.url().to_string();
    let (route, query) = url.split_once('?').unwrap_or((url.as_str(), ""));
    if route != "/play" {
        return respond_error(request, 404, "not found");
    }

    let mut path = None;
    let mut supplied_token = None;
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        let decoded = percent_decode(value);
        match key {
            "path" => path = Some(decoded),
            "token" => supplied_token = Some(decoded),
            _ => {}
        }
    }

    if supplied_token.as_deref() != Some(token) {
        return respond_error(request, 403, "forbidden");
    }
    let Some(path) = path else {
        return respond_error(request, 400, "missing `path` query parameter");
    };

    serve_file(request, &path);
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

/// What a probe tells us about whether a source can play in the webview as-is.
pub struct Probe {
    /// True when the container + codecs are web-playable (H.264/AAC in mp4/mov)
    /// and so need no transcode.
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
    Ok(Probe {
        passthrough: is_web_playable(&report),
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
    use super::{is_web_playable, parse_fps, percent_decode, start};
    use std::io::{Read, Write};
    use std::net::TcpStream;

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
