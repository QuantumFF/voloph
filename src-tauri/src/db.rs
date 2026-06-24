use std::path::Path;

use rusqlite::Connection;
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
            capture_day TEXT NOT NULL
        );",
    )?;
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
            "SELECT id, path, file_size, quick_hash, capture_day
             FROM recordings WHERE session_id = ?1 ORDER BY path",
        )?;
        let recordings = rstmt
            .query_map([id], |row| {
                Ok(Recording {
                    id: row.get(0)?,
                    path: row.get(1)?,
                    file_size: row.get(2)?,
                    quick_hash: row.get(3)?,
                    capture_day: row.get(4)?,
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
