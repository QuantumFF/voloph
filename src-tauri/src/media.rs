//! Media analysis sidecars for import: audio PCM, motion, and probes.
//!
//! Recordings are decoded, seeked, and rendered by embedded libmpv (ADR 0008),
//! which handles any codec and sparse GOPs natively and opens originals straight
//! from disk — there is no playback transport here. The import **transcode is
//! eliminated** (ADR 0005 superseded): originals are never modified and a
//! recording is playable immediately after import. Import is just probe (for
//! capture date and frame rate) + segmentation.
//!
//! ffmpeg/ffprobe are the bundled sidecars from ADR 0004.

use std::path::PathBuf;
use std::process::{Command, Stdio};

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

/// Resolve a bundled sidecar binary. Tauri places `externalBin` next to the
/// app executable (and copies them beside the dev binary), so we look there.
pub(crate) fn sidecar_path(name: &str) -> PathBuf {
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

/// Confirm a recording is readable so the importer can mark it `ready` (libmpv
/// plays any codec and seeks sparse GOPs directly — ADR 0008 — so there is no
/// transcode decision left to make, and no per-recording value left to capture).
/// Returns an error string if the probe fails entirely (missing file, unreadable,
/// sidecar not resolvable) so the caller can mark the recording `failed` rather
/// than silently retry forever.
pub fn probe(path: &str) -> Result<(), String> {
    let output = Command::new(sidecar_path("ffprobe"))
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type",
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

    Ok(())
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

#[cfg(test)]
mod tests {
    use super::{parse_capture_date, CaptureDate};

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
}
