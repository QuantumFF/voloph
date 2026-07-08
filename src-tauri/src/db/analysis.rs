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

use super::{is_hand_touched, library_path_of, parse_waveform, waveform_to_json};

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

struct AnalysisRow {
    library: String,
    file_size: i64,
    quick_hash: String,
    capture_day: String,
    duration_ms: Option<i64>,
    waveform_json: Option<String>,
}

/// Read the DB row + rallies for recording `id` and assemble its pristine machine
/// Analysis (ADR 0013), or `None` when the row vanished or is not a shared-library
/// recording (only shared recordings publish — no one else can reach the local
/// library's bytes). Returns the recording's content key alongside so the caller
/// can name the file.
fn analysis_of(conn: &Connection, id: i64) -> Option<(String, i64, Analysis)> {
    analysis_of_impl(conn, id, true)
}

/// As [`analysis_of`] but without the shared-library gate — for the invariant's
/// local→shared copy journey, where the pristine analysis lives on the *local*
/// row while its identical bytes already exist in the shared library (ADR 0013).
fn analysis_of_any_library(conn: &Connection, id: i64) -> Option<(String, i64, Analysis)> {
    analysis_of_impl(conn, id, false)
}

fn analysis_of_impl(conn: &Connection, id: i64, require_shared: bool) -> Option<(String, i64, Analysis)> {
    let row = conn
        .query_row(
            "SELECT library, file_size, quick_hash, capture_day, duration_ms, waveform
             FROM recordings WHERE id = ?1",
            [id],
            |r| {
                Ok(AnalysisRow {
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
        Ok(Some(r)) if !require_shared || r.library == "shared" => r,
        Ok(_) => return None, // local recording, or the row vanished — nothing to publish
        Err(e) => {
            log::error!("analysis: could not read recording {id} to publish: {e}");
            return None;
        }
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
            return None;
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
    Some((row.quick_hash, row.file_size, analysis))
}

/// Publish (overwrite) the Analysis for recording `id` at segmentation completion
/// (ADR 0013). A no-op for a local-library recording, or when the shared library
/// is not designated. Reads the pristine machine output straight from the DB —
/// the caller has just committed it — and writes it temp-then-rename so a
/// half-written file can never be adopted. Every failure (read-only mount,
/// dropped NAS, missing row) is logged and swallowed: the analysis already lives
/// in the DB, and an Analysis is plumbing the user never knew existed.
pub fn publish_analysis(conn: &Connection, id: i64) {
    let Some((quick_hash, file_size, analysis)) = analysis_of(conn, id) else {
        return;
    };
    let Ok(Some(root)) = library_path_of(conn, "shared") else {
        return; // shared library not designated on this device
    };
    let dir = Path::new(&root).join(".voloph").join("analysis");
    let name = analysis_file_name(&quick_hash, file_size);
    if let Err(e) = write_analysis(&dir, &name, &analysis) {
        // Silent to the user (ADR 0013): the analysis is already in the DB.
        log::warn!("analysis: could not publish {name}: {e}");
    }
}

/// Restore the publication invariant (ADR 0013): every shared-library recording
/// whose analysis in the local DB is still machine-pristine (`ready` and not
/// hand-touched) has its `.vanalysis` file on disk. Write-if-absent — an existing
/// readable file is left alone, a missing or unreadable one is (re)written. Called
/// after any scan or analysis pass, so day-one backfill, publish-failure retry, and
/// the local→shared copy journey all fall out of this one rule. A no-op outside a
/// designated shared library; the existence check touches only the analysis folder,
/// adding no per-recording network reads beyond it.
pub fn publish_missing_analyses(conn: &Connection) {
    let Ok(Some(root)) = library_path_of(conn, "shared") else {
        return; // shared library not designated — nothing to publish into
    };

    // Machine-pristine analyzed recordings whose bytes live in the shared library:
    // `ready` (the compute is spent), not hand-touched (hand-work is review state
    // and never enters an Analysis), and content-keyed to a shared row. The self-join
    // also catches the local→shared copy — a `ready` *local* original whose identical
    // bytes were copied into the shared library — so its Analysis publishes without
    // re-analysis (ADR 0013). A hand-touched local original has no pristine draft
    // left, so `is_hand_touched` below drops it; that path stays with the carry offer.
    let ids: Vec<i64> = match conn
        .prepare(
            "SELECT DISTINCT r.id FROM recordings r
             JOIN recordings s
               ON s.library = 'shared'
              AND s.quick_hash = r.quick_hash
              AND s.file_size = r.file_size
             WHERE r.segment_state = 'ready'",
        )
        .and_then(|mut stmt| {
            stmt.query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<_>>>()
        }) {
        Ok(ids) => ids,
        Err(e) => {
            log::error!("analysis: could not list recordings to publish: {e}");
            return;
        }
    };

    let dir = Path::new(&root).join(".voloph").join("analysis");
    for id in ids {
        match is_hand_touched(conn, id) {
            Ok(true) => continue, // hand-touched never publishes (ADR 0013)
            Ok(false) => {}
            Err(e) => {
                log::error!("analysis: could not check hand-touch for {id}: {e}");
                continue;
            }
        }
        let Some((quick_hash, file_size, analysis)) = analysis_of_any_library(conn, id) else {
            continue;
        };
        // Present and readable → invariant already holds, leave it alone. Missing or
        // unreadable (wrong version, truncated) counts as absent and is (re)written.
        if read_analysis(&root, &quick_hash, file_size).is_some() {
            continue;
        }
        let name = analysis_file_name(&quick_hash, file_size);
        if let Err(e) = write_analysis(&dir, &name, &analysis) {
            log::warn!("analysis: could not publish {name}: {e}");
        }
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

/// Read the published Analysis for a recording keyed by `quick_hash` + `file_size`
/// (ADR 0013), or `None` when there is no matching file, it cannot be read, or it
/// is not a readable Analysis (malformed, wrong format, or a newer version). Failure
/// is silent by design — an Analysis is plumbing the user never knew existed, so an
/// unreadable file just means the normal pipeline analyzes as if it weren't there.
fn read_analysis(root: &str, quick_hash: &str, file_size: i64) -> Option<Analysis> {
    let path = Path::new(root)
        .join(".voloph")
        .join("analysis")
        .join(analysis_file_name(quick_hash, file_size));
    let bytes = std::fs::read(&path).ok()?;
    let analysis: Analysis = serde_json::from_slice(&bytes).ok()?;
    // Format/version envelope check (ADR 0013): a wrong format or a version from a
    // newer Voloph is ignored, not adopted, so a future format change stays safe.
    if analysis.format != ANALYSIS_FORMAT || analysis.version > ANALYSIS_VERSION {
        return None;
    }
    Some(analysis)
}

/// Silently adopt a published Analysis for the shared-library recordings that a
/// scan just left machine-pristine and unanalyzed (ADR 0013): for each shared
/// recording still `unknown`/`failed` and not hand-touched, look up its Analysis by
/// quick hash + size and, if one is readable, register the carried draft timeline,
/// waveform, duration, and mark it probed + segmented — no staging, no probe, no
/// segmentation. Already-analyzed (`ready`) and hand-touched recordings are never
/// consulted or changed. A missing, unreadable, or wrong-version file is skipped
/// silently, leaving the recording for the normal pipeline. Runs inside the
/// caller's transaction so a whole scan's adoptions commit atomically.
pub(crate) fn adopt_analyses(tx: &rusqlite::Transaction) -> rusqlite::Result<()> {
    let Ok(Some(root)) = library_path_of(tx, "shared") else {
        return Ok(()); // shared library not designated — nothing to adopt into
    };

    // Candidates: shared recordings the pipeline has not produced a draft for yet.
    // `ready` is excluded (the compute is spent; ADR 0013 never churns it).
    let candidates: Vec<(i64, String, i64)> = {
        let mut stmt = tx.prepare(
            "SELECT id, quick_hash, file_size FROM recordings
             WHERE library = 'shared' AND segment_state IN ('unknown', 'failed')",
        )?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        rows
    };

    for (id, quick_hash, file_size) in candidates {
        // A hand-touched recording is never touched and never even asked (ADR 0013).
        // Belt-and-braces: an unanalyzed recording carries no rallies/annotations, so
        // this only guards against a future state that resets segmentation but keeps
        // hand-work.
        if is_hand_touched(tx, id)? {
            continue;
        }
        let Some(analysis) = read_analysis(&root, &quick_hash, file_size) else {
            continue;
        };
        tx.execute("DELETE FROM rallies WHERE recording_id = ?1", [id])?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO rallies (recording_id, start_ms, end_ms, confidence)
                 VALUES (?1, ?2, ?3, ?4)",
            )?;
            for r in &analysis.rallies {
                stmt.execute(rusqlite::params![id, r.start_ms, r.end_ms, r.confidence])?;
            }
        }
        // Playable straight away, like a bundle-registered recording: the draft came
        // from the Analysis, so probe + segment are both satisfied (ADR 0008/0013).
        tx.execute(
            "UPDATE recordings
             SET probe_state = 'ready', segment_state = 'ready', duration_ms = ?1, waveform = ?2
             WHERE id = ?3",
            rusqlite::params![analysis.duration_ms, waveform_to_json(&analysis.waveform), id],
        )?;
    }
    Ok(())
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
