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

use super::draft::{is_analysis_file_of, write_draft, DraftStamp};
use super::{is_hand_touched, library_path_of, parse_waveform};

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
    /// Which segmenter made the draft (ADR 0019). Absent in files written before it,
    /// which v6 made; older builds ignore the field.
    #[serde(default = "v6_id")]
    pub segmenter: String,
    pub segmenter_version: u32,
    pub capture_day: String,
    pub duration_ms: Option<i64>,
    pub waveform: Vec<f32>,
    pub rallies: Vec<AnalysisRally>,
}

fn v6_id() -> String {
    crate::segment::Segmenter::V6.id().to_string()
}

impl Analysis {
    /// Which segmenter drew this Analysis's draft (ADR 0019).
    pub fn stamp(&self) -> DraftStamp {
        DraftStamp {
            id: self.segmenter.clone(),
            version: self.segmenter_version,
        }
    }
}

struct AnalysisRow {
    library: String,
    file_size: i64,
    quick_hash: String,
    capture_day: String,
    duration_ms: Option<i64>,
    waveform_json: Option<String>,
    segmenter_version: Option<i64>,
    segmenter_id: Option<String>,
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

fn analysis_of_impl(
    conn: &Connection,
    id: i64,
    require_shared: bool,
) -> Option<(String, i64, Analysis)> {
    let row = conn
        .query_row(
            "SELECT library, file_size, quick_hash, capture_day, duration_ms, waveform,
                    segmenter_version, segmenter_id
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
                    segmenter_id: r.get(7)?,
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

    // The segmenter that actually drew this draft (ADR 0013/0019), so the published
    // file names and describes itself honestly — a draft adopted from an older
    // Analysis republishes as that older version, not as the active one. A `ready`
    // row with no stamp (pre-versioning, or registered from a bundle or a carried
    // copy, which never stamp — and never set an id either) counts as current v6,
    // as the stale path counts it.
    let stamp = DraftStamp::from_row(row.segmenter_id, row.segmenter_version)
        .unwrap_or_else(|| DraftStamp::of(crate::segment::Segmenter::V6));
    let analysis = Analysis {
        format: ANALYSIS_FORMAT.to_string(),
        version: ANALYSIS_VERSION,
        segmenter: stamp.id,
        segmenter_version: stamp.version,
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
    let name = analysis.stamp().file_name(&quick_hash, file_size);
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
        let name = analysis.stamp().file_name(&quick_hash, file_size);
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
///
/// Only Analyses from `segmenter` count (ADR 0019): a library adopts drafts from the
/// segmenter it uses, never another one's. One older than that segmenter's current
/// version is skipped too, as the stale path would re-derive it on the next scan
/// anyway — and a pre-v7 v6 draft has the old 1.2 s padding baked in, which
/// playback would then pad again (ADR 0020).
fn read_best_analysis(
    root: &str,
    quick_hash: &str,
    file_size: i64,
    segmenter: crate::segment::Segmenter,
) -> Option<Analysis> {
    let dir = Path::new(root).join(".voloph").join("analysis");
    let mut best: Option<Analysis> = None;
    for entry in std::fs::read_dir(&dir).ok()?.flatten() {
        if !is_analysis_file_of(&entry.file_name().to_string_lossy(), quick_hash, file_size) {
            continue;
        }
        let Some(analysis) = read_analysis_file(&entry.path()) else {
            continue;
        };
        // The file's own stamp decides, not its name.
        if !analysis.stamp().is_current_for(segmenter) {
            continue;
        }
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
    let segmenter = super::library_segmenter(tx, "shared")?;

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
        let Some(analysis) = read_best_analysis(&root, &quick_hash, file_size, segmenter) else {
            continue;
        };
        // Carry the Analysis's stamp onto the row (#80, ADR 0019) so a later bump or
        // switch can tell an adopted draft is stale exactly as it tells a locally
        // drawn one — and so republishing this draft names itself by who made it.
        write_draft(
            tx,
            id,
            &analysis.stamp(),
            analysis.duration_ms,
            &analysis.waveform,
            analysis
                .rallies
                .iter()
                .map(|r| (r.start_ms, r.end_ms, r.confidence)),
        )?;
        // Playable straight away, like a bundle-registered recording: the draft came
        // from the Analysis, so probe + segment are both satisfied (ADR 0008/0013).
        tx.execute(
            "UPDATE recordings SET probe_state = 'ready' WHERE id = ?1",
            [id],
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
            segmenter: "v6".to_string(),
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
        let v6 = |version| DraftStamp {
            id: "v6".to_string(),
            version,
        };
        let name = v6(1).file_name("abc123", 4242);
        assert_eq!(name, "abc123_4242.vanalysis");
        // A later segmenter version is published alongside under a `_s{N}` name so
        // it never overwrites the v1 sibling (ADR 0013/0015).
        assert_eq!(v6(2).file_name("abc123", 4242), "abc123_4242_s2.vanalysis");

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

    /// Every Analysis file name a build publishes (ADR 0013/0019). These live in
    /// users' shared folders and older builds look for them by name, so any change
    /// here strands published drafts.
    #[test]
    fn analysis_file_names_are_pinned() {
        let cases = [
            ("v6", 0, "abc_42.vanalysis"),
            ("v6", 1, "abc_42.vanalysis"),
            ("v6", 2, "abc_42_s2.vanalysis"),
            ("v6", 6, "abc_42_s6.vanalysis"),
            ("v6", 7, "abc_42_s7.vanalysis"),
            ("v6", 12, "abc_42_s12.vanalysis"),
            ("student", 1, "abc_42.student1.vanalysis"),
            ("student", 2, "abc_42.student2.vanalysis"),
            ("teacher", 1, "abc_42.teacher1.vanalysis"),
            // An id written by a newer build republishes under its own name.
            ("coach", 3, "abc_42.coach3.vanalysis"),
        ];
        for (id, version, want) in cases {
            assert_eq!(
                DraftStamp {
                    id: id.to_string(),
                    version
                }
                .file_name("abc", 42),
                want,
                "{id} v{version}"
            );
        }
        // The shipping segmenters at their current versions.
        use crate::segment::Segmenter;
        let current: Vec<_> = Segmenter::ALL
            .into_iter()
            .map(|s| DraftStamp::of(s).file_name("abc", 42))
            .collect();
        assert_eq!(
            current,
            [
                "abc_42.student1.vanalysis",
                "abc_42.teacher1.vanalysis",
                "abc_42_s7.vanalysis"
            ]
        );
    }

    fn analysis_by(segmenter: &str, segmenter_version: u32, duration_ms: i64) -> Analysis {
        Analysis {
            format: ANALYSIS_FORMAT.to_string(),
            version: ANALYSIS_VERSION,
            segmenter: segmenter.to_string(),
            segmenter_version,
            capture_day: "2026-01-01".to_string(),
            duration_ms: Some(duration_ms),
            waveform: vec![],
            rallies: vec![],
        }
    }

    /// Adoption's read side: only this content key's files, only the library's
    /// segmenter at its current version or newer, the highest version winning. A key
    /// that is a prefix of another (`abc_12` vs `abc_123`) never reads the other's.
    #[test]
    fn adoption_reads_only_its_own_content_keys_files() {
        use crate::segment::Segmenter;
        let root = std::env::temp_dir().join(format!(
            "voloph-analysis-read-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let dir = root.join(".voloph").join("analysis");
        let put = |name: &str, a: &Analysis| write_analysis(&dir, name, a).unwrap();
        let root_s = root.to_string_lossy().into_owned();
        let best = |size: i64, s: Segmenter| {
            read_best_analysis(&root_s, "abc", size, s).and_then(|a| a.duration_ms)
        };

        // The longer key's files, for every segmenter.
        put("abc_123_s9.vanalysis", &analysis_by("v6", 9, 1239));
        put(
            "abc_123.student5.vanalysis",
            &analysis_by("student", 5, 1235),
        );
        put(
            "abc_123.teacher5.vanalysis",
            &analysis_by("teacher", 5, 1236),
        );
        for s in Segmenter::ALL {
            assert_eq!(best(12, s), None, "{s:?} read another key's file");
        }

        // v6: the legacy bare name and a stale `_s6` are too old; the highest wins.
        put("abc_12.vanalysis", &analysis_by("v6", 1, 1));
        put("abc_12_s6.vanalysis", &analysis_by("v6", 6, 6));
        assert_eq!(best(12, Segmenter::V6), None);
        put("abc_12_s7.vanalysis", &analysis_by("v6", 7, 7));
        put("abc_12_s8.vanalysis", &analysis_by("v6", 8, 8));
        assert_eq!(best(12, Segmenter::V6), Some(8));
        // Temp and foreign files beside them are never read.
        put(".abc_12_s9.vanalysis.tmp", &analysis_by("v6", 9, 9));
        put("abc_12_s9.vanalysis.bak", &analysis_by("v6", 9, 9));
        assert_eq!(best(12, Segmenter::V6), Some(8));

        // The learned segmenters read only their own.
        assert_eq!(best(12, Segmenter::Student), None);
        put("abc_12.student1.vanalysis", &analysis_by("student", 1, 101));
        put("abc_12.teacher1.vanalysis", &analysis_by("teacher", 1, 201));
        assert_eq!(best(12, Segmenter::Student), Some(101));
        assert_eq!(best(12, Segmenter::Teacher), Some(201));
        assert_eq!(best(12, Segmenter::V6), Some(8));
        // A newer build's student draft is preferred; an unknown id is never taken.
        put("abc_12.student2.vanalysis", &analysis_by("student", 2, 102));
        put("abc_12.coach1.vanalysis", &analysis_by("coach", 1, 301));
        assert_eq!(best(12, Segmenter::Student), Some(102));
        // The file's contents decide, not its name.
        put("abc_12_s20.vanalysis", &analysis_by("student", 3, 103));
        assert_eq!(best(12, Segmenter::Student), Some(103));
        assert_eq!(best(12, Segmenter::V6), Some(8));

        std::fs::remove_dir_all(&root).ok();
    }
}
