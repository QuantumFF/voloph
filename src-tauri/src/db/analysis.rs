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

/// The Analysis file's name in `.voloph/analysis/` for a given segmenter version
/// (ADR 0013/0015). The base is quick hash + file size — the recording's existing
/// content-key identity, surviving renames and moves and deduping copies. Version 1
/// keeps the bare `{key}.vanalysis` name it has always used, so older Voloph builds
/// (segmenter v1) still find and adopt it; a newer version gets a `_s{N}` suffix so
/// it is **published alongside** the stale one rather than overwriting it — nothing
/// in shared storage is ever deleted, and adoption prefers the highest version.
pub(crate) fn analysis_file_name(quick_hash: &str, file_size: i64, segmenter_version: u32) -> String {
    if segmenter_version <= 1 {
        format!("{quick_hash}_{file_size}.vanalysis")
    } else {
        format!("{quick_hash}_{file_size}_s{segmenter_version}.vanalysis")
    }
}

/// The content-key filename prefix shared by every version's Analysis for one
/// recording — the base `{quick_hash}_{file_size}` (ADR 0013/0015). Used to
/// enumerate all published versions of a recording's Analysis when adopting the
/// highest one; anything after this prefix is either `.vanalysis` (v1) or
/// `_s{N}.vanalysis` (a later version).
fn analysis_key_prefix(quick_hash: &str, file_size: i64) -> String {
    format!("{quick_hash}_{file_size}")
}

struct AnalysisRow {
    library: String,
    file_size: i64,
    quick_hash: String,
    capture_day: String,
    duration_ms: Option<i64>,
    waveform_json: Option<String>,
    segmenter_version: Option<i64>,
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
            "SELECT library, file_size, quick_hash, capture_day, duration_ms, waveform,
                    segmenter_version
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
                    segmenter_version: r.get(6)?,
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
        // The version that actually produced this draft (ADR 0013/0015), so the
        // published file names and describes itself honestly — a draft adopted from
        // an older Analysis republishes as that older version, not as the active one.
        // A row with no stamp (only unsegmented rows, which never reach here) falls
        // back to the active version.
        segmenter_version: row
            .segmenter_version
            .map(|v| v as u32)
            .unwrap_or(crate::segment::SEGMENTER_VERSION),
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
    let name = analysis_file_name(&quick_hash, file_size, analysis.segmenter_version);
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
        // The invariant is per version (ADR 0013/0015): this draft's own
        // `.vanalysis` should exist. A newer-version draft publishes *alongside* the
        // stale one under its own name rather than checking for any file — so a bump
        // backfills the superseding file without touching the older sibling. Present
        // and readable → leave it alone; missing or unreadable (truncated) → rewrite.
        let name = analysis_file_name(&quick_hash, file_size, analysis.segmenter_version);
        if read_analysis_file(&dir.join(&name)).is_some() {
            continue;
        }
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

/// Parse the `.vanalysis` at `path`, or `None` when it cannot be read or is not a
/// readable Analysis — malformed, wrong format, or a wire `version` from a newer
/// Voloph (ADR 0013). Failure is silent by design: an Analysis is plumbing the user
/// never knew existed, so an unreadable file just means the pipeline analyzes as if
/// it weren't there. Note the wire `version` (the file *format*) is distinct from
/// the `segmenter_version` (which segmenter produced the draft); only the former
/// gates readability, since a newer segmenter's output is still readable JSON.
fn read_analysis_file(path: &Path) -> Option<Analysis> {
    let bytes = std::fs::read(path).ok()?;
    let analysis: Analysis = serde_json::from_slice(&bytes).ok()?;
    if analysis.format != ANALYSIS_FORMAT || analysis.version > ANALYSIS_VERSION {
        return None;
    }
    Some(analysis)
}

/// The **highest-version** published Analysis for a recording keyed by its content
/// key (quick hash and file size; ADR 0013/0015), or `None` when none is readable.
/// Every version of the Analysis for one recording shares the content-key prefix and lives
/// alongside its siblings (nothing is ever deleted from shared storage), so this
/// enumerates them and prefers the one the highest segmenter produced — the best
/// draft, which is what adoption should carry. A malformed or wrong-format sibling
/// is skipped silently, never blocking a good one.
fn read_best_analysis(root: &str, quick_hash: &str, file_size: i64) -> Option<Analysis> {
    let dir = Path::new(root).join(".voloph").join("analysis");
    let prefix = analysis_key_prefix(quick_hash, file_size);
    let mut best: Option<Analysis> = None;
    for entry in std::fs::read_dir(&dir).ok()?.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // Match this recording's content key exactly: either the bare v1 name or a
        // `_s{N}` versioned sibling. The `_` guard stops `<hash>_<size>` from also
        // matching a longer size that happens to start with these digits.
        let is_ours = name == format!("{prefix}.vanalysis")
            || name.starts_with(&format!("{prefix}_s")) && name.ends_with(".vanalysis");
        if !is_ours {
            continue;
        }
        let Some(analysis) = read_analysis_file(&entry.path()) else {
            continue;
        };
        if best
            .as_ref()
            .is_none_or(|b| analysis.segmenter_version > b.segmenter_version)
        {
            best = Some(analysis);
        }
    }
    best
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
        // Prefer the highest-version Analysis when several are published for these
        // bytes (ADR 0013/0015): every device converges on the best draft, older
        // files stay on disk.
        let Some(analysis) = read_best_analysis(&root, &quick_hash, file_size) else {
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
        // Carry the Analysis's segmenter version onto the row (#80) so a later bump
        // can tell an adopted draft is stale exactly as it tells a locally produced
        // one — and so republishing this draft names itself by the version that made it.
        tx.execute(
            "UPDATE recordings
             SET probe_state = 'ready', segment_state = 'ready', duration_ms = ?1, waveform = ?2,
                 segmenter_version = ?3
             WHERE id = ?4",
            rusqlite::params![
                analysis.duration_ms,
                waveform_to_json(&analysis.waveform),
                analysis.segmenter_version,
                id
            ],
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
        let name = analysis_file_name("abc123", 4242, 1);
        assert_eq!(name, "abc123_4242.vanalysis");
        // A later segmenter version is published alongside under a `_s{N}` name so
        // it never overwrites the v1 sibling (ADR 0013/0015).
        assert_eq!(
            analysis_file_name("abc123", 4242, 2),
            "abc123_4242_s2.vanalysis"
        );

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
