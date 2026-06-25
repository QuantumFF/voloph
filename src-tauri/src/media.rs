//! Playback source for the in-app player.
//!
//! Recordings are served to the webview `<video>` element through the custom
//! `stream://` URI scheme registered in [`crate::run`], rather than the raw
//! asset protocol. WebKitGTK on Linux cannot decode HEVC/H.265 (the iPhone
//! default), so a source whose codec is not web-playable is transcoded on the
//! fly to H.264 video + AAC audio via the bundled ffmpeg sidecar (ADR 0004).
//! Web-friendly sources (H.264/AAC in an mp4/mov container) are served as-is,
//! honoring HTTP range requests so native seeking works without re-encoding.
//!
//! Transcoding is live (no transcoded artifact is ever written to disk, keeping
//! the reference-in-place model intact). Seeking into a transcoded source is
//! done by restarting the ffmpeg pipeline from the requested timestamp via the
//! `t` query parameter (`-ss`); byte-range seeking into transcoded output is
//! out of scope.

use std::borrow::Cow;

use tauri::http::{Request, Response, StatusCode};
use tauri::{AppHandle, Runtime, UriSchemeResponder};
use tauri_plugin_shell::ShellExt;

/// The custom URI scheme the player points `<video>` at.
pub const SCHEME: &str = "stream";

/// What a probe tells us about whether a source can play in the webview as-is.
struct Probe {
    /// True when the container + codecs are web-playable (H.264/AAC in mp4/mov)
    /// and so can be served without re-encoding.
    passthrough: bool,
}

/// Decode the on-disk path and optional start timestamp from a `stream://`
/// request. The path is carried as a percent-encoded query parameter (`path`)
/// rather than in the URL path, so absolute paths with arbitrary characters
/// survive the round-trip on every platform. `t` is the seek offset in seconds.
fn parse_request<T>(request: &Request<T>) -> Option<(String, Option<f64>)> {
    let uri = request.uri();
    let query = uri.query()?;

    let mut path = None;
    let mut t = None;
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=')?;
        let decoded = percent_decode(value);
        match key {
            "path" => path = Some(decoded),
            "t" => t = decoded.parse::<f64>().ok(),
            _ => {}
        }
    }
    path.map(|p| (p, t))
}

/// Minimal percent-decoding for the request query (avoids a url-crate dep,
/// matching the no-extra-dependency style of `db.rs`). Handles `%XX` and `+`.
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

fn error_response(status: StatusCode, message: &str) -> Response<Cow<'static, [u8]>> {
    Response::builder()
        .status(status)
        .header("Content-Type", "text/plain")
        .body(Cow::Owned(message.as_bytes().to_vec()))
        .expect("static error response is valid")
}

/// Probe a recording with the `ffprobe` sidecar to decide passthrough vs
/// transcode. Returns an error string if the probe fails entirely (missing
/// file, unreadable, sidecar not resolvable) — the handler surfaces it to the
/// player rather than serving a black frame.
async fn probe<R: Runtime>(app: &AppHandle<R>, path: &str) -> Result<Probe, String> {
    let output = app
        .shell()
        .sidecar("ffprobe")
        .map_err(|e| format!("ffprobe sidecar unavailable: {e}"))?
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
        .await
        .map_err(|e| format!("ffprobe failed: {e}"))?;

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

/// Serve a web-playable source straight from disk, honoring a `Range` header so
/// the webview's native seek bar works. No ffmpeg process is spawned.
fn serve_passthrough<T>(
    path: &str,
    request: &Request<T>,
) -> Response<Cow<'static, [u8]>> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("could not read recording: {e}"),
            )
        }
    };
    let total = bytes.len() as u64;

    // Parse a single `bytes=start-end` range if present; otherwise serve whole.
    let range = request
        .headers()
        .get("range")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("bytes="))
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

    match range {
        Some((start, end)) => {
            let slice = bytes[start as usize..=end as usize].to_vec();
            Response::builder()
                .status(StatusCode::PARTIAL_CONTENT)
                .header("Content-Type", "video/mp4")
                .header("Accept-Ranges", "bytes")
                .header("Content-Range", format!("bytes {start}-{end}/{total}"))
                .header("Content-Length", (end - start + 1).to_string())
                .body(Cow::Owned(slice))
                .expect("range response is valid")
        }
        None => Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "video/mp4")
            .header("Accept-Ranges", "bytes")
            .header("Content-Length", total.to_string())
            .body(Cow::Owned(bytes))
            .expect("full response is valid"),
    }
}

/// Transcode a source to fragmented H.264/AAC mp4 with the ffmpeg sidecar and
/// return the full result. `start` (seconds) seeks via `-ss` before input, so a
/// seek restarts the pipeline from that point. The whole transcoded body is
/// buffered because the custom-protocol responder takes an owned body, not a
/// stream; this keeps nothing on disk.
async fn serve_transcode<R: Runtime>(
    app: &AppHandle<R>,
    path: &str,
    start: Option<f64>,
) -> Response<Cow<'static, [u8]>> {
    let mut args: Vec<String> = Vec::new();
    if let Some(t) = start {
        if t > 0.0 {
            // Seek before -i for a fast keyframe-accurate restart.
            args.push("-ss".into());
            args.push(format!("{t}"));
        }
    }
    args.extend(
        [
            "-i", path, "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p", "-c:a",
            "aac", "-movflags", "frag_keyframe+empty_moov+default_base_moof", "-f", "mp4", "-",
        ]
        .iter()
        .map(|s| s.to_string()),
    );

    let command = match app.shell().sidecar("ffmpeg") {
        Ok(c) => c,
        Err(e) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("ffmpeg sidecar unavailable: {e}"),
            )
        }
    };

    let output = match command.args(args).output().await {
        Ok(o) => o,
        Err(e) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("transcode failed to start: {e}"),
            )
        }
    };

    if !output.status.success() || output.stdout.is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("transcode failed: {stderr}"),
        );
    }

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "video/mp4")
        // Transcoded output has no stable byte layout for range seeking; the
        // player seeks by restarting the stream from a timestamp instead.
        .header("Accept-Ranges", "none")
        .header("Cache-Control", "no-store")
        .body(Cow::Owned(output.stdout))
        .expect("transcode response is valid")
}

/// Handle one `stream://` request: probe the source, then either pass it
/// through or transcode it, and hand the response to the responder. Runs the
/// async media work on the Tauri runtime so the webview thread is never
/// blocked.
pub fn handle<R: Runtime>(
    app: AppHandle<R>,
    request: Request<Vec<u8>>,
    responder: UriSchemeResponder,
) {
    let Some((path, start)) = parse_request(&request) else {
        responder.respond(error_response(
            StatusCode::BAD_REQUEST,
            "stream request is missing a `path` query parameter",
        ));
        return;
    };

    tauri::async_runtime::spawn(async move {
        let response = match probe(&app, &path).await {
            Ok(probe) if probe.passthrough => serve_passthrough(&path, &request),
            Ok(_) => serve_transcode(&app, &path, start).await,
            Err(message) => error_response(StatusCode::INTERNAL_SERVER_ERROR, &message),
        };
        responder.respond(response);
    });
}

#[cfg(test)]
mod tests {
    use super::is_web_playable;

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
}
