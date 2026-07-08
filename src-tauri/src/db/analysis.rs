//! Publishing the machine **Analysis** as a content-keyed file in the shared
//! library (ADR 0013): the pristine machine output of segmenting one recording —
//! the draft timeline with per-rally confidence, the waveform, the duration, and
//! the capture day — written as a small JSON `.vanalysis` file under
//! `.voloph/analysis/` at the shared library root, so another user of the same
//! library adopts it instead of re-burning the compute.
//!
//! Impersonal by construction: no annotations, no flags, no sharer identity — a
//! pure function of the recording's bytes and the segmenter (ADR 0013), which is
//! why it needs no offer and no consent. Only shared-library recordings publish;
//! the local library's bytes no one else can reach. Failure is silent: the
//! analysis still lands in the DB as today.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use super::{library_path_of, parse_waveform};

/// The on-disk Analysis format tag and current version — the `format`/`version`
/// envelope (like `.vbundle`) that keeps the file backward-readable across format
/// changes. Ignored today; readers key backward-compat off it (ADR 0013).
pub const ANALYSIS_FORMAT: &str = "voloph-analysis";
pub const ANALYSIS_VERSION: u32 = 1;

/// A rally in a published Analysis: the machine's draft interval with its
/// per-region confidence (uncertain regions surface from low confidence). No
/// flag — flags are review state and never enter an Analysis (ADR 0013).
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct AnalysisRally {
    pub start_ms: i64,
    pub end_ms: i64,
    pub confidence: f64,
}

/// One recording's pristine machine output (ADR 0013). `format`/`version` tag the
/// wire format; `segmenter_version` lets a future segmenter spot stale Analyses.
#[derive(Debug, Serialize, Deserialize)]
pub struct Analysis {
    pub format: String,
    pub version: u32,
    pub segmenter_version: u32,
    pub capture_day: String,
    pub duration_ms: Option<i64>,
    pub waveform: Vec<f32>,
    pub rallies: Vec<AnalysisRally>,
}

/// The Analysis file's name in `.voloph/analysis/`: quick hash + file size, the
/// recording's existing content-key identity (ADR 0013). Survives renames and
/// moves within the library and dedups copies.
fn analysis_file_name(quick_hash: &str, file_size: i64) -> String {
    format!("{quick_hash}_{file_size}.vanalysis")
}

/// Publish (overwrite) the Analysis for recording `id` at segmentation completion
/// (ADR 0013). A no-op for a local-library recording, or when the shared library
/// is not designated. Reads the pristine machine output straight from the DB —
/// the caller has just committed it — and writes it temp-then-rename so a
/// half-written file can never be adopted. Every failure (read-only mount,
/// dropped NAS, missing row) is logged and swallowed: the analysis already lives
/// in the DB, and an Analysis is plumbing the user never knew existed.
pub fn publish_analysis(conn: &Connection, id: i64) {
    // Only shared-library recordings publish — no one else can reach the local
    // library's bytes (ADR 0013).
    struct Row {
        library: String,
        file_size: i64,
        quick_hash: String,
        capture_day: String,
        duration_ms: Option<i64>,
        waveform_json: Option<String>,
    }
    let row = conn
        .query_row(
            "SELECT library, file_size, quick_hash, capture_day, duration_ms, waveform
             FROM recordings WHERE id = ?1",
            [id],
            |r| {
                Ok(Row {
                    library: r.get(0)?,
                    file_size: r.get(1)?,
                    quick_hash: r.get(2)?,
                    capture_day: r.get(3)?,
                    duration_ms: r.get(4)?,
                    waveform_json: r.get(5)?,
                })
            },
        )
        .optional();
    let row = match row {
        Ok(Some(r)) if r.library == "shared" => r,
        Ok(_) => return, // local recording, or the row vanished — nothing to publish
        Err(e) => {
            log::error!("analysis: could not read recording {id} to publish: {e}");
            return;
        }
    };

    let Ok(Some(root)) = library_path_of(conn, "shared") else {
        return; // shared library not designated on this device
    };

    let rallies = match conn
        .prepare("SELECT start_ms, end_ms, confidence FROM rallies WHERE recording_id = ?1 ORDER BY start_ms")
        .and_then(|mut stmt| {
            stmt.query_map([id], |r| {
                Ok(AnalysisRally {
                    start_ms: r.get(0)?,
                    end_ms: r.get(1)?,
                    confidence: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
        }) {
        Ok(r) => r,
        Err(e) => {
            log::error!("analysis: could not read rallies for recording {id}: {e}");
            return;
        }
    };

    let analysis = Analysis {
        format: ANALYSIS_FORMAT.to_string(),
        version: ANALYSIS_VERSION,
        segmenter_version: crate::segment::SEGMENTER_VERSION,
        capture_day: row.capture_day,
        duration_ms: row.duration_ms,
        waveform: parse_waveform(row.waveform_json.as_deref()),
        rallies,
    };

    let dir = Path::new(&root).join(".voloph").join("analysis");
    let name = analysis_file_name(&row.quick_hash, row.file_size);
    if let Err(e) = write_analysis(&dir, &name, &analysis) {
        // Silent to the user (ADR 0013): the analysis is already in the DB.
        log::warn!("analysis: could not publish {name}: {e}");
    }
}

/// Write `analysis` to `dir/name` temp-then-rename so a half-written file is never
/// adopted (ADR 0013). Creates the `.voloph/analysis/` folder if absent. The temp
/// file sits in the same folder as the target so the rename stays on one
/// filesystem (an atomic move); a leftover temp from a crash is harmless clutter.
fn write_analysis(dir: &Path, name: &str, analysis: &Analysis) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let json = serde_json::to_vec_pretty(analysis)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let tmp = dir.join(format!(".{name}.tmp"));
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, dir.join(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_is_atomic_and_leaves_no_temp() {
        let tmpdir = std::env::temp_dir().join(format!("voloph-analysis-{}", std::process::id()));
        let dir = tmpdir.join(".voloph").join("analysis");
        let analysis = Analysis {
            format: ANALYSIS_FORMAT.to_string(),
            version: ANALYSIS_VERSION,
            segmenter_version: 1,
            capture_day: "2026-01-01".to_string(),
            duration_ms: Some(1000),
            waveform: vec![0.1, 0.2],
            rallies: vec![AnalysisRally {
                start_ms: 0,
                end_ms: 500,
                confidence: 0.9,
            }],
        };
        let name = analysis_file_name("abc123", 4242);
        assert_eq!(name, "abc123_4242.vanalysis");

        write_analysis(&dir, &name, &analysis).unwrap();

        // The final file is there and round-trips; no temp left behind.
        let back: Analysis =
            serde_json::from_slice(&std::fs::read(dir.join(&name)).unwrap()).unwrap();
        assert_eq!(back.format, ANALYSIS_FORMAT);
        assert_eq!(back.rallies, analysis.rallies);
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "a temp file was left behind");

        std::fs::remove_dir_all(&tmpdir).ok();
    }
}
