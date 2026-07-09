//! Tests for the metadata database. Kept in one module so `use super::*`
//! reaches every submodule's re-exported API and its `pub(crate)` helpers.

use super::*;
use rusqlite::Connection;
use std::path::Path;

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
    let id = recording_timeline(&conn, "/r.mp4")
        .unwrap()
        .unwrap()
        .rallies[0]
        .id;
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
    let id = recording_timeline(&conn, "/r.mp4")
        .unwrap()
        .unwrap()
        .rallies[0]
        .id;
    assert!(delete_rally(&conn, "/r.mp4", id).unwrap());
    assert_eq!(intervals(&conn, "/r.mp4"), vec![(8000, 12000, 0.3)]);
}

#[test]
fn delete_recording_discards_review_and_gcs_the_session() {
    let (conn, _) = db_with_rallies("/r.mp4", &[(1000, 5000), (8000, 12000)]);
    add_annotation(&conn, "/r.mp4", 2000, "good").unwrap();

    assert!(delete_recording(&conn, "/r.mp4").unwrap());

    let count = |sql: &str| conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
    assert_eq!(count("SELECT COUNT(*) FROM recordings"), 0);
    assert_eq!(count("SELECT COUNT(*) FROM rallies"), 0);
    assert_eq!(count("SELECT COUNT(*) FROM annotations"), 0);
    // The session it emptied is garbage-collected, mirroring the re-home GC.
    assert_eq!(count("SELECT COUNT(*) FROM sessions"), 0);
}

#[test]
fn delete_recording_is_false_when_absent() {
    let (conn, _) = db_with_rallies("/r.mp4", &[(1000, 5000)]);
    assert!(!delete_recording(&conn, "/gone.mp4").unwrap());
    // The real recording is untouched.
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM recordings", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn split_is_update_plus_add() {
    // The frontend composes split from update + add; exercise the same here.
    let (conn, _) = db_with_rallies("/r.mp4", &[(1000, 9000)]);
    let id = recording_timeline(&conn, "/r.mp4")
        .unwrap()
        .unwrap()
        .rallies[0]
        .id;
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
    let id = recording_timeline(&conn, "/r.mp4")
        .unwrap()
        .unwrap()
        .rallies[0]
        .id;
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
    let a_id = recording_timeline(&conn, "/a.mp4")
        .unwrap()
        .unwrap()
        .rallies[0]
        .id;
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
    let id = recording_timeline(&conn, "/r.mp4")
        .unwrap()
        .unwrap()
        .rallies[0]
        .id;
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
    let a_id = recording_timeline(&conn, "/a.mp4")
        .unwrap()
        .unwrap()
        .rallies[0]
        .id;
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
        &[crate::segment::Rally {
            start_ms: 0,
            end_ms: 5_000,
            confidence: 1.0,
        }],
        &[],
    )
    .unwrap();

    // Moment on the long rally in /a and the short rally in /b.
    let a_long = recording_timeline(&conn, "/a.mp4")
        .unwrap()
        .unwrap()
        .rallies[0]
        .id;
    let a_short = recording_timeline(&conn, "/a.mp4")
        .unwrap()
        .unwrap()
        .rallies[1]
        .id;
    set_rally_flag(&conn, "/a.mp4", a_long, true).unwrap();
    let m1 = add_annotation(&conn, "/a.mp4", 1_000, "mistake")
        .unwrap()
        .unwrap();
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
    assert!(mistakes
        .iter()
        .all(|r| r.annotations.len() == 1 && r.annotations[0].verdict == "mistake"));
    assert_ne!(mistakes[0].rally_id, a_short);

    // aspect=execution AND verdict=mistake → only /a's long rally.
    let combo = filter_moments(&conn, Some("mistake"), Some("execution"), None, None).unwrap();
    assert_eq!(combo.len(), 1);
    assert_eq!(combo[0].rally_id, a_long);
    assert_eq!(combo[0].annotations[0].aspect.as_deref(), Some("execution"));

    // Combined moment + rally filters: mistake in a *long* rally → still /a only.
    let combo2 = filter_moments(&conn, Some("mistake"), None, Some(true), None).unwrap();
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
                 -- The real shipped child tables carry FK clauses on recordings;
                 -- a RENAME of recordings rewrites these, so the fixture must
                 -- have them for the migration test to be honest.
                 CREATE TABLE rallies (
                    id INTEGER PRIMARY KEY,
                    recording_id INTEGER NOT NULL REFERENCES recordings(id) ON DELETE CASCADE,
                    start_ms INTEGER NOT NULL, end_ms INTEGER NOT NULL,
                    confidence REAL NOT NULL, flagged INTEGER NOT NULL DEFAULT 0);
                 CREATE TABLE annotations (
                    id INTEGER PRIMARY KEY,
                    recording_id INTEGER NOT NULL REFERENCES recordings(id) ON DELETE CASCADE,
                    time_ms INTEGER NOT NULL, verdict TEXT NOT NULL,
                    aspect TEXT, note TEXT);
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
        .query_row("SELECT library FROM sessions WHERE id = 1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(session_lib, "local");
    // The rally survived the rebuild.
    let flagged: i64 = conn
        .query_row(
            "SELECT flagged FROM rallies WHERE recording_id = 7",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(flagged, 1);
    // Writes into the child tables still work after the rebuild — the rename
    // must not leave rallies/annotations pointing at a dropped table
    // ("no such table: main.recordings_old" from the media worker otherwise).
    conn.execute(
            "INSERT INTO rallies (recording_id, start_ms, end_ms, confidence) VALUES (7, 6000, 9000, 0.8)",
            [],
        )
        .unwrap();
    conn.execute(
        "INSERT INTO annotations (recording_id, time_ms, verdict) VALUES (7, 6500, 'good')",
        [],
    )
    .unwrap();
    drop(conn);
    let _ = std::fs::remove_file(&path);
}

/// A DB already bitten by the pre-fix #62 migration (its RENAME rewrote the
/// child tables' FK clauses to `recordings_old`, then dropped that table)
/// heals on open: the child tables are rebuilt against `recordings` with
/// every row kept, and timeline writes work again.
#[test]
fn open_repairs_child_tables_broken_by_earlier_rename() {
    let path = std::env::temp_dir().join(format!(
        "voloph-repair-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    // Reproduce the corrupted state the bad migration left behind: new-shape
    // sessions/recordings, but child FKs pointing at the dropped table.
    {
        let c = Connection::open(&path).unwrap();
        c.execute_batch(
                "PRAGMA foreign_keys = OFF;
                 CREATE TABLE sessions (
                    id INTEGER PRIMARY KEY, capture_day TEXT NOT NULL,
                    library TEXT NOT NULL DEFAULT 'local', UNIQUE(library, capture_day));
                 CREATE TABLE recordings (
                    id INTEGER PRIMARY KEY,
                    session_id INTEGER NOT NULL REFERENCES sessions(id),
                    path TEXT NOT NULL, library TEXT NOT NULL DEFAULT 'local',
                    file_size INTEGER NOT NULL, quick_hash TEXT NOT NULL,
                    capture_day TEXT NOT NULL,
                    probe_state TEXT NOT NULL DEFAULT 'unknown',
                    segment_state TEXT NOT NULL DEFAULT 'unknown',
                    date_state TEXT NOT NULL DEFAULT 'unknown',
                    duration_ms INTEGER, waveform TEXT, UNIQUE(library, path));
                 CREATE TABLE rallies (
                    id INTEGER PRIMARY KEY,
                    recording_id INTEGER NOT NULL REFERENCES \"recordings_old\"(id) ON DELETE CASCADE,
                    start_ms INTEGER NOT NULL, end_ms INTEGER NOT NULL,
                    confidence REAL NOT NULL, flagged INTEGER NOT NULL DEFAULT 0);
                 CREATE INDEX idx_rallies_recording ON rallies(recording_id);
                 CREATE TABLE annotations (
                    id INTEGER PRIMARY KEY,
                    recording_id INTEGER NOT NULL REFERENCES \"recordings_old\"(id) ON DELETE CASCADE,
                    time_ms INTEGER NOT NULL, verdict TEXT NOT NULL,
                    aspect TEXT, note TEXT);
                 CREATE INDEX idx_annotations_recording ON annotations(recording_id);
                 INSERT INTO sessions (id, capture_day) VALUES (1, '2026-01-01');
                 INSERT INTO recordings (id, session_id, path, library, file_size, quick_hash, capture_day)
                    VALUES (7, 1, 'day1/game.mp4', 'local', 0, '', '2026-01-01');
                 INSERT INTO rallies (recording_id, start_ms, end_ms, confidence, flagged)
                    VALUES (7, 0, 5000, 1.0, 1);
                 INSERT INTO annotations (recording_id, time_ms, verdict) VALUES (7, 100, 'good');",
            )
            .unwrap();
    }
    let conn = open(&path).unwrap();
    // Existing review state survived the repair…
    let flagged: i64 = conn
        .query_row(
            "SELECT flagged FROM rallies WHERE recording_id = 7",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(flagged, 1);
    let verdict: String = conn
        .query_row(
            "SELECT verdict FROM annotations WHERE recording_id = 7",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(verdict, "good");
    // …and the media worker's write path works again.
    conn.execute(
            "INSERT INTO rallies (recording_id, start_ms, end_ms, confidence) VALUES (7, 6000, 9000, 0.8)",
            [],
        )
        .unwrap();
    conn.execute(
        "INSERT INTO annotations (recording_id, time_ms, verdict) VALUES (7, 6500, 'bad')",
        [],
    )
    .unwrap();
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
    assert_eq!(
        list_sessions(&conn).unwrap()[0].recordings[0].path,
        shared_abs
    );

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
    assert_eq!(
        list_sessions(&conn).unwrap()[0].recordings[0].path,
        local_abs
    );
    // The local recording has no rallies (the shared one's are not its own).
    assert!(recording_timeline(&conn, &local_abs)
        .unwrap()
        .unwrap()
        .rallies
        .is_empty());

    // Switch back to shared: nothing lost — the flagged rally is still there.
    set_active_kind(&conn, "shared").unwrap();
    assert_eq!(
        recording_timeline(&conn, &shared_abs)
            .unwrap()
            .unwrap()
            .rallies[0]
            .flagged,
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
        recording_timeline(&conn, &moved_abs)
            .unwrap()
            .unwrap()
            .rallies[0]
            .flagged,
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
    assert_eq!(
        rec_id,
        recording_id(
            &conn,
            &absolute(&shared.0.to_string_lossy(), "archive/game.mp4")
        )
        .unwrap()
        .unwrap()
    );
    let lib: String = conn
        .query_row(
            "SELECT library FROM recordings WHERE id = ?1",
            [rec_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(lib, "shared");
    let shared_abs = absolute(&shared.0.to_string_lossy(), "archive/game.mp4");
    assert!(
        recording_timeline(&conn, &shared_abs)
            .unwrap()
            .unwrap()
            .rallies[0]
            .flagged
    );
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
    assert!(
        recording_timeline(&conn, &back_abs)
            .unwrap()
            .unwrap()
            .rallies[0]
            .flagged
    );
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
    let local_id = recording_id(&conn, &local_abs).unwrap().unwrap();
    // The local copy is fully analyzed (segmented, with a duration + waveform)
    // and hand-touched (a flagged rally plus an annotation).
    conn.execute(
        "UPDATE recordings
             SET segment_state = 'ready', duration_ms = 60000, waveform = '[0.1,0.2,0.3]'
             WHERE id = ?1",
        [local_id],
    )
    .unwrap();
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

    // Declining (not calling carry_review) leaves the shared copy un-analyzed
    // and un-touched.
    set_active_kind(&conn, "shared").unwrap();
    let before = recording_timeline(&conn, &shared_abs).unwrap().unwrap();
    assert_eq!(before.segment_state, "unknown");
    assert!(before.rallies.is_empty());
    assert!(recording_annotations(&conn, &shared_abs)
        .unwrap()
        .is_empty());

    // Accepting carries the whole review over: the shared copy gets the flagged
    // rally, the annotation, *and* the analyzed timeline — segmented, with the
    // duration + waveform — so the media worker never re-analyzes it (which
    // would wipe the carried flags and segments, ADR 0011).
    assert!(carry_review(&mut conn, &offers[0].from_path, &offers[0].to_path).unwrap());
    let after = recording_timeline(&conn, &shared_abs).unwrap().unwrap();
    assert_eq!(after.segment_state, "ready");
    assert_eq!(after.duration_ms, Some(60000));
    assert!(!after.waveform.is_empty());
    assert!(after.rallies[0].flagged);
    assert_eq!(
        recording_annotations(&conn, &shared_abs).unwrap()[0].verdict,
        "mistake"
    );

    // Now both sides are hand-touched → no more offers (no merge; out of scope).
    assert!(carry_offers(&conn).unwrap().is_empty());
}

/// Dismissing a carry-over offer (ADR 0011) persists: it stops being surfaced
/// for that copy without carrying anything, and stays quiet across later scans.
#[test]
fn dismiss_carry_silences_the_offer() {
    let local = TempLib::new();
    let shared = TempLib::new();
    let mut conn = open(Path::new(":memory:")).unwrap();

    local.write("game.mp4", b"same");
    designate_library(&mut conn, "local", &local.0, "local").unwrap();
    scan_library(&mut conn).unwrap();
    let local_abs = absolute(&local.0.to_string_lossy(), "game.mp4");
    flag_first_rally(&conn, &local_abs); // hand-touch the local copy

    designate_library(&mut conn, "shared", &shared.0, "network").unwrap();
    shared.write("game.mp4", b"same");
    scan_library(&mut conn).unwrap();
    let shared_abs = absolute(&shared.0.to_string_lossy(), "game.mp4");

    // One offer, into the shared copy — until it is dismissed.
    let offers = carry_offers(&conn).unwrap();
    assert_eq!(offers.len(), 1);
    dismiss_carry(&conn, &offers[0].to_path).unwrap();
    assert!(carry_offers(&conn).unwrap().is_empty());

    // Dismissal carried nothing: the shared copy is still un-touched and
    // un-analyzed, and it stays quiet on a re-scan (the persisted decision).
    let tl = recording_timeline(&conn, &shared_abs).unwrap().unwrap();
    assert!(tl.rallies.is_empty());
    assert_eq!(tl.segment_state, "unknown");
    scan_library(&mut conn).unwrap();
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
        .query_row(
            "SELECT id FROM sessions WHERE capture_day = ?1",
            [day],
            |r| r.get(0),
        )
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
        vec![(1500, "good".to_string()), (3200, "mistake".to_string()),]
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
        vec![(2000, "good".to_string()), (2000, "mistake".to_string()),]
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
    assert!(recording_annotations(&conn, "/missing.mp4")
        .unwrap()
        .is_empty());
}

/// Deleting a recording cascades to its annotations (they key off the
/// recording, so they travel/vanish with it).
#[test]
fn deleting_a_recording_cascades_to_its_annotations() {
    let (conn, id) = db_with_rallies("/r.mp4", &[(1000, 5000)]);
    add_annotation(&conn, "/r.mp4", 2000, "good").unwrap();
    conn.execute("DELETE FROM recordings WHERE id = ?1", [id])
        .unwrap();
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
    let id = add_annotation(&conn, "/r.mp4", 2000, "bad")
        .unwrap()
        .unwrap();
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
    let a = add_annotation(&conn, "/r.mp4", 2000, "good")
        .unwrap()
        .unwrap();
    let b = add_annotation(&conn, "/r.mp4", 3000, "bad")
        .unwrap()
        .unwrap();

    assert!(!update_annotation(&conn, "/missing.mp4", a, "good", None, None).unwrap());
    assert!(!delete_annotation(&conn, "/missing.mp4", a).unwrap());
    assert!(!delete_annotation(&conn, "/r.mp4", 9999).unwrap());
    assert_eq!(verdicts(&conn, "/r.mp4").len(), 2);

    assert!(delete_annotation(&conn, "/r.mp4", a).unwrap());
    assert_eq!(verdicts(&conn, "/r.mp4"), vec![(3000, "bad".to_string())]);
    assert_eq!(recording_annotations(&conn, "/r.mp4").unwrap()[0].id, b);
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
    assert!(aspect_vocabulary(&conn)
        .unwrap()
        .contains(&"serve-toss".to_string()));

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
    assert!(
        recording_id(&conn, &absolute(&lib.0.to_string_lossy(), "a.mp4"))
            .unwrap()
            .is_none()
    );
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
    assert!(recording_timeline(&conn, &a_abs)
        .unwrap()
        .unwrap()
        .rallies
        .is_empty());
    assert_eq!(recording_annotations(&conn, &a_abs).unwrap().len(), 1);
    // b.mp4 took the bundle: it had no rallies in the bundle → its draft is gone.
    assert!(recording_timeline(&conn, &b_abs)
        .unwrap()
        .unwrap()
        .rallies
        .is_empty());

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
    assert!(receive_session_bundle(
        &mut conn,
        r#"{"format":"nope","version":1,"capture_day":"","sharer_label":"","recordings":[]}"#
    )
    .is_err());
    let newer = format!(
        r#"{{"format":"{BUNDLE_FORMAT}","version":{},"capture_day":"","sharer_label":"","recordings":[]}}"#,
        BUNDLE_VERSION + 1
    );
    assert!(receive_session_bundle(&mut conn, &newer).is_err());
}

// --- Bundle discovery (issue #67) -------------------------------------

/// Write `sharer`'s bundle for `a.mp4`+`b.mp4` (both real files present) into
/// the shared root, returning its file name. Mirrors what `share_session_bundle`
/// drops in — the sharer's own device wrote it, we discover it.
fn drop_bundle(lib: &TempLib, sharer: &str, day: &str) -> String {
    let bundle = SessionBundle {
        format: BUNDLE_FORMAT.to_string(),
        version: BUNDLE_VERSION,
        capture_day: day.to_string(),
        sharer_label: sharer.to_string(),
        recordings: vec![
            BundleRecording {
                path: "a.mp4".into(),
                quick_hash: quick_hash(&lib.0.join("a.mp4"), 4).unwrap(),
                file_size: 4,
                capture_day: day.to_string(),
                duration_ms: Some(60000),
                waveform: vec![],
                rallies: vec![BundleRally {
                    start_ms: 0,
                    end_ms: 5000,
                    confidence: 0.5,
                    flagged: true,
                }],
                annotations: vec![],
            },
            BundleRecording {
                path: "b.mp4".into(),
                quick_hash: quick_hash(&lib.0.join("b.mp4"), 4).unwrap(),
                file_size: 4,
                capture_day: day.to_string(),
                duration_ms: Some(60000),
                waveform: vec![],
                rallies: vec![],
                annotations: vec![],
            },
        ],
    };
    let file = bundle_file_name(day, sharer);
    std::fs::write(
        lib.0.join(&file),
        serde_json::to_vec_pretty(&bundle).unwrap(),
    )
    .unwrap();
    file
}

/// A dropped-in bundle is discovered and offered by session + sharer; two
/// sharers' bundles for the same day are two independent offers; the user's
/// own bundle is never offered back (AC1, AC4, AC5).
#[test]
fn discovers_foreign_bundles_and_never_own() {
    let lib = TempLib::new();
    lib.write("a.mp4", b"aaaa");
    lib.write("b.mp4", b"bbbb");
    drop_bundle(&lib, "alice", "2026-01-01");
    drop_bundle(&lib, "bob", "2026-01-01");
    // This device shares under "carol", so carol's bundle is not offered back.
    drop_bundle(&lib, "carol", "2026-01-01");
    let mut conn = open(Path::new(":memory:")).unwrap();
    designate_library(&mut conn, "shared", &lib.0, "network").unwrap();
    meta_set(&conn, "sharer_label", "carol").unwrap();

    let offers = discover_bundles(&conn);
    assert_eq!(offers.len(), 2); // alice + bob, not carol
    assert_eq!(offers[0].sharer_label, "alice");
    assert_eq!(offers[1].sharer_label, "bob");
    assert!(offers.iter().all(|o| o.capture_day == "2026-01-01"));
    assert!(offers.iter().all(|o| !o.is_update));
    assert!(offers.iter().all(|o| o.bundle_path.ends_with(".vbundle")));
}

/// Declining a bundle stops it being offered until it changes; a re-shared
/// (rewritten) bundle is offered again as an update (AC3).
#[test]
fn declined_bundle_stays_quiet_until_reshared() {
    let lib = TempLib::new();
    lib.write("a.mp4", b"aaaa");
    lib.write("b.mp4", b"bbbb");
    let file = drop_bundle(&lib, "alice", "2026-01-01");
    let mut conn = open(Path::new(":memory:")).unwrap();
    designate_library(&mut conn, "shared", &lib.0, "network").unwrap();

    let offers = discover_bundles(&conn);
    assert_eq!(offers.len(), 1);
    decline_bundle(&conn, &offers[0].bundle_path).unwrap();
    // Declined + unchanged → not re-offered.
    assert!(discover_bundles(&conn).is_empty());

    // Re-share: rewrite the file so its signature changes. Sleep a hair so the
    // mtime actually advances even on coarse-resolution filesystems, and pad
    // the bytes so the size differs too.
    std::thread::sleep(std::time::Duration::from_millis(10));
    std::fs::write(lib.0.join(&file), b"{\"changed\": true}").unwrap();
    drop_bundle(&lib, "alice", "2026-01-01"); // valid contents again, new bytes
    let reoffered = discover_bundles(&conn);
    assert_eq!(reoffered.len(), 1);
    assert!(reoffered[0].is_update); // AC3: offered again as an update
}

/// A pending offer holds its recordings out of the analysis queue so accepting
/// it skips their probe/segmentation/staging; declining releases them back
/// (AC2). Verified through both the one-at-a-time queue and the staged batch.
#[test]
fn pending_offer_holds_recordings_out_of_analysis() {
    let lib = TempLib::new();
    lib.write("a.mp4", b"aaaa");
    lib.write("b.mp4", b"bbbb");
    drop_bundle(&lib, "alice", "2026-01-01"); // covers a.mp4 + b.mp4
    let mut conn = open(Path::new(":memory:")).unwrap();
    designate_library(&mut conn, "shared", &lib.0, "network").unwrap();
    assert_eq!(scan_library(&mut conn).unwrap().registered, 2);
    // Settle the capture-date phase (headers-only, never held back) so this
    // test isolates the probe/segment queue the offer actually gates.
    conn.execute(
        "UPDATE recordings SET date_state = 'refined' WHERE library = 'shared'",
        [],
    )
    .unwrap();

    // Both recordings are covered by the pending offer → no probe/segment work,
    // no staging.
    assert!(next_media_work(&conn).unwrap().is_none());
    assert!(pending_analysis(&conn).unwrap().is_empty());

    // Decline → the recordings return to the queue for the app to analyze.
    let offers = discover_bundles(&conn);
    decline_bundle(&conn, &offers[0].bundle_path).unwrap();
    assert!(next_media_work(&conn).unwrap().is_some());
    assert_eq!(pending_analysis(&conn).unwrap().len(), 2);
}

/// A recording covered by a pending bundle offer that *also* has a matching
/// Analysis adopts it immediately and keeps the offer pending (ADR 0013, issue
/// #72): playable at once from the machine draft, accepting the bundle later
/// applies its review over machine-only state silently, and declining leaves the
/// adopted draft intact and triggers no analysis. A covered recording with no
/// Analysis keeps today's held-out behavior.
#[test]
fn pending_offer_coexists_with_analysis_adoption() {
    let lib = TempLib::new();
    let a_abs = lib.write("a.mp4", b"aaaa");
    lib.write("b.mp4", b"bbbb");
    let bundle_file = drop_bundle(&lib, "alice", "2026-01-01"); // covers a.mp4 + b.mp4
    // Only a.mp4 has a published Analysis (a machine draft distinct from the
    // bundle's); b.mp4 is covered but unanalyzed.
    publish_test_analysis(
        &lib,
        &a_abs,
        vec![AnalysisRally {
            start_ms: 1000,
            end_ms: 2000,
            confidence: 0.4,
        }],
    );

    let mut conn = open(Path::new(":memory:")).unwrap();
    designate_library(&mut conn, "shared", &lib.0, "network").unwrap();
    assert_eq!(scan_library(&mut conn).unwrap().registered, 2);
    conn.execute(
        "UPDATE recordings SET date_state = 'refined' WHERE library = 'shared'",
        [],
    )
    .unwrap();

    // AC1: a.mp4 registered + playable from the Analysis before the offer is
    // answered. AC4: b.mp4 (no Analysis) stays held out, unanalyzed.
    let a_tl = recording_timeline(&conn, &a_abs.to_string_lossy())
        .unwrap()
        .unwrap();
    assert_eq!(a_tl.segment_state, "ready");
    assert_eq!(a_tl.duration_ms, Some(42_000));
    assert_eq!(
        a_tl.rallies.iter().map(|r| r.start_ms).collect::<Vec<_>>(),
        vec![1000]
    );
    let b_abs = lib.0.join("b.mp4");
    assert_eq!(
        recording_timeline(&conn, &b_abs.to_string_lossy())
            .unwrap()
            .unwrap()
            .segment_state,
        "unknown"
    );

    // The offer is still pending after adoption (nothing consumed it).
    let offers = discover_bundles(&conn);
    assert_eq!(offers.len(), 1);
    let bundle_path = lib.0.join(&bundle_file);
    let bundle_json = std::fs::read_to_string(&bundle_path).unwrap();

    // AC2: accepting the bundle applies its review silently — adopted state is
    // machine-only, so a.mp4's rally is replaced (flagged, from the bundle) with
    // no keep-mine conflict. b.mp4 is a no-op (the bundle carries no state for it,
    // matching its empty machine-only state), so exactly one recording changes.
    let result = receive_session_bundle(&mut conn, &bundle_json).unwrap();
    assert_eq!(result.applied, 1);
    assert!(result.conflicts.is_empty());
    assert!(result.refused.is_empty());
    let a_tl = recording_timeline(&conn, &a_abs.to_string_lossy())
        .unwrap()
        .unwrap();
    assert_eq!(
        a_tl.rallies.iter().map(|r| r.start_ms).collect::<Vec<_>>(),
        vec![0] // the bundle's rally, not the adopted 1000ms one
    );
    assert!(a_tl.rallies[0].flagged);
}

/// AC3: declining the offer leaves the adopted Analysis intact and starts no
/// analysis — the adopted a.mp4 stays `ready` and out of the media queue.
#[test]
fn declining_after_adoption_keeps_the_draft_and_runs_no_analysis() {
    let lib = TempLib::new();
    let a_abs = lib.write("a.mp4", b"aaaa");
    lib.write("b.mp4", b"bbbb");
    drop_bundle(&lib, "alice", "2026-01-01");
    publish_test_analysis(
        &lib,
        &a_abs,
        vec![AnalysisRally {
            start_ms: 1000,
            end_ms: 2000,
            confidence: 0.4,
        }],
    );
    let mut conn = open(Path::new(":memory:")).unwrap();
    designate_library(&mut conn, "shared", &lib.0, "network").unwrap();
    scan_library(&mut conn).unwrap();
    conn.execute(
        "UPDATE recordings SET date_state = 'refined' WHERE library = 'shared'",
        [],
    )
    .unwrap();

    let offers = discover_bundles(&conn);
    decline_bundle(&conn, &offers[0].bundle_path).unwrap();

    // a.mp4's adopted draft is untouched by the decline.
    let a_tl = recording_timeline(&conn, &a_abs.to_string_lossy())
        .unwrap()
        .unwrap();
    assert_eq!(a_tl.segment_state, "ready");
    assert_eq!(
        a_tl.rallies.iter().map(|r| r.start_ms).collect::<Vec<_>>(),
        vec![1000]
    );
    // Only b.mp4 (never analyzed) returns to the queue; a.mp4 needs no analysis.
    let pending = pending_analysis(&conn).unwrap();
    assert_eq!(pending.len(), 1);
    assert!(pending[0].path.ends_with("b.mp4"));
}

// --- Publishing the Analysis at completion (ADR 0013, issue #69) ---------

/// Read the `.vanalysis` published for `abs` under the shared library `root`, if
/// any. Content-keyed by quick hash + size (ADR 0013), at the *active* segmenter
/// version's file name — where a fresh publish lands (issue #80).
fn read_published_analysis(root: &Path, abs: &Path) -> Option<Analysis> {
    let meta = std::fs::metadata(abs).unwrap();
    let hash = quick_hash(abs, meta.len()).unwrap();
    let name = analysis_file_name(&hash, meta.len() as i64, crate::segment::SEGMENTER_VERSION);
    let path = root.join(".voloph").join("analysis").join(name);
    std::fs::read(path)
        .ok()
        .map(|bytes| serde_json::from_slice(&bytes).unwrap())
}

#[test]
fn segmenting_a_shared_recording_publishes_a_content_keyed_analysis() {
    let lib = TempLib::new();
    let abs = lib.write("game.mp4", b"aaaa");
    let mut conn = open(Path::new(":memory:")).unwrap();
    designate_library(&mut conn, "shared", &lib.0, "network").unwrap();
    assert_eq!(scan_library(&mut conn).unwrap().registered, 1);
    let id = recording_id(&conn, &abs.to_string_lossy()).unwrap().unwrap();

    let rallies = vec![crate::segment::Rally {
        start_ms: 1000,
        end_ms: 5000,
        confidence: 0.4,
    }];
    save_rallies(&mut conn, id, 60000, &rallies, &[0.1, 0.2]).unwrap();
    publish_analysis(&conn, id);

    let a = read_published_analysis(&lib.0, &abs).expect("analysis published");
    assert_eq!(a.format, ANALYSIS_FORMAT);
    assert_eq!(a.version, ANALYSIS_VERSION);
    assert_eq!(a.segmenter_version, crate::segment::SEGMENTER_VERSION);
    let expected_day: String = conn
        .query_row("SELECT capture_day FROM recordings WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(a.capture_day, expected_day);
    assert_eq!(a.duration_ms, Some(60000));
    assert_eq!(a.waveform, vec![0.1, 0.2]);
    assert_eq!(
        a.rallies,
        vec![AnalysisRally {
            start_ms: 1000,
            end_ms: 5000,
            confidence: 0.4
        }]
    );
}

#[test]
fn re_analyze_overwrites_the_published_analysis() {
    let lib = TempLib::new();
    let abs = lib.write("game.mp4", b"aaaa");
    let mut conn = open(Path::new(":memory:")).unwrap();
    designate_library(&mut conn, "shared", &lib.0, "network").unwrap();
    scan_library(&mut conn).unwrap();
    let id = recording_id(&conn, &abs.to_string_lossy()).unwrap().unwrap();

    save_rallies(
        &mut conn,
        id,
        60000,
        &[crate::segment::Rally {
            start_ms: 1000,
            end_ms: 2000,
            confidence: 0.3,
        }],
        &[],
    )
    .unwrap();
    publish_analysis(&conn, id);

    // A fresh analysis with a different timeline republishes over the same file.
    save_rallies(
        &mut conn,
        id,
        70000,
        &[crate::segment::Rally {
            start_ms: 500,
            end_ms: 9000,
            confidence: 0.9,
        }],
        &[],
    )
    .unwrap();
    publish_analysis(&conn, id);

    let a = read_published_analysis(&lib.0, &abs).unwrap();
    assert_eq!(a.duration_ms, Some(70000));
    assert_eq!(
        a.rallies,
        vec![AnalysisRally {
            start_ms: 500,
            end_ms: 9000,
            confidence: 0.9
        }]
    );
}

#[test]
fn segmenting_a_local_recording_publishes_nothing() {
    let lib = TempLib::new();
    let abs = lib.write("game.mp4", b"aaaa");
    let mut conn = open(Path::new(":memory:")).unwrap();
    designate_library(&mut conn, "local", &lib.0, "local").unwrap();
    scan_library(&mut conn).unwrap();
    let id = recording_id(&conn, &abs.to_string_lossy()).unwrap().unwrap();

    save_rallies(
        &mut conn,
        id,
        60000,
        &[crate::segment::Rally {
            start_ms: 1000,
            end_ms: 5000,
            confidence: 0.4,
        }],
        &[],
    )
    .unwrap();
    publish_analysis(&conn, id);

    // No `.voloph/analysis/` folder for a local library — no one can reach it.
    assert!(!lib.0.join(".voloph").join("analysis").exists());
    assert!(read_published_analysis(&lib.0, &abs).is_none());
}

// --- Publication invariant: backfill/retry on scan (ADR 0013, issue #71) --

/// Set up a shared library with one `ready`, machine-analyzed recording whose
/// `.vanalysis` has been removed — simulating pre-feature compute or a failed
/// publish. Returns the connection, the recording's abs path, and its id.
fn shared_analyzed_without_file() -> (Connection, TempLib, std::path::PathBuf, i64) {
    let lib = TempLib::new();
    let abs = lib.write("game.mp4", b"aaaa");
    let mut conn = open(Path::new(":memory:")).unwrap();
    designate_library(&mut conn, "shared", &lib.0, "network").unwrap();
    scan_library(&mut conn).unwrap();
    let id = recording_id(&conn, &abs.to_string_lossy()).unwrap().unwrap();
    save_rallies(
        &mut conn,
        id,
        60000,
        &[crate::segment::Rally {
            start_ms: 1000,
            end_ms: 5000,
            confidence: 0.4,
        }],
        &[0.1, 0.2],
    )
    .unwrap();
    // Drop whatever save_rallies/completion published, so the file is absent.
    std::fs::remove_dir_all(lib.0.join(".voloph").join("analysis")).ok();
    (conn, lib, abs, id)
}

#[test]
fn scan_backfills_missing_analysis_for_a_pristine_recording() {
    let (mut conn, lib, abs, _id) = shared_analyzed_without_file();
    assert!(read_published_analysis(&lib.0, &abs).is_none());

    scan_library(&mut conn).unwrap();

    let a = read_published_analysis(&lib.0, &abs).expect("backfilled on scan");
    assert_eq!(a.duration_ms, Some(60000));
    assert_eq!(
        a.rallies,
        vec![AnalysisRally {
            start_ms: 1000,
            end_ms: 5000,
            confidence: 0.4
        }]
    );
}

#[test]
fn scan_never_publishes_a_hand_touched_recording() {
    let (mut conn, lib, abs, id) = shared_analyzed_without_file();
    // Hand-correct a rally (confidence bumped to CORRECTED) — now hand-touched.
    conn.execute(
        "UPDATE rallies SET confidence = 1.0 WHERE recording_id = ?1",
        [id],
    )
    .unwrap();

    scan_library(&mut conn).unwrap();

    assert!(read_published_analysis(&lib.0, &abs).is_none());
}

#[test]
fn scan_replaces_an_unreadable_file() {
    let (mut conn, lib, abs, _id) = shared_analyzed_without_file();
    let dir = lib.0.join(".voloph").join("analysis");
    let meta = std::fs::metadata(&abs).unwrap();
    let hash = quick_hash(&abs, meta.len()).unwrap();
    let name = format!("{hash}_{}.vanalysis", meta.len());
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(&name), b"not json").unwrap();

    scan_library(&mut conn).unwrap();

    // The garbage was treated as absent and replaced with a readable Analysis.
    let a = read_published_analysis(&lib.0, &abs).expect("unreadable replaced");
    assert_eq!(a.duration_ms, Some(60000));
}

#[test]
fn scan_leaves_a_valid_existing_file_alone() {
    let (mut conn, lib, abs, _id) = shared_analyzed_without_file();
    // A valid file already on disk, carrying a distinct duration the DB does not.
    publish_test_analysis(&lib, &abs, vec![]);
    let before = read_published_analysis(&lib.0, &abs).unwrap();
    assert_eq!(before.duration_ms, Some(42_000)); // publish_test_analysis's marker

    scan_library(&mut conn).unwrap();

    // Untouched — not overwritten with the DB's 60000-ms analysis.
    let after = read_published_analysis(&lib.0, &abs).unwrap();
    assert_eq!(after.duration_ms, Some(42_000));
}

#[test]
fn scan_publishes_a_pristine_local_recording_copied_into_shared() {
    let local = TempLib::new();
    let shared = TempLib::new();
    let l_abs = local.write("game.mp4", b"aaaa");
    let mut conn = open(Path::new(":memory:")).unwrap();

    // Analyze the recording in the local library — pristine, machine `ready`.
    designate_library(&mut conn, "local", &local.0, "local").unwrap();
    scan_library(&mut conn).unwrap();
    let l_id = recording_id(&conn, &l_abs.to_string_lossy()).unwrap().unwrap();
    save_rallies(
        &mut conn,
        l_id,
        60000,
        &[crate::segment::Rally {
            start_ms: 1000,
            end_ms: 5000,
            confidence: 0.4,
        }],
        &[0.1, 0.2],
    )
    .unwrap();

    // The identical bytes are copied into the shared library and scanned.
    let s_abs = shared.write("copy.mp4", b"aaaa");
    designate_library(&mut conn, "shared", &shared.0, "network").unwrap();
    scan_library(&mut conn).unwrap();

    // The content-keyed Analysis is published from the local original — no
    // re-analysis. Keyed by hash+size, it is readable at the shared copy's key.
    let a = read_published_analysis(&shared.0, &s_abs).expect("published from local copy");
    assert_eq!(a.duration_ms, Some(60000));
    assert_eq!(
        a.rallies,
        vec![AnalysisRally {
            start_ms: 1000,
            end_ms: 5000,
            confidence: 0.4
        }]
    );
}

#[test]
fn scan_does_not_publish_a_hand_touched_local_original() {
    let local = TempLib::new();
    let shared = TempLib::new();
    let l_abs = local.write("game.mp4", b"aaaa");
    let mut conn = open(Path::new(":memory:")).unwrap();
    designate_library(&mut conn, "local", &local.0, "local").unwrap();
    scan_library(&mut conn).unwrap();
    let l_id = recording_id(&conn, &l_abs.to_string_lossy()).unwrap().unwrap();
    save_rallies(
        &mut conn,
        l_id,
        60000,
        &[crate::segment::Rally {
            start_ms: 1000,
            end_ms: 5000,
            confidence: 0.4,
        }],
        &[],
    )
    .unwrap();
    // Hand-correct it — the pristine draft no longer exists (ADR 0013).
    conn.execute(
        "UPDATE rallies SET confidence = 1.0 WHERE recording_id = ?1",
        [l_id],
    )
    .unwrap();

    let s_abs = shared.write("copy.mp4", b"aaaa");
    designate_library(&mut conn, "shared", &shared.0, "network").unwrap();
    scan_library(&mut conn).unwrap();

    // Nothing published — that path stays with the carry-over offer.
    assert!(read_published_analysis(&shared.0, &s_abs).is_none());
}

// --- Adopting a published Analysis on scan (ADR 0013, issue #70) ---------

/// Publish an Analysis into the shared library for the file at `abs`, keyed by its
/// quick hash + size, carrying the given draft. Simulates another user having
/// already analyzed these exact bytes.
fn publish_test_analysis(lib: &TempLib, abs: &Path, rallies: Vec<AnalysisRally>) {
    let meta = std::fs::metadata(abs).unwrap();
    let hash = quick_hash(abs, meta.len()).unwrap();
    let analysis = Analysis {
        format: ANALYSIS_FORMAT.to_string(),
        version: ANALYSIS_VERSION,
        segmenter_version: crate::segment::SEGMENTER_VERSION,
        capture_day: "2026-01-01".to_string(),
        duration_ms: Some(42_000),
        waveform: vec![0.3, 0.7],
        rallies,
    };
    let dir = lib.0.join(".voloph").join("analysis");
    std::fs::create_dir_all(&dir).unwrap();
    // At the active version's file name, mirroring where a real publish lands
    // (and where `read_published_analysis` looks).
    let name = analysis_file_name(&hash, meta.len() as i64, crate::segment::SEGMENTER_VERSION);
    std::fs::write(dir.join(name), serde_json::to_vec(&analysis).unwrap()).unwrap();
}

/// A fresh device scanning a shared library with a published Analysis adopts it:
/// the recording arrives probed + segmented with the carried draft, so nothing is
/// left in the probe/segment queue for it.
#[test]
fn fresh_scan_adopts_a_published_analysis() {
    let lib = TempLib::new();
    let abs = lib.write("game.mp4", b"aaaa");
    publish_test_analysis(
        &lib,
        &abs,
        vec![AnalysisRally {
            start_ms: 1000,
            end_ms: 5000,
            confidence: 0.4,
        }],
    );

    let mut conn = open(Path::new(":memory:")).unwrap();
    designate_library(&mut conn, "shared", &lib.0, "network").unwrap();
    assert_eq!(scan_library(&mut conn).unwrap().registered, 1);

    let tl = recording_timeline(&conn, &abs.to_string_lossy())
        .unwrap()
        .unwrap();
    assert_eq!(tl.segment_state, "ready");
    assert_eq!(tl.duration_ms, Some(42_000));
    assert_eq!(tl.waveform, vec![0.3, 0.7]);
    assert_eq!(
        tl.rallies
            .iter()
            .map(|r| (r.start_ms, r.end_ms))
            .collect::<Vec<_>>(),
        vec![(1000, 5000)]
    );

    // No staging/probe/segmentation work remains — the whole pipeline was skipped.
    conn.execute(
        "UPDATE recordings SET date_state = 'refined' WHERE library = 'shared'",
        [],
    )
    .unwrap();
    assert!(next_media_work(&conn).unwrap().is_none());
    assert!(pending_analysis(&conn).unwrap().is_empty());
}

/// A known-but-unanalyzed recording (registered on an earlier scan, still queued)
/// adopts an Analysis that appears later and leaves the analysis queue.
#[test]
fn known_but_unanalyzed_recording_adopts_on_rescan() {
    let lib = TempLib::new();
    let abs = lib.write("game.mp4", b"aaaa");
    let mut conn = open(Path::new(":memory:")).unwrap();
    designate_library(&mut conn, "shared", &lib.0, "network").unwrap();
    scan_library(&mut conn).unwrap();
    conn.execute(
        "UPDATE recordings SET date_state = 'refined', probe_state = 'ready' WHERE library = 'shared'",
        [],
    )
    .unwrap();
    // Queued for segmentation before any Analysis exists.
    assert!(matches!(
        next_media_work(&conn).unwrap(),
        Some(MediaWork::Segment(..))
    ));

    // Another user publishes; a re-scan adopts it and the queue drains.
    publish_test_analysis(
        &lib,
        &abs,
        vec![AnalysisRally {
            start_ms: 0,
            end_ms: 2000,
            confidence: 0.5,
        }],
    );
    scan_library(&mut conn).unwrap();
    assert_eq!(
        recording_timeline(&conn, &abs.to_string_lossy())
            .unwrap()
            .unwrap()
            .segment_state,
        "ready"
    );
    assert!(next_media_work(&conn).unwrap().is_none());
}

/// An already-analyzed recording is never churned, even when a differing Analysis
/// is published — the local draft stands (ADR 0013).
#[test]
fn already_analyzed_recording_is_not_churned() {
    let lib = TempLib::new();
    let abs = lib.write("game.mp4", b"aaaa");
    let mut conn = open(Path::new(":memory:")).unwrap();
    designate_library(&mut conn, "shared", &lib.0, "network").unwrap();
    scan_library(&mut conn).unwrap();
    let id = recording_id(&conn, &abs.to_string_lossy()).unwrap().unwrap();
    // Local segmentation lands first.
    save_rallies(
        &mut conn,
        id,
        99_000,
        &[crate::segment::Rally {
            start_ms: 100,
            end_ms: 200,
            confidence: 0.6,
        }],
        &[0.9],
    )
    .unwrap();

    // A differing published Analysis appears; a re-scan must not adopt over it.
    publish_test_analysis(
        &lib,
        &abs,
        vec![AnalysisRally {
            start_ms: 5000,
            end_ms: 9000,
            confidence: 0.1,
        }],
    );
    scan_library(&mut conn).unwrap();

    let tl = recording_timeline(&conn, &abs.to_string_lossy())
        .unwrap()
        .unwrap();
    assert_eq!(tl.duration_ms, Some(99_000));
    assert_eq!(
        tl.rallies
            .iter()
            .map(|r| (r.start_ms, r.end_ms))
            .collect::<Vec<_>>(),
        vec![(100, 200)]
    );
}

/// A corrupt or wrong-version `.vanalysis` is skipped silently — the recording is
/// left unanalyzed for the normal pipeline (ADR 0013's silent-failure inversion).
#[test]
fn corrupt_or_wrong_version_analysis_is_ignored() {
    let lib = TempLib::new();
    let abs = lib.write("game.mp4", b"aaaa");
    let meta = std::fs::metadata(&abs).unwrap();
    let hash = quick_hash(&abs, meta.len()).unwrap();
    let dir = lib.0.join(".voloph").join("analysis");
    std::fs::create_dir_all(&dir).unwrap();
    let name = format!("{hash}_{}.vanalysis", meta.len());
    // A future-version envelope (otherwise well-formed) must not be adopted.
    let future = serde_json::json!({
        "format": ANALYSIS_FORMAT,
        "version": ANALYSIS_VERSION + 1,
        "segmenter_version": 1,
        "capture_day": "2026-01-01",
        "duration_ms": 1000,
        "waveform": [0.1],
        "rallies": [],
    });
    std::fs::write(dir.join(&name), serde_json::to_vec(&future).unwrap()).unwrap();

    let mut conn = open(Path::new(":memory:")).unwrap();
    designate_library(&mut conn, "shared", &lib.0, "network").unwrap();
    scan_library(&mut conn).unwrap();
    assert_eq!(
        recording_timeline(&conn, &abs.to_string_lossy())
            .unwrap()
            .unwrap()
            .segment_state,
        "unknown"
    );

    // Garbage bytes are likewise ignored.
    std::fs::write(dir.join(&name), b"not json at all").unwrap();
    scan_library(&mut conn).unwrap();
    assert_eq!(
        recording_timeline(&conn, &abs.to_string_lossy())
            .unwrap()
            .unwrap()
            .segment_state,
        "unknown"
    );
}

// --- Version-aware staleness (ADR 0013/0015, issue #80) ------------------
//
// `SEGMENTER_VERSION` is 1 while this lands, so a real bump cannot be forced from
// a test. Staleness is instead exercised the same way a bump would present it: a
// `ready` recording carrying a *lower* stored `segmenter_version` (0) is exactly
// what an untouched recording looks like once the active segmenter outranks it.

/// Publish an Analysis into the shared library for `abs`, stamped with an explicit
/// segmenter version and named by that version (v1 → bare content key, later →
/// `_s{N}`), so several versions can sit alongside one another. Returns the
/// filename written, so a test can assert older files stay on disk.
fn publish_versioned_analysis(
    lib: &TempLib,
    abs: &Path,
    segmenter_version: u32,
    rallies: Vec<AnalysisRally>,
) -> String {
    let meta = std::fs::metadata(abs).unwrap();
    let hash = quick_hash(abs, meta.len()).unwrap();
    let analysis = Analysis {
        format: ANALYSIS_FORMAT.to_string(),
        version: ANALYSIS_VERSION,
        segmenter_version,
        capture_day: "2026-01-01".to_string(),
        duration_ms: Some(1000 * segmenter_version as i64),
        waveform: vec![],
        rallies,
    };
    let name = analysis_file_name(&hash, meta.len() as i64, segmenter_version);
    let dir = lib.0.join(".voloph").join("analysis");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(&name), serde_json::to_vec(&analysis).unwrap()).unwrap();
    name
}

/// A fresh machine analysis stamps the active segmenter version onto the row, so
/// the row records which segmenter produced its draft.
#[test]
fn save_rallies_stamps_the_active_segmenter_version() {
    let (mut conn, id) = db_with_rallies("/x/game.mp4", &[]);
    save_rallies(
        &mut conn,
        id,
        60000,
        &[crate::segment::Rally {
            start_ms: 0,
            end_ms: 100,
            confidence: 0.5,
        }],
        &[],
    )
    .unwrap();
    let v: Option<i64> = conn
        .query_row(
            "SELECT segmenter_version FROM recordings WHERE id = ?1",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(v, Some(crate::segment::SEGMENTER_VERSION as i64));
}

/// An untouched recording whose draft an outranked segmenter produced (a lower
/// stored version) is silently re-queued on scan: its draft is dropped and it
/// returns to `unknown` so the background worker re-analyzes it.
#[test]
fn scan_resegments_an_untouched_stale_recording() {
    let lib = TempLib::new();
    let abs = lib.write("game.mp4", b"aaaa");
    let mut conn = open(Path::new(":memory:")).unwrap();
    designate_library(&mut conn, "local", &lib.0, "local").unwrap();
    scan_library(&mut conn).unwrap();
    let id = recording_id(&conn, &abs.to_string_lossy()).unwrap().unwrap();
    save_rallies(
        &mut conn,
        id,
        60000,
        &[crate::segment::Rally {
            start_ms: 0,
            end_ms: 100,
            confidence: 0.5,
        }],
        &[0.1],
    )
    .unwrap();
    // A ready-segmented recording has been probed; age it so the active segmenter
    // (v1) now outranks this draft (stamped v0).
    conn.execute(
        "UPDATE recordings SET segmenter_version = 0, probe_state = 'ready' WHERE id = ?1",
        [id],
    )
    .unwrap();

    scan_library(&mut conn).unwrap();

    let tl = recording_timeline(&conn, &abs.to_string_lossy())
        .unwrap()
        .unwrap();
    assert_eq!(tl.segment_state, "unknown");
    assert!(tl.rallies.is_empty());
    // Re-queued for the worker: probe stays ready (bytes unchanged), segment pending.
    conn.execute("UPDATE recordings SET date_state = 'refined'", [])
        .unwrap();
    assert!(matches!(
        next_media_work(&conn).unwrap(),
        Some(MediaWork::Segment(..))
    ));
}

/// A hand-touched recording is never re-analyzed by a version bump — corrections
/// are the most authoritative data in the system (ADR 0015).
#[test]
fn scan_never_resegments_a_hand_touched_stale_recording() {
    let lib = TempLib::new();
    let abs = lib.write("game.mp4", b"aaaa");
    let mut conn = open(Path::new(":memory:")).unwrap();
    designate_library(&mut conn, "local", &lib.0, "local").unwrap();
    scan_library(&mut conn).unwrap();
    let id = recording_id(&conn, &abs.to_string_lossy()).unwrap().unwrap();
    save_rallies(
        &mut conn,
        id,
        60000,
        &[crate::segment::Rally {
            start_ms: 0,
            end_ms: 100,
            confidence: 0.5,
        }],
        &[0.1],
    )
    .unwrap();
    // Hand-correct a rally (confidence bumped to CORRECTED) and age it.
    conn.execute(
        "UPDATE rallies SET confidence = 1.0 WHERE recording_id = ?1",
        [id],
    )
    .unwrap();
    conn.execute(
        "UPDATE recordings SET segmenter_version = 0 WHERE id = ?1",
        [id],
    )
    .unwrap();

    scan_library(&mut conn).unwrap();

    // Draft stands, nothing dropped, still `ready` — never re-analyzed, never asked.
    let tl = recording_timeline(&conn, &abs.to_string_lossy())
        .unwrap()
        .unwrap();
    assert_eq!(tl.segment_state, "ready");
    assert_eq!(
        tl.rallies.iter().map(|r| r.start_ms).collect::<Vec<_>>(),
        vec![0]
    );
}

/// A draft stamped with the current version (or none) is never stale — the feature
/// lands inert, doing nothing until a real `SEGMENTER_VERSION` bump.
#[test]
fn a_current_version_recording_is_left_alone() {
    let lib = TempLib::new();
    let abs = lib.write("game.mp4", b"aaaa");
    let mut conn = open(Path::new(":memory:")).unwrap();
    designate_library(&mut conn, "local", &lib.0, "local").unwrap();
    scan_library(&mut conn).unwrap();
    let id = recording_id(&conn, &abs.to_string_lossy()).unwrap().unwrap();
    save_rallies(
        &mut conn,
        id,
        60000,
        &[crate::segment::Rally {
            start_ms: 0,
            end_ms: 100,
            confidence: 0.5,
        }],
        &[0.1],
    )
    .unwrap();

    scan_library(&mut conn).unwrap();

    assert_eq!(
        recording_timeline(&conn, &abs.to_string_lossy())
            .unwrap()
            .unwrap()
            .segment_state,
        "ready"
    );
}

/// Adoption prefers the highest segmenter version when several Analyses exist for
/// the same content key, and every older file stays on disk (nothing is deleted
/// from shared storage).
#[test]
fn scan_adopts_the_highest_version_and_keeps_older_files() {
    let lib = TempLib::new();
    let abs = lib.write("game.mp4", b"aaaa");
    // Two Analyses for the same bytes: an older v1 and a newer v2, side by side.
    let v1_name = publish_versioned_analysis(
        &lib,
        &abs,
        1,
        vec![AnalysisRally {
            start_ms: 0,
            end_ms: 1000,
            confidence: 0.2,
        }],
    );
    let v2_name = publish_versioned_analysis(
        &lib,
        &abs,
        2,
        vec![AnalysisRally {
            start_ms: 5000,
            end_ms: 9000,
            confidence: 0.9,
        }],
    );
    assert_ne!(v1_name, v2_name, "the two versions have distinct filenames");

    let mut conn = open(Path::new(":memory:")).unwrap();
    designate_library(&mut conn, "shared", &lib.0, "network").unwrap();
    scan_library(&mut conn).unwrap();

    // The v2 draft (highest version) was adopted, not v1.
    let tl = recording_timeline(&conn, &abs.to_string_lossy())
        .unwrap()
        .unwrap();
    assert_eq!(tl.duration_ms, Some(2000)); // publish_versioned_analysis marks v2 → 2000
    assert_eq!(
        tl.rallies
            .iter()
            .map(|r| (r.start_ms, r.end_ms))
            .collect::<Vec<_>>(),
        vec![(5000, 9000)]
    );
    let id = recording_id(&conn, &abs.to_string_lossy()).unwrap().unwrap();
    let v: Option<i64> = conn
        .query_row(
            "SELECT segmenter_version FROM recordings WHERE id = ?1",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(v, Some(2));

    // Both files remain on disk — nothing was deleted from shared storage.
    let dir = lib.0.join(".voloph").join("analysis");
    assert!(dir.join(&v1_name).exists(), "older Analysis was deleted");
    assert!(dir.join(&v2_name).exists());
}

/// A newer-version Analysis is published *alongside* a stale one — a bump backfills
/// its own versioned file without touching the older sibling. (Simulated by
/// stamping the row with a higher version than the file already on disk.)
#[test]
fn publish_writes_alongside_without_deleting_the_older_version() {
    let lib = TempLib::new();
    let abs = lib.write("game.mp4", b"aaaa");
    let mut conn = open(Path::new(":memory:")).unwrap();
    designate_library(&mut conn, "shared", &lib.0, "network").unwrap();
    scan_library(&mut conn).unwrap();
    let id = recording_id(&conn, &abs.to_string_lossy()).unwrap().unwrap();
    save_rallies(
        &mut conn,
        id,
        60000,
        &[crate::segment::Rally {
            start_ms: 0,
            end_ms: 100,
            confidence: 0.5,
        }],
        &[],
    )
    .unwrap();
    // Stamp the row as a v1 draft (a stale, pre-bump publish) and publish it.
    conn.execute(
        "UPDATE recordings SET segmenter_version = 1 WHERE id = ?1",
        [id],
    )
    .unwrap();
    publish_analysis(&conn, id);
    let meta = std::fs::metadata(&abs).unwrap();
    let hash = quick_hash(&abs, meta.len()).unwrap();
    let v1_name = analysis_file_name(&hash, meta.len() as i64, 1);
    let dir = lib.0.join(".voloph").join("analysis");
    assert!(dir.join(&v1_name).exists());

    // The row's draft is now a v2 draft; publishing writes a `_s2` file alongside.
    conn.execute(
        "UPDATE recordings SET segmenter_version = 2 WHERE id = ?1",
        [id],
    )
    .unwrap();
    publish_analysis(&conn, id);

    let v2_name = analysis_file_name(&hash, meta.len() as i64, 2);
    assert!(dir.join(&v2_name).exists(), "v2 published alongside");
    assert!(dir.join(&v1_name).exists(), "v1 left in place");
}

/// On an upgraded DB, the migration stamps every already-segmented row with the
/// current version while leaving unsegmented rows unstamped, so pre-feature drafts
/// are treated as current (not spuriously stale) and the feature stays inert.
#[test]
fn migration_backfills_ready_recordings_to_the_current_version() {
    // Build a pre-#80 `recordings` table (no `segmenter_version` column) with a
    // `ready` and an `unknown` row directly in a file DB, then run `open` to drive
    // the real ADD COLUMN + backfill migration against it.
    let db = std::env::temp_dir().join(format!(
        "voloph-mig-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE sessions (id INTEGER PRIMARY KEY, capture_day TEXT NOT NULL,
             library TEXT NOT NULL DEFAULT 'local', UNIQUE(library, capture_day));
         CREATE TABLE recordings (
            id INTEGER PRIMARY KEY,
            session_id INTEGER NOT NULL REFERENCES sessions(id),
            path TEXT NOT NULL,
            library TEXT NOT NULL DEFAULT 'local',
            file_size INTEGER NOT NULL,
            quick_hash TEXT NOT NULL,
            capture_day TEXT NOT NULL,
            probe_state TEXT NOT NULL DEFAULT 'unknown',
            segment_state TEXT NOT NULL DEFAULT 'unknown',
            date_state TEXT NOT NULL DEFAULT 'unknown',
            duration_ms INTEGER,
            waveform TEXT,
            UNIQUE(library, path));
         INSERT INTO sessions (id, capture_day) VALUES (1, '2026-01-01');
         INSERT INTO recordings (session_id, path, file_size, quick_hash, capture_day, segment_state)
            VALUES (1, 'a.mp4', 1, 'h', '2026-01-01', 'ready'),
                   (1, 'b.mp4', 2, 'h2', '2026-01-01', 'unknown');",
    )
    .unwrap();
    // No `segmenter_version` column exists yet — this is the pre-feature shape.
    assert!(conn
        .prepare("SELECT segmenter_version FROM recordings LIMIT 0")
        .is_err());
    drop(conn);

    // Re-open through `open` so the migration (ADD COLUMN + backfill) runs.
    let conn = open(Path::new(&db)).unwrap();
    let ready_v: Option<i64> = conn
        .query_row(
            "SELECT segmenter_version FROM recordings WHERE path = 'a.mp4'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let unknown_v: Option<i64> = conn
        .query_row(
            "SELECT segmenter_version FROM recordings WHERE path = 'b.mp4'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(ready_v, Some(crate::segment::SEGMENTER_VERSION as i64));
    assert_eq!(unknown_v, None);
    std::fs::remove_file(&db).ok();
}

/// A version bump does not disturb bundle receive: a recipient carrying a stale
/// machine-only draft still applies the bundle's hand-corrected state over it, and
/// afterwards that recording is hand-touched, so no later bump can re-analyze it.
#[test]
fn version_bump_does_not_disturb_bundle_receive() {
    let lib = TempLib::new();
    let json = shared_bundle_json(&lib);
    let mut conn = recipient_db(&lib);
    // The recipient has scanned and machine-analyzed a.mp4 to a stale draft (v0).
    scan_library(&mut conn).unwrap();
    let a_abs = absolute(&lib.0.to_string_lossy(), "a.mp4");
    let a_id = recording_id(&conn, &a_abs).unwrap().unwrap();
    save_rallies(
        &mut conn,
        a_id,
        60000,
        &[crate::segment::Rally {
            start_ms: 0,
            end_ms: 100,
            confidence: 0.5,
        }],
        &[],
    )
    .unwrap();
    conn.execute(
        "UPDATE recordings SET segmenter_version = 0 WHERE id = ?1",
        [a_id],
    )
    .unwrap();

    // Receiving applies the bundle's review over the machine-only (if stale) draft,
    // exactly as it does today — the version bump changes nothing here. a.mp4's
    // hand-touched state applies (machine-only is replaced silently); nothing is
    // refused or surfaced as a conflict on account of the stale version.
    let result = receive_session_bundle(&mut conn, &json).unwrap();
    assert!(result.applied >= 1);
    assert!(result.refused.is_empty());
    assert!(result.conflicts.is_empty());

    // The bundle's flagged/annotated state makes the recording hand-touched, so a
    // subsequent scan (with any version bump) never re-analyzes it.
    let a_id = recording_id(&conn, &a_abs).unwrap().unwrap();
    assert!(is_hand_touched(&conn, a_id).unwrap());
    conn.execute(
        "UPDATE recordings SET segmenter_version = 0 WHERE id = ?1",
        [a_id],
    )
    .unwrap();
    scan_library(&mut conn).unwrap();
    assert_eq!(
        recording_timeline(&conn, &a_abs).unwrap().unwrap().segment_state,
        "ready"
    );
}
