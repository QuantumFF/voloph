//! The background media-work queue (ADR 0002): what needs a capture-date probe,
//! a playability probe, or segmentation, and persisting the draft timeline that
//! segmentation produces. Recordings a pending bundle offer covers are held out
//! of analysis (ADR 0012).

use rusqlite::{Connection, OptionalExtension};

use super::{absolute, active_kind, library_path_of, pending_bundle_paths, stored_key};

/// One unit of work for the background media worker, in priority order: probe a
/// recording for its frame rate (which also marks it playable), then produce its
/// draft timeline (segment). Segmentation is gated on `probe_state = 'ready'`
/// so it always runs after the probe.
#[derive(Debug, PartialEq)]
pub enum MediaWork {
    /// Capture day not yet refined — read the camera's embedded creation date and
    /// re-home the recording's session if it differs from the provisional
    /// mtime-derived day (see [`super::refine_capture_day`]).
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
    // Recordings a pending bundle offer covers are held out of the probe/segment
    // pipeline (ADR 0012, issue #67): accepting the offer registers them ready
    // from the bundle, so analysis (and its network staging) never runs on them.
    // The capture-date phase above is not held back — it reads only headers.
    let covered = pending_bundle_paths(conn);
    // Phase 1: anything not yet probed (skipping covered recordings).
    let mut pstmt = conn.prepare(
        "SELECT id, path FROM recordings
         WHERE library = ?1 AND probe_state = 'unknown' ORDER BY id",
    )?;
    let probe = pstmt
        .query_map([&kind], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .filter_map(Result::ok)
        .find(|(_, rel)| !covered.contains(rel));
    if let Some((id, rel)) = probe {
        return Ok(Some(MediaWork::Probe(id, absolute(&library, &rel))));
    }
    // Phase 2: anything probed but not yet segmented (skipping covered recordings).
    let mut sstmt = conn.prepare(
        "SELECT id, path FROM recordings
         WHERE library = ?1 AND probe_state = 'ready' AND segment_state = 'unknown'
         ORDER BY id",
    )?;
    let segment = sstmt
        .query_map([&kind], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .filter_map(Result::ok)
        .find(|(_, rel)| !covered.contains(rel));
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
    // Recordings a pending bundle offer covers are held out of staging + analysis
    // (ADR 0012, issue #67) so accepting the offer skips that network copy entirely.
    let covered = pending_bundle_paths(conn);
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
            Ok((
                rel.clone(),
                PendingAnalysis {
                    id,
                    path: absolute(&library, &rel),
                    needs_probe: probe_state == "unknown",
                    // Segment when it hasn't been done — after the probe, if the probe
                    // marks the file playable. `unknown` here means either already
                    // probed-ready (probe skipped) or about to be probed this pass.
                    needs_segment: segment_state == "unknown",
                },
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .filter(|(rel, _)| !covered.contains(rel))
        .map(|(_, item)| item)
        .collect();
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
    let waveform_json = super::waveform_to_json(waveform);
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
