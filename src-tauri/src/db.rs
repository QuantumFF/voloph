use std::path::Path;

use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

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
    /// Playability lifecycle: `unknown` (not yet probed), `ready` (probed —
    /// libmpv can play the original directly, ADR 0008), or `failed` (probe could
    /// not read the file).
    pub probe_state: String,
    /// Segmentation lifecycle (ADR 0002): `unknown` (not yet segmented or
    /// queued), `ready` (draft timeline produced), or `failed` (audio
    /// extraction / segmentation error). Only starts once `probe_state` is
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
    /// Known recordings re-linked to a new relative path after being moved or
    /// renamed inside the library (matched by quick hash + size; ADR 0011). Their
    /// review state follows them, since re-linking rewrites the row's `path` and
    /// everything else keys off the row.
    pub relocated: usize,
    /// Absolute paths of known recordings that could not be found anywhere under
    /// the library after the scan — reported to the user rather than silently kept
    /// as dead entries (ADR 0011). Their rows and review state are retained, so a
    /// recording that reappears later (same hash + size) re-links on a later scan.
    pub unresolved: Vec<String>,
}

/// Open (creating if needed) the metadata database and ensure the schema exists.
pub fn open(db_path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(db_path)?;
    conn.pragma_update(None, "foreign_keys", true)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS sessions (
            id          INTEGER PRIMARY KEY,
            capture_day TEXT NOT NULL,
            -- Which library this session belongs to (ADR 0011): 'local' or
            -- 'shared'. A session never spans libraries, so a capture day is
            -- unique only within its library.
            library     TEXT NOT NULL DEFAULT 'local',
            UNIQUE(library, capture_day)
        );
        CREATE TABLE IF NOT EXISTS recordings (
            id          INTEGER PRIMARY KEY,
            session_id  INTEGER NOT NULL REFERENCES sessions(id),
            path        TEXT NOT NULL,
            -- Which library this recording belongs to (ADR 0011). Its `path` is
            -- relative to that library's folder, so the relative key is unique
            -- only within the library — the same relative path may exist in both.
            library     TEXT NOT NULL DEFAULT 'local',
            file_size   INTEGER NOT NULL,
            quick_hash  TEXT NOT NULL,
            capture_day TEXT NOT NULL,
            probe_state TEXT NOT NULL DEFAULT 'unknown',
            segment_state   TEXT NOT NULL DEFAULT 'unknown',
            date_state      TEXT NOT NULL DEFAULT 'unknown',
            duration_ms     INTEGER,
            waveform        TEXT,
            UNIQUE(library, path)
        );
        CREATE TABLE IF NOT EXISTS rallies (
            id           INTEGER PRIMARY KEY,
            recording_id INTEGER NOT NULL REFERENCES recordings(id) ON DELETE CASCADE,
            start_ms     INTEGER NOT NULL,
            end_ms       INTEGER NOT NULL,
            confidence   REAL NOT NULL,
            flagged      INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_rallies_recording ON rallies(recording_id);
        CREATE TABLE IF NOT EXISTS annotations (
            id           INTEGER PRIMARY KEY,
            recording_id INTEGER NOT NULL REFERENCES recordings(id) ON DELETE CASCADE,
            time_ms      INTEGER NOT NULL,
            verdict      TEXT NOT NULL,
            aspect       TEXT,
            note         TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_annotations_recording ON annotations(recording_id);
        CREATE TABLE IF NOT EXISTS scanned_folders (
            path TEXT NOT NULL UNIQUE
        );
        -- The libraries (ADR 0011): at most one of each `kind` ('local',
        -- 'shared'). `path` is where the library is mounted *on this device* — a
        -- per-device fact that never enters shared metadata. `mount` is the
        -- locality the user declared for that mount: 'local' or 'network', an
        -- explicit choice (never filesystem detection); the local library is
        -- always 'local'. A recording's stored `path` is relative to its
        -- library's folder, with the absolute path computed at use time.
        CREATE TABLE IF NOT EXISTS libraries (
            id    INTEGER PRIMARY KEY,
            path  TEXT NOT NULL,
            kind  TEXT NOT NULL DEFAULT 'local' UNIQUE,
            mount TEXT NOT NULL DEFAULT 'local'
        );
        -- Per-device app state as a key/value store. Holds `active_library` —
        -- the kind ('local'|'shared') the switcher currently has active; the
        -- session list, filters, and review all scope to it (ADR 0011).
        CREATE TABLE IF NOT EXISTS meta (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );",
    )?;
    // Upgrade DBs created before later slices. The CREATE above is a no-op when
    // a table already exists, so add new columns here; ignore the
    // duplicate-column error when one is already present.
    // The probe-state column was historically named `transcode_state` (ADR 0005,
    // since superseded by ADR 0008 — there is no transcode step). Rename it in
    // place where present so the value survives; the RENAME runs before the
    // ADD COLUMN so a DB that already has `transcode_state` keeps its data, while
    // a much older DB with neither column gains a fresh `probe_state`.
    let _ = conn.execute(
        "ALTER TABLE recordings RENAME COLUMN transcode_state TO probe_state",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE recordings ADD COLUMN probe_state TEXT NOT NULL DEFAULT 'unknown'",
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
    // Enrich verdict annotations with a structured aspect and a free-text note
    // (issue #9). Both nullable — a fast-captured annotation carries a verdict
    // only until it is enriched. Added here so DBs from issue #8 gain the columns.
    let _ = conn.execute("ALTER TABLE annotations ADD COLUMN aspect TEXT", []);
    let _ = conn.execute("ALTER TABLE annotations ADD COLUMN note TEXT", []);
    // A rally-level flag meaning "this rally matters" (issue #10) — orthogonal to
    // its annotations, the source material for an export reel. Added here so DBs
    // from earlier slices gain the column. Cleared on re-analyze along with every
    // other manual rally correction (the rally row is replaced; ADR 0002).
    let _ = conn.execute("ALTER TABLE rallies ADD COLUMN flagged INTEGER NOT NULL DEFAULT 0", []);
    // Two typed libraries (issue #62): the shared library adds a `mount` locality
    // to `libraries`, and every session/recording is tagged with the library it
    // belongs to so the same relative path or capture day can exist in both. A DB
    // from #60/#61 held only the local library, so its rows migrate to 'local'.
    let _ = conn.execute("ALTER TABLE libraries ADD COLUMN mount TEXT NOT NULL DEFAULT 'local'", []);
    // A pre-#62 `sessions`/`recordings` still carries the old table-level
    // UNIQUE(capture_day) / UNIQUE(path), which would forbid the shared library
    // from reusing a relative path or day already present under local. Detect the
    // old schema by the absent `library` column and rebuild the two tables with
    // the library-scoped uniques, migrating every row to 'local'. A fresh DB
    // already has the new shape (inline in the CREATE above), so this is skipped.
    let has_library_column = conn
        .prepare("SELECT library FROM recordings LIMIT 0")
        .is_ok();
    if !has_library_column {
        migrate_to_typed_libraries(&conn)?;
    }
    Ok(conn)
}

/// One-time rebuild of `sessions` and `recordings` for the two-library model
/// (issue #62): recreate them with a `library` column and library-scoped uniques,
/// copying every existing row across as 'local'. Runs only on a pre-#62 DB (the
/// old table-level UNIQUE(path)/UNIQUE(capture_day) cannot be dropped in place).
fn migrate_to_typed_libraries(conn: &Connection) -> rusqlite::Result<()> {
    conn.pragma_update(None, "foreign_keys", false)?;
    conn.execute_batch(
        "BEGIN;
         ALTER TABLE sessions RENAME TO sessions_old;
         ALTER TABLE recordings RENAME TO recordings_old;
         CREATE TABLE sessions (
            id          INTEGER PRIMARY KEY,
            capture_day TEXT NOT NULL,
            library     TEXT NOT NULL DEFAULT 'local',
            UNIQUE(library, capture_day)
         );
         CREATE TABLE recordings (
            id          INTEGER PRIMARY KEY,
            session_id  INTEGER NOT NULL REFERENCES sessions(id),
            path        TEXT NOT NULL,
            library     TEXT NOT NULL DEFAULT 'local',
            file_size   INTEGER NOT NULL,
            quick_hash  TEXT NOT NULL,
            capture_day TEXT NOT NULL,
            probe_state TEXT NOT NULL DEFAULT 'unknown',
            segment_state   TEXT NOT NULL DEFAULT 'unknown',
            date_state      TEXT NOT NULL DEFAULT 'unknown',
            duration_ms     INTEGER,
            waveform        TEXT,
            UNIQUE(library, path)
         );
         INSERT INTO sessions (id, capture_day, library)
            SELECT id, capture_day, 'local' FROM sessions_old;
         INSERT INTO recordings
            (id, session_id, path, library, file_size, quick_hash, capture_day,
             probe_state, segment_state, date_state, duration_ms, waveform)
            SELECT id, session_id, path, 'local', file_size, quick_hash, capture_day,
                   probe_state, segment_state, date_state, duration_ms, waveform
            FROM recordings_old;
         DROP TABLE recordings_old;
         DROP TABLE sessions_old;
         COMMIT;",
    )?;
    conn.pragma_update(None, "foreign_keys", true)?;
    Ok(())
}

fn is_video(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| VIDEO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// A designated library and how this device reaches it (ADR 0011).
#[derive(Debug, Clone, Serialize)]
pub struct Library {
    /// 'local' or 'shared'.
    pub kind: String,
    /// Where the library is mounted *on this device* — a per-device fact that
    /// never enters shared metadata.
    pub path: String,
    /// Declared locality of that mount: 'local' or 'network' (ADR 0011). Always
    /// 'local' for the local library.
    pub mount: String,
}

/// The kind of library the switcher currently has active (ADR 0011), defaulting
/// to 'local'. The session list, filters, and review all scope to it.
pub fn active_kind(conn: &Connection) -> rusqlite::Result<String> {
    let stored: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'active_library'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(stored.unwrap_or_else(|| "local".to_string()))
}

/// The folder of the library of `kind` as mounted on this device, or `None` when
/// that kind has not been designated. `path` is per-device (ADR 0011).
fn library_path_of(conn: &Connection, kind: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT path FROM libraries WHERE kind = ?1",
        [kind],
        |row| row.get(0),
    )
    .optional()
}

/// The folder of the **active** library (ADR 0011), or `None` when the active
/// kind has not been designated yet. Every recording in the active library has a
/// stored path relative to this folder; the absolute path is computed at use time
/// by joining it. Path-keyed and listing operations all resolve against this.
pub fn library_path(conn: &Connection) -> rusqlite::Result<Option<String>> {
    library_path_of(conn, &active_kind(conn)?)
}

/// Every designated library with how this device reaches it, plus which kind is
/// active — what the switcher UI needs (ADR 0011).
pub fn library_state(conn: &Connection) -> rusqlite::Result<(Vec<Library>, String)> {
    let mut stmt = conn.prepare("SELECT kind, path, mount FROM libraries ORDER BY kind")?;
    let libraries = stmt
        .query_map([], |row| {
            Ok(Library {
                kind: row.get(0)?,
                path: row.get(1)?,
                mount: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((libraries, active_kind(conn)?))
}

/// Make `kind` the active library (ADR 0011). Idempotent; the caller ensures the
/// kind is designated. The session list, filters, and review all scope to it, and
/// switching back and forth loses nothing — each library's rows are tagged with
/// their kind and simply stop resolving while another is active.
pub fn set_active_kind(conn: &Connection, kind: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES ('active_library', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [kind],
    )?;
    Ok(())
}

/// Read a per-device app-state value from `meta`, or `None` when unset.
pub fn meta_get(conn: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| row.get(0))
        .optional()
}

/// Write a per-device app-state value to `meta` (upsert).
pub fn meta_set(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![key, value],
    )?;
    Ok(())
}

/// The absolute path of a recording stored library-relative under `library`.
fn absolute(library: &str, relative: &str) -> String {
    Path::new(library)
        .join(relative)
        .to_string_lossy()
        .into_owned()
}

/// The path of `absolute` relative to `library`, or `None` when it does not lie
/// under the library folder (a recording outside the library — it does not exist
/// to the app, ADR 0011).
fn relative(library: &str, absolute: &str) -> Option<String> {
    Path::new(absolute)
        .strip_prefix(library)
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

/// Map an absolute path to the active library's kind and the recording's stored
/// (library-relative) key, or `None` when there is no active library or the path
/// lies outside it. Every path-keyed command hands db.rs an absolute path (the
/// frontend and media worker only ever see absolutes); this is the single seam
/// that maps back to the stored key — and, since the relative key is unique only
/// within a library, its kind, so a same-named recording in the other library is
/// never touched.
fn stored_key(conn: &Connection, absolute_path: &str) -> rusqlite::Result<Option<(String, String)>> {
    let kind = active_kind(conn)?;
    Ok(library_path_of(conn, &kind)?
        .and_then(|lib| relative(&lib, absolute_path))
        .map(|rel| (kind, rel)))
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

/// Designate (or re-designate) `folder` as the library of `kind` ('local' or
/// 'shared'), declaring where it is mounted here and its `mount` locality ('local'
/// or 'network' — an explicit user choice, never filesystem detection; ADR 0011).
/// `kind` becomes the active library. Runs an **adoption pass** over that
/// library's own recordings: every already-known recording of this kind found
/// under `folder` — by its stored path lying under the folder — is converted to
/// the library's relative identity with all review state intact (rallies,
/// annotations, flags, and session grouping key off the recording row, so they
/// travel untouched).
///
/// At most one library of each kind (`libraries.kind` is UNIQUE); re-designating
/// replaces the existing one of that kind, re-pointing its whole world without
/// touching the other library's recordings. Idempotent and safe on a fresh
/// install (no recordings → the pass is a no-op). A recording outside the new
/// folder keeps its old key and stops appearing (ADR 0011). Locating *moved*
/// files by quick hash and reporting the unresolved happens in the follow-up scan
/// (issue #61); this pass adopts by path prefix only.
pub fn designate_library(
    conn: &mut Connection,
    kind: &str,
    folder: &Path,
    mount: &str,
) -> rusqlite::Result<()> {
    let folder_str = folder.to_string_lossy().into_owned();
    // The previous folder of *this* kind, to resolve its recordings' current
    // absolute paths (the other kind's recordings are left untouched).
    let previous = library_path_of(conn, kind)?;
    let tx = conn.transaction()?;
    // At most one library per kind; replacing its folder re-points that world.
    tx.execute(
        "INSERT INTO libraries (kind, path, mount) VALUES (?1, ?2, ?3)
         ON CONFLICT(kind) DO UPDATE SET path = excluded.path, mount = excluded.mount",
        rusqlite::params![kind, &folder_str, mount],
    )?;

    // Adoption: rewrite each known recording of this kind so its stored path is
    // relative to the new folder. Its current *absolute* path is its stored key
    // resolved against the previous folder of this kind (or the stored key itself
    // when there was none yet — a pre-library DB stored absolute paths).
    let mut stmt = tx.prepare("SELECT id, path FROM recordings WHERE library = ?1")?;
    let rows: Vec<(i64, String)> = stmt
        .query_map([kind], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    for (id, stored) in rows {
        let absolute_path = match &previous {
            Some(lib) => absolute(lib, &stored),
            None => stored.clone(), // pre-library DB: stored key was absolute
        };
        if let Some(rel) = relative(&folder_str, &absolute_path) {
            if rel != stored {
                tx.execute(
                    "UPDATE recordings SET path = ?1 WHERE id = ?2",
                    rusqlite::params![rel, id],
                )?;
            }
        }
        // Recordings not under the new folder keep their key and simply stop
        // resolving — they no longer appear (ADR 0011).
    }
    // The designated kind becomes active (the switcher points at what was just set
    // up). set_active_kind uses the meta k/v store, safe inside this transaction.
    tx.execute(
        "INSERT INTO meta (key, value) VALUES ('active_library', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [kind],
    )?;
    tx.commit()?;
    Ok(())
}

/// Walk the active library folder, register any video files as recordings
/// (referenced in place) with a **library-relative** identity, and group them
/// into sessions by capture day. Idempotent: files already registered are left
/// untouched, so re-scanning never duplicates. Errors when no library has been
/// designated (ADR 0011 — scanning means scanning the library).
///
/// Completes the adoption pass (ADR 0011). Beyond registering new files, a video
/// on disk that is not registered at its relative path but whose quick hash + size
/// match a **known recording whose own file has gone missing** is treated as that
/// recording moved or renamed inside the library: its row's `path` is rewritten to
/// the new relative key, so its timeline, annotations, and flags follow it (they
/// key off the row, not the path). Known recordings still absent from the library
/// after the walk are returned as `unresolved` — retained, not deleted, so one that
/// reappears later (same hash + size) re-links on a subsequent scan.
pub fn scan_library(conn: &mut Connection) -> rusqlite::Result<ScanResult> {
    let mut registered = 0usize;
    let mut skipped = 0usize;
    let mut relocated = 0usize;

    let kind = active_kind(conn)?;
    let Some(library) = library_path_of(conn, &kind)? else {
        return Ok(ScanResult {
            registered: 0,
            skipped: 0,
            relocated: 0,
            unresolved: Vec::new(),
        });
    };
    let folder = Path::new(&library);

    let tx = conn.transaction()?;

    // Known recordings *of the active library* that resolve under it (relative
    // keys — an absolute key is an unadopted outsider, ADR 0011). Track which are
    // still present on disk; whatever remains missing after the walk is a
    // relocation candidate or, if unmatched, unresolved.
    let mut missing: std::collections::HashMap<String, i64> = {
        let mut stmt = tx.prepare("SELECT id, path FROM recordings WHERE library = ?1")?;
        let rows: Vec<(i64, String)> = stmt
            .query_map([&kind], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        rows.into_iter()
            .filter(|(_, rel)| !Path::new(rel).is_absolute())
            .filter(|(_, rel)| !folder.join(rel).exists())
            .map(|(id, rel)| (rel, id))
            .collect()
    };

    // Cross-library re-home candidates (ADR 0011): recordings of the *other*
    // library whose own file has gone missing from *its* folder. A vanished
    // recording that reappears (hash + size) under this library was moved between
    // libraries — re-home it, carrying its review state and re-homing its session.
    // A recording whose file still exists in its own library is a copy, not a move,
    // and is left for the explicit carry-over offer (`carry_offers`), never
    // re-homed silently. Keyed by id so the walk can match against them too.
    let mut cross_missing: Vec<i64> = {
        let mut ids = Vec::new();
        let mut stmt = tx.prepare("SELECT kind, path FROM libraries")?;
        let others: Vec<(String, String)> = stmt
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        drop(stmt);
        for (other_kind, other_folder) in others {
            if other_kind == kind {
                continue;
            }
            let other_root = Path::new(&other_folder);
            let mut rstmt =
                tx.prepare("SELECT id, path FROM recordings WHERE library = ?1")?;
            let rows: Vec<(i64, String)> = rstmt
                .query_map([&other_kind], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<_>>()?;
            for (id, rel) in rows {
                if !Path::new(&rel).is_absolute() && !other_root.join(&rel).exists() {
                    ids.push(id);
                }
            }
        }
        ids
    };

    for entry in walkdir::WalkDir::new(folder)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !entry.file_type().is_file() || !is_video(path) {
            continue;
        }
        // Identity is the path relative to the library folder (ADR 0011).
        let Some(rel) = path
            .strip_prefix(folder)
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
        else {
            continue;
        };

        // Idempotent dedup: skip files already registered in this library by their
        // relative key (the same key may exist in the other library).
        let already: bool = tx
            .query_row(
                "SELECT 1 FROM recordings WHERE library = ?1 AND path = ?2",
                rusqlite::params![&kind, &rel],
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
        let hash = quick_hash(path, file_size).unwrap_or_default();

        // Relocation: an unregistered file whose quick hash + size match a known
        // recording whose own file is gone is that recording moved/renamed inside
        // the library. Re-link it (rewrite the row's path) so its review state
        // follows; do not register a duplicate.
        let moved = missing.iter().find_map(|(old_rel, &id)| {
            let (o_size, o_hash): (i64, String) = tx
                .query_row(
                    "SELECT file_size, quick_hash FROM recordings WHERE id = ?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap_or((-1, String::new()));
            (o_size == file_size as i64 && o_hash == hash).then(|| (old_rel.clone(), id))
        });
        if let Some((old_rel, id)) = moved {
            tx.execute(
                "UPDATE recordings SET path = ?1 WHERE id = ?2",
                rusqlite::params![rel, id],
            )?;
            missing.remove(&old_rel);
            relocated += 1;
            continue;
        }

        // Cross-library re-home (ADR 0011): an unregistered file matching a
        // recording of the *other* library whose own file is gone is that
        // recording moved between libraries. Re-home the row into this library —
        // rewrite its `path`, `library`, and session — so its whole review state
        // follows it, and garbage-collect the session it emptied in the other
        // library. Only fires for a genuine move (the source file is gone); a copy
        // (both files present) is left for the explicit carry-over offer.
        let rehomed = cross_missing.iter().find(|&&id| {
            let (o_size, o_hash): (i64, String) = tx
                .query_row(
                    "SELECT file_size, quick_hash FROM recordings WHERE id = ?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap_or((-1, String::new()));
            o_size == file_size as i64 && o_hash == hash
        });
        if let Some(&id) = rehomed {
            rehome_recording(&tx, id, &kind, &rel)?;
            cross_missing.retain(|&x| x != id);
            relocated += 1;
            continue;
        }

        let day = capture_day(meta.modified().unwrap_or(std::time::UNIX_EPOCH));

        // Session grouping is per-library (a session never spans libraries).
        tx.execute(
            "INSERT OR IGNORE INTO sessions (capture_day, library) VALUES (?1, ?2)",
            rusqlite::params![&day, &kind],
        )?;
        let session_id: i64 = tx.query_row(
            "SELECT id FROM sessions WHERE capture_day = ?1 AND library = ?2",
            rusqlite::params![&day, &kind],
            |row| row.get(0),
        )?;

        tx.execute(
            "INSERT INTO recordings (session_id, path, library, file_size, quick_hash, capture_day)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![session_id, rel, &kind, file_size as i64, hash, day],
        )?;
        registered += 1;
    }
    tx.commit()?;

    // Whatever known recordings are still missing after the walk could not be
    // re-linked — report them (as absolute paths) rather than delete them.
    let mut unresolved: Vec<String> =
        missing.keys().map(|rel| absolute(&library, rel)).collect();
    unresolved.sort();

    Ok(ScanResult {
        registered,
        skipped,
        relocated,
        unresolved,
    })
}

/// Re-home a recording (identified by row `id`) into the `kind` library at the
/// library-relative key `rel`, carrying its review state (ADR 0011). Rewrites the
/// row's `path`, `library`, and moves it under a session of its own capture day in
/// the target library — creating that session if needed and garbage-collecting the
/// source session it emptied. Rallies and annotations key off the recording row,
/// so they travel untouched. Runs inside the caller's transaction.
fn rehome_recording(
    tx: &rusqlite::Transaction,
    id: i64,
    kind: &str,
    rel: &str,
) -> rusqlite::Result<()> {
    let (old_session, day): (i64, String) = tx.query_row(
        "SELECT session_id, capture_day FROM recordings WHERE id = ?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    tx.execute(
        "INSERT OR IGNORE INTO sessions (capture_day, library) VALUES (?1, ?2)",
        rusqlite::params![&day, kind],
    )?;
    let new_session: i64 = tx.query_row(
        "SELECT id FROM sessions WHERE capture_day = ?1 AND library = ?2",
        rusqlite::params![&day, kind],
        |r| r.get(0),
    )?;
    tx.execute(
        "UPDATE recordings SET path = ?1, library = ?2, session_id = ?3 WHERE id = ?4",
        rusqlite::params![rel, kind, new_session, id],
    )?;
    if old_session != new_session {
        tx.execute(
            "DELETE FROM sessions WHERE id = ?1
             AND NOT EXISTS (SELECT 1 FROM recordings WHERE session_id = ?1)",
            [old_session],
        )?;
    }
    Ok(())
}

/// Whether a recording carries **hand-touched** review state (ADR 0011): a
/// flagged rally, a hand-corrected rally (confidence bumped to
/// `CORRECTED_CONFIDENCE` by an inline edit), or any annotation. Pure
/// machine-produced segmentation (uncertain rallies, no flags, no annotations)
/// does not count — the cross-library carry-over only ever moves work a human did.
fn is_hand_touched(conn: &Connection, id: i64) -> rusqlite::Result<bool> {
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

/// A cross-library carry-over offer (ADR 0011): the same content (quick hash +
/// size) exists as a recording in *both* libraries — a copy, not a move — and
/// exactly one side has hand-touched review state while the other has none. The
/// app offers to carry that review to the other copy; it never migrates silently,
/// and never offers when both sides are hand-touched (no merge — out of scope).
#[derive(Debug, Serialize)]
pub struct CarryOffer {
    /// Absolute path of the copy that *has* the review, resolved on this device.
    pub from_path: String,
    /// Absolute path of the copy that would *receive* it.
    pub to_path: String,
    /// Library kind of the copy that would receive the carried-over review.
    pub to_kind: String,
}

/// Cross-library carry-over offers (ADR 0011): find every pair of recordings —
/// one in each library — that share a quick hash + size (identical content copied
/// across, both files present) where exactly one side is hand-touched. Each is
/// surfaced as an offer to carry the review to the un-touched copy; the caller
/// applies an accepted one with [`carry_review`]. Only pairs whose *both* files
/// currently resolve under their libraries are offered (a vanished side is a
/// re-home, handled on scan, not a copy). Returns nothing when either library is
/// undesignated.
pub fn carry_offers(conn: &Connection) -> rusqlite::Result<Vec<CarryOffer>> {
    let (Some(local), Some(shared)) = (
        library_path_of(conn, "local")?,
        library_path_of(conn, "shared")?,
    ) else {
        return Ok(Vec::new());
    };
    // Same content in both libraries: join local vs shared recordings on hash+size.
    let mut stmt = conn.prepare(
        "SELECT l.id, l.path, s.id, s.path
         FROM recordings l JOIN recordings s
           ON l.quick_hash = s.quick_hash AND l.file_size = s.file_size
         WHERE l.library = 'local' AND s.library = 'shared'",
    )?;
    let pairs: Vec<(i64, String, i64, String)> = stmt
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    let mut offers = Vec::new();
    for (l_id, l_rel, s_id, s_rel) in pairs {
        // Both copies must currently exist on disk — a missing side is a move,
        // re-homed on scan, never a copy offer (AC: offer only when both exist).
        if Path::new(&l_rel).is_absolute() || Path::new(&s_rel).is_absolute() {
            continue;
        }
        if !Path::new(&local).join(&l_rel).exists()
            || !Path::new(&shared).join(&s_rel).exists()
        {
            continue;
        }
        let l_touched = is_hand_touched(conn, l_id)?;
        let s_touched = is_hand_touched(conn, s_id)?;
        // Offer only when exactly one side is hand-touched (no merge; never silent).
        match (l_touched, s_touched) {
            (true, false) => offers.push(CarryOffer {
                from_path: absolute(&local, &l_rel),
                to_path: absolute(&shared, &s_rel),
                to_kind: "shared".to_string(),
            }),
            (false, true) => offers.push(CarryOffer {
                from_path: absolute(&shared, &s_rel),
                to_path: absolute(&local, &l_rel),
                to_kind: "local".to_string(),
            }),
            _ => {}
        }
    }
    Ok(offers)
}

/// Apply an accepted carry-over offer (ADR 0011): copy the review state (draft
/// timeline rallies with their flags, and annotations) from the recording at
/// `from_path` onto the copy at `to_path` in the other library. The receiving
/// copy's own state is replaced (it had none — the offer is only made when one
/// side is un-touched), so this is idempotent. A no-op when either path is not a
/// registered recording. Returns whether anything was carried.
pub fn carry_review(conn: &mut Connection, from_path: &str, to_path: &str) -> rusqlite::Result<bool> {
    // Either side may lie in the non-active library, so resolve both against
    // every library's folder rather than only the active one.
    let (Some(from_id), Some(to_id)) = (
        recording_id_any_library(conn, from_path)?,
        recording_id_any_library(conn, to_path)?,
    ) else {
        return Ok(false);
    };
    let tx = conn.transaction()?;
    // Replace the receiver's (empty) timeline + annotations with the source's.
    tx.execute("DELETE FROM rallies WHERE recording_id = ?1", [to_id])?;
    tx.execute("DELETE FROM annotations WHERE recording_id = ?1", [to_id])?;
    tx.execute(
        "INSERT INTO rallies (recording_id, start_ms, end_ms, confidence, flagged)
         SELECT ?1, start_ms, end_ms, confidence, flagged FROM rallies WHERE recording_id = ?2",
        rusqlite::params![to_id, from_id],
    )?;
    tx.execute(
        "INSERT INTO annotations (recording_id, time_ms, verdict, aspect, note)
         SELECT ?1, time_ms, verdict, aspect, note FROM annotations WHERE recording_id = ?2",
        rusqlite::params![to_id, from_id],
    )?;
    tx.commit()?;
    Ok(true)
}

/// The database id of the recording at absolute `path` in *whichever* library it
/// lies under — the cross-library counterpart of [`recording_id`], which resolves
/// only against the active library. Used by carry-over, whose receiving copy is by
/// definition in the non-active library.
fn recording_id_any_library(conn: &Connection, path: &str) -> rusqlite::Result<Option<i64>> {
    let mut stmt = conn.prepare("SELECT kind, path FROM libraries")?;
    let libs: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    for (kind, folder) in libs {
        if let Some(rel) = relative(&folder, path) {
            let id: Option<i64> = conn
                .query_row(
                    "SELECT id FROM recordings WHERE library = ?1 AND path = ?2",
                    rusqlite::params![&kind, &rel],
                    |r| r.get(0),
                )
                .optional()?;
            if id.is_some() {
                return Ok(id);
            }
        }
    }
    Ok(None)
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

/// All sessions (newest day first) with their recordings nested under them. Each
/// recording's `path` is resolved to its **absolute** location under the active
/// library (ADR 0011); recordings that do not resolve under the library — a video
/// outside it — are omitted, so they never appear in the app. With no library
/// designated nothing resolves and the list is empty.
pub fn list_sessions(conn: &Connection) -> rusqlite::Result<Vec<Session>> {
    let kind = active_kind(conn)?;
    let Some(library) = library_path_of(conn, &kind)? else {
        return Ok(Vec::new());
    };
    // Only the active library's sessions (ADR 0011 — the switcher scopes the list).
    let mut stmt = conn.prepare(
        "SELECT id, capture_day FROM sessions WHERE library = ?1 ORDER BY capture_day DESC",
    )?;
    let sessions: Vec<(i64, String)> = stmt
        .query_map([&kind], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;

    let mut out = Vec::with_capacity(sessions.len());
    for (id, capture_day) in sessions {
        let mut rstmt = conn.prepare(
            "SELECT r.id, r.path, r.file_size, r.quick_hash, r.capture_day,
                    r.probe_state, r.segment_state, r.duration_ms,
                    (SELECT COUNT(*) FROM rallies WHERE recording_id = r.id)
             FROM recordings r WHERE r.session_id = ?1 ORDER BY r.path",
        )?;
        let recordings = rstmt
            .query_map([id], |row| {
                let stored: String = row.get(1)?;
                Ok((
                    stored,
                    Recording {
                        id: row.get(0)?,
                        path: String::new(), // filled in below with the absolute path
                        file_size: row.get(2)?,
                        quick_hash: row.get(3)?,
                        capture_day: row.get(4)?,
                        probe_state: row.get(5)?,
                        segment_state: row.get(6)?,
                        duration_ms: row.get(7)?,
                        rally_count: row.get(8)?,
                    },
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            // A stored path that is itself absolute is an unadopted recording
            // outside the library; drop it (ADR 0011).
            .filter(|(stored, _)| !Path::new(stored).is_absolute())
            .map(|(stored, mut rec)| {
                rec.path = absolute(&library, &stored);
                rec
            })
            .collect::<Vec<_>>();
        if recordings.is_empty() {
            continue;
        }
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
/// draft timeline (segment). Segmentation is gated on `probe_state = 'ready'`
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
    // The worker runs ffmpeg/ffprobe against real files, so it needs absolute
    // paths (ADR 0011 — the absolute path is computed at use time). Scoped to the
    // active library: its recordings resolve against the active mount, and the
    // worker follows the switcher (a fresh worker is spawned on each switch). With
    // no active library there is nothing resolvable to work on.
    let kind = active_kind(conn)?;
    let Some(library) = library_path_of(conn, &kind)? else {
        return Ok(None);
    };
    // Phase 0: anything whose capture day is still the provisional mtime guess.
    // Runs first so a recording settles into the right session as quickly as
    // possible; independent of the probe/segment pipeline below.
    let date = conn
        .query_row(
            "SELECT id, path FROM recordings
             WHERE library = ?1 AND date_state = 'unknown' ORDER BY id LIMIT 1",
            [&kind],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    if let Some((id, rel)) = date {
        return Ok(Some(MediaWork::CaptureDate(id, absolute(&library, &rel))));
    }
    // Phase 1: anything not yet probed.
    let probe = conn
        .query_row(
            "SELECT id, path FROM recordings
             WHERE library = ?1 AND probe_state = 'unknown' ORDER BY id LIMIT 1",
            [&kind],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    if let Some((id, rel)) = probe {
        return Ok(Some(MediaWork::Probe(id, absolute(&library, &rel))));
    }
    // Phase 2: anything probed but not yet segmented.
    let segment = conn
        .query_row(
            "SELECT id, path FROM recordings
             WHERE library = ?1 AND probe_state = 'ready' AND segment_state = 'unknown'
             ORDER BY id LIMIT 1",
            [&kind],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    Ok(segment.map(|(id, rel)| MediaWork::Segment(id, absolute(&library, &rel))))
}

/// The declared locality ('local' or 'network') of the **active** library's mount
/// on this device (ADR 0011), or `None` when no library is designated. Analysis
/// stages recordings only when this is `network`; local mounts analyze in place.
pub fn active_mount(conn: &Connection) -> rusqlite::Result<Option<String>> {
    let kind = active_kind(conn)?;
    conn.query_row(
        "SELECT mount FROM libraries WHERE kind = ?1",
        [&kind],
        |row| row.get(0),
    )
    .optional()
}

/// A recording in the active library still needing analysis, with its absolute
/// path on this device (ADR 0011) and which phases remain. Used by the staged
/// network pipeline, which stages the file once and runs every remaining phase
/// against the copy — so it needs the whole per-recording picture up front,
/// unlike [`next_media_work`]'s one-item-at-a-time queue.
#[derive(Debug, PartialEq)]
pub struct PendingAnalysis {
    pub id: i64,
    pub path: String,
    pub needs_probe: bool,
    pub needs_segment: bool,
}

/// Every recording in the active library still needing a probe or segmentation,
/// lowest id first, with absolute paths resolved against the active mount. Drives
/// the staged network pipeline (copy-ahead needs to see the next recording). The
/// capture-date phase is intentionally excluded — it reads only container headers,
/// so it never benefits from staging and runs unstaged in the ordinary queue.
pub fn pending_analysis(conn: &Connection) -> rusqlite::Result<Vec<PendingAnalysis>> {
    let kind = active_kind(conn)?;
    let Some(library) = library_path_of(conn, &kind)? else {
        return Ok(Vec::new());
    };
    let mut stmt = conn.prepare(
        "SELECT id, path, probe_state, segment_state FROM recordings
         WHERE library = ?1
           AND (probe_state = 'unknown'
                OR (probe_state = 'ready' AND segment_state = 'unknown'))
         ORDER BY id",
    )?;
    let rows = stmt
        .query_map([&kind], |row| {
            let id: i64 = row.get(0)?;
            let rel: String = row.get(1)?;
            let probe_state: String = row.get(2)?;
            let segment_state: String = row.get(3)?;
            Ok(PendingAnalysis {
                id,
                path: absolute(&library, &rel),
                needs_probe: probe_state == "unknown",
                // Segment when it hasn't been done — after the probe, if the probe
                // marks the file playable. `unknown` here means either already
                // probed-ready (probe skipped) or about to be probed this pass.
                needs_segment: segment_state == "unknown",
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Record a probe's outcome (issue #19): the playability state — `ready` once
/// probed, since libmpv plays the original directly (ADR 0008), or `failed`.
pub fn set_probe_result(conn: &Connection, id: i64, state: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE recordings SET probe_state = ?1 WHERE id = ?2",
        rusqlite::params![state, id],
    )?;
    Ok(())
}

/// Reset a recording's draft timeline so the media worker re-segments it on its
/// next pass: drop its rallies and return it to `unknown`. Backs the Re-analyze
/// action used while tuning the segmenter (ADR 0002). A no-op when `path` is not
/// registered.
pub fn reset_segmentation(conn: &Connection, path: &str) -> rusqlite::Result<()> {
    let Some((kind, key)) = stored_key(conn, path)? else {
        return Ok(());
    };
    conn.execute(
        "DELETE FROM rallies WHERE recording_id =
            (SELECT id FROM recordings WHERE library = ?1 AND path = ?2)",
        rusqlite::params![&kind, &key],
    )?;
    conn.execute(
        "UPDATE recordings
         SET segment_state = 'unknown', duration_ms = NULL, waveform = NULL
         WHERE library = ?1 AND path = ?2",
        rusqlite::params![&kind, &key],
    )?;
    Ok(())
}

/// Reset every recording's draft timeline in the **active** library so the media
/// worker re-segments it on its next pass — the bulk Re-analyze-all counterpart of
/// [`reset_segmentation`]. Scoped to the active library like everything the
/// session list drives (ADR 0011). Like a per-recording re-analyze, this discards
/// manual corrections (ADR 0002).
pub fn reset_all_segmentation(conn: &Connection) -> rusqlite::Result<()> {
    let kind = active_kind(conn)?;
    conn.execute(
        "DELETE FROM rallies WHERE recording_id IN
            (SELECT id FROM recordings WHERE library = ?1)",
        [&kind],
    )?;
    conn.execute(
        "UPDATE recordings SET segment_state = 'unknown', duration_ms = NULL, waveform = NULL
         WHERE library = ?1",
        [&kind],
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

/// Confidence stamped on a hand-corrected rally. The user has confirmed it by
/// editing it, so it is fully certain — never an uncertain region (ADR 0002).
const CORRECTED_CONFIDENCE: f64 = 1.0;

/// The database id of the recording at `path`, or `None` when unregistered.
/// The inline-correction commands resolve the recording first, then scope every
/// edit to its rallies, so a stray id from another recording cannot be touched.
fn recording_id(conn: &Connection, path: &str) -> rusqlite::Result<Option<i64>> {
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
        .query_map([rid], |row| {
            Ok(Annotation {
                id: row.get(0)?,
                time_ms: row.get(1)?,
                verdict: row.get(2)?,
                aspect: row.get(3)?,
                note: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(annotations)
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
                |row| {
                    Ok(Annotation {
                        id: row.get(0)?,
                        time_ms: row.get(1)?,
                        verdict: row.get(2)?,
                        aspect: row.get(3)?,
                        note: row.get(4)?,
                    })
                },
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

// --- Session bundles (ADR 0012, issue #65) -------------------------------
//
// A session bundle is a metadata-only snapshot of one session's review, written
// as a file into the shared library so another device/person can pick the review
// up (receive lands in #66). It carries, per recording, only what is portable
// across devices: the library-relative path (ADR 0011), the quick hash + size
// for strict verification on receive, the capture day, duration, waveform, the
// full timeline (rallies with machine confidence — low confidence marks an
// uncertain region), annotations, and flags. Nothing per-device: no mount path,
// no locality. No video bytes.
//
// The file is JSON, tagged with a format id and a version so a future format
// change stays backward-readable — bundles live in users' shared folders (ADR
// 0012). serde_json (not the hand-rolled waveform parser) because a receiver in
// #66 must parse this back robustly.

/// The on-disk bundle format tag and current version. Bumped only on a
/// breaking format change; readers key backward-compat off it.
pub const BUNDLE_FORMAT: &str = "voloph-session-bundle";
pub const BUNDLE_VERSION: u32 = 1;

/// A rally in a bundled timeline. Same shape as [`TimelineRally`] but its own
/// type so the wire format is decoupled from the in-app struct.
#[derive(Debug, Serialize, Deserialize)]
pub struct BundleRally {
    pub start_ms: i64,
    pub end_ms: i64,
    pub confidence: f64,
    pub flagged: bool,
}

/// A verdict annotation in a bundle. Row ids are dropped — the receiver mints
/// its own; only the portable fields travel.
#[derive(Debug, Serialize, Deserialize)]
pub struct BundleAnnotation {
    pub time_ms: i64,
    pub verdict: String,
    pub aspect: Option<String>,
    pub note: Option<String>,
}

/// One recording's review state in a bundle — everything portable, nothing
/// per-device.
#[derive(Debug, Serialize, Deserialize)]
pub struct BundleRecording {
    /// Library-relative path (ADR 0011); resolves against the recipient's own
    /// mount of the same shared library.
    pub path: String,
    pub quick_hash: String,
    pub file_size: i64,
    pub capture_day: String,
    pub duration_ms: Option<i64>,
    pub waveform: Vec<f32>,
    pub rallies: Vec<BundleRally>,
    pub annotations: Vec<BundleAnnotation>,
}

/// A whole session's review state as handed off (ADR 0012). `format`/`version`
/// tag the wire format for backward-readable evolution.
#[derive(Debug, Serialize, Deserialize)]
pub struct SessionBundle {
    pub format: String,
    pub version: u32,
    pub capture_day: String,
    /// Who shared it — names the bundle alongside the session, so one person's
    /// re-share overwrites only their own bundle (ADR 0012).
    pub sharer_label: String,
    pub recordings: Vec<BundleRecording>,
}

/// Build the bundle for a session by its row id, drawing every recording's
/// timeline (rallies + waveform + duration) and annotations straight from the
/// DB. Returns `None` when the session id does not exist. Reads by id, not by
/// absolute path, so it is independent of which library is mounted where.
pub fn build_session_bundle(
    conn: &Connection,
    session_id: i64,
    sharer_label: &str,
) -> rusqlite::Result<Option<SessionBundle>> {
    let capture_day: Option<String> = conn
        .query_row(
            "SELECT capture_day FROM sessions WHERE id = ?1",
            [session_id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(capture_day) = capture_day else {
        return Ok(None);
    };

    // One recording row, collected up front so the per-recording rally/annotation
    // sub-queries below can borrow `conn` again (can't nest prepared statements
    // while the outer query_map still holds it).
    struct Row {
        id: i64,
        path: String,
        file_size: i64,
        quick_hash: String,
        capture_day: String,
        duration_ms: Option<i64>,
        waveform_json: Option<String>,
    }
    let mut rstmt = conn.prepare(
        "SELECT id, path, file_size, quick_hash, capture_day, duration_ms, waveform
         FROM recordings WHERE session_id = ?1 ORDER BY path",
    )?;
    let rows: Vec<Row> = rstmt
        .query_map([session_id], |row| {
            Ok(Row {
                id: row.get(0)?,
                path: row.get(1)?,
                file_size: row.get(2)?,
                quick_hash: row.get(3)?,
                capture_day: row.get(4)?,
                duration_ms: row.get(5)?,
                waveform_json: row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;

    let mut recordings = Vec::with_capacity(rows.len());
    for Row {
        id: rid,
        path,
        file_size,
        quick_hash,
        capture_day: cap_day,
        duration_ms,
        waveform_json,
    } in rows
    {
        let mut ral = conn.prepare(
            "SELECT start_ms, end_ms, confidence, flagged FROM rallies
             WHERE recording_id = ?1 ORDER BY start_ms",
        )?;
        let rallies = ral
            .query_map([rid], |row| {
                Ok(BundleRally {
                    start_ms: row.get(0)?,
                    end_ms: row.get(1)?,
                    confidence: row.get(2)?,
                    flagged: row.get::<_, i64>(3)? != 0,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut ann = conn.prepare(
            "SELECT time_ms, verdict, aspect, note FROM annotations
             WHERE recording_id = ?1 ORDER BY time_ms, id",
        )?;
        let annotations = ann
            .query_map([rid], |row| {
                Ok(BundleAnnotation {
                    time_ms: row.get(0)?,
                    verdict: row.get(1)?,
                    aspect: row.get(2)?,
                    note: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        recordings.push(BundleRecording {
            path,
            quick_hash,
            file_size,
            capture_day: cap_day,
            duration_ms,
            waveform: parse_waveform(waveform_json.as_deref()),
            rallies,
            annotations,
        });
    }

    Ok(Some(SessionBundle {
        format: BUNDLE_FORMAT.to_string(),
        version: BUNDLE_VERSION,
        capture_day,
        sharer_label: sharer_label.to_string(),
        recordings,
    }))
}

/// The file name for a bundle in the shared library: session day + sharer label,
/// so it is identifiable by both and one person's re-share overwrites only their
/// own (ADR 0012). The label is sanitized to a safe file stem.
pub fn bundle_file_name(capture_day: &str, sharer_label: &str) -> String {
    let safe: String = sharer_label
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    let safe = safe.trim_matches('-');
    let safe = if safe.is_empty() { "unnamed" } else { safe };
    format!("{capture_day}__{safe}.vbundle")
}

// --- Receiving session bundles (ADR 0012, issue #66) ---------------------
//
// Receive applies a bundle's review state against this device's shared library.
// It is self-sufficient (ADR 0012): a recording the recipient never scanned is
// registered straight from the bundle after the file at its relative path is
// verified by quick hash + size — no probe, no segmentation. A mismatch refuses
// that one recording (naming the file) while the rest of the bundle still
// applies. Where the recipient already has state: machine-only state is replaced
// silently; hand-touched state is left alone and surfaced as a keep-mine-or-take-
// theirs choice (nothing merges). Receiving the same bundle twice is a no-op.

/// One recording a receive could not apply, named so the user knows which file.
#[derive(Debug, Serialize)]
pub struct BundleRefusal {
    /// Absolute path (on this device) of the file that failed verification.
    pub path: String,
    pub reason: String,
}

/// The outcome of receiving a bundle. `applied` counts recordings taken silently
/// (registered fresh or replacing machine-only state); `refused` names files that
/// failed verification; `conflicts` are the library-relative paths of recordings
/// the recipient has hand-touched — each awaits a keep-mine-or-take-theirs choice
/// via [`resolve_bundle_conflict`], nothing having been changed for them.
#[derive(Debug, Serialize)]
pub struct ReceiveResult {
    pub applied: usize,
    pub refused: Vec<BundleRefusal>,
    /// Library-relative paths of hand-touched recordings awaiting the user's
    /// per-recording choice.
    pub conflicts: Vec<String>,
}

/// The waveform peaks serialized exactly as [`save_rallies`] writes them — a
/// compact JSON array of two-decimal floats.
fn waveform_to_json(waveform: &[f32]) -> String {
    format!(
        "[{}]",
        waveform
            .iter()
            .map(|p| format!("{p:.2}"))
            .collect::<Vec<_>>()
            .join(",")
    )
}

/// Verify that the file at `abs` is the recording the bundle describes: it exists,
/// its size matches, and its quick hash matches. There is no innocent mismatch
/// (ADR 0012) — a mismatch means the file at that path is not this recording.
fn verify_bundle_file(abs: &Path, expect_hash: &str, file_size: i64) -> bool {
    let Ok(meta) = std::fs::metadata(abs) else {
        return false;
    };
    if meta.len() as i64 != file_size {
        return false;
    }
    quick_hash(abs, meta.len()).map(|h| h == expect_hash).unwrap_or(false)
}

/// Write a bundle recording's carried state onto recording row `id`, replacing any
/// prior timeline and annotations (whole-recording, never merged; ADR 0012) and
/// marking it segmented so it plays immediately. Runs inside the caller's
/// transaction.
fn apply_bundle_state(
    tx: &rusqlite::Transaction,
    id: i64,
    rec: &BundleRecording,
) -> rusqlite::Result<()> {
    tx.execute("DELETE FROM rallies WHERE recording_id = ?1", [id])?;
    tx.execute("DELETE FROM annotations WHERE recording_id = ?1", [id])?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO rallies (recording_id, start_ms, end_ms, confidence, flagged)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        for r in &rec.rallies {
            stmt.execute(rusqlite::params![
                id,
                r.start_ms,
                r.end_ms,
                r.confidence,
                r.flagged as i64
            ])?;
        }
    }
    {
        let mut stmt = tx.prepare(
            "INSERT INTO annotations (recording_id, time_ms, verdict, aspect, note)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        for a in &rec.annotations {
            stmt.execute(rusqlite::params![
                id,
                a.time_ms,
                a.verdict,
                a.aspect,
                a.note
            ])?;
        }
    }
    // Registered-from-bundle recordings are playable straight away (ADR 0012): the
    // file is verified and the carried timeline is its draft. Probe is 'ready'
    // (libmpv plays the original directly, ADR 0008) and segmentation 'ready' (the
    // draft came in the bundle — no local segmentation of network video).
    tx.execute(
        "UPDATE recordings
         SET probe_state = 'ready', segment_state = 'ready', duration_ms = ?1, waveform = ?2
         WHERE id = ?3",
        rusqlite::params![rec.duration_ms, waveform_to_json(&rec.waveform), id],
    )?;
    Ok(())
}

/// Whether recording `id`'s current timeline and annotations already equal the
/// bundle's — the test for the idempotent no-op (ADR 0012). Compares the ordered
/// rally intervals (with confidence + flag) and ordered annotations; row ids are
/// ignored (the recipient minted its own). Confidence compares with a small
/// epsilon so a float round-trip through JSON does not read as a difference.
fn state_matches_bundle(
    conn: &Connection,
    id: i64,
    rec: &BundleRecording,
) -> rusqlite::Result<bool> {
    let mut rstmt = conn.prepare(
        "SELECT start_ms, end_ms, confidence, flagged FROM rallies
         WHERE recording_id = ?1 ORDER BY start_ms",
    )?;
    let rallies: Vec<(i64, i64, f64, bool)> = rstmt
        .query_map([id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get::<_, i64>(3)? != 0))
        })?
        .collect::<rusqlite::Result<_>>()?;
    if rallies.len() != rec.rallies.len() {
        return Ok(false);
    }
    for (cur, b) in rallies.iter().zip(&rec.rallies) {
        if cur.0 != b.start_ms
            || cur.1 != b.end_ms
            || (cur.2 - b.confidence).abs() > 1e-6
            || cur.3 != b.flagged
        {
            return Ok(false);
        }
    }

    let mut astmt = conn.prepare(
        "SELECT time_ms, verdict, aspect, note FROM annotations
         WHERE recording_id = ?1 ORDER BY time_ms, id",
    )?;
    let anns: Vec<(i64, String, Option<String>, Option<String>)> = astmt
        .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;
    if anns.len() != rec.annotations.len() {
        return Ok(false);
    }
    for (cur, b) in anns.iter().zip(&rec.annotations) {
        if cur.0 != b.time_ms
            || cur.1 != b.verdict
            || cur.2 != b.aspect
            || cur.3 != b.note
        {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Receive a session bundle into this device's **shared** library (ADR 0012).
/// `bundle_json` is the `.vbundle` file's contents. Registers unknown recordings
/// after verifying their file, replaces machine-only state silently, and reports
/// hand-touched recordings as conflicts for a keep-mine-or-take-theirs choice —
/// leaving those untouched. Idempotent: a second receive of the same bundle finds
/// every recording already matching and reports nothing to resolve.
pub fn receive_session_bundle(
    conn: &mut Connection,
    bundle_json: &str,
) -> Result<ReceiveResult, String> {
    let bundle: SessionBundle =
        serde_json::from_str(bundle_json).map_err(|e| format!("not a valid bundle: {e}"))?;
    if bundle.format != BUNDLE_FORMAT {
        return Err("this file is not a Voloph session bundle".into());
    }
    if bundle.version > BUNDLE_VERSION {
        return Err(format!(
            "this bundle is from a newer version of Voloph (format v{})",
            bundle.version
        ));
    }
    let library = library_path_of(conn, "shared")
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "designate the shared library before receiving a bundle".to_string())?;

    let mut applied = 0usize;
    let mut refused = Vec::new();
    let mut conflicts = Vec::new();

    for rec in &bundle.recordings {
        let abs = absolute(&library, &rec.path);
        let existing: Option<i64> = conn
            .query_row(
                "SELECT id FROM recordings WHERE library = 'shared' AND path = ?1",
                [&rec.path],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;

        // Already exactly this state (a second receive of the same bundle, or the
        // recipient's own copy of a review they shared) → nothing to do, no
        // conflict (ADR 0012: receiving twice is a no-op). Checked before the
        // hand-touched branch because the bundle's own flags/annotations make the
        // recording read as hand-touched once applied.
        if let Some(id) = existing {
            if state_matches_bundle(conn, id, rec).map_err(|e| e.to_string())? {
                continue;
            }
            // Hand-touched local state differing from the bundle is never
            // overwritten silently — surface it as a choice and move on, having
            // changed nothing (ADR 0012).
            if is_hand_touched(conn, id).map_err(|e| e.to_string())? {
                conflicts.push(rec.path.clone());
                continue;
            }
        }

        // Every applied recording is verified against the file on disk — for a
        // fresh registration this is self-sufficiency; for a machine-only replace
        // it is the same strict check (there is no innocent mismatch, ADR 0012).
        if !verify_bundle_file(Path::new(&abs), &rec.quick_hash, rec.file_size) {
            refused.push(BundleRefusal {
                path: abs,
                reason: "file does not match the bundle (missing, or different content)".into(),
            });
            continue;
        }

        let tx = conn.transaction().map_err(|e| e.to_string())?;
        let id = match existing {
            Some(id) => id,
            None => {
                tx.execute(
                    "INSERT OR IGNORE INTO sessions (capture_day, library) VALUES (?1, 'shared')",
                    [&rec.capture_day],
                )
                .map_err(|e| e.to_string())?;
                let session_id: i64 = tx
                    .query_row(
                        "SELECT id FROM sessions WHERE capture_day = ?1 AND library = 'shared'",
                        [&rec.capture_day],
                        |r| r.get(0),
                    )
                    .map_err(|e| e.to_string())?;
                tx.execute(
                    "INSERT INTO recordings
                        (session_id, path, library, file_size, quick_hash, capture_day)
                     VALUES (?1, ?2, 'shared', ?3, ?4, ?5)",
                    rusqlite::params![
                        session_id,
                        &rec.path,
                        rec.file_size,
                        &rec.quick_hash,
                        &rec.capture_day
                    ],
                )
                .map_err(|e| e.to_string())?;
                tx.last_insert_rowid()
            }
        };
        apply_bundle_state(&tx, id, rec).map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        applied += 1;
    }

    Ok(ReceiveResult {
        applied,
        refused,
        conflicts,
    })
}

/// Resolve one keep-mine-or-take-theirs conflict from a received bundle (ADR
/// 0012). `path` is the recording's library-relative path (as reported in
/// [`ReceiveResult::conflicts`]). `take_theirs` replaces the recipient's whole
/// timeline and annotations with the bundle's after re-verifying the file; keep-
/// mine (`false`) is a no-op. Re-reads the bundle so no server-side state is held
/// between the offer and the choice. Returns whether the recording was replaced.
pub fn resolve_bundle_conflict(
    conn: &mut Connection,
    bundle_json: &str,
    path: &str,
    take_theirs: bool,
) -> Result<bool, String> {
    if !take_theirs {
        return Ok(false);
    }
    let bundle: SessionBundle =
        serde_json::from_str(bundle_json).map_err(|e| format!("not a valid bundle: {e}"))?;
    let rec = bundle
        .recordings
        .iter()
        .find(|r| r.path == path)
        .ok_or_else(|| "recording not in bundle".to_string())?;
    let library = library_path_of(conn, "shared")
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "the shared library is not designated".to_string())?;
    let abs = absolute(&library, &rec.path);
    if !verify_bundle_file(Path::new(&abs), &rec.quick_hash, rec.file_size) {
        return Err(format!(
            "file does not match the bundle: {abs}"
        ));
    }
    let id: i64 = conn
        .query_row(
            "SELECT id FROM recordings WHERE library = 'shared' AND path = ?1",
            [&rec.path],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    apply_bundle_state(&tx, id, rec).map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An in-memory DB with the schema and one segmented recording carrying the
    /// given rally intervals — the starting point for an inline-correction test.
    fn db_with_rallies(path: &str, intervals: &[(i64, i64)]) -> (Connection, i64) {
        let mut conn = open(Path::new(":memory:")).unwrap();
        // An empty-string library makes stored path == absolute path (join/strip
        // are identity), so these tests can keep using absolute paths as both the
        // stored key and the lookup key while going through the resolution seam.
        set_test_library(&conn);
        conn.execute(
            "INSERT INTO sessions (capture_day) VALUES ('2026-01-01')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO recordings (session_id, path, file_size, quick_hash, capture_day, probe_state, segment_state, duration_ms)
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
            "INSERT INTO recordings (session_id, path, file_size, quick_hash, capture_day, probe_state, segment_state, duration_ms)
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

    fn flags(conn: &Connection, path: &str) -> Vec<bool> {
        recording_timeline(conn, path)
            .unwrap()
            .unwrap()
            .rallies
            .into_iter()
            .map(|r| r.flagged)
            .collect()
    }

    /// A rally starts unflagged; flagging toggles it on and off and persists,
    /// independent of any annotations (issue #10).
    #[test]
    fn flag_toggles_and_persists() {
        let (conn, _) = db_with_rallies("/r.mp4", &[(1000, 5000), (8000, 12000)]);
        let id = recording_timeline(&conn, "/r.mp4").unwrap().unwrap().rallies[0].id;
        assert_eq!(flags(&conn, "/r.mp4"), vec![false, false]);
        assert!(set_rally_flag(&conn, "/r.mp4", id, true).unwrap());
        assert_eq!(flags(&conn, "/r.mp4"), vec![true, false]);
        assert!(set_rally_flag(&conn, "/r.mp4", id, false).unwrap());
        assert_eq!(flags(&conn, "/r.mp4"), vec![false, false]);
    }

    /// Flagging is scoped to the recording, and an unknown rally/path touches
    /// nothing (issue #10).
    #[test]
    fn flag_is_scoped_to_its_recording() {
        let (conn, _) = db_with_rallies("/a.mp4", &[(1000, 5000)]);
        conn.execute(
            "INSERT INTO recordings (session_id, path, file_size, quick_hash, capture_day, probe_state, segment_state, duration_ms)
             VALUES (1, '/b.mp4', 0, '', '2026-01-01', 'ready', 'ready', 100000)",
            [],
        )
        .unwrap();
        let a_id = recording_timeline(&conn, "/a.mp4").unwrap().unwrap().rallies[0].id;
        assert!(!set_rally_flag(&conn, "/b.mp4", a_id, true).unwrap());
        assert!(!set_rally_flag(&conn, "/missing.mp4", a_id, true).unwrap());
        assert!(!set_rally_flag(&conn, "/a.mp4", 9999, true).unwrap());
        assert_eq!(flags(&conn, "/a.mp4"), vec![false]);
    }

    /// Cross-session filtering (issue #11): filters combine with AND, verdict /
    /// aspect select rallies containing a matching moment (and attach it), length
    /// is derived from duration, flag filters the rally. Two sessions here so
    /// "across all sessions" is actually exercised.
    #[test]
    fn filter_moments_combines_across_sessions() {
        let (mut conn, _) = db_with_rallies("/a.mp4", &[(0, 20_000), (30_000, 33_000)]);
        // A second session/recording so the query spans sessions.
        conn.execute(
            "INSERT INTO sessions (capture_day) VALUES ('2026-02-02')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO recordings (session_id, path, file_size, quick_hash, capture_day, probe_state, segment_state, duration_ms)
             VALUES (2, '/b.mp4', 0, '', '2026-02-02', 'ready', 'ready', 100000)",
            [],
        )
        .unwrap();
        save_rallies(
            &mut conn,
            2,
            100_000,
            &[crate::segment::Rally { start_ms: 0, end_ms: 5_000, confidence: 1.0 }],
            &[],
        )
        .unwrap();

        // Moment on the long rally in /a and the short rally in /b.
        let a_long = recording_timeline(&conn, "/a.mp4").unwrap().unwrap().rallies[0].id;
        let a_short = recording_timeline(&conn, "/a.mp4").unwrap().unwrap().rallies[1].id;
        set_rally_flag(&conn, "/a.mp4", a_long, true).unwrap();
        let m1 = add_annotation(&conn, "/a.mp4", 1_000, "mistake").unwrap().unwrap();
        update_annotation(&conn, "/a.mp4", m1, "mistake", Some("execution"), None).unwrap();
        add_annotation(&conn, "/a.mp4", 31_000, "good").unwrap(); // in the short rally
        add_annotation(&conn, "/b.mp4", 500, "mistake").unwrap(); // execution left null

        // No filter → every rally, newest session first, no attached annotations.
        let all = filter_moments(&conn, None, None, None, None).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].capture_day, "2026-02-02"); // newest first
        assert!(all.iter().all(|r| r.annotations.is_empty()));

        // Length filter is derived from duration: one long rally (the 20s one).
        let long = filter_moments(&conn, None, None, Some(true), None).unwrap();
        assert_eq!(long.len(), 1);
        assert_eq!(long[0].rally_id, a_long);
        assert!(long[0].long && long[0].flagged);

        // Flag filter: only the flagged rally.
        let flagged = filter_moments(&conn, None, None, None, Some(true)).unwrap();
        assert_eq!(flagged.len(), 1);
        assert_eq!(flagged[0].rally_id, a_long);

        // verdict=mistake → two rallies (a_long, /b), each with its mistake attached.
        let mistakes = filter_moments(&conn, Some("mistake"), None, None, None).unwrap();
        assert_eq!(mistakes.len(), 2);
        assert!(mistakes.iter().all(|r| r.annotations.len() == 1
            && r.annotations[0].verdict == "mistake"));
        assert_ne!(mistakes[0].rally_id, a_short);

        // aspect=execution AND verdict=mistake → only /a's long rally.
        let combo =
            filter_moments(&conn, Some("mistake"), Some("execution"), None, None).unwrap();
        assert_eq!(combo.len(), 1);
        assert_eq!(combo[0].rally_id, a_long);
        assert_eq!(combo[0].annotations[0].aspect.as_deref(), Some("execution"));

        // Combined moment + rally filters: mistake in a *long* rally → still /a only.
        let combo2 =
            filter_moments(&conn, Some("mistake"), None, Some(true), None).unwrap();
        assert_eq!(combo2.len(), 1);
        assert_eq!(combo2[0].rally_id, a_long);
    }

    /// The stored (library-relative) path key of the recording at row `id`.
    fn stored_path(conn: &Connection, id: i64) -> String {
        conn.query_row("SELECT path FROM recordings WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .unwrap()
    }

    /// Designating a library over a pre-library DB (recordings stored by absolute
    /// path) adopts every recording under the folder to library-relative identity,
    /// preserving its review state, while `list_sessions` resolves the relative key
    /// back to the absolute path; a recording outside the folder is dropped from
    /// the app (ADR 0011). Fresh-install safety: designating with no recordings is
    /// a no-op, and re-designating is idempotent.
    #[test]
    fn designate_adopts_by_prefix_and_drops_outsiders() {
        let mut conn = open(Path::new(":memory:")).unwrap();
        // Fresh install: designating before any recordings exist is safe.
        designate_library(&mut conn, "local", Path::new("/lib"), "local").unwrap();
        assert!(list_sessions(&conn).unwrap().is_empty());

        // Simulate a pre-library DB: recordings stored by absolute path, one under
        // the library-to-be and one outside it. Insert directly (no library seam).
        conn.execute("DELETE FROM libraries", []).unwrap();
        let inside = insert_recording(&conn, "/lib/day1/game.mp4", "2026-01-01");
        conn.execute("DELETE FROM libraries", []).unwrap(); // insert_recording re-adds one
        let outside = insert_recording(&conn, "/elsewhere/other.mp4", "2026-01-02");
        conn.execute("DELETE FROM libraries", []).unwrap();
        // Review state on the inside recording must survive adoption.
        conn.execute(
            "INSERT INTO rallies (recording_id, start_ms, end_ms, confidence, flagged)
             VALUES (?1, 0, 5000, 1.0, 1)",
            [inside],
        )
        .unwrap();

        designate_library(&mut conn, "local", Path::new("/lib"), "local").unwrap();

        // Inside recording adopted to a relative key; outside kept its absolute one.
        assert_eq!(stored_path(&conn, inside), "day1/game.mp4");
        assert_eq!(stored_path(&conn, outside), "/elsewhere/other.mp4");

        // list_sessions resolves the relative key back to the absolute path and
        // drops the outsider entirely.
        let sessions = list_sessions(&conn).unwrap();
        assert_eq!(sessions.len(), 1);
        let recs = &sessions[0].recordings;
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].path, "/lib/day1/game.mp4");
        assert_eq!(recs[0].rally_count, 1); // review state intact

        // The adopted recording is reachable by its absolute path through the seam.
        assert!(recording_timeline(&conn, "/lib/day1/game.mp4")
            .unwrap()
            .is_some());
        assert!(set_rally_flag(&conn, "/lib/day1/game.mp4", 1, false).unwrap());

        // Re-designating the same folder is idempotent.
        designate_library(&mut conn, "local", Path::new("/lib"), "local").unwrap();
        assert_eq!(stored_path(&conn, inside), "day1/game.mp4");
    }

    /// Re-designating from one library folder to another re-adopts recordings by
    /// their absolute location (computed against the previous library), so a
    /// recording that lies under both keeps resolving.
    #[test]
    fn redesignate_readopts_from_previous_library() {
        let mut conn = open(Path::new(":memory:")).unwrap();
        designate_library(&mut conn, "local", Path::new("/lib/a"), "local").unwrap();
        // Register a recording relative to /lib/a directly.
        conn.execute(
            "INSERT INTO sessions (capture_day) VALUES ('2026-01-01')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO recordings (session_id, path, file_size, quick_hash, capture_day)
             VALUES ((SELECT id FROM sessions LIMIT 1), 'sub/game.mp4', 0, '', '2026-01-01')",
            [],
        )
        .unwrap();
        // Its absolute path is /lib/a/sub/game.mp4. Re-designate the parent /lib:
        // it should re-adopt to a/sub/game.mp4 and still resolve.
        designate_library(&mut conn, "local", Path::new("/lib"), "local").unwrap();
        let recs = &list_sessions(&conn).unwrap()[0].recordings;
        assert_eq!(recs[0].path, "/lib/a/sub/game.mp4");
    }

    /// A pre-#62 DB (sessions/recordings with the old table-level UNIQUE and no
    /// `library` column) upgrades in place: `open` rebuilds both tables, tags every
    /// existing row 'local', and preserves the recording's row (id + review state).
    #[test]
    fn open_migrates_pre_library_column_db() {
        let path = std::env::temp_dir().join(format!(
            "voloph-migrate-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        // Build the old schema (pre-#62) by hand and seed a recording + rally.
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(
                "CREATE TABLE sessions (
                    id INTEGER PRIMARY KEY, capture_day TEXT NOT NULL UNIQUE);
                 CREATE TABLE recordings (
                    id INTEGER PRIMARY KEY, session_id INTEGER NOT NULL,
                    path TEXT NOT NULL UNIQUE, file_size INTEGER NOT NULL,
                    quick_hash TEXT NOT NULL, capture_day TEXT NOT NULL,
                    probe_state TEXT NOT NULL DEFAULT 'unknown',
                    segment_state TEXT NOT NULL DEFAULT 'unknown',
                    date_state TEXT NOT NULL DEFAULT 'unknown',
                    duration_ms INTEGER, waveform TEXT);
                 CREATE TABLE rallies (
                    id INTEGER PRIMARY KEY, recording_id INTEGER NOT NULL,
                    start_ms INTEGER NOT NULL, end_ms INTEGER NOT NULL,
                    confidence REAL NOT NULL, flagged INTEGER NOT NULL DEFAULT 0);
                 INSERT INTO sessions (id, capture_day) VALUES (1, '2026-01-01');
                 INSERT INTO recordings (id, session_id, path, file_size, quick_hash, capture_day)
                    VALUES (7, 1, 'day1/game.mp4', 0, '', '2026-01-01');
                 INSERT INTO rallies (recording_id, start_ms, end_ms, confidence, flagged)
                    VALUES (7, 0, 5000, 1.0, 1);",
            )
            .unwrap();
        }
        // Upgrade path: open() rebuilds the tables with a `library` column.
        let conn = open(&path).unwrap();
        let (id, lib): (i64, String) = conn
            .query_row(
                "SELECT id, library FROM recordings WHERE path = 'day1/game.mp4'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(id, 7); // same row (review state kept)
        assert_eq!(lib, "local"); // migrated to the local library
        let session_lib: String = conn
            .query_row("SELECT library FROM sessions WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(session_lib, "local");
        // The rally survived the rebuild.
        let flagged: i64 = conn
            .query_row("SELECT flagged FROM rallies WHERE recording_id = 7", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(flagged, 1);
        drop(conn);
        let _ = std::fs::remove_file(&path);
    }

    /// Two typed libraries coexist (issue #62): a shared library is designated with
    /// its per-device mount and declared locality, the switcher scopes the session
    /// list and path-keyed edits to the active library, and switching back and
    /// forth loses nothing. A recording relative path that exists in both libraries
    /// resolves to the right file per active library.
    #[test]
    fn two_libraries_switch_and_scope() {
        let local = TempLib::new();
        let shared = TempLib::new();
        // The same relative path in both libraries, distinct contents.
        local.write("day1/game.mp4", b"local-bytes");
        shared.write("day1/game.mp4", b"shared-bytes");
        let mut conn = open(Path::new(":memory:")).unwrap();

        // Designate local (mount always local) and scan it.
        designate_library(&mut conn, "local", &local.0, "local").unwrap();
        assert_eq!(scan_library(&mut conn).unwrap().registered, 1);
        assert_eq!(active_kind(&conn).unwrap(), "local");

        // Designate the shared library on its own mount, declared network. Adoption
        // runs on it too (empty here — the follow-up scan registers the file). It
        // becomes active, so the list now shows the shared library only.
        designate_library(&mut conn, "shared", &shared.0, "network").unwrap();
        assert_eq!(active_kind(&conn).unwrap(), "shared");
        assert_eq!(scan_library(&mut conn).unwrap().registered, 1);
        let shared_abs = absolute(&shared.0.to_string_lossy(), "day1/game.mp4");
        assert_eq!(list_sessions(&conn).unwrap()[0].recordings[0].path, shared_abs);

        // The switcher state carries both libraries with their per-device mount and
        // declared locality; local is always 'local', shared is 'network' here.
        let (libs, active) = library_state(&conn).unwrap();
        assert_eq!(active, "shared");
        assert_eq!(libs.len(), 2);
        let mount_of = |k: &str| libs.iter().find(|l| l.kind == k).unwrap().mount.clone();
        assert_eq!(mount_of("local"), "local");
        assert_eq!(mount_of("shared"), "network");

        // A flag set on the shared recording is scoped to the shared library.
        let shared_rally = add_rally(&conn, &shared_abs, 0, 5000).unwrap().unwrap();
        assert!(set_rally_flag(&conn, &shared_abs, shared_rally, true).unwrap());

        // Switch back to local: its own recording (same relative path, different
        // file) shows, and the shared flag is not visible here.
        set_active_kind(&conn, "local").unwrap();
        let local_abs = absolute(&local.0.to_string_lossy(), "day1/game.mp4");
        assert_eq!(list_sessions(&conn).unwrap()[0].recordings[0].path, local_abs);
        // The local recording has no rallies (the shared one's are not its own).
        assert!(recording_timeline(&conn, &local_abs)
            .unwrap()
            .unwrap()
            .rallies
            .is_empty());

        // Switch back to shared: nothing lost — the flagged rally is still there.
        set_active_kind(&conn, "shared").unwrap();
        assert_eq!(
            recording_timeline(&conn, &shared_abs).unwrap().unwrap().rallies[0].flagged,
            true
        );
    }

    /// The staged network pipeline (issue #64) reads the active library's declared
    /// locality and the list of recordings still needing analysis. `active_mount`
    /// reflects the active library ('network' vs 'local'); `pending_analysis`
    /// lists exactly the recordings needing a probe or segment, with the phases
    /// each still needs, and empties as they complete.
    #[test]
    fn active_mount_and_pending_analysis_drive_staging() {
        let local = TempLib::new();
        let shared = TempLib::new();
        local.write("a.mp4", b"local-a");
        shared.write("s1.mp4", b"shared-1");
        shared.write("s2.mp4", b"shared-2");
        let mut conn = open(Path::new(":memory:")).unwrap();

        // Local library: mount is always local, so nothing stages.
        designate_library(&mut conn, "local", &local.0, "local").unwrap();
        scan_library(&mut conn).unwrap();
        assert_eq!(active_mount(&conn).unwrap().as_deref(), Some("local"));

        // Shared library declared network: active_mount reports network, and both
        // freshly-scanned recordings are pending both phases.
        designate_library(&mut conn, "shared", &shared.0, "network").unwrap();
        scan_library(&mut conn).unwrap();
        assert_eq!(active_mount(&conn).unwrap().as_deref(), Some("network"));
        let pending = pending_analysis(&conn).unwrap();
        assert_eq!(pending.len(), 2);
        assert!(pending.iter().all(|p| p.needs_probe && p.needs_segment));
        // Paths are absolute under the shared mount.
        assert!(pending
            .iter()
            .all(|p| p.path.starts_with(&*shared.0.to_string_lossy())));

        // Probe one recording ready: it now needs only segmentation, not a probe.
        set_probe_result(&conn, pending[0].id, "ready").unwrap();
        let after_probe = pending_analysis(&conn).unwrap();
        let first = after_probe.iter().find(|p| p.id == pending[0].id).unwrap();
        assert!(!first.needs_probe && first.needs_segment);

        // Segment it: it drops out of the pending list entirely.
        save_rallies(&mut conn, pending[0].id, 1000, &[], &[]).unwrap();
        let remaining = pending_analysis(&conn).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, pending[1].id);
    }

    /// A throwaway directory under the OS temp dir, removed on drop — enough for the
    /// scan tests, which need real files on disk (no tempfile dep, ponytail).
    struct TempLib(std::path::PathBuf);
    impl TempLib {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "voloph-test-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            TempLib(dir)
        }
        /// Write `contents` to `rel` under the library (creating parents), returning
        /// the absolute path. Distinct contents → distinct quick hash.
        fn write(&self, rel: &str, contents: &[u8]) -> std::path::PathBuf {
            let path = self.0.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, contents).unwrap();
            path
        }
    }
    impl Drop for TempLib {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The adoption pass re-links a recording moved/renamed inside the library by
    /// quick hash + size, carrying its review state to the new path; a recording
    /// that vanished entirely is reported unresolved (not deleted) and re-links on
    /// its own when it reappears. Re-designating re-runs the same pass.
    #[test]
    fn scan_relocates_by_hash_and_reports_unresolved() {
        let lib = TempLib::new();
        lib.write("day1/game.mp4", b"aaaa");
        lib.write("day1/other.mp4", b"bbbb");
        let mut conn = open(Path::new(":memory:")).unwrap();

        designate_library(&mut conn, "local", &lib.0, "local").unwrap();
        assert_eq!(scan_library(&mut conn).unwrap().registered, 2); // both files
        assert_eq!(scan_library(&mut conn).unwrap().registered, 0); // idempotent
        let game_abs = absolute(&lib.0.to_string_lossy(), "day1/game.mp4");
        // Flag a rally on game.mp4 so we can prove its review state follows it.
        let game_id = recording_id(&conn, &game_abs).unwrap().unwrap();
        conn.execute(
            "INSERT INTO rallies (recording_id, start_ms, end_ms, confidence, flagged)
             VALUES (?1, 0, 5000, 1.0, 1)",
            [game_id],
        )
        .unwrap();

        // Move game.mp4 to a new subfolder+name; delete other.mp4 entirely.
        std::fs::create_dir_all(lib.0.join("moved")).unwrap();
        std::fs::rename(lib.0.join("day1/game.mp4"), lib.0.join("moved/renamed.mp4")).unwrap();
        std::fs::remove_file(lib.0.join("day1/other.mp4")).unwrap();

        let result = scan_library(&mut conn).unwrap();
        assert_eq!(result.relocated, 1);
        assert_eq!(result.registered, 0);
        // game.mp4 re-linked to its new key, same row, review state intact.
        assert_eq!(stored_path(&conn, game_id), "moved/renamed.mp4");
        let moved_abs = absolute(&lib.0.to_string_lossy(), "moved/renamed.mp4");
        assert_eq!(
            recording_timeline(&conn, &moved_abs).unwrap().unwrap().rallies[0].flagged,
            true
        );
        // other.mp4 is gone and unmatched → unresolved, but its row is retained.
        let other_abs = absolute(&lib.0.to_string_lossy(), "day1/other.mp4");
        assert_eq!(result.unresolved, vec![other_abs.clone()]);
        assert!(recording_id(&conn, &other_abs).unwrap().is_some());

        // It reappears (same content, anywhere) → re-links automatically, no longer
        // unresolved, and does not double-register.
        lib.write("archive/other.mp4", b"bbbb");
        let result = scan_library(&mut conn).unwrap();
        assert_eq!(result.relocated, 1);
        assert!(result.unresolved.is_empty());
        assert_eq!(result.registered, 0);

        // Re-designating a subfolder re-runs adoption: game.mp4 lies under it and
        // re-links; the archive copy falls outside and drops off as unresolved.
        let result = designate_library_then_scan(&mut conn, &lib.0.join("moved"));
        assert!(stored_path(&conn, game_id) == "renamed.mp4");
        assert!(!result.unresolved.contains(&moved_abs));
    }

    /// Designate + the scan lib.rs runs after it — the real re-designation path.
    fn designate_library_then_scan(conn: &mut Connection, folder: &Path) -> ScanResult {
        designate_library(conn, "local", folder, "local").unwrap();
        scan_library(conn).unwrap()
    }

    /// Flag the recording's first rally so it counts as hand-touched review state.
    fn flag_first_rally(conn: &Connection, abs: &str) {
        let id = recording_id(conn, abs).unwrap().unwrap();
        conn.execute(
            "INSERT INTO rallies (recording_id, start_ms, end_ms, confidence, flagged)
             VALUES (?1, 0, 5000, 0.3, 1)",
            [id],
        )
        .unwrap();
    }

    /// Cross-library re-home (issue #63): a recording deleted from the local
    /// library and reappearing (hash + size) under the shared library re-homes
    /// automatically, its review state and session moving to the shared library.
    /// The reverse direction works too.
    #[test]
    fn cross_library_rehome_follows_a_moved_recording() {
        let local = TempLib::new();
        let shared = TempLib::new();
        let mut conn = open(Path::new(":memory:")).unwrap();

        // A recording in the local library, reviewed (a flagged rally).
        local.write("day1/game.mp4", b"the-bytes");
        designate_library(&mut conn, "local", &local.0, "local").unwrap();
        assert_eq!(scan_library(&mut conn).unwrap().registered, 1);
        let local_abs = absolute(&local.0.to_string_lossy(), "day1/game.mp4");
        flag_first_rally(&conn, &local_abs);
        let rec_id = recording_id(&conn, &local_abs).unwrap().unwrap();

        // Designate shared. The file is *moved* to the shared library: gone from
        // local, present (same content) under shared.
        designate_library(&mut conn, "shared", &shared.0, "network").unwrap();
        std::fs::remove_file(local.0.join("day1/game.mp4")).unwrap();
        shared.write("archive/game.mp4", b"the-bytes");

        // Scanning shared (now active) re-homes the recording rather than
        // registering a fresh one.
        let result = scan_library(&mut conn).unwrap();
        assert_eq!(result.registered, 0);
        assert_eq!(result.relocated, 1);

        // Same row, now in the shared library at its new relative key, review intact.
        assert_eq!(rec_id, recording_id(&conn, &absolute(&shared.0.to_string_lossy(), "archive/game.mp4")).unwrap().unwrap());
        let lib: String = conn
            .query_row("SELECT library FROM recordings WHERE id = ?1", [rec_id], |r| r.get(0))
            .unwrap();
        assert_eq!(lib, "shared");
        let shared_abs = absolute(&shared.0.to_string_lossy(), "archive/game.mp4");
        assert!(recording_timeline(&conn, &shared_abs).unwrap().unwrap().rallies[0].flagged);
        // Its session travelled: the shared library holds it, local is emptied.
        assert!(!list_sessions(&conn).unwrap().is_empty());
        set_active_kind(&conn, "local").unwrap();
        assert!(list_sessions(&conn).unwrap().is_empty());

        // Reverse: move it back to local. Scanning local re-homes it back.
        std::fs::remove_file(shared.0.join("archive/game.mp4")).unwrap();
        local.write("back/game.mp4", b"the-bytes");
        let result = scan_library(&mut conn).unwrap();
        assert_eq!(result.relocated, 1);
        assert_eq!(result.registered, 0);
        let back_abs = absolute(&local.0.to_string_lossy(), "back/game.mp4");
        assert_eq!(rec_id, recording_id(&conn, &back_abs).unwrap().unwrap());
        assert!(recording_timeline(&conn, &back_abs).unwrap().unwrap().rallies[0].flagged);
    }

    /// A copy — the same content present in *both* libraries — is never re-homed
    /// silently (issue #63): it stays a distinct recording in each library, and the
    /// carry-over is only ever offered.
    #[test]
    fn copy_present_in_both_is_not_rehomed() {
        let local = TempLib::new();
        let shared = TempLib::new();
        let mut conn = open(Path::new(":memory:")).unwrap();

        local.write("game.mp4", b"same");
        designate_library(&mut conn, "local", &local.0, "local").unwrap();
        scan_library(&mut conn).unwrap();
        let local_id = recording_id(&conn, &absolute(&local.0.to_string_lossy(), "game.mp4"))
            .unwrap()
            .unwrap();

        designate_library(&mut conn, "shared", &shared.0, "network").unwrap();
        shared.write("game.mp4", b"same"); // a copy — local still has its own
        let result = scan_library(&mut conn).unwrap();
        // Both files present → a fresh registration under shared, not a re-home.
        assert_eq!(result.registered, 1);
        assert_eq!(result.relocated, 0);
        let shared_id = recording_id(&conn, &absolute(&shared.0.to_string_lossy(), "game.mp4"))
            .unwrap()
            .unwrap();
        assert_ne!(local_id, shared_id); // two distinct recordings, one per library
    }

    /// Carry-over offer (issue #63): same content in both libraries, one side
    /// hand-touched and the other not → an offer to carry the review to the
    /// un-touched copy; accepting copies the review; both-touched offers nothing.
    #[test]
    fn carry_offer_and_apply() {
        let local = TempLib::new();
        let shared = TempLib::new();
        let mut conn = open(Path::new(":memory:")).unwrap();

        local.write("game.mp4", b"same");
        designate_library(&mut conn, "local", &local.0, "local").unwrap();
        scan_library(&mut conn).unwrap();
        let local_abs = absolute(&local.0.to_string_lossy(), "game.mp4");
        // Hand-touch the local copy: a flagged rally plus an annotation.
        flag_first_rally(&conn, &local_abs);
        add_annotation(&conn, &local_abs, 1000, "mistake").unwrap();

        designate_library(&mut conn, "shared", &shared.0, "network").unwrap();
        shared.write("game.mp4", b"same");
        scan_library(&mut conn).unwrap(); // registers the shared copy (un-touched)
        let shared_abs = absolute(&shared.0.to_string_lossy(), "game.mp4");

        // Exactly one offer, carrying local's review to the shared copy.
        let offers = carry_offers(&conn).unwrap();
        assert_eq!(offers.len(), 1);
        assert_eq!(offers[0].from_path, local_abs);
        assert_eq!(offers[0].to_path, shared_abs);
        assert_eq!(offers[0].to_kind, "shared");

        // Declining (not calling carry_review) leaves the shared copy untouched.
        set_active_kind(&conn, "shared").unwrap();
        assert!(recording_timeline(&conn, &shared_abs).unwrap().unwrap().rallies.is_empty());
        assert!(recording_annotations(&conn, &shared_abs).unwrap().is_empty());

        // Accepting carries the review over: the shared copy now has the flagged
        // rally and the annotation.
        assert!(carry_review(&mut conn, &offers[0].from_path, &offers[0].to_path).unwrap());
        assert!(recording_timeline(&conn, &shared_abs).unwrap().unwrap().rallies[0].flagged);
        assert_eq!(recording_annotations(&conn, &shared_abs).unwrap()[0].verdict, "mistake");

        // Now both sides are hand-touched → no more offers (no merge; out of scope).
        assert!(carry_offers(&conn).unwrap().is_empty());
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

    /// Designate an empty-string library so absolute paths round-trip unchanged
    /// through the resolution seam (see `db_with_rallies`). Idempotent.
    fn set_test_library(conn: &Connection) {
        conn.execute(
            "INSERT OR IGNORE INTO libraries (kind, path) VALUES ('local', '')",
            [],
        )
        .unwrap();
    }

    /// Inserts a recording on `day` under a session for that day, returning its id.
    fn insert_recording(conn: &Connection, path: &str, day: &str) -> i64 {
        set_test_library(conn);
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

    fn verdicts(conn: &Connection, path: &str) -> Vec<(i64, String)> {
        recording_annotations(conn, path)
            .unwrap()
            .into_iter()
            .map(|a| (a.time_ms, a.verdict))
            .collect()
    }

    /// A dropped verdict persists pinned to its timestamp and reads back in
    /// timestamp order (issue #8, the fast capture path).
    #[test]
    fn annotation_persists_at_its_timestamp() {
        let (conn, _) = db_with_rallies("/r.mp4", &[(1000, 5000)]);
        add_annotation(&conn, "/r.mp4", 3200, "mistake").unwrap();
        add_annotation(&conn, "/r.mp4", 1500, "good").unwrap();
        assert_eq!(
            verdicts(&conn, "/r.mp4"),
            vec![
                (1500, "good".to_string()),
                (3200, "mistake".to_string()),
            ]
        );
    }

    /// Two annotations may sit at the same timestamp — a moment with mixed
    /// verdicts is recorded as more than one (glossary).
    #[test]
    fn annotations_can_share_a_timestamp() {
        let (conn, _) = db_with_rallies("/r.mp4", &[(1000, 5000)]);
        add_annotation(&conn, "/r.mp4", 2000, "good").unwrap();
        add_annotation(&conn, "/r.mp4", 2000, "mistake").unwrap();
        assert_eq!(
            verdicts(&conn, "/r.mp4"),
            vec![
                (2000, "good".to_string()),
                (2000, "mistake".to_string()),
            ]
        );
    }

    /// Annotations are scoped to their recording: dropping one on `/a.mp4` never
    /// shows up under `/b.mp4`, and an unregistered path is a no-op.
    #[test]
    fn annotations_are_scoped_to_their_recording() {
        let (conn, _) = db_with_rallies("/a.mp4", &[(1000, 5000)]);
        conn.execute(
            "INSERT INTO recordings (session_id, path, file_size, quick_hash, capture_day, probe_state, segment_state, duration_ms)
             VALUES (1, '/b.mp4', 0, '', '2026-01-01', 'ready', 'ready', 100000)",
            [],
        )
        .unwrap();
        add_annotation(&conn, "/a.mp4", 2000, "bad").unwrap();
        assert!(add_annotation(&conn, "/missing.mp4", 2000, "bad")
            .unwrap()
            .is_none());
        assert_eq!(verdicts(&conn, "/a.mp4"), vec![(2000, "bad".to_string())]);
        assert!(verdicts(&conn, "/b.mp4").is_empty());
        assert!(recording_annotations(&conn, "/missing.mp4").unwrap().is_empty());
    }

    /// Deleting a recording cascades to its annotations (they key off the
    /// recording, so they travel/vanish with it).
    #[test]
    fn deleting_a_recording_cascades_to_its_annotations() {
        let (conn, id) = db_with_rallies("/r.mp4", &[(1000, 5000)]);
        add_annotation(&conn, "/r.mp4", 2000, "good").unwrap();
        conn.execute("DELETE FROM recordings WHERE id = ?1", [id]).unwrap();
        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM annotations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining, 0);
    }

    /// Enriching an annotation sets its aspect and note and can re-classify its
    /// verdict; they read back and clearing a field sets it null (issue #9).
    #[test]
    fn annotation_enriches_and_clears() {
        let (conn, _) = db_with_rallies("/r.mp4", &[(1000, 5000)]);
        let id = add_annotation(&conn, "/r.mp4", 2000, "bad").unwrap().unwrap();
        assert!(update_annotation(
            &conn,
            "/r.mp4",
            id,
            "mistake",
            Some("execution"),
            Some("late smash into the net"),
        )
        .unwrap());
        let a = &recording_annotations(&conn, "/r.mp4").unwrap()[0];
        assert_eq!(a.verdict, "mistake");
        assert_eq!(a.aspect.as_deref(), Some("execution"));
        assert_eq!(a.note.as_deref(), Some("late smash into the net"));

        update_annotation(&conn, "/r.mp4", id, "mistake", None, None).unwrap();
        let a = &recording_annotations(&conn, "/r.mp4").unwrap()[0];
        assert_eq!(a.aspect, None);
        assert_eq!(a.note, None);
    }

    /// Update and delete are scoped to the recording, and delete removes just the
    /// one annotation (issue #9). A stray path or unknown id touches nothing.
    #[test]
    fn annotation_update_and_delete_are_scoped() {
        let (conn, _) = db_with_rallies("/r.mp4", &[(1000, 5000)]);
        let a = add_annotation(&conn, "/r.mp4", 2000, "good").unwrap().unwrap();
        let b = add_annotation(&conn, "/r.mp4", 3000, "bad").unwrap().unwrap();

        assert!(!update_annotation(&conn, "/missing.mp4", a, "good", None, None).unwrap());
        assert!(!delete_annotation(&conn, "/missing.mp4", a).unwrap());
        assert!(!delete_annotation(&conn, "/r.mp4", 9999).unwrap());
        assert_eq!(verdicts(&conn, "/r.mp4").len(), 2);

        assert!(delete_annotation(&conn, "/r.mp4", a).unwrap());
        assert_eq!(verdicts(&conn, "/r.mp4"), vec![(3000, "bad".to_string())]);
        assert_eq!(
            recording_annotations(&conn, "/r.mp4").unwrap()[0].id,
            b
        );
    }

    /// A bundle carries every portable field of every recording in a session —
    /// relative path, hash + size, capture day, duration, waveform, timeline
    /// (with confidence + flag), and annotations — and nothing per-device.
    #[test]
    fn bundle_carries_a_session_review() {
        let (conn, rid) = db_with_rallies("r.mp4", &[(1000, 5000), (8000, 12000)]);
        // Flag one rally and annotate, so both travel in the bundle.
        let tl = recording_timeline(&conn, "r.mp4").unwrap().unwrap();
        set_rally_flag(&conn, "r.mp4", tl.rallies[0].id, true).unwrap();
        add_annotation(&conn, "r.mp4", 2000, "good").unwrap();

        let bundle = build_session_bundle(&conn, 1, "alice").unwrap().unwrap();
        assert_eq!(bundle.format, BUNDLE_FORMAT);
        assert_eq!(bundle.version, BUNDLE_VERSION);
        assert_eq!(bundle.capture_day, "2026-01-01");
        assert_eq!(bundle.sharer_label, "alice");
        assert_eq!(bundle.recordings.len(), 1);
        let rec = &bundle.recordings[0];
        assert_eq!(rec.path, "r.mp4"); // library-relative, not absolute
        assert_eq!(rec.rallies.len(), 2);
        assert!(rec.rallies[0].flagged);
        assert!(!rec.rallies[1].flagged);
        assert_eq!(rec.annotations.len(), 1);
        assert_eq!(rec.annotations[0].verdict, "good");
        assert_eq!(rec.duration_ms, Some(100_000));
        let _ = rid;

        // Round-trips through JSON (the on-disk form).
        let json = serde_json::to_string(&bundle).unwrap();
        assert!(json.contains("\"format\":\"voloph-session-bundle\""));

        // A missing session id yields no bundle.
        assert!(build_session_bundle(&conn, 999, "alice").unwrap().is_none());
    }

    /// The bundle file name keys on session day + sharer label and sanitizes the
    /// label into a safe stem, so a re-share overwrites only that sharer's file.
    #[test]
    fn bundle_file_name_keys_on_day_and_sharer() {
        assert_eq!(
            bundle_file_name("2026-01-01", "alice"),
            "2026-01-01__alice.vbundle"
        );
        assert_eq!(
            bundle_file_name("2026-01-01", "Bob's phone"),
            "2026-01-01__Bob-s-phone.vbundle"
        );
        // Two sharers on the same day produce two distinct files.
        assert_ne!(
            bundle_file_name("2026-01-01", "alice"),
            bundle_file_name("2026-01-01", "bob")
        );
        // An empty/garbage label still yields a usable stem.
        assert_eq!(
            bundle_file_name("2026-01-01", "///"),
            "2026-01-01__unnamed.vbundle"
        );
    }

    // --- Receiving bundles (issue #66) ------------------------------------

    /// A sharer's DB over the shared library `lib`, with two recordings under it
    /// (both real files so quick hashes verify), the first flagged + annotated
    /// so it carries hand-touched state and a custom aspect. Returns the DB and
    /// the built bundle's JSON, ready to receive on another device pointed at the
    /// same shared folder.
    fn shared_bundle_json(lib: &TempLib) -> String {
        lib.write("a.mp4", b"aaaa");
        lib.write("b.mp4", b"bbbb");
        let mut conn = open(Path::new(":memory:")).unwrap();
        designate_library(&mut conn, "shared", &lib.0, "network").unwrap();
        assert_eq!(scan_library(&mut conn).unwrap().registered, 2);
        let a_abs = absolute(&lib.0.to_string_lossy(), "a.mp4");
        // Give a.mp4 a machine timeline, then flag a rally + annotate with an
        // aspect outside the seeded vocabulary — hand-touched, carried in the bundle.
        let a_id = recording_id(&conn, &a_abs).unwrap().unwrap();
        conn.execute(
            "INSERT INTO rallies (recording_id, start_ms, end_ms, confidence, flagged)
             VALUES (?1, 1000, 5000, 0.4, 1), (?1, 8000, 9000, 0.3, 0)",
            [a_id],
        )
        .unwrap();
        conn.execute(
            "UPDATE recordings SET segment_state = 'ready', duration_ms = 60000 WHERE id = ?1",
            [a_id],
        )
        .unwrap();
        add_annotation(&conn, &a_abs, 2000, "good").unwrap();
        update_annotation(
            &conn,
            &a_abs,
            recording_annotations(&conn, &a_abs).unwrap()[0].id,
            "good",
            Some("serve-toss"),
            None,
        )
        .unwrap();
        // Group both recordings under one session so the bundle spans them.
        let session_id: i64 = conn
            .query_row(
                "SELECT session_id FROM recordings WHERE id = ?1",
                [a_id],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "UPDATE recordings SET session_id = ?1 WHERE library = 'shared'",
            [session_id],
        )
        .unwrap();
        let bundle = build_session_bundle(&conn, session_id, "alice")
            .unwrap()
            .unwrap();
        serde_json::to_string(&bundle).unwrap()
    }

    fn recipient_db(lib: &TempLib) -> Connection {
        let mut conn = open(Path::new(":memory:")).unwrap();
        designate_library(&mut conn, "shared", &lib.0, "network").unwrap();
        conn
    }

    /// Receiving on a device that never scanned the session registers every
    /// recording (self-sufficient), applies the carried timeline/annotations, and
    /// the recordings play immediately (segment_state ready with a duration). An
    /// imported aspect enters the recipient's vocabulary. A second receive is a no-op.
    #[test]
    fn receive_registers_and_applies_on_a_fresh_device() {
        let lib = TempLib::new();
        let json = shared_bundle_json(&lib);
        let mut conn = recipient_db(&lib);

        let result = receive_session_bundle(&mut conn, &json).unwrap();
        assert_eq!(result.applied, 2);
        assert!(result.refused.is_empty());
        assert!(result.conflicts.is_empty());

        // a.mp4 registered with the sharer's timeline, flag, annotation, aspect.
        let a_abs = absolute(&lib.0.to_string_lossy(), "a.mp4");
        let tl = recording_timeline(&conn, &a_abs).unwrap().unwrap();
        assert_eq!(tl.rallies.len(), 2);
        assert!(tl.rallies[0].flagged);
        let anns = recording_annotations(&conn, &a_abs).unwrap();
        assert_eq!(anns.len(), 1);
        assert_eq!(anns[0].aspect.as_deref(), Some("serve-toss"));
        // Plays immediately: probed + segmented ready with a learned duration.
        let (probe, seg, dur): (String, String, Option<i64>) = conn
            .query_row(
                "SELECT probe_state, segment_state, duration_ms FROM recordings
                 WHERE library = 'shared' AND path = 'a.mp4'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(probe, "ready");
        assert_eq!(seg, "ready");
        assert_eq!(dur, Some(60000));
        // The imported aspect appears in the recipient's vocabulary (AC4).
        assert!(aspect_vocabulary(&conn).unwrap().contains(&"serve-toss".to_string()));

        // Receiving the identical bundle again changes nothing, resolves nothing.
        let again = receive_session_bundle(&mut conn, &json).unwrap();
        assert_eq!(again.applied, 0);
        assert!(again.refused.is_empty());
        assert!(again.conflicts.is_empty());
    }

    /// A recording whose file fails hash + size verification is refused by name;
    /// the rest of the bundle still applies.
    #[test]
    fn receive_refuses_a_mismatched_file_and_applies_the_rest() {
        let lib = TempLib::new();
        let json = shared_bundle_json(&lib);
        // Corrupt a.mp4's bytes so its quick hash no longer matches the bundle.
        std::fs::write(lib.0.join("a.mp4"), b"tampered-different-length").unwrap();
        let mut conn = recipient_db(&lib);

        let result = receive_session_bundle(&mut conn, &json).unwrap();
        assert_eq!(result.applied, 1); // b.mp4 still applies
        assert_eq!(result.refused.len(), 1);
        assert!(result.refused[0].path.ends_with("a.mp4")); // names the file
        // The refused recording was not registered.
        assert!(recording_id(&conn, &absolute(&lib.0.to_string_lossy(), "a.mp4"))
            .unwrap()
            .is_none());
    }

    /// Machine-only local state is replaced silently; hand-touched local state is
    /// surfaced as a keep-mine-or-take-theirs conflict (nothing changed), which the
    /// user resolves per recording at whole-recording granularity.
    #[test]
    fn receive_replaces_machine_state_and_offers_conflict_on_hand_touched() {
        let lib = TempLib::new();
        let json = shared_bundle_json(&lib);
        let mut conn = recipient_db(&lib);
        // The recipient scanned first, so both recordings exist locally.
        assert_eq!(scan_library(&mut conn).unwrap().registered, 2);
        let a_abs = absolute(&lib.0.to_string_lossy(), "a.mp4");
        let b_abs = absolute(&lib.0.to_string_lossy(), "b.mp4");
        // b.mp4: a machine-only draft (uncertain rally, no flag/annotation).
        let b_id = recording_id(&conn, &b_abs).unwrap().unwrap();
        conn.execute(
            "INSERT INTO rallies (recording_id, start_ms, end_ms, confidence, flagged)
             VALUES (?1, 0, 1000, 0.2, 0)",
            [b_id],
        )
        .unwrap();
        // a.mp4: hand-touched (an annotation of the recipient's own).
        add_annotation(&conn, &a_abs, 500, "bad").unwrap();

        let result = receive_session_bundle(&mut conn, &json).unwrap();
        // b.mp4 machine-only → replaced silently; a.mp4 hand-touched → conflict.
        assert_eq!(result.applied, 1);
        assert_eq!(result.conflicts, vec!["a.mp4".to_string()]);
        // a.mp4 untouched so far: still the recipient's single annotation, no bundle rallies.
        assert!(recording_timeline(&conn, &a_abs).unwrap().unwrap().rallies.is_empty());
        assert_eq!(recording_annotations(&conn, &a_abs).unwrap().len(), 1);
        // b.mp4 took the bundle: it had no rallies in the bundle → its draft is gone.
        assert!(recording_timeline(&conn, &b_abs).unwrap().unwrap().rallies.is_empty());

        // Keep-mine: a.mp4 is left exactly as the recipient had it.
        assert!(!resolve_bundle_conflict(&mut conn, &json, "a.mp4", false).unwrap());
        assert_eq!(recording_annotations(&conn, &a_abs).unwrap().len(), 1);

        // Take-theirs: a.mp4 wholly replaced by the bundle (2 rallies, sharer's annotation).
        assert!(resolve_bundle_conflict(&mut conn, &json, "a.mp4", true).unwrap());
        let tl = recording_timeline(&conn, &a_abs).unwrap().unwrap();
        assert_eq!(tl.rallies.len(), 2);
        assert!(tl.rallies[0].flagged);
        let anns = recording_annotations(&conn, &a_abs).unwrap();
        assert_eq!(anns.len(), 1);
        assert_eq!(anns[0].aspect.as_deref(), Some("serve-toss"));
    }

    /// A file/format that is not a Voloph bundle is rejected before any state
    /// changes; a bundle from a newer format version is refused too.
    #[test]
    fn receive_rejects_a_non_bundle_and_newer_version() {
        let lib = TempLib::new();
        let mut conn = recipient_db(&lib);
        assert!(receive_session_bundle(&mut conn, "{not json").is_err());
        assert!(receive_session_bundle(&mut conn, r#"{"format":"nope","version":1,"capture_day":"","sharer_label":"","recordings":[]}"#).is_err());
        let newer = format!(
            r#"{{"format":"{BUNDLE_FORMAT}","version":{},"capture_day":"","sharer_label":"","recordings":[]}}"#,
            BUNDLE_VERSION + 1
        );
        assert!(receive_session_bundle(&mut conn, &newer).is_err());
    }
}
