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
    /// Transcode lifecycle (see ADR 0005): `unknown` (not yet probed), `pending`
    /// (web-incompatible, transcode queued/in progress), `ready` (playable —
    /// either already web-playable or transcoded in place), or `failed`
    /// (probe/transcode error).
    pub transcode_state: String,
    /// Segmentation lifecycle (ADR 0002): `unknown` (not yet segmented or
    /// queued), `ready` (draft timeline produced), or `failed` (audio
    /// extraction / segmentation error). Only starts once `transcode_state` is
    /// `ready`, so it always runs against the final, playable bytes.
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
        CREATE INDEX IF NOT EXISTS idx_rallies_recording ON rallies(recording_id);",
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
    let _ = conn.execute("ALTER TABLE recordings ADD COLUMN duration_ms INTEGER", []);
    // Downsampled audio waveform peaks for the timeline strip (issue #6), stored
    // as a JSON array of normalized `[0,1]` floats. Produced alongside the draft
    // timeline during segmentation; null until a recording is segmented.
    let _ = conn.execute("ALTER TABLE recordings ADD COLUMN waveform TEXT", []);
    Ok(conn)
}

fn is_video(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| VIDEO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// Capture day as `YYYY-MM-DD`, derived from the file's modified time (UTC).
/// ffprobe-derived dates land in a later slice (issue #3).
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

/// One unit of work for the background media worker, in priority order: make a
/// recording playable first (probe, then transcode), then produce its draft
/// timeline (segment). Segmentation is gated on `transcode_state = 'ready'` so
/// it always runs against the final, playable bytes.
#[derive(Debug, PartialEq)]
pub enum MediaWork {
    /// Web-playability not yet determined — run ffprobe (ADR 0005).
    Probe(i64, String),
    /// Web-incompatible — transcode in place (ADR 0005).
    Transcode(i64, String),
    /// Playable but not yet segmented — extract audio and segment (ADR 0002).
    Segment(i64, String),
}

/// The next unit of background media work, lowest id first within each phase, or
/// `None` when every recording is both playable and segmented (or has failed).
pub fn next_media_work(conn: &Connection) -> rusqlite::Result<Option<MediaWork>> {
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
    // Phase 2: anything probed as web-incompatible, awaiting transcode.
    let transcode = conn
        .query_row(
            "SELECT id, path FROM recordings
             WHERE transcode_state = 'pending' ORDER BY id LIMIT 1",
            [],
            |row| Ok(MediaWork::Transcode(row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if transcode.is_some() {
        return Ok(transcode);
    }
    // Phase 3: anything playable but not yet segmented.
    conn.query_row(
        "SELECT id, path FROM recordings
         WHERE transcode_state = 'ready' AND segment_state = 'unknown'
         ORDER BY id LIMIT 1",
        [],
        |row| Ok(MediaWork::Segment(row.get(0)?, row.get(1)?)),
    )
    .optional()
}

/// Advance a recording's transcode lifecycle state.
pub fn set_transcode_state(conn: &Connection, id: i64, state: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE recordings SET transcode_state = ?1 WHERE id = ?2",
        rusqlite::params![state, id],
    )?;
    Ok(())
}

/// Mark a recording `ready` after an in-place transcode, refreshing the stored
/// file size and quick hash to match the new bytes on disk (ADR 0003) so the
/// move-detection fingerprint stays valid.
pub fn mark_transcoded(conn: &Connection, id: i64, path: &str) -> rusqlite::Result<()> {
    let p = Path::new(path);
    let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
    let hash = quick_hash(p, size).unwrap_or_default();
    conn.execute(
        "UPDATE recordings
         SET file_size = ?1, quick_hash = ?2, transcode_state = 'ready'
         WHERE id = ?3",
        rusqlite::params![size as i64, hash, id],
    )?;
    Ok(())
}

/// The transcode state of the recording at `path`, used by the player to decide
/// whether it can load the file yet. `None` when the path is not registered.
pub fn recording_transcode_state(
    conn: &Connection,
    path: &str,
) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT transcode_state FROM recordings WHERE path = ?1",
        [path],
        |row| row.get(0),
    )
    .optional()
}

/// Reset a recording's draft timeline so the media worker re-segments it on its
/// next pass: drop its rallies and return it to `unknown`. Backs the Re-analyze
/// action used while tuning the segmenter (ADR 0002) — it re-runs segmentation
/// without paying for another transcode. A no-op when `path` is not registered.
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
    pub rallies: Vec<crate::segment::Rally>,
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
        "SELECT start_ms, end_ms, confidence FROM rallies
         WHERE recording_id = ?1 ORDER BY start_ms",
    )?;
    let rallies = stmt
        .query_map([id], |row| {
            Ok(crate::segment::Rally {
                start_ms: row.get(0)?,
                end_ms: row.get(1)?,
                confidence: row.get(2)?,
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
