//! Registering recordings referenced in place (ADR 0003): the library scan and
//! its relocation/re-home reconciliation (ADR 0011), the nested session listing,
//! and forgetting a recording.

use std::collections::HashMap;
use std::path::Path;

use rusqlite::Connection;
use serde::Serialize;

use super::{absolute, active_kind, capture_day, is_video, library_path_of, recording_id};

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

/// Cheap content fingerprint: file size plus a SHA-256 of the leading bytes.
/// Used to re-locate a file moved outside the app (ADR 0003), not for integrity.
pub(crate) fn quick_hash(path: &Path, file_size: u64) -> std::io::Result<String> {
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

/// Known recordings of `kind` whose relative key no longer resolves under
/// `folder` — the candidates for relocation (matched by hash) or, if unmatched,
/// the unresolved list. Keyed relative-path → id. Absolute keys are unadopted
/// outsiders (ADR 0011), skipped.
fn missing_in_library(
    tx: &rusqlite::Transaction,
    kind: &str,
    folder: &Path,
) -> rusqlite::Result<HashMap<String, i64>> {
    let mut stmt = tx.prepare("SELECT id, path FROM recordings WHERE library = ?1")?;
    let rows: Vec<(i64, String)> = stmt
        .query_map([kind], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows
        .into_iter()
        .filter(|(_, rel)| !Path::new(rel).is_absolute())
        .filter(|(_, rel)| !folder.join(rel).exists())
        .map(|(id, rel)| (rel, id))
        .collect())
}

/// Ids of the *other* libraries' recordings whose own file has gone missing —
/// re-home candidates (ADR 0011). A vanished recording that reappears under this
/// library was moved between libraries; one whose file still exists is a copy,
/// left for the explicit carry-over offer.
fn cross_library_missing(tx: &rusqlite::Transaction, active: &str) -> rusqlite::Result<Vec<i64>> {
    let mut ids = Vec::new();
    let mut stmt = tx.prepare("SELECT kind, path FROM libraries")?;
    let others: Vec<(String, String)> = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);
    for (other_kind, other_folder) in others {
        if other_kind == active {
            continue;
        }
        let other_root = Path::new(&other_folder);
        let mut rstmt = tx.prepare("SELECT id, path FROM recordings WHERE library = ?1")?;
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
    Ok(ids)
}

/// The stored size + quick hash of recording `id`, or a sentinel that matches
/// nothing when the row cannot be read.
fn recording_fingerprint(tx: &rusqlite::Transaction, id: i64) -> (i64, String) {
    tx.query_row(
        "SELECT file_size, quick_hash FROM recordings WHERE id = ?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .unwrap_or((-1, String::new()))
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

    // Known recordings *of the active library* that no longer resolve under it,
    // tracked so whatever stays missing after the walk is a relocation candidate
    // or, if unmatched, unresolved.
    let mut missing = missing_in_library(&tx, &kind, folder)?;
    // Re-home candidates: the *other* libraries' vanished recordings, keyed by id
    // so the walk can match a reappeared file against them.
    let mut cross_missing = cross_library_missing(&tx, &kind)?;

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
            let (o_size, o_hash) = recording_fingerprint(&tx, id);
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
            let (o_size, o_hash) = recording_fingerprint(&tx, id);
            o_size == file_size as i64 && o_hash == hash
        });
        if let Some(&id) = rehomed {
            rehome_recording(&tx, id, &kind, &rel)?;
            cross_missing.retain(|&x| x != id);
            relocated += 1;
            continue;
        }

        register_recording(&tx, &kind, &rel, file_size as i64, &hash, &meta)?;
        registered += 1;
    }

    // Silently adopt any published Analysis (ADR 0013) for recordings this scan
    // left unanalyzed — freshly registered or known-but-queued — so covered files
    // arrive playable with their draft timeline, skipping the whole probe/segment
    // pipeline. A no-op outside the shared library.
    super::adopt_analyses(&tx)?;

    tx.commit()?;

    // Whatever known recordings are still missing after the walk could not be
    // re-linked — report them (as absolute paths) rather than delete them.
    let mut unresolved: Vec<String> = missing.keys().map(|rel| absolute(&library, rel)).collect();
    unresolved.sort();

    Ok(ScanResult {
        registered,
        skipped,
        relocated,
        unresolved,
    })
}

/// Register a fresh video as a recording under `kind`, grouping it into a session
/// of its provisional (mtime-derived) capture day — created if it does not exist.
fn register_recording(
    tx: &rusqlite::Transaction,
    kind: &str,
    rel: &str,
    file_size: i64,
    hash: &str,
    meta: &std::fs::Metadata,
) -> rusqlite::Result<()> {
    let day = capture_day(meta.modified().unwrap_or(std::time::UNIX_EPOCH));
    // Session grouping is per-library (a session never spans libraries).
    tx.execute(
        "INSERT OR IGNORE INTO sessions (capture_day, library) VALUES (?1, ?2)",
        rusqlite::params![&day, kind],
    )?;
    let session_id: i64 = tx.query_row(
        "SELECT id FROM sessions WHERE capture_day = ?1 AND library = ?2",
        rusqlite::params![&day, kind],
        |row| row.get(0),
    )?;
    tx.execute(
        "INSERT INTO recordings (session_id, path, library, file_size, quick_hash, capture_day)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![session_id, rel, kind, file_size, hash, day],
    )?;
    Ok(())
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

/// Forget a recording and all its review state (the amber "not found" list,
/// ADR 0011): a recording whose file has vanished from the library is retained
/// with its review so it re-links when the file returns, but the user can choose
/// to discard it instead. Deletes the recording row — rallies and annotations
/// cascade off it — and garbage-collects the session it emptied, mirroring the
/// re-home GC. Scoped to the active library via `path`. Returns `false` when the
/// recording is not found.
pub fn delete_recording(conn: &Connection, path: &str) -> rusqlite::Result<bool> {
    let Some(rid) = recording_id(conn, path)? else {
        return Ok(false);
    };
    let session: i64 = conn.query_row(
        "SELECT session_id FROM recordings WHERE id = ?1",
        [rid],
        |r| r.get(0),
    )?;
    conn.execute("DELETE FROM recordings WHERE id = ?1", [rid])?;
    conn.execute(
        "DELETE FROM sessions WHERE id = ?1
         AND NOT EXISTS (SELECT 1 FROM recordings WHERE session_id = ?1)",
        [session],
    )?;
    Ok(true)
}
