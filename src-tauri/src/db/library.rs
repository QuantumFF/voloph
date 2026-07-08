//! The two typed libraries (ADR 0011): designation, the active-kind switcher,
//! per-device `meta` state, and the path-resolution seam every path-keyed query
//! goes through to map an absolute path to its stored library-relative key.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

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
pub(crate) fn library_path_of(conn: &Connection, kind: &str) -> rusqlite::Result<Option<String>> {
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
    conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
        row.get(0)
    })
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
pub(crate) fn absolute(library: &str, relative: &str) -> String {
    Path::new(library)
        .join(relative)
        .to_string_lossy()
        .into_owned()
}

/// The path of `absolute` relative to `library`, or `None` when it does not lie
/// under the library folder (a recording outside the library — it does not exist
/// to the app, ADR 0011).
pub(crate) fn relative(library: &str, absolute: &str) -> Option<String> {
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
pub(crate) fn stored_key(
    conn: &Connection,
    absolute_path: &str,
) -> rusqlite::Result<Option<(String, String)>> {
    let kind = active_kind(conn)?;
    Ok(library_path_of(conn, &kind)?
        .and_then(|lib| relative(&lib, absolute_path))
        .map(|rel| (kind, rel)))
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
