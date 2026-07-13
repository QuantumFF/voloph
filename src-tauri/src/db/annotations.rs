//! Verdict annotations pinned to a moment (CONTEXT.md), and the cross-session
//! moment filter that is the payoff of the structured data (issue #11).

use rusqlite::Connection;
use serde::Serialize;

use super::{absolute, active_kind, library_path_of, recording_id};

/// A verdict annotation as the player needs it (issue #8): its row `id`, the
/// recording-local absolute timestamp it is pinned to, and its one-keystroke
/// verdict. The rally it belongs to is *not* stored — it is implied by which
/// rally's range contains `time_ms` (glossary), so moving a rally boundary never
/// disturbs an annotation. The quick verdict is optionally enriched (issue #9)
/// with a structured `aspect` (the dimension it judges) and a free-text `note`
/// (where shot type lives); both are null until the user enriches it.
#[derive(Debug, Serialize)]
pub struct Annotation {
    pub id: i64,
    pub time_ms: i64,
    pub verdict: String,
    pub aspect: Option<String>,
    pub note: Option<String>,
}

/// Drop a verdict annotation at `time_ms` (recording-local) on the recording at
/// `path` (issue #8 — the fast capture path: verdict only, no pause). Scoped to
/// the recording at `path` like the inline rally edits, so a stray path writes
/// nothing. Returns the new annotation's id, or `None` when `path` is not a
/// registered recording. Nothing prevents two annotations at the same timestamp:
/// a moment with mixed verdicts is recorded as more than one (glossary).
pub fn add_annotation(
    conn: &Connection,
    path: &str,
    time_ms: i64,
    verdict: &str,
) -> rusqlite::Result<Option<i64>> {
    let Some(rid) = recording_id(conn, path)? else {
        return Ok(None);
    };
    conn.execute(
        "INSERT INTO annotations (recording_id, time_ms, verdict) VALUES (?1, ?2, ?3)",
        rusqlite::params![rid, time_ms.max(0), verdict],
    )?;
    Ok(Some(conn.last_insert_rowid()))
}

/// Every verdict annotation on the recording at `path`, in timestamp order, so
/// the player can lay their markers over the timeline strip (issue #8) and show
/// their aspect/note in the inspector (issue #9). Empty when the recording has
/// none or `path` is not registered.
pub fn recording_annotations(conn: &Connection, path: &str) -> rusqlite::Result<Vec<Annotation>> {
    let Some(rid) = recording_id(conn, path)? else {
        return Ok(Vec::new());
    };
    let mut stmt = conn.prepare(
        "SELECT id, time_ms, verdict, aspect, note FROM annotations
         WHERE recording_id = ?1 ORDER BY time_ms, id",
    )?;
    let annotations = stmt
        .query_map([rid], map_annotation)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(annotations)
}

/// Map an `annotations` row selected as `id, time_ms, verdict, aspect, note` (in that
/// column order) into an [`Annotation`]. Shared by every query that reads them back so
/// the column order lives in one place.
fn map_annotation(row: &rusqlite::Row) -> rusqlite::Result<Annotation> {
    Ok(Annotation {
        id: row.get(0)?,
        time_ms: row.get(1)?,
        verdict: row.get(2)?,
        aspect: row.get(3)?,
        note: row.get(4)?,
    })
}

/// Enrich or re-classify one annotation (issue #9): set its `verdict`, structured
/// `aspect`, and free-text `note`. Scoped to the recording at `path` like the
/// rally edits, so a stray path touches nothing. `aspect`/`note` are set as given
/// — pass `None` to clear a field. Returns `false` when the recording or
/// annotation is not found.
pub fn update_annotation(
    conn: &Connection,
    path: &str,
    id: i64,
    verdict: &str,
    aspect: Option<&str>,
    note: Option<&str>,
) -> rusqlite::Result<bool> {
    let Some(rid) = recording_id(conn, path)? else {
        return Ok(false);
    };
    let changed = conn.execute(
        "UPDATE annotations SET verdict = ?1, aspect = ?2, note = ?3
         WHERE id = ?4 AND recording_id = ?5",
        rusqlite::params![verdict, aspect, note, id, rid],
    )?;
    Ok(changed > 0)
}

/// Remove one annotation (issue #9). Scoped to the recording at `path`. Returns
/// `false` when the recording or annotation is not found.
pub fn delete_annotation(conn: &Connection, path: &str, id: i64) -> rusqlite::Result<bool> {
    let Some(rid) = recording_id(conn, path)? else {
        return Ok(false);
    };
    let changed = conn.execute(
        "DELETE FROM annotations WHERE id = ?1 AND recording_id = ?2",
        rusqlite::params![id, rid],
    )?;
    Ok(changed > 0)
}

/// Rally-length threshold in ms (CONTEXT.md: every rally is classified long or
/// short by its duration against a threshold, objectively — never from quality).
/// Mirrors `LONG_RALLY_MS` in the player frontend; kept here so the cross-session
/// filter derives length in SQL without a round-trip.
const LONG_RALLY_MS: i64 = 15_000;

/// A rally matching a cross-session filter (issue #11), lifted with enough
/// context to identify it and jump straight to it. The jump target is the
/// containing recording's `path` opened at the rally's `start_ms`; `session_id`
/// / `capture_day` name the outing, `recording_id` disambiguates within it. When
/// the filter carries a verdict or aspect, `annotations` holds the rally's
/// matching moments (so their notes/aspects show in the results); a length- or
/// flag-only filter leaves it empty (the rally itself is the result).
#[derive(Debug, Serialize)]
pub struct FilteredRally {
    pub session_id: i64,
    pub capture_day: String,
    pub recording_id: i64,
    pub recording_path: String,
    pub rally_id: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    /// Derived from duration against `LONG_RALLY_MS`, never from quality.
    pub long: bool,
    pub flagged: bool,
    pub annotations: Vec<Annotation>,
}

/// Cross-session filter over rallies and their annotations (issue #11 — the
/// payoff of the structured data). Every filter is optional and they combine with
/// AND: a rally is returned when it satisfies every set filter. `verdict`/`aspect`
/// filter on the rally's annotations (the rally must contain a moment matching
/// them, and only the matching moments come back attached); `length`
/// (`Some(true)` = long) and `flagged` filter the rally itself. With no filter
/// set every rally is returned. Results are newest session first, then in
/// recording/timeline order — the order the user reviews by.
pub fn filter_moments(
    conn: &Connection,
    verdict: Option<&str>,
    aspect: Option<&str>,
    length: Option<bool>,
    flagged: Option<bool>,
) -> rusqlite::Result<Vec<FilteredRally>> {
    // An annotation matches the moment filters (verdict/aspect); its timestamp is
    // inside the rally's span (glossary — the rally owns the moment). The rally is
    // kept only when at least one such annotation exists (when either is set).
    let annotation_matches =
        "a.recording_id = r.recording_id AND a.time_ms >= r.start_ms AND a.time_ms < r.end_ms
         AND (?1 IS NULL OR a.verdict = ?1)
         AND (?2 IS NULL OR a.aspect = ?2)";
    // Scoped to the active library (ADR 0011 — the switcher scopes cross-session
    // filters). `?5` is the active kind; only its recordings and sessions match.
    let sql = format!(
        "SELECT s.id, s.capture_day, rec.id, rec.path,
                r.id, r.start_ms, r.end_ms, r.flagged
         FROM rallies r
         JOIN recordings rec ON rec.id = r.recording_id
         JOIN sessions s ON s.id = rec.session_id
         WHERE rec.library = ?5
           AND (?3 IS NULL OR (r.end_ms - r.start_ms >= {LONG_RALLY_MS}) = ?3)
           AND (?4 IS NULL OR r.flagged = ?4)
           AND ((?1 IS NULL AND ?2 IS NULL)
                OR EXISTS (SELECT 1 FROM annotations a WHERE {annotation_matches}))
         ORDER BY s.capture_day DESC, rec.path, r.start_ms"
    );
    // The jump target opens the recording by its absolute path (ADR 0011); with
    // no active library nothing resolves, so return no results.
    let kind = active_kind(conn)?;
    let Some(library) = library_path_of(conn, &kind)? else {
        return Ok(Vec::new());
    };
    let mut stmt = conn.prepare(&sql)?;
    let want_annotations = verdict.is_some() || aspect.is_some();
    let rows: Vec<FilteredRally> = stmt
        .query_map(
            rusqlite::params![verdict, aspect, length, flagged, kind],
            |row| {
                let start_ms: i64 = row.get(5)?;
                let end_ms: i64 = row.get(6)?;
                let stored: String = row.get(3)?;
                Ok(FilteredRally {
                    session_id: row.get(0)?,
                    capture_day: row.get(1)?,
                    recording_id: row.get(2)?,
                    recording_path: absolute(&library, &stored),
                    rally_id: row.get(4)?,
                    start_ms,
                    end_ms,
                    long: end_ms - start_ms >= LONG_RALLY_MS,
                    flagged: row.get::<_, i64>(7)? != 0,
                    annotations: Vec::new(),
                })
            },
        )?
        .collect::<rusqlite::Result<_>>()?;

    // Attach the matching moments only when a moment filter is active — a
    // length/flag-only query is about the rally itself, so its annotations would
    // just be noise in the results.
    if !want_annotations {
        return Ok(rows);
    }
    let mut astmt = conn.prepare(
        "SELECT id, time_ms, verdict, aspect, note FROM annotations
         WHERE recording_id = ?1 AND time_ms >= ?2 AND time_ms < ?3
           AND (?4 IS NULL OR verdict = ?4)
           AND (?5 IS NULL OR aspect = ?5)
         ORDER BY time_ms, id",
    )?;
    let mut out = rows;
    for r in &mut out {
        r.annotations = astmt
            .query_map(
                rusqlite::params![r.recording_id, r.start_ms, r.end_ms, verdict, aspect],
                map_annotation,
            )?
            .collect::<rusqlite::Result<_>>()?;
    }
    Ok(out)
}

/// Distinct aspects present on annotations in the active library — the aspect
/// vocabulary as it actually exists in the data (CONTEXT.md: a user-editable
/// vocabulary, not a fixed enum). The frontend unions this with its seeded list
/// so aspects a received bundle imported (ADR 0012) — which may lie outside the
/// seeds — still appear as filter options. Sorted for a stable order.
pub fn aspect_vocabulary(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let kind = active_kind(conn)?;
    let mut stmt = conn.prepare(
        "SELECT DISTINCT a.aspect FROM annotations a
         JOIN recordings rec ON rec.id = a.recording_id
         WHERE rec.library = ?1 AND a.aspect IS NOT NULL
         ORDER BY a.aspect",
    )?;
    let out = stmt.query_map([kind], |row| row.get(0))?.collect();
    out
}
