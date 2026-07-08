//! Deriving a recording's session day (CONTEXT.md: the embedded capture date,
//! falling back to mtime) and re-homing a recording once the embedded date is
//! read (ADR 0007).

use rusqlite::{Connection, OptionalExtension};

/// The session capture day (`YYYY-MM-DD`) for a recording, preferring the date
/// the camera embedded in the file over the file's mtime. mtime is
/// content-independent and gets clobbered by copying off a camera/SD card, by
/// cloud sync, and by our own in-place transcode (ADR 0005), so it is an
/// unreliable signal for *when* a recording was made; the embedded creation date
/// rides with the bytes. We take the first source that yields a plausible date:
///   1. `com.apple.quicktime.creationdate` — local wall-clock **with** offset, so
///      its date portion is the player's local day (a late-evening session stays
///      on its own day instead of rolling into the next UTC one).
///   2. `creation_time` — ISO-8601 UTC; grouped by its UTC day.
///   3. file mtime (UTC day) — last resort.
///
/// A tag that is absent, unparseable, or implausible (the `0000-00-00` a
/// metadata-stripped transcode used to leave behind, a dead-clock epoch date) is
/// skipped, not trusted, so it falls through to the next source.
pub fn derive_capture_day(
    embedded: &crate::media::CaptureDate,
    modified: std::time::SystemTime,
) -> String {
    embedded
        .quicktime_creationdate
        .as_deref()
        .and_then(day_from_iso)
        .or_else(|| embedded.creation_time.as_deref().and_then(day_from_iso))
        .unwrap_or_else(|| capture_day(modified))
}

/// Extract a plausible `YYYY-MM-DD` from the front of an ISO-8601-ish timestamp
/// (e.g. `2024-03-15T21:30:00+0800`). Returns `None` unless the string starts
/// with a well-formed date in a plausible year range, so a garbage tag value
/// falls through to the next capture-date source rather than mis-grouping.
fn day_from_iso(timestamp: &str) -> Option<String> {
    let date = timestamp.get(..10)?;
    let b = date.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let year: i32 = date.get(0..4)?.parse().ok()?;
    let month: u32 = date.get(5..7)?.parse().ok()?;
    let day: u32 = date.get(8..10)?.parse().ok()?;
    if !(2000..=2100).contains(&year) || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(date.to_string())
}

/// Capture day as `YYYY-MM-DD`, derived from the file's modified time (UTC). The
/// provisional day at scan time and the final fallback in [`derive_capture_day`]
/// when a recording carries no usable embedded capture date.
pub(crate) fn capture_day(modified: std::time::SystemTime) -> String {
    let secs = modified
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    days_to_ymd(secs.div_euclid(86_400))
}

/// Convert a count of days since 1970-01-01 into a `YYYY-MM-DD` string.
/// Civil-from-days algorithm (Howard Hinnant), avoids a date-crate dependency.
fn days_to_ymd(days: i64) -> String {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}", y, m, d)
}

/// Re-derive a recording's capture day from its embedded metadata (falling back
/// to mtime) and re-home it under the matching session — creating that session if
/// it does not exist and deleting its previous session if re-homing leaves it
/// empty — then mark its capture date `refined` so it is not probed again.
///
/// Runs in one transaction so the recording is never momentarily attached to two
/// sessions (or none), and is idempotent: a recording already on the correct day
/// stays put and only its `date_state` flips. Rallies and annotations key off the
/// recording, not the session, so they travel with it automatically.
pub fn refine_capture_day(
    conn: &mut Connection,
    id: i64,
    path: &str,
    embedded: &crate::media::CaptureDate,
) -> rusqlite::Result<()> {
    let modified = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(std::time::UNIX_EPOCH);
    let day = derive_capture_day(embedded, modified);

    let tx = conn.transaction()?;
    let old: Option<(i64, String)> = tx
        .query_row(
            "SELECT session_id, library FROM recordings WHERE id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    // The recording may have been removed between scan and this pass; nothing to
    // re-home, so just drop out without touching any session.
    let Some((old_session, library)) = old else {
        tx.commit()?;
        return Ok(());
    };

    // Re-home within the recording's own library — a session never spans libraries.
    tx.execute(
        "INSERT OR IGNORE INTO sessions (capture_day, library) VALUES (?1, ?2)",
        rusqlite::params![&day, &library],
    )?;
    let new_session: i64 = tx.query_row(
        "SELECT id FROM sessions WHERE capture_day = ?1 AND library = ?2",
        rusqlite::params![&day, &library],
        |row| row.get(0),
    )?;

    tx.execute(
        "UPDATE recordings
         SET capture_day = ?1, session_id = ?2, date_state = 'refined'
         WHERE id = ?3",
        rusqlite::params![day, new_session, id],
    )?;

    // Garbage-collect the old session if this was its last recording.
    if old_session != new_session {
        tx.execute(
            "DELETE FROM sessions WHERE id = ?1
             AND NOT EXISTS (SELECT 1 FROM recordings WHERE session_id = ?1)",
            [old_session],
        )?;
    }
    tx.commit()?;
    Ok(())
}
