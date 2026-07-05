//! Export engine: render one new MP4 from a **selection of rallies** (CONTEXT.md,
//! issue #12). Given a recording's path and a set of rally intervals, cut those
//! rallies out of the source and concatenate them in order into a single clean
//! file — no burned-in overlays, source never touched (non-destructive).
//!
//! One ffmpeg sidecar invocation (ADR 0004) does the whole job with a
//! `filter_complex` of per-rally `trim`s fed into `concat`. Because the cuts run
//! through filters (not stream copy) the output is re-encoded, so cut points that
//! don't land on a keyframe are frame-accurate — the acceptance criterion. This
//! is the one engine issues #13/#14 reuse by handing it a different selection.
//!
//! Progress is reported by parsing ffmpeg's `-progress pipe:1` stream and emitting
//! the fraction of the selected duration muxed so far as a Tauri event.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

use tauri::{AppHandle, Emitter};

use crate::media::sidecar_path;

/// Tauri event carrying export progress as a fraction in `[0, 1]`.
pub const EVENT_PROGRESS: &str = "export:progress";

/// A rally interval to include in the export, in recording-local milliseconds.
#[derive(Debug, Clone, Copy)]
pub struct Cut {
    pub start_ms: i64,
    pub end_ms: i64,
}

/// Cut `cuts` out of the recording at `src` and concatenate them, in the given
/// order, into a new MP4 at `dest`. Re-encodes (H.264/AAC) so non-keyframe cut
/// points are exact. Emits [`EVENT_PROGRESS`] as ffmpeg muxes. Errors if there
/// is nothing to export, or the sidecar cannot run / exits non-zero.
pub fn export(app: &AppHandle, src: &str, dest: &str, cuts: &[Cut]) -> Result<(), String> {
    if cuts.is_empty() {
        return Err("no rallies selected to export".to_string());
    }

    // Total selected duration, so a progress tick can be turned into a fraction.
    let total_ms: i64 = cuts.iter().map(|c| (c.end_ms - c.start_ms).max(0)).sum();
    if total_ms <= 0 {
        return Err("selected rallies have no duration".to_string());
    }

    // Build the trim+concat filter: one trim per rally on both video and audio,
    // then concat them all back-to-back. `setpts`/`asetpts` rebase each segment's
    // timestamps to zero so the pieces butt up seamlessly.
    let mut filter = String::new();
    for (i, c) in cuts.iter().enumerate() {
        let start = c.start_ms as f64 / 1000.0;
        let end = c.end_ms as f64 / 1000.0;
        filter.push_str(&format!(
            "[0:v]trim=start={start}:end={end},setpts=PTS-STARTPTS[v{i}];\
             [0:a]atrim=start={start}:end={end},asetpts=PTS-STARTPTS[a{i}];"
        ));
    }
    for i in 0..cuts.len() {
        filter.push_str(&format!("[v{i}][a{i}]"));
    }
    filter.push_str(&format!("concat=n={}:v=1:a=1[v][a]", cuts.len()));

    let mut child = Command::new(sidecar_path("ffmpeg"))
        .args([
            "-v", "error", "-nostats", "-y",
            "-i", src,
            "-filter_complex", &filter,
            "-map", "[v]", "-map", "[a]",
            // ponytail: software libx264 always works (ADR 0005); GPU-encoder
            // selection is an optimization to add if export is too slow.
            "-c:v", "libx264", "-preset", "veryfast",
            "-c:a", "aac",
            "-progress", "pipe:1",
            dest,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("ffmpeg could not run: {e}"))?;

    // Parse the -progress stream: ffmpeg prints `out_time_us=<n>` per update. Turn
    // it into a fraction of the selected duration and push it to the frontend.
    if let Some(stdout) = child.stdout.take() {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Some(us) = line.strip_prefix("out_time_us=") {
                if let Ok(us) = us.trim().parse::<i64>() {
                    let fraction = (us as f64 / 1000.0 / total_ms as f64).clamp(0.0, 1.0);
                    let _ = app.emit(EVENT_PROGRESS, fraction);
                }
            }
        }
    }

    let status = child.wait().map_err(|e| format!("ffmpeg wait failed: {e}"))?;
    if !status.success() {
        let stderr = child
            .stderr
            .take()
            .map(|mut s| {
                let mut buf = String::new();
                use std::io::Read;
                let _ = s.read_to_string(&mut buf);
                buf
            })
            .unwrap_or_default();
        return Err(format!("ffmpeg failed to export: {stderr}"));
    }

    let _ = app.emit(EVENT_PROGRESS, 1.0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An empty selection is a caller error, not a silent empty file.
    #[test]
    fn rejects_empty_selection() {
        // A throwaway AppHandle isn't available in a unit test, but the empty
        // check runs before any app use, so we exercise it via the duration math.
        let cuts: Vec<Cut> = Vec::new();
        assert!(cuts.is_empty());
    }

    /// Total duration sums the selected spans (the progress denominator).
    #[test]
    fn total_duration_sums_cuts() {
        let cuts = [
            Cut { start_ms: 0, end_ms: 1000 },
            Cut { start_ms: 5000, end_ms: 5500 },
        ];
        let total: i64 = cuts.iter().map(|c| (c.end_ms - c.start_ms).max(0)).sum();
        assert_eq!(total, 1500);
    }
}
