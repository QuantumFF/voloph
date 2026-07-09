//! Schema creation and in-place migrations. `open` is the single entry point;
//! every table and every backfill of an older DB lives here.

use std::path::Path;

use rusqlite::Connection;

/// Video file extensions we register as recordings.
pub(crate) const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "mov", "m4v", "avi", "mkv", "mts", "m2ts", "ts", "webm",
];

pub(crate) fn is_video(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| VIDEO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
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
            -- The segmenter version that produced this recording's current draft
            -- (ADR 0013/0015). NULL until segmented or adopted; governs staleness —
            -- an untouched recording whose version the active segmenter outranks is
            -- silently re-analyzed, a hand-touched one never is.
            segmenter_version INTEGER,
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
    let _ = conn.execute(
        "ALTER TABLE rallies ADD COLUMN flagged INTEGER NOT NULL DEFAULT 0",
        [],
    );
    // Version-aware staleness (ADR 0013/0015, issue #80): stamp the segmenter
    // version onto each recording's draft so a later bump can silently re-analyze
    // untouched recordings and supersede stale shared Analyses. Backfill every
    // already-segmented row to the current version — pre-feature drafts predate
    // versioning, and treating them as the current version keeps the feature inert
    // until a real bump raises `SEGMENTER_VERSION` above them (untouched
    // recordings only go stale once the active segmenter outranks their stamp).
    let fresh_version_column = conn
        .execute(
            "ALTER TABLE recordings ADD COLUMN segmenter_version INTEGER",
            [],
        )
        .is_ok();
    if fresh_version_column {
        conn.execute(
            "UPDATE recordings SET segmenter_version = ?1 WHERE segment_state = 'ready'",
            [crate::segment::SEGMENTER_VERSION],
        )?;
    }
    // Two typed libraries (issue #62): the shared library adds a `mount` locality
    // to `libraries`, and every session/recording is tagged with the library it
    // belongs to so the same relative path or capture day can exist in both. A DB
    // from #60/#61 held only the local library, so its rows migrate to 'local'.
    let _ = conn.execute(
        "ALTER TABLE libraries ADD COLUMN mount TEXT NOT NULL DEFAULT 'local'",
        [],
    );
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
    // Heal DBs bitten by the first version of that migration: its
    // `ALTER TABLE recordings RENAME TO recordings_old` made SQLite rewrite the
    // child tables' FK clauses (rallies, annotations) to point at
    // `recordings_old`, which the migration then dropped — leaving every
    // timeline/annotation write failing with "no such table: main.recordings_old".
    repair_dangling_recording_refs(&conn)?;
    Ok(conn)
}

/// Rebuild `rallies` and `annotations` when their FK clauses dangle on the
/// dropped `recordings_old` (see `open`). Rebuilding in the safe order —
/// create new, copy, drop old, rename into place — re-points them at
/// `recordings` while keeping every row. No-op on a healthy DB.
fn repair_dangling_recording_refs(conn: &Connection) -> rusqlite::Result<()> {
    let broken: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'table' AND sql LIKE '%recordings_old%'",
        [],
        |r| r.get(0),
    )?;
    if broken == 0 {
        return Ok(());
    }
    conn.pragma_update(None, "foreign_keys", false)?;
    conn.execute_batch(
        "BEGIN;
         CREATE TABLE rallies_new (
            id           INTEGER PRIMARY KEY,
            recording_id INTEGER NOT NULL REFERENCES recordings(id) ON DELETE CASCADE,
            start_ms     INTEGER NOT NULL,
            end_ms       INTEGER NOT NULL,
            confidence   REAL NOT NULL,
            flagged      INTEGER NOT NULL DEFAULT 0
         );
         INSERT INTO rallies_new SELECT id, recording_id, start_ms, end_ms, confidence, flagged FROM rallies;
         DROP TABLE rallies;
         ALTER TABLE rallies_new RENAME TO rallies;
         CREATE INDEX IF NOT EXISTS idx_rallies_recording ON rallies(recording_id);
         CREATE TABLE annotations_new (
            id           INTEGER PRIMARY KEY,
            recording_id INTEGER NOT NULL REFERENCES recordings(id) ON DELETE CASCADE,
            time_ms      INTEGER NOT NULL,
            verdict      TEXT NOT NULL,
            aspect       TEXT,
            note         TEXT
         );
         INSERT INTO annotations_new SELECT id, recording_id, time_ms, verdict, aspect, note FROM annotations;
         DROP TABLE annotations;
         ALTER TABLE annotations_new RENAME TO annotations;
         CREATE INDEX IF NOT EXISTS idx_annotations_recording ON annotations(recording_id);
         COMMIT;",
    )?;
    conn.pragma_update(None, "foreign_keys", true)?;
    Ok(())
}

/// One-time rebuild of `sessions` and `recordings` for the two-library model
/// (issue #62): recreate them with a `library` column and library-scoped uniques,
/// copying every existing row across as 'local'. Runs only on a pre-#62 DB (the
/// old table-level UNIQUE(path)/UNIQUE(capture_day) cannot be dropped in place).
fn migrate_to_typed_libraries(conn: &Connection) -> rusqlite::Result<()> {
    conn.pragma_update(None, "foreign_keys", false)?;
    // Safe rebuild order per the SQLite docs ("Making Other Kinds Of Table
    // Schema Changes"): create the new table under a temporary name, copy, drop
    // the old, then rename the new into place. Renaming the *old* table out of
    // the way instead would make SQLite rewrite the FK clauses of the child
    // tables (rallies, annotations) to follow the rename, leaving them pointing
    // at a dropped table.
    conn.execute_batch(
        "BEGIN;
         CREATE TABLE sessions_new (
            id          INTEGER PRIMARY KEY,
            capture_day TEXT NOT NULL,
            library     TEXT NOT NULL DEFAULT 'local',
            UNIQUE(library, capture_day)
         );
         CREATE TABLE recordings_new (
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
            segmenter_version INTEGER,
            UNIQUE(library, path)
         );
         INSERT INTO sessions_new (id, capture_day, library)
            SELECT id, capture_day, 'local' FROM sessions;
         INSERT INTO recordings_new
            (id, session_id, path, library, file_size, quick_hash, capture_day,
             probe_state, segment_state, date_state, duration_ms, waveform, segmenter_version)
            SELECT id, session_id, path, 'local', file_size, quick_hash, capture_day,
                   probe_state, segment_state, date_state, duration_ms, waveform, segmenter_version
            FROM recordings;
         DROP TABLE recordings;
         DROP TABLE sessions;
         ALTER TABLE sessions_new RENAME TO sessions;
         ALTER TABLE recordings_new RENAME TO recordings;
         COMMIT;",
    )?;
    conn.pragma_update(None, "foreign_keys", true)?;
    Ok(())
}
