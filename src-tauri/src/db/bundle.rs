//! Session bundles (ADR 0012): a metadata-only snapshot of one session's review
//! written as a `.vbundle` file into the shared library, and the receive/discover
//! side that applies and surfaces the bundles other people drop in. No video
//! bytes, nothing per-device.
//!
//! The file is JSON, tagged with a format id and a version so a future format
//! change stays backward-readable — bundles live in users' shared folders.
//! serde_json (not the hand-rolled waveform parser) because a receiver must
//! parse this back robustly.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use super::{
    absolute, active_kind, is_hand_touched, library_path_of, meta_get, parse_waveform, quick_hash,
};

/// The on-disk bundle format tag and current version. Bumped only on a
/// breaking format change; readers key backward-compat off it.
pub const BUNDLE_FORMAT: &str = "voloph-session-bundle";
pub const BUNDLE_VERSION: u32 = 1;

/// A rally in a bundled timeline. Same shape as [`super::TimelineRally`] but its
/// own type so the wire format is decoupled from the in-app struct.
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

/// The waveform peaks serialized as a compact JSON array of two-decimal floats —
/// finer precision is invisible on a strip a few hundred pixels wide and only
/// bloats the row. The single writer of the `recordings.waveform` column, shared
/// by segmentation persistence and bundle receive.
pub(crate) fn waveform_to_json(waveform: &[f32]) -> String {
    format!(
        "[{}]",
        waveform
            .iter()
            .map(|p| format!("{p:.2}"))
            .collect::<Vec<_>>()
            .join(",")
    )
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
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
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
    quick_hash(abs, meta.len())
        .map(|h| h == expect_hash)
        .unwrap_or(false)
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
                id, a.time_ms, a.verdict, a.aspect, a.note
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
        if cur.0 != b.time_ms || cur.1 != b.verdict || cur.2 != b.aspect || cur.3 != b.note {
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
        return Err(format!("file does not match the bundle: {abs}"));
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

// --- Bundle discovery (ADR 0012, issue #67) ------------------------------
//
// Scanning the shared library surfaces `.vbundle` files another person dropped
// in, offering each by session + sharer label. Discovery is ordered before
// analysis: a recording that a pending offer covers is held out of the media
// worker's probe/segment/stage queue (`next_media_work`/`pending_analysis`), so
// accepting the offer registers it ready from the bundle and that work never
// runs (ADR 0011, ADR 0012). Your own bundle is never offered back — it is
// keyed by your sharer label. Declining records the bundle's on-disk signature
// (size + mtime) so it stops nagging until the sharer re-shares (the file
// changes); a changed bundle is re-offered as an update.

/// A discovered shared bundle awaiting a receive-it? offer (ADR 0012). Named by
/// session day + sharer label so the same session from two sharers reads as two
/// independent offers.
#[derive(Debug, Serialize)]
pub struct BundleOffer {
    /// Absolute path of the `.vbundle` on this device — handed straight back to
    /// [`receive_session_bundle`] on accept, or [`decline_bundle`] on decline.
    pub bundle_path: String,
    pub capture_day: String,
    pub sharer_label: String,
    /// A re-share of a bundle the user previously declined (its file changed) —
    /// offered again, marked as an update rather than a first-time offer.
    pub is_update: bool,
}

/// A shared bundle as listed for browsing (issue): its identity plus a summary
/// of what it carries, and whether the user has already acted on it. Unlike
/// [`BundleOffer`], this describes *every* foreign bundle in the shared library
/// regardless of the seen ledger — so a review that scrolled past its offer (was
/// received or declined) can still be found and re-received per session.
#[derive(Debug, Serialize)]
pub struct BundleSummary {
    /// Absolute path of the `.vbundle`, handed straight to [`receive_session_bundle`].
    pub bundle_path: String,
    pub capture_day: String,
    pub sharer_label: String,
    pub recording_count: usize,
    pub rally_count: usize,
    pub annotation_count: usize,
    /// Already received or declined at its current signature (in the seen
    /// ledger) — i.e. no longer surfaced as a standing offer.
    pub seen: bool,
}

/// A bundle file's change signature: its size and mtime, cheap to read and
/// enough to notice a re-share without hashing the file. `None` when the file
/// cannot be stat'd (vanished between listing and here).
fn bundle_signature(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    Some(format!("{}:{}", meta.len(), mtime))
}

/// The map of declined bundles (file name → signature at decline time), read
/// from `meta`. A bundle whose current signature still matches its recorded one
/// stays declined; any change re-offers it (ADR 0012).
fn declined_bundles(conn: &Connection) -> HashMap<String, String> {
    meta_get(conn, "declined_bundles")
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// One parsed shared bundle: its file, the parsed contents, and its current
/// on-disk signature — the shared shape behind both the offer list and the
/// worker's coverage set.
struct DiscoveredBundle {
    path: PathBuf,
    bundle: SessionBundle,
    signature: String,
}

impl DiscoveredBundle {
    /// The bundle file's name, as keyed in the declined/seen ledger.
    fn name(&self) -> String {
        self.path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string()
    }
}

/// List every readable `.vbundle` in the shared library root that is not this
/// device's own (by sharer label). Malformed, foreign-format, or newer-version
/// files are skipped silently — discovery never errors on a bad drop-in. Returns
/// nothing when the shared library is not designated.
fn list_shared_bundles(conn: &Connection) -> Vec<DiscoveredBundle> {
    let Ok(Some(root)) = library_path_of(conn, "shared") else {
        return Vec::new();
    };
    let own = meta_get(conn, "sharer_label").ok().flatten();
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("vbundle") {
            continue;
        }
        let Ok(json) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(bundle) = serde_json::from_str::<SessionBundle>(&json) else {
            continue;
        };
        if bundle.format != BUNDLE_FORMAT || bundle.version > BUNDLE_VERSION {
            continue;
        }
        // Never offer a bundle back to the person who shared it (ADR 0012).
        if own.as_deref() == Some(bundle.sharer_label.as_str()) {
            continue;
        }
        let Some(signature) = bundle_signature(&path) else {
            continue;
        };
        out.push(DiscoveredBundle {
            path,
            bundle,
            signature,
        });
    }
    out
}

/// The offers surfaced on a scan/refresh of the shared library (ADR 0012):
/// every foreign `.vbundle` in the shared root, minus those the user declined
/// and that have not changed since. A declined bundle whose file changed is
/// re-offered with `is_update` set. Ordered by session day then sharer label
/// for a stable list.
pub fn discover_bundles(conn: &Connection) -> Vec<BundleOffer> {
    let declined = declined_bundles(conn);
    let mut offers = Vec::new();
    for d in list_shared_bundles(conn) {
        // Declined and unchanged → stay quiet. Declined but changed → re-offer as
        // an update. Never declined → a first-time offer.
        let prior = declined.get(&d.name());
        if prior == Some(&d.signature) {
            continue;
        }
        offers.push(BundleOffer {
            bundle_path: d.path.to_string_lossy().into_owned(),
            capture_day: d.bundle.capture_day,
            sharer_label: d.bundle.sharer_label,
            is_update: prior.is_some(),
        });
    }
    offers.sort_by(|a, b| {
        a.capture_day
            .cmp(&b.capture_day)
            .then_with(|| a.sharer_label.cmp(&b.sharer_label))
    });
    offers
}

/// Every foreign shared bundle in the shared library (issue), each summarized —
/// who shared it, how much review it carries, and whether it has already been
/// received or declined. Independent of the seen ledger (which only governs what
/// `discover_bundles` nags about), so the per-session bundle browser can list a
/// review that has already been accepted and offer to re-receive it. Ordered by
/// session day then sharer label.
pub fn list_bundles(conn: &Connection) -> Vec<BundleSummary> {
    let declined = declined_bundles(conn);
    let mut out: Vec<BundleSummary> = list_shared_bundles(conn)
        .into_iter()
        .map(|d| {
            let seen = declined.get(&d.name()) == Some(&d.signature);
            let bundle_path = d.path.to_string_lossy().into_owned();
            let recording_count = d.bundle.recordings.len();
            let rally_count: usize = d.bundle.recordings.iter().map(|r| r.rallies.len()).sum();
            let annotation_count: usize = d
                .bundle
                .recordings
                .iter()
                .map(|r| r.annotations.len())
                .sum();
            BundleSummary {
                bundle_path,
                capture_day: d.bundle.capture_day,
                sharer_label: d.bundle.sharer_label,
                recording_count,
                rally_count,
                annotation_count,
                seen,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        a.capture_day
            .cmp(&b.capture_day)
            .then_with(|| a.sharer_label.cmp(&b.sharer_label))
    });
    out
}

/// The library-relative paths of every recording covered by a bundle currently
/// on offer (ADR 0012) — the set the media worker holds out of analysis so an
/// accepted offer's recordings are never probed, segmented, or staged before the
/// user decides. Empty unless the shared library is active (bundles only live
/// and resolve there).
pub(crate) fn pending_bundle_paths(conn: &Connection) -> HashSet<String> {
    if active_kind(conn).unwrap_or_default() != "shared" {
        return HashSet::new();
    }
    let declined = declined_bundles(conn);
    let mut covered = HashSet::new();
    for d in list_shared_bundles(conn) {
        // A declined, unchanged bundle no longer holds its recordings back — the
        // user chose to let analysis proceed on them.
        if declined.get(&d.name()) == Some(&d.signature) {
            continue;
        }
        for rec in d.bundle.recordings {
            covered.insert(rec.path);
        }
    }
    covered
}

/// Record `bundle_path`'s current signature in the "seen" ledger so it stops
/// nagging until it changes (ADR 0012). Both declining an offer and finishing
/// its receive land here — either way the user has dealt with this exact bundle,
/// and only a re-share (a new signature) should surface it again, as an update.
/// Re-recording a changed bundle simply overwrites the stored signature.
pub fn decline_bundle(conn: &Connection, bundle_path: &str) -> Result<(), String> {
    let signature = bundle_signature(Path::new(bundle_path))
        .ok_or_else(|| "bundle file could not be read".to_string())?;
    let name = Path::new(bundle_path)
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| "bad bundle path".to_string())?
        .to_string();
    let mut declined = declined_bundles(conn);
    declined.insert(name, signature);
    let json = serde_json::to_string(&declined).map_err(|e| e.to_string())?;
    super::meta_set(conn, "declined_bundles", &json).map_err(|e| e.to_string())
}
