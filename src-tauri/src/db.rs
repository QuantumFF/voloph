use std::path::Path;

use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

/// Video file extensions we register as recordings.
const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "mov", "m4v", "avi", "mkv", "mts", "m2ts", "ts", "webm",
];

/// A recording referenced in place on disk.
#[derive(Debug, Serialize)]
pub struct Recording {
    pub id: i64,
    pub path: String,
    pub file_size: i64,
    pub quick_hash: String,
    pub capture_day: String,
    /// Playability lifecycle: `unknown` (not yet probed), `ready` (probed — the
    /// frame rate is captured and libmpv can play the original directly, ADR
    /// 0008), or `failed` (probe could not read the file). The column keeps its
    /// historical `transcode_state` name; there is no longer a transcode step
    /// (ADR 0005 superseded), so it never holds `pending`.
    pub transcode_state: String,
    /// Segmentation lifecycle (ADR 0002): `unknown` (not yet segmented or
    /// queued), `ready` (draft timeline produced), or `failed` (audio
    /// extraction / segmentation error). Only starts once `transcode_state` is
    /// `ready` (i.e. the recording has been probed).
    pub segment_state: String,
    /// Recording duration in milliseconds, learned during segmentation. `None`
    /// until segmented; the player uses it to lay rallies out over the timeline.
    pub duration_ms: Option<i64>,
    /// Number of rallies in the draft timeline — a glanceable count in the
    /// session list once segmentation is done.
    pub rally_count: i64,
}

/// A session: one capture day, holding one or more recordings.
#[derive(Debug, Serialize)]
pub struct Session {
    pub id: i64,
    pub capture_day: String,
    pub recordings: Vec<Recording>,
}

/// Summary of a scan pass.
#[derive(Debug, Serialize)]
pub struct ScanResult {
    pub registered: usize,
    pub skipped: usize,
}

/// Open (creating if needed) the metadata database and ensure the schema exists.
pub fn open(db_path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(db_path)?;
    conn.pragma_update(None, "foreign_keys", true)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS sessions (
            id          INTEGER PRIMARY KEY,
            capture_day TEXT NOT NULL UNIQUE
        );
        CREATE TABLE IF NOT EXISTS recordings (
            id          INTEGER PRIMARY KEY,
            session_id  INTEGER NOT NULL REFERENCES sessions(id),
            path        TEXT NOT NULL UNIQUE,
            file_size   INTEGER NOT NULL,
            quick_hash  TEXT NOT NULL,
            capture_day TEXT NOT NULL,
            transcode_state TEXT NOT NULL DEFAULT 'unknown',
            segment_state   TEXT NOT NULL DEFAULT 'unknown',
            date_state      TEXT NOT NULL DEFAULT 'unknown',
            duration_ms     INTEGER,
            waveform        TEXT
        );
        CREATE TABLE IF NOT EXISTS rallies (
            id           INTEGER PRIMARY KEY,
            recording_id INTEGER NOT NULL REFERENCES recordings(id) ON DELETE CASCADE,
            start_ms     INTEGER NOT NULL,
            end_ms       INTEGER NOT NULL,
            confidence   REAL NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_rallies_recording ON rallies(recording_id);
        CREATE TABLE IF NOT EXISTS scanned_folders (
            path TEXT NOT NULL UNIQUE
        );",
    )?;
    // Upgrade DBs created before later slices. The CREATE above is a no-op when
    // a table already exists, so add new columns here; ignore the
    // duplicate-column error when one is already present.
    let _ = conn.execute(
        "ALTER TABLE recordings ADD COLUMN transcode_state TEXT NOT NULL DEFAULT 'unknown'",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE recordings ADD COLUMN segment_state TEXT NOT NULL DEFAULT 'unknown'",
        [],
    );
    // Capture-date lifecycle: `unknown` until the media worker reads the embedded
    // creation date and re-homes the session (see `refine_capture_day`), then
    // `refined`. Adding it as `unknown` to an existing DB re-derives every
    // already-imported recording's day on the next worker pass — the backfill
    // that fixes recordings grouped by a wrong mtime.
    let _ = conn.execute(
        "ALTER TABLE recordings ADD COLUMN date_state TEXT NOT NULL DEFAULT 'unknown'",
        [],
    );
    let _ = conn.execute("ALTER TABLE recordings ADD COLUMN duration_ms INTEGER", []);
    // Downsampled audio waveform peaks for the timeline strip (issue #6), stored
    // as a JSON array of normalized `[0,1]` floats. Produced alongside the draft
    // timeline during segmentation; null until a recording is segmented.
    let _ = conn.execute("ALTER TABLE recordings ADD COLUMN waveform TEXT", []);
    // Video frame rate captured at probe time (issue #19) so the player can
    // frame-step exactly. Null until probed; the player defaults to 30 fps when
    // unknown. The in-place transcode does not resample fps, so the probed value
    // stays valid afterward.
    let _ = conn.execute("ALTER TABLE recordings ADD COLUMN fps REAL", []);
    Ok(conn)
}

fn is_video(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| VIDEO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

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
fn capture_day(modified: std::time::SystemTime) -> String {
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

/// Cheap content fingerprint: file size plus a SHA-256 of the leading bytes.
/// Used to re-locate a file moved outside the app (ADR 0003), not for integrity.
fn quick_hash(path: &Path, file_size: u64) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;

    const SAMPLE: usize = 64 * 1024;
    let mut file = std::fs::File::open(path)?;
    let mut buf = vec![0u8; SAMPLE];
    let read = file.read(&mut buf)?;
    let mut hasher = Sha256::new();
    hasher.update(file_size.to_le_bytes());
    hasher.update(&buf[..read]);
    Ok(format!("{:x}", hasher.finalize()))
}

/// Walk `folder`, register any video files as recordings (referenced in place),
/// and group them into sessions by capture day. Idempotent: files already
/// registered by path are left untouched, so re-scanning never duplicates.
pub fn scan_folder(conn: &mut Connection, folder: &Path) -> rusqlite::Result<ScanResult> {
    let mut registered = 0usize;
    let mut skipped = 0usize;

    let tx = conn.transaction()?;
    // Remember the scanned root so Refresh can re-walk it for files added since
    // (see [`scanned_folders`]). Idempotent, like the recording dedup below.
    tx.execute(
        "INSERT OR IGNORE INTO scanned_folders (path) VALUES (?1)",
        [&folder.to_string_lossy().to_string()],
    )?;
    for entry in walkdir::WalkDir::new(folder)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !entry.file_type().is_file() || !is_video(path) {
            continue;
        }
        let path_str = path.to_string_lossy().to_string();

        // Idempotent dedup: skip files already registered by path.
        let already: bool = tx.query_row(
            "SELECT 1 FROM recordings WHERE path = ?1",
            [&path_str],
            |_| Ok(true),
        )
        .unwrap_or(false);
        if already {
            skipped += 1;
            continue;
        }

        let meta = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let file_size = meta.len();
        let day = capture_day(meta.modified().unwrap_or(std::time::UNIX_EPOCH));
        let hash = quick_hash(path, file_size).unwrap_or_default();

        tx.execute(
            "INSERT OR IGNORE INTO sessions (capture_day) VALUES (?1)",
            [&day],
        )?;
        let session_id: i64 = tx.query_row(
            "SELECT id FROM sessions WHERE capture_day = ?1",
            [&day],
            |row| row.get(0),
        )?;

        tx.execute(
            "INSERT INTO recordings (session_id, path, file_size, quick_hash, capture_day)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![session_id, path_str, file_size as i64, hash, day],
        )?;
        registered += 1;
    }
    tx.commit()?;

    Ok(ScanResult {
        registered,
        skipped,
    })
}

/// Every folder root a previous scan registered, so Refresh can re-walk them all
/// for recordings added since the last scan (see [`scan_folder`]).
pub fn scanned_folders(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT path FROM scanned_folders ORDER BY path")?;
    let folders = stmt
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(folders)
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
    let old_session: Option<i64> = tx
        .query_row(
            "SELECT session_id FROM recordings WHERE id = ?1",
            [id],
            |row| row.get(0),
        )
        .optional()?;
    // The recording may have been removed between scan and this pass; nothing to
    // re-home, so just drop out without touching any session.
    let Some(old_session) = old_session else {
        tx.commit()?;
        return Ok(());
    };

    tx.execute(
        "INSERT OR IGNORE INTO sessions (capture_day) VALUES (?1)",
        [&day],
    )?;
    let new_session: i64 = tx.query_row(
        "SELECT id FROM sessions WHERE capture_day = ?1",
        [&day],
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

/// All sessions (newest day first) with their recordings nested under them.
pub fn list_sessions(conn: &Connection) -> rusqlite::Result<Vec<Session>> {
    let mut stmt =
        conn.prepare("SELECT id, capture_day FROM sessions ORDER BY capture_day DESC")?;
    let sessions: Vec<(i64, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;

    let mut out = Vec::with_capacity(sessions.len());
    for (id, capture_day) in sessions {
        let mut rstmt = conn.prepare(
            "SELECT r.id, r.path, r.file_size, r.quick_hash, r.capture_day,
                    r.transcode_state, r.segment_state, r.duration_ms,
                    (SELECT COUNT(*) FROM rallies WHERE recording_id = r.id)
             FROM recordings r WHERE r.session_id = ?1 ORDER BY r.path",
        )?;
        let recordings = rstmt
            .query_map([id], |row| {
                Ok(Recording {
                    id: row.get(0)?,
                    path: row.get(1)?,
                    file_size: row.get(2)?,
                    quick_hash: row.get(3)?,
                    capture_day: row.get(4)?,
                    transcode_state: row.get(5)?,
                    segment_state: row.get(6)?,
                    duration_ms: row.get(7)?,
                    rally_count: row.get(8)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        out.push(Session {
            id,
            capture_day,
            recordings,
        });
    }
    Ok(out)
}

/// One unit of work for the background media worker, in priority order: probe a
/// recording for its frame rate (which also marks it playable), then produce its
/// draft timeline (segment). Segmentation is gated on `transcode_state = 'ready'`
/// so it always runs after the probe.
#[derive(Debug, PartialEq)]
pub enum MediaWork {
    /// Capture day not yet refined — read the camera's embedded creation date and
    /// re-home the recording's session if it differs from the provisional
    /// mtime-derived day (see [`refine_capture_day`]).
    CaptureDate(i64, String),
    /// Not yet probed — run ffprobe for the frame rate and mark it playable
    /// (issue #19; libmpv plays the original directly, ADR 0008).
    Probe(i64, String),
    /// Playable but not yet segmented — extract audio and segment (ADR 0002).
    Segment(i64, String),
}

/// The next unit of background media work, lowest id first within each phase, or
/// `None` when every recording is both probed and segmented (or has failed).
pub fn next_media_work(conn: &Connection) -> rusqlite::Result<Option<MediaWork>> {
    // Phase 0: anything whose capture day is still the provisional mtime guess.
    // Runs first so a recording settles into the right session as quickly as
    // possible; independent of the probe/segment pipeline below.
    let date = conn
        .query_row(
            "SELECT id, path FROM recordings
             WHERE date_state = 'unknown' ORDER BY id LIMIT 1",
            [],
            |row| Ok(MediaWork::CaptureDate(row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if date.is_some() {
        return Ok(date);
    }
    // Phase 1: anything not yet probed.
    let probe = conn
        .query_row(
            "SELECT id, path FROM recordings
             WHERE transcode_state = 'unknown' ORDER BY id LIMIT 1",
            [],
            |row| Ok(MediaWork::Probe(row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if probe.is_some() {
        return Ok(probe);
    }
    // Phase 2: anything probed but not yet segmented.
    conn.query_row(
        "SELECT id, path FROM recordings
         WHERE transcode_state = 'ready' AND segment_state = 'unknown'
         ORDER BY id LIMIT 1",
        [],
        |row| Ok(MediaWork::Segment(row.get(0)?, row.get(1)?)),
    )
    .optional()
}

/// Record a probe's outcome (issue #19): the playability state (`ready` once
/// probed, since libmpv plays the original directly — ADR 0008 — or `failed`)
/// and the captured frame rate, which the player frame-steps by.
pub fn set_probe_result(
    conn: &Connection,
    id: i64,
    state: &str,
    fps: Option<f64>,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE recordings SET transcode_state = ?1, fps = ?2 WHERE id = ?3",
        rusqlite::params![state, fps, id],
    )?;
    Ok(())
}

/// Reset a recording's draft timeline so the media worker re-segments it on its
/// next pass: drop its rallies and return it to `unknown`. Backs the Re-analyze
/// action used while tuning the segmenter (ADR 0002). A no-op when `path` is not
/// registered.
pub fn reset_segmentation(conn: &Connection, path: &str) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM rallies WHERE recording_id = (SELECT id FROM recordings WHERE path = ?1)",
        [path],
    )?;
    conn.execute(
        "UPDATE recordings
         SET segment_state = 'unknown', duration_ms = NULL, waveform = NULL
         WHERE path = ?1",
        [path],
    )?;
    Ok(())
}

/// Reset every recording's draft timeline so the media worker re-segments the
/// whole library on its next pass — the bulk Re-analyze-all counterpart of
/// [`reset_segmentation`]. Like a per-recording re-analyze, this discards manual
/// corrections (ADR 0002).
pub fn reset_all_segmentation(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM rallies", [])?;
    conn.execute(
        "UPDATE recordings SET segment_state = 'unknown', duration_ms = NULL, waveform = NULL",
        [],
    )?;
    Ok(())
}

/// Advance a recording's segmentation lifecycle state (ADR 0002).
pub fn set_segment_state(conn: &Connection, id: i64, state: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE recordings SET segment_state = ?1 WHERE id = ?2",
        rusqlite::params![state, id],
    )?;
    Ok(())
}

/// Persist a recording's draft timeline: replace any prior rallies, store the
/// learned duration, and mark it segmented — all in one transaction so a
/// re-segment is atomic and never leaves a half-written timeline. Replacing
/// rather than appending keeps re-runs idempotent (matching the scan's contract).
pub fn save_rallies(
    conn: &mut Connection,
    recording_id: i64,
    duration_ms: i64,
    rallies: &[crate::segment::Rally],
    waveform: &[f32],
) -> rusqlite::Result<()> {
    // The waveform is a small fixed-length float array (segment::WAVEFORM_BUCKETS),
    // stored as a compact JSON array of two-decimal peaks — finer precision is
    // invisible on a strip a few hundred pixels wide and only bloats the row.
    let waveform_json = format!(
        "[{}]",
        waveform
            .iter()
            .map(|p| format!("{p:.2}"))
            .collect::<Vec<_>>()
            .join(",")
    );
    let tx = conn.transaction()?;
    tx.execute(
        "DELETE FROM rallies WHERE recording_id = ?1",
        [recording_id],
    )?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO rallies (recording_id, start_ms, end_ms, confidence)
             VALUES (?1, ?2, ?3, ?4)",
        )?;
        for r in rallies {
            stmt.execute(rusqlite::params![
                recording_id,
                r.start_ms,
                r.end_ms,
                r.confidence
            ])?;
        }
    }
    tx.execute(
        "UPDATE recordings SET segment_state = 'ready', duration_ms = ?1, waveform = ?2 WHERE id = ?3",
        rusqlite::params![duration_ms, waveform_json, recording_id],
    )?;
    tx.commit()
}

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
    let row = conn
        .query_row(
            "SELECT id, segment_state, duration_ms, waveform FROM recordings WHERE path = ?1",
            [path],
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
        "SELECT id, start_ms, end_ms, confidence FROM rallies
         WHERE recording_id = ?1 ORDER BY start_ms",
    )?;
    let rallies = stmt
        .query_map([id], |row| {
            Ok(TimelineRally {
                id: row.get(0)?,
                start_ms: row.get(1)?,
                end_ms: row.get(2)?,
                confidence: row.get(3)?,
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

/// Confidence stamped on a hand-corrected rally. The user has confirmed it by
/// editing it, so it is fully certain — never an uncertain region (ADR 0002).
const CORRECTED_CONFIDENCE: f64 = 1.0;

/// The database id of the recording at `path`, or `None` when unregistered.
/// The inline-correction commands resolve the recording first, then scope every
/// edit to its rallies, so a stray id from another recording cannot be touched.
fn recording_id(conn: &Connection, path: &str) -> rusqlite::Result<Option<i64>> {
    conn.query_row("SELECT id FROM recordings WHERE path = ?1", [path], |row| {
        row.get(0)
    })
    .optional()
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
    let (lo, hi) = if start_ms <= end_ms {
        (start_ms, end_ms)
    } else {
        (end_ms, start_ms)
    };
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
    let (lo, hi) = if start_ms <= end_ms {
        (start_ms, end_ms)
    } else {
        (end_ms, start_ms)
    };
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

/// Parse the stored waveform JSON (a bare array of floats written by
/// `save_rallies`) back into peaks. A null/absent or malformed value yields an
/// empty waveform — the strip then just omits the waveform, harmless. Hand-parsed
/// to avoid pulling in a JSON dependency for one trivial array.
fn parse_waveform(json: Option<&str>) -> Vec<f32> {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// An in-memory DB with the schema and one segmented recording carrying the
    /// given rally intervals — the starting point for an inline-correction test.
    fn db_with_rallies(path: &str, intervals: &[(i64, i64)]) -> (Connection, i64) {
        let mut conn = open(Path::new(":memory:")).unwrap();
        conn.execute(
            "INSERT INTO sessions (capture_day) VALUES ('2026-01-01')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO recordings (session_id, path, file_size, quick_hash, capture_day, transcode_state, segment_state, duration_ms)
             VALUES (1, ?1, 0, '', '2026-01-01', 'ready', 'ready', 100000)",
            [path],
        )
        .unwrap();
        let rallies: Vec<crate::segment::Rally> = intervals
            .iter()
            .map(|&(start_ms, end_ms)| crate::segment::Rally {
                start_ms,
                end_ms,
                confidence: 0.3, // start uncertain, so a correction flipping to 1.0 is visible
            })
            .collect();
        save_rallies(&mut conn, 1, 100_000, &rallies, &[]).unwrap();
        (conn, 1)
    }

    fn intervals(conn: &Connection, path: &str) -> Vec<(i64, i64, f64)> {
        recording_timeline(conn, path)
            .unwrap()
            .unwrap()
            .rallies
            .into_iter()
            .map(|r| (r.start_ms, r.end_ms, r.confidence))
            .collect()
    }

    #[test]
    fn adjust_moves_a_boundary_and_marks_it_certain() {
        let (conn, _) = db_with_rallies("/r.mp4", &[(1000, 5000)]);
        let id = recording_timeline(&conn, "/r.mp4").unwrap().unwrap().rallies[0].id;
        assert!(update_rally(&conn, "/r.mp4", id, 2000, 6000).unwrap());
        assert_eq!(intervals(&conn, "/r.mp4"), vec![(2000, 6000, 1.0)]);
    }

    #[test]
    fn add_creates_a_rally_over_a_missed_span() {
        let (conn, _) = db_with_rallies("/r.mp4", &[(1000, 5000)]);
        let new_id = add_rally(&conn, "/r.mp4", 8000, 12000).unwrap().unwrap();
        assert!(new_id > 0);
        // Ordered by start, the new rally lands after the original, fully certain.
        assert_eq!(
            intervals(&conn, "/r.mp4"),
            vec![(1000, 5000, 0.3), (8000, 12000, 1.0)]
        );
    }

    #[test]
    fn delete_removes_a_false_rally() {
        let (conn, _) = db_with_rallies("/r.mp4", &[(1000, 5000), (8000, 12000)]);
        let id = recording_timeline(&conn, "/r.mp4").unwrap().unwrap().rallies[0].id;
        assert!(delete_rally(&conn, "/r.mp4", id).unwrap());
        assert_eq!(intervals(&conn, "/r.mp4"), vec![(8000, 12000, 0.3)]);
    }

    #[test]
    fn split_is_update_plus_add() {
        // The frontend composes split from update + add; exercise the same here.
        let (conn, _) = db_with_rallies("/r.mp4", &[(1000, 9000)]);
        let id = recording_timeline(&conn, "/r.mp4").unwrap().unwrap().rallies[0].id;
        update_rally(&conn, "/r.mp4", id, 1000, 5000).unwrap();
        add_rally(&conn, "/r.mp4", 5000, 9000).unwrap();
        assert_eq!(
            intervals(&conn, "/r.mp4"),
            vec![(1000, 5000, 1.0), (5000, 9000, 1.0)]
        );
    }

    #[test]
    fn merge_is_update_plus_delete() {
        let (conn, _) = db_with_rallies("/r.mp4", &[(1000, 5000), (8000, 12000)]);
        let tl = recording_timeline(&conn, "/r.mp4").unwrap().unwrap();
        let (first, second) = (tl.rallies[0].id, tl.rallies[1].id);
        update_rally(&conn, "/r.mp4", first, 1000, 12000).unwrap();
        delete_rally(&conn, "/r.mp4", second).unwrap();
        assert_eq!(intervals(&conn, "/r.mp4"), vec![(1000, 12000, 1.0)]);
    }

    #[test]
    fn reversed_drag_is_normalized() {
        let (conn, _) = db_with_rallies("/r.mp4", &[(1000, 5000)]);
        let id = recording_timeline(&conn, "/r.mp4").unwrap().unwrap().rallies[0].id;
        // Dragging the start edge past the end swaps them rather than storing a
        // backwards interval.
        update_rally(&conn, "/r.mp4", id, 7000, 3000).unwrap();
        assert_eq!(intervals(&conn, "/r.mp4"), vec![(3000, 7000, 1.0)]);
    }

    #[test]
    fn edits_are_scoped_to_their_recording() {
        let (conn, _) = db_with_rallies("/a.mp4", &[(1000, 5000)]);
        conn.execute(
            "INSERT INTO recordings (session_id, path, file_size, quick_hash, capture_day, transcode_state, segment_state, duration_ms)
             VALUES (1, '/b.mp4', 0, '', '2026-01-01', 'ready', 'ready', 100000)",
            [],
        )
        .unwrap();
        let a_id = recording_timeline(&conn, "/a.mp4").unwrap().unwrap().rallies[0].id;
        // Using /a.mp4's rally id under /b.mp4's path must touch nothing.
        assert!(!update_rally(&conn, "/b.mp4", a_id, 0, 1).unwrap());
        assert!(!delete_rally(&conn, "/b.mp4", a_id).unwrap());
        assert_eq!(intervals(&conn, "/a.mp4"), vec![(1000, 5000, 0.3)]);
    }

    #[test]
    fn edits_on_an_unregistered_path_are_no_ops() {
        let (conn, _) = db_with_rallies("/r.mp4", &[(1000, 5000)]);
        assert!(!update_rally(&conn, "/missing.mp4", 1, 0, 1).unwrap());
        assert!(add_rally(&conn, "/missing.mp4", 0, 1).unwrap().is_none());
        assert!(!delete_rally(&conn, "/missing.mp4", 1).unwrap());
    }

    use crate::media::CaptureDate;
    use std::time::UNIX_EPOCH;

    /// The offset-bearing Apple tag wins, and its *local* day is used — here the
    /// recording is at 00:30 local on the 16th but only 16:30 UTC on the 15th, so
    /// grouping by the local day (16th) is the difference that matters.
    #[test]
    fn capture_day_prefers_quicktime_local_day() {
        let embedded = CaptureDate {
            quicktime_creationdate: Some("2024-03-16T00:30:00+0800".to_string()),
            creation_time: Some("2024-03-15T16:30:00.000000Z".to_string()),
        };
        assert_eq!(derive_capture_day(&embedded, UNIX_EPOCH), "2024-03-16");
    }

    /// With no Apple tag, the UTC `creation_time` day is used; with neither tag,
    /// the day falls back to the file's mtime.
    #[test]
    fn capture_day_falls_back_through_sources() {
        let creation_only = CaptureDate {
            quicktime_creationdate: None,
            creation_time: Some("2024-03-15T16:30:00.000000Z".to_string()),
        };
        assert_eq!(derive_capture_day(&creation_only, UNIX_EPOCH), "2024-03-15");
        // mtime fallback: UNIX_EPOCH is 1970-01-01.
        assert_eq!(
            derive_capture_day(&CaptureDate::default(), UNIX_EPOCH),
            "1970-01-01"
        );
    }

    /// A garbage embedded value (the `0000-00-00` a stripped transcode leaves, or
    /// a dead-clock year) is not trusted — it falls through to the next source.
    #[test]
    fn capture_day_skips_implausible_embedded_dates() {
        let junk = CaptureDate {
            quicktime_creationdate: Some("0000-00-00T00:00:00Z".to_string()),
            creation_time: Some("1899-12-31T00:00:00Z".to_string()),
        };
        // Both implausible → mtime fallback (UNIX_EPOCH → 1970-01-01).
        assert_eq!(derive_capture_day(&junk, UNIX_EPOCH), "1970-01-01");
    }

    /// Inserts a recording on `day` under a session for that day, returning its id.
    fn insert_recording(conn: &Connection, path: &str, day: &str) -> i64 {
        conn.execute(
            "INSERT OR IGNORE INTO sessions (capture_day) VALUES (?1)",
            [day],
        )
        .unwrap();
        let session_id: i64 = conn
            .query_row("SELECT id FROM sessions WHERE capture_day = ?1", [day], |r| {
                r.get(0)
            })
            .unwrap();
        conn.execute(
            "INSERT INTO recordings (session_id, path, file_size, quick_hash, capture_day)
             VALUES (?1, ?2, 0, '', ?3)",
            rusqlite::params![session_id, path, day],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn day_of(conn: &Connection, path: &str) -> String {
        conn.query_row(
            "SELECT capture_day FROM recordings WHERE path = ?1",
            [path],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn session_count(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
            .unwrap()
    }

    /// Refining a recording onto a different day re-homes it under a new session
    /// and garbage-collects the old session it emptied.
    #[test]
    fn refine_rehomes_and_gcs_emptied_session() {
        let mut conn = open(Path::new(":memory:")).unwrap();
        let id = insert_recording(&conn, "/r.mp4", "2026-01-01"); // wrong provisional day
        let embedded = CaptureDate {
            quicktime_creationdate: Some("2024-03-15T21:00:00+0800".to_string()),
            creation_time: None,
        };
        refine_capture_day(&mut conn, id, "/r.mp4", &embedded).unwrap();

        assert_eq!(day_of(&conn, "/r.mp4"), "2024-03-15");
        // Old (emptied) session gone, new one present → exactly one session.
        assert_eq!(session_count(&conn), 1);
        assert_eq!(
            conn.query_row("SELECT capture_day FROM sessions", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "2024-03-15"
        );
    }

    /// Re-homing one recording out of a shared session leaves that session intact
    /// for the recordings that remain.
    #[test]
    fn refine_keeps_session_shared_by_others() {
        let mut conn = open(Path::new(":memory:")).unwrap();
        let a = insert_recording(&conn, "/a.mp4", "2026-01-01");
        insert_recording(&conn, "/b.mp4", "2026-01-01"); // shares the day with /a
        let embedded = CaptureDate {
            quicktime_creationdate: Some("2024-03-15T10:00:00+0800".to_string()),
            creation_time: None,
        };
        refine_capture_day(&mut conn, a, "/a.mp4", &embedded).unwrap();

        assert_eq!(day_of(&conn, "/a.mp4"), "2024-03-15");
        assert_eq!(day_of(&conn, "/b.mp4"), "2026-01-01"); // untouched
        assert_eq!(session_count(&conn), 2); // both days survive
    }

    /// Refining a recording already on the correct day is a no-op for grouping and
    /// creates no duplicate session.
    #[test]
    fn refine_is_idempotent_on_correct_day() {
        let mut conn = open(Path::new(":memory:")).unwrap();
        let embedded = CaptureDate {
            quicktime_creationdate: Some("2024-03-15T10:00:00+0800".to_string()),
            creation_time: None,
        };
        let id = insert_recording(&conn, "/r.mp4", "2024-03-15");
        refine_capture_day(&mut conn, id, "/r.mp4", &embedded).unwrap();
        refine_capture_day(&mut conn, id, "/r.mp4", &embedded).unwrap();
        assert_eq!(day_of(&conn, "/r.mp4"), "2024-03-15");
        assert_eq!(session_count(&conn), 1);
    }
}
