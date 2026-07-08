//! Cross-library carry-over (ADR 0011): when the same content is a copy in both
//! libraries and only one side is hand-touched, offer to carry that review onto
//! the untouched copy. Never migrates silently, never merges.

use std::collections::HashSet;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

use super::{absolute, is_hand_touched, library_path_of, meta_get, meta_set, relative};

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
/// re-home, handled on scan, not a copy). Offers the user has dismissed
/// ([`dismiss_carry`]) are held back. Returns nothing when either library is
/// undesignated.
pub fn carry_offers(conn: &Connection) -> rusqlite::Result<Vec<CarryOffer>> {
    let (Some(local), Some(shared)) = (
        library_path_of(conn, "local")?,
        library_path_of(conn, "shared")?,
    ) else {
        return Ok(Vec::new());
    };
    let dismissed = dismissed_carries(conn);
    // Same content in both libraries: join local vs shared recordings on hash+size.
    let mut stmt = conn.prepare(
        "SELECT l.id, l.path, s.id, s.path, l.quick_hash
         FROM recordings l JOIN recordings s
           ON l.quick_hash = s.quick_hash AND l.file_size = s.file_size
         WHERE l.library = 'local' AND s.library = 'shared'",
    )?;
    let pairs: Vec<(i64, String, i64, String, String)> = stmt
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    drop(stmt);

    let mut offers = Vec::new();
    for (l_id, l_rel, s_id, s_rel, quick_hash) in pairs {
        // Both copies must currently exist on disk — a missing side is a move,
        // re-homed on scan, never a copy offer (AC: offer only when both exist).
        if Path::new(&l_rel).is_absolute() || Path::new(&s_rel).is_absolute() {
            continue;
        }
        if !Path::new(&local).join(&l_rel).exists() || !Path::new(&shared).join(&s_rel).exists() {
            continue;
        }
        let l_touched = is_hand_touched(conn, l_id)?;
        let s_touched = is_hand_touched(conn, s_id)?;
        // Offer only when exactly one side is hand-touched (no merge; never silent).
        let offer = match (l_touched, s_touched) {
            (true, false) => Some(CarryOffer {
                from_path: absolute(&local, &l_rel),
                to_path: absolute(&shared, &s_rel),
                to_kind: "shared".to_string(),
            }),
            (false, true) => Some(CarryOffer {
                from_path: absolute(&shared, &s_rel),
                to_path: absolute(&local, &l_rel),
                to_kind: "local".to_string(),
            }),
            _ => None,
        };
        // An offer the user dismissed stays quiet (keyed by content + destination).
        if let Some(offer) = offer {
            if !dismissed.contains(&carry_key(&offer.to_kind, &quick_hash)) {
                offers.push(offer);
            }
        }
    }
    Ok(offers)
}

/// The stable key a carry-over dismissal is stored under: the destination library
/// kind plus the content's quick hash. Content-addressed rather than path-addressed
/// so a dismissal survives the library being re-mounted at a different folder, and
/// so it is direction-specific (dismissing a carry *into* shared does not silence a
/// later carry *into* local).
fn carry_key(to_kind: &str, quick_hash: &str) -> String {
    format!("{to_kind}:{quick_hash}")
}

/// The set of carry-over offers the user has dismissed (ADR 0011), keyed by
/// [`carry_key`] and read from `meta`. A dismissed offer is not surfaced again by
/// [`carry_offers`] until it is re-created by fresh review on the source side (a new
/// quick hash never collides; a re-touch of the same content stays dismissed —
/// dismiss means "not this copy", not "not once").
fn dismissed_carries(conn: &Connection) -> HashSet<String> {
    meta_get(conn, "dismissed_carries")
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Dismiss the carry-over offer whose receiving copy is at `to_path` (ADR 0011):
/// record it so [`carry_offers`] stops surfacing it. Persisted (unlike a transient
/// "not now") and keyed by the receiving copy's content + library kind, so it holds
/// across restarts and re-mounts. A no-op when `to_path` is not a registered
/// recording.
pub fn dismiss_carry(conn: &Connection, to_path: &str) -> Result<(), String> {
    let Some(to_id) = recording_id_any_library(conn, to_path).map_err(|e| e.to_string())? else {
        return Ok(());
    };
    let (kind, quick_hash): (String, String) = conn
        .query_row(
            "SELECT library, quick_hash FROM recordings WHERE id = ?1",
            [to_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|e| e.to_string())?;
    let mut dismissed = dismissed_carries(conn);
    dismissed.insert(carry_key(&kind, &quick_hash));
    let json = serde_json::to_string(&dismissed).map_err(|e| e.to_string())?;
    meta_set(conn, "dismissed_carries", &json).map_err(|e| e.to_string())
}

/// Apply an accepted carry-over offer (ADR 0011): copy the review state — the draft
/// timeline rallies with their flags, the annotations, *and* the analyzed timeline
/// itself (duration + waveform, marking the receiver segmented) — from the recording
/// at `from_path` onto the copy at `to_path` in the other library. The receiving
/// copy's own state is replaced (it had none — the offer is only made when one side
/// is un-touched), so this is idempotent. A no-op when either path is not a
/// registered recording. Returns whether anything was carried.
pub fn carry_review(
    conn: &mut Connection,
    from_path: &str,
    to_path: &str,
) -> rusqlite::Result<bool> {
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
    // Carry the analyzed timeline itself, not only the review layered on top of it:
    // copy the source's duration + waveform and mark the receiver segmented. Without
    // this the receiver stays `segment_state = 'unknown'` and the media worker
    // re-segments it on its next pass — and that re-segmentation (`save_rallies`)
    // deletes the carried rallies, taking the flags and the segments with them and
    // leaving only the annotations behind (the very bug this fixes). Both copies are
    // byte-identical (matched on quick hash + size, ADR 0011), so the source's
    // waveform and duration are valid for the receiver. Mirrors `apply_bundle_state`.
    tx.execute(
        "UPDATE recordings
         SET segment_state = 'ready',
             duration_ms = (SELECT duration_ms FROM recordings WHERE id = ?2),
             waveform = (SELECT waveform FROM recordings WHERE id = ?2)
         WHERE id = ?1",
        rusqlite::params![to_id, from_id],
    )?;
    tx.commit()?;
    Ok(true)
}

/// The database id of the recording at absolute `path` in *whichever* library it
/// lies under — the cross-library counterpart of [`super::recording_id`], which
/// resolves only against the active library. Used by carry-over, whose receiving
/// copy is by definition in the non-active library.
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
