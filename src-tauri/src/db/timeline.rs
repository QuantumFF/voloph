//! The draft timeline (ADR 0002): reading a recording's rallies for the player
//! and the five inline corrections (adjust, split, merge, add, delete) plus the
//! rally flag. Rallies are the atomic unit of review (CONTEXT.md).

use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

use super::stored_key;

/// Confidence stamped on a hand-corrected rally. The user has confirmed it by
/// editing it, so it is fully certain — never an uncertain region (ADR 0002).
pub(crate) const CORRECTED_CONFIDENCE: f64 = 1.0;

/// A rally as the player needs it: the segmenter's interval plus its database
/// `id`, so inline timeline corrections (issue #7) can target a specific rally
/// row. Distinct from [`crate::segment::Rally`] (the segmenter's id-less output)
/// because once persisted a rally is identified by its row, not by re-running
/// the heuristic.
#[derive(Debug, Serialize)]
pub struct TimelineRally {
    pub id: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub confidence: f64,
    /// Whether the user flagged this rally as one that matters (issue #10) — the
    /// source material for an export reel, orthogonal to its annotations.
    pub flagged: bool,
}

/// A recording's draft timeline as the player needs it: where the recording is
/// in its segmentation lifecycle, its duration, and the rally intervals.
#[derive(Debug, Serialize)]
pub struct Timeline {
    /// Segmentation state (ADR 0002): `unknown` (still queued/processing),
    /// `ready` (rallies below are the draft), or `failed`.
    pub segment_state: String,
    /// Recording duration in ms (`None` until segmented), so the player can lay
    /// rallies out over the full span.
    pub duration_ms: Option<i64>,
    pub rallies: Vec<TimelineRally>,
    /// Downsampled audio waveform peaks in `[0, 1]` (issue #6) for the timeline
    /// strip; empty until segmented. Shuttle hits show as spikes, so rally
    /// boundaries can be eyeballed against the rally blocks laid over them.
    pub waveform: Vec<f32>,
}

/// The draft timeline for the recording at `path`, for the player. `None` when
/// the path is not a registered recording (e.g. opened straight from a dialog).
pub fn recording_timeline(conn: &Connection, path: &str) -> rusqlite::Result<Option<Timeline>> {
    let Some((kind, key)) = stored_key(conn, path)? else {
        return Ok(None);
    };
    let row = conn
        .query_row(
            "SELECT id, segment_state, duration_ms, waveform FROM recordings
             WHERE library = ?1 AND path = ?2",
            rusqlite::params![&kind, &key],
            |row| {
                let id: i64 = row.get(0)?;
                let state: String = row.get(1)?;
                let duration: Option<i64> = row.get(2)?;
                let waveform: Option<String> = row.get(3)?;
                Ok((id, state, duration, waveform))
            },
        )
        .optional()?;
    let Some((id, segment_state, duration_ms, waveform_json)) = row else {
        return Ok(None);
    };
    let waveform = parse_waveform(waveform_json.as_deref());

    let mut stmt = conn.prepare(
        "SELECT id, start_ms, end_ms, confidence, flagged FROM rallies
         WHERE recording_id = ?1 ORDER BY start_ms",
    )?;
    let rallies = stmt
        .query_map([id], |row| {
            Ok(TimelineRally {
                id: row.get(0)?,
                start_ms: row.get(1)?,
                end_ms: row.get(2)?,
                confidence: row.get(3)?,
                flagged: row.get::<_, i64>(4)? != 0,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    Ok(Some(Timeline {
        segment_state,
        duration_ms,
        rallies,
        waveform,
    }))
}

/// The database id of the recording at `path`, or `None` when unregistered.
/// The inline-correction commands resolve the recording first, then scope every
/// edit to its rallies, so a stray id from another recording cannot be touched.
pub(crate) fn recording_id(conn: &Connection, path: &str) -> rusqlite::Result<Option<i64>> {
    let Some((kind, key)) = stored_key(conn, path)? else {
        return Ok(None);
    };
    conn.query_row(
        "SELECT id FROM recordings WHERE library = ?1 AND path = ?2",
        rusqlite::params![&kind, &key],
        |row| row.get(0),
    )
    .optional()
}

/// Whether a recording carries **hand-touched** review state (ADR 0011): a
/// flagged rally, a hand-corrected rally (confidence bumped to
/// `CORRECTED_CONFIDENCE` by an inline edit), or any annotation. Pure
/// machine-produced segmentation (uncertain rallies, no flags, no annotations)
/// does not count — the cross-library carry-over only ever moves work a human did.
pub(crate) fn is_hand_touched(conn: &Connection, id: i64) -> rusqlite::Result<bool> {
    let touched: bool = conn.query_row(
        "SELECT
            EXISTS(SELECT 1 FROM rallies WHERE recording_id = ?1
                   AND (flagged = 1 OR confidence >= ?2))
            OR EXISTS(SELECT 1 FROM annotations WHERE recording_id = ?1)",
        rusqlite::params![id, CORRECTED_CONFIDENCE],
        |r| Ok(r.get::<_, i64>(0)? != 0),
    )?;
    Ok(touched)
}

/// Move a rally's boundaries (issue #7 — adjust, and the mechanic behind split
/// and merge). `start_ms`/`end_ms` are clamped to a sane order and the edit is
/// scoped to the recording at `path` so only its own rally can be moved. The
/// rally becomes fully certain (a hand-corrected boundary is no longer doubted).
/// Returns `false` when the recording or rally is not found.
///
/// Annotations are pinned to absolute time, not to a rally (glossary), so moving
/// a boundary cannot disturb them — there is nothing to cascade.
pub fn update_rally(
    conn: &Connection,
    path: &str,
    rally_id: i64,
    start_ms: i64,
    end_ms: i64,
) -> rusqlite::Result<bool> {
    let Some(rid) = recording_id(conn, path)? else {
        return Ok(false);
    };
    let (lo, hi) = ordered(start_ms, end_ms);
    let changed = conn.execute(
        "UPDATE rallies SET start_ms = ?1, end_ms = ?2, confidence = ?3
         WHERE id = ?4 AND recording_id = ?5",
        rusqlite::params![lo, hi, CORRECTED_CONFIDENCE, rally_id, rid],
    )?;
    Ok(changed > 0)
}

/// Create a rally over a span the segmenter missed (issue #7 — add, and the
/// mechanic behind split). Scoped to the recording at `path`; the new rally is
/// fully certain. Returns the new rally's id, or `None` when `path` is not
/// registered.
pub fn add_rally(
    conn: &Connection,
    path: &str,
    start_ms: i64,
    end_ms: i64,
) -> rusqlite::Result<Option<i64>> {
    let Some(rid) = recording_id(conn, path)? else {
        return Ok(None);
    };
    let (lo, hi) = ordered(start_ms, end_ms);
    conn.execute(
        "INSERT INTO rallies (recording_id, start_ms, end_ms, confidence)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![rid, lo, hi, CORRECTED_CONFIDENCE],
    )?;
    Ok(Some(conn.last_insert_rowid()))
}

/// Remove a rally (issue #7 — delete a false positive, leaving its span a
/// derived gap; also the mechanic behind merge). Scoped to the recording at
/// `path`. Returns `false` when the recording or rally is not found.
pub fn delete_rally(conn: &Connection, path: &str, rally_id: i64) -> rusqlite::Result<bool> {
    let Some(rid) = recording_id(conn, path)? else {
        return Ok(false);
    };
    let changed = conn.execute(
        "DELETE FROM rallies WHERE id = ?1 AND recording_id = ?2",
        rusqlite::params![rally_id, rid],
    )?;
    Ok(changed > 0)
}

/// Set a rally's flag (issue #10 — "this rally matters", the export-reel source).
/// Scoped to the recording at `path` like the inline rally edits, so a stray id
/// from another recording cannot be touched. Idempotent — setting the current
/// value again is harmless. Returns `false` when the recording or rally is not
/// found.
pub fn set_rally_flag(
    conn: &Connection,
    path: &str,
    rally_id: i64,
    flagged: bool,
) -> rusqlite::Result<bool> {
    let Some(rid) = recording_id(conn, path)? else {
        return Ok(false);
    };
    let changed = conn.execute(
        "UPDATE rallies SET flagged = ?1 WHERE id = ?2 AND recording_id = ?3",
        rusqlite::params![flagged as i64, rally_id, rid],
    )?;
    Ok(changed > 0)
}

/// The two boundaries in ascending order, so a reversed drag still yields a
/// well-formed rally interval.
fn ordered(a: i64, b: i64) -> (i64, i64) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

/// Parse the stored waveform JSON (a bare array of floats written by
/// `save_rallies`) back into peaks. A null/absent or malformed value yields an
/// empty waveform — the strip then just omits the waveform, harmless. Hand-parsed
/// to avoid pulling in a JSON dependency for one trivial array.
pub(crate) fn parse_waveform(json: Option<&str>) -> Vec<f32> {
    let Some(json) = json else {
        return Vec::new();
    };
    json.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .filter_map(|s| s.trim().parse::<f32>().ok())
        .collect()
}
