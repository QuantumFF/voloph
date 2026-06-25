//! Playback source for the in-app player.
//!
//! Recordings are served to the webview `<video>` element over a small HTTP
//! server bound to `127.0.0.1` on an ephemeral port, rather than the raw asset
//! protocol. WebKitGTK on Linux cannot decode HEVC/H.265 (the iPhone default),
//! so a source whose codec is not web-playable is transcoded on the fly to
//! H.264 video + AAC audio via the bundled ffmpeg sidecar (ADR 0004) and
//! **streamed** to the webview with chunked transfer encoding, so playback
//! starts as the first fragments arrive instead of waiting for the whole file.
//! Web-friendly sources (H.264/AAC in an mp4/mov container) are served straight
//! from disk, honoring HTTP range requests so native seeking works without
//! re-encoding.
//!
//! Transcoding is live (no transcoded artifact is ever written to disk, keeping
//! the reference-in-place model intact). Because a transcoded stream has no
//! stable byte layout, seeking into it is done by restarting ffmpeg from the
//! requested timestamp via the `t` query parameter (`-ss`); the player reloads
//! the stream and the previous ffmpeg process is killed when its connection
//! drops.
//!
//! A streaming HTTP server is used because Tauri's custom-protocol responder
//! only accepts a fully-owned response body, which would force the entire
//! transcode to be buffered in memory before a single byte reached the webview.

use std::io::{self, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use sha2::{Digest, Sha256};
use tiny_http::{Header, Request, Response, Server, StatusCode};

/// Number of worker threads draining the request queue. A handful is plenty:
/// at most one recording plays at a time, plus the odd seek-restart overlap.
const WORKERS: usize = 4;

/// How the frontend reaches the playback server: the origin to build URLs from
/// and a per-launch token that must accompany every request, so other local
/// processes can't drive the ffmpeg sidecar against arbitrary files.
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
/// guessing the URL and using us as an ffmpeg gadget against arbitrary paths.
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

/// What a probe tells us about whether a source can play in the webview as-is.
struct Probe {
    /// True when the container + codecs are web-playable (H.264/AAC in mp4/mov)
    /// and so can be served without re-encoding.
    passthrough: bool,
}

/// Route one request: validate the token, probe the source, then pass it
/// through (with range support) or stream a live transcode.
fn handle_request(request: Request, token: &str) {
    let url = request.url().to_string();
    let (route, query) = url.split_once('?').unwrap_or((url.as_str(), ""));
    if route != "/play" {
        respond_error(request, 404, "not found");
        return;
    }

    let mut path = None;
    let mut start = None;
    let mut supplied_token = None;
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        let decoded = percent_decode(value);
        match key {
            "path" => path = Some(decoded),
            "t" => start = decoded.parse::<f64>().ok(),
            "token" => supplied_token = Some(decoded),
            _ => {}
        }
    }

    if supplied_token.as_deref() != Some(token) {
        respond_error(request, 403, "forbidden");
        return;
    }
    let Some(path) = path else {
        respond_error(request, 400, "missing `path` query parameter");
        return;
    };

    match probe(&path) {
        Ok(probe) if probe.passthrough => serve_passthrough(request, &path),
        Ok(_) => serve_transcode(request, &path, start),
        Err(message) => respond_error(request, 500, &message),
    }
}

fn respond_error(request: Request, code: u16, message: &str) {
    let _ = request.respond(Response::from_string(message).with_status_code(StatusCode(code)));
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

/// Probe a recording with the `ffprobe` sidecar to decide passthrough vs
/// transcode. Returns an error string if the probe fails entirely (missing
/// file, unreadable, sidecar not resolvable) — the handler surfaces it to the
/// player rather than serving a black frame.
fn probe(path: &str) -> Result<Probe, String> {
    let output = Command::new(sidecar_path("ffprobe"))
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type,codec_name:format=format_name",
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
    })
}

/// Decide whether an ffprobe report describes a source the webview can play
/// directly: an mp4/mov-family container carrying H.264 video and, if present,
/// AAC audio. Anything else (HEVC, AV1, mkv, AC-3 audio, …) is transcoded.
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

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes())
        .expect("static header name/value are ASCII")
}

/// Serve a web-playable source straight from disk, honoring a `Range` header so
/// the webview's native seek bar works. No ffmpeg process is spawned.
fn serve_passthrough(request: Request, path: &str) {
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

/// ffmpeg's piped stdout, paired with its `Child` so the process is killed when
/// the response reader is dropped — i.e. when the webview disconnects (seek or
/// navigate-away). Without this, every seek would orphan an ffmpeg process.
struct TranscodeStream {
    child: Child,
    stdout: ChildStdout,
}

impl Read for TranscodeStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.stdout.read(buf)
    }
}

impl Drop for TranscodeStream {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Transcode a source to fragmented H.264/AAC mp4 with the ffmpeg sidecar and
/// stream stdout to the webview with chunked transfer encoding, so playback
/// starts almost immediately. `start` (seconds) seeks via `-ss` before input,
/// so a seek restarts the pipeline from that point. Nothing is written to disk.
fn serve_transcode(request: Request, path: &str, start: Option<f64>) {
    let mut command = Command::new(sidecar_path("ffmpeg"));
    command.args(["-v", "error", "-nostats"]);
    if let Some(t) = start {
        if t > 0.0 {
            // Seek before -i for a fast keyframe-accurate restart.
            command.args(["-ss", &format!("{t}")]);
        }
    }
    command
        .args([
            "-i",
            path,
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-movflags",
            "frag_keyframe+empty_moov+default_base_moof",
            "-f",
            "mp4",
            "pipe:1",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null());

    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => return respond_error(request, 500, &format!("transcode failed to start: {e}")),
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        return respond_error(request, 500, "transcode produced no output stream");
    };

    // No Content-Length → chunked transfer; the body streams as ffmpeg emits it.
    let response = Response::new(
        StatusCode(200),
        vec![
            header("Content-Type", "video/mp4"),
            // Transcoded output has no stable byte layout for range seeking; the
            // player seeks by restarting the stream from a timestamp instead.
            header("Accept-Ranges", "none"),
            header("Cache-Control", "no-store"),
        ],
        TranscodeStream { child, stdout },
        None,
        None,
    );
    let _ = request.respond(response);
}

/// Minimal percent-decoding for query values (avoids a url-crate dep, matching
/// the no-extra-dependency style of `db.rs`). Handles `%XX` and `+`.
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
    use super::{is_web_playable, percent_decode};

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
    fn percent_decode_handles_paths_and_spaces() {
        assert_eq!(percent_decode("%2Fhome%2Fa+b.mov"), "/home/a b.mov");
    }
}
