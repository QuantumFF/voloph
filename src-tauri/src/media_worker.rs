//! The background media worker: it drains the media-work queue (ADR 0002) —
//! refine each recording's capture day, probe it for playability, then segment
//! it — without holding the DB lock across the slow ffmpeg/segmentation work.
//!
//! On a network-declared mount (ADR 0011) the probe+segment phases route through
//! [`run_staged_analysis`], copying each file into a local scratch area once and
//! analyzing the copy; a local mount analyzes in place.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rusqlite::Connection;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::{db, draft, media, segment, staging};

/// Tauri event carrying how far a recording's background analysis has progressed
/// (issue #81, spec #75 user story #13), so the session list can show a
/// remaining-time estimate on the "Analyzing…" row. Emitted repeatedly as the
/// pass decodes the recording, then once more at completion.
pub const EVENT_ANALYSIS_PROGRESS: &str = "analysis:progress";

/// One `analysis:progress` tick: how much of recording `recording_id`'s footage
/// the current pass has processed (`processed_ms`) out of its total
/// (`total_ms`), and how long the pass has been running (`elapsed_ms`). The
/// frontend turns processed-vs-total, paced by elapsed wall time, into a live
/// estimate — no rate is hardcoded anywhere. `total_ms` is `None` when the
/// recording's duration could not be probed; the UI then shows a bare spinner.
#[derive(Clone, Serialize)]
struct AnalysisProgress {
    recording_id: i64,
    processed_ms: i64,
    total_ms: Option<i64>,
    elapsed_ms: i64,
}

/// The single SQLite metadata connection. An `Arc` so the background media
/// worker can share it with the Tauri commands.
pub(crate) struct Db(pub Arc<Mutex<Connection>>);

/// Guard so at most one background media worker runs at a time. A worker drains
/// every pending unit of media work — probe (capture date + fps), then segment
/// (ADR 0002) — before exiting, so anything registered while it runs is picked
/// up on the same pass; a scan that arrives after it exits starts a fresh worker.
pub(crate) struct MediaWorker(pub Arc<AtomicBool>);

/// Where analysis stages recordings copied off a network-declared mount (ADR
/// 0011). A per-run local scratch area, wiped on launch so an interrupted run
/// leaves no orphaned copies; empty until the first stage.
pub(crate) struct StagingDir(pub std::path::PathBuf);

/// Start the background media worker unless one is already running. It drains
/// every pending unit of work — probe each `unknown` recording for its frame
/// rate, then segment each unsegmented one (ADR 0002) — without holding the DB
/// lock across the slow ffmpeg/segment work, so playback and timeline queries
/// stay responsive.
pub(crate) fn spawn_media_worker(app: &AppHandle) {
    let running = app.state::<MediaWorker>().0.clone();
    if running
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return; // a worker is already draining the queue
    }

    let conn = app.state::<Db>().0.clone();
    let staging_dir = app.state::<StagingDir>().0.clone();
    let app = app.clone();
    std::thread::spawn(move || {
        run_media_worker(&app, &conn, &staging_dir);
        running.store(false, Ordering::SeqCst);
    });
}

/// Drain the media-work queue (see [`db::next_media_work`]) until none remains.
/// Each DB touch takes the lock only briefly; the ffmpeg probe and the audio
/// extraction + segmentation run unlocked.
///
/// On a network-declared mount (ADR 0011) the probe+segment phases route through
/// [`run_staged_analysis`] instead of streaming each file: every recording is
/// copied once into `staging_dir`, analyzed locally, then evicted. The
/// capture-date phase always streams — it reads only container headers, so
/// staging would not save a network crossing.
fn run_media_worker(app: &AppHandle, conn: &Mutex<Connection>, staging_dir: &std::path::Path) {
    loop {
        let work = match conn.lock() {
            Ok(c) => db::next_media_work(&c),
            Err(e) => {
                log::error!("media worker: db lock poisoned: {e}");
                return;
            }
        };
        let work = match work {
            Ok(Some(w)) => w,
            Ok(None) => return, // queue empty
            Err(e) => {
                log::error!("media worker: query failed: {e}");
                return;
            }
        };

        match work {
            db::MediaWork::CaptureDate(id, path) => refine_recording_date(conn, id, &path),
            db::MediaWork::Probe(_, _) | db::MediaWork::Segment(_, _) => {
                if is_network_mount(conn) {
                    // Stage the whole pending batch once (copy-ahead overlaps the
                    // next copy with the current analysis), then loop to pick up
                    // any capture-date work or newly-arrived recordings.
                    run_staged_analysis(app, conn, staging_dir);
                } else {
                    match work {
                        db::MediaWork::Probe(id, path) => {
                            probe_recording(conn, id, &path);
                        }
                        db::MediaWork::Segment(id, path) => segment_recording(app, conn, id, &path),
                        db::MediaWork::CaptureDate(..) => unreachable!(),
                    }
                }
            }
        }
    }
}

/// Whether the active library's mount is declared network (ADR 0011); errors and
/// an absent library both read as "not network" (analyze in place).
fn is_network_mount(conn: &Mutex<Connection>) -> bool {
    conn.lock()
        .ok()
        .and_then(|c| db::active_mount(&c).ok().flatten())
        .as_deref()
        == Some("network")
}

/// Stage every pending recording in the active library once, run its remaining
/// probe/segment phases against the local copy, and evict it (ADR 0011). Copy-
/// ahead stages the next recording while the current one is analyzed. Returns
/// after one pass over the snapshot; the caller loops to catch new work.
fn run_staged_analysis(app: &AppHandle, conn: &Mutex<Connection>, staging_dir: &std::path::Path) {
    let pending = match conn.lock() {
        Ok(c) => db::pending_analysis(&c).unwrap_or_default(),
        Err(e) => {
            log::error!("media worker: db lock poisoned listing pending analysis: {e}");
            return;
        }
    };
    if pending.is_empty() {
        return;
    }
    let mut cache = staging::StagingCache::new(staging_dir.to_path_buf(), staging::budget_bytes());
    for (i, item) in pending.iter().enumerate() {
        // A re-analyze or fresh scan can flip the active library mid-batch; bail so
        // the outer loop re-decides. (The cache drops here, evicting any prefetch.)
        if !is_network_mount(conn) {
            return;
        }
        let next = pending.get(i + 1).map(|n| std::path::Path::new(&n.path));
        let staged = match cache.stage(std::path::Path::new(&item.path), next) {
            Ok(s) => s,
            Err(e) => {
                // Staging failed (mount vanished, disk full): mark this recording
                // failed so the pipeline does not loop, and move on.
                log::warn!("media worker: staging failed for {}: {e}", item.path);
                if item.needs_probe {
                    if let Ok(c) = conn.lock() {
                        let _ = db::set_probe_result(&c, item.id, "failed");
                    }
                } else {
                    set_segment_state(conn, item.id, "failed", &item.path);
                }
                continue;
            }
        };
        let local = staged.path().to_string_lossy().into_owned();
        // Probe first; segmentation is gated on the probe marking the file
        // playable, exactly as the streamed pipeline gates it.
        let probe_ok = if item.needs_probe {
            probe_recording(conn, item.id, &local) == "ready"
        } else {
            true
        };
        if item.needs_segment && probe_ok {
            segment_recording(app, conn, item.id, &local);
        }
        // `staged` drops here → the copy is evicted before the next iteration.
    }
}

/// Confirm a recording is readable and record the outcome (ADR 0008): `ready`
/// once probed, `failed` otherwise. Returns the state it recorded so a staged
/// pipeline can gate segmentation on it. `path` is whatever local path the caller
/// wants ffprobe to read — the original, or a staged copy.
fn probe_recording(conn: &Mutex<Connection>, id: i64, path: &str) -> &'static str {
    let state = match media::probe(path) {
        Ok(()) => "ready",
        Err(e) => {
            log::warn!("media worker: probe failed for {path}: {e}");
            "failed"
        }
    };
    if let Ok(c) = conn.lock() {
        if let Err(e) = db::set_probe_result(&c, id, state) {
            log::error!("media worker: could not record probe for {path}: {e}");
        }
    }
    state
}

/// Draft the recording's timeline with its library's segmenter (ADR 0019; see
/// [`draft::draft`]) and persist it stamped with the segmenter that drew it. The slow
/// drafting happens unlocked; only the segmenter lookup, the final persist (and the
/// failure mark) take the lock.
fn segment_recording(app: &AppHandle, conn: &Mutex<Connection>, id: i64, path: &str) {
    // The recording's total duration paces the remaining-time estimate (issue
    // #81): each progress tick reports footage decoded so far out of this total.
    // A missing duration is non-fatal — the estimate just does not show.
    let total_ms = media::probe_duration_ms(path);
    let chosen = match conn.lock() {
        Ok(c) => db::recording_segmenter(&c, id).unwrap_or_else(|e| {
            log::error!("media worker: could not read the segmenter for {path}: {e}");
            segment::DEFAULT_SEGMENTER
        }),
        Err(e) => {
            log::error!("media worker: db lock poisoned reading the segmenter for {path}: {e}");
            segment::DEFAULT_SEGMENTER
        }
    };
    let mut ticks = AnalysisTicks::new(app, id, total_ms);
    let draft = match draft::draft(path, chosen, &mut ticks) {
        Ok(d) => d,
        Err(e) => {
            log::warn!("media worker: {e} for {path}");
            set_segment_state(conn, id, "failed", path);
            return;
        }
    };
    // Final tick: the pass is done, so processed == total. The waveform-derived
    // duration is the authoritative length, so report it as both (the container
    // probe can be slightly off). The UI clears the estimate once the row flips
    // to `ready` on its next poll; this just avoids a stale mid-pass number.
    ticks.emit(draft.duration_ms, Some(draft.duration_ms));
    log::info!(
        "media worker: segmented {path} into {} rallies ({} ms, {} v{})",
        draft.rallies.len(),
        draft.duration_ms,
        draft.made_by.id(),
        draft.made_by.version()
    );

    match conn.lock() {
        Ok(mut c) => {
            if let Err(e) = db::save_rallies(
                &mut c,
                id,
                draft.duration_ms,
                &draft.rallies,
                &draft.waveform,
                draft.made_by,
            ) {
                log::error!("media worker: could not save timeline for {path}: {e}");
            } else {
                // Publish the pristine machine Analysis for adoption by other users
                // of a shared library (ADR 0013). A no-op for a local recording, and
                // silent on failure — the timeline is already saved above.
                db::publish_analysis(&c, id);
            }
        }
        Err(e) => log::error!("media worker: db lock poisoned saving {path}: {e}"),
    }
}

/// Read a recording's embedded capture date with ffprobe and re-home its session
/// to the matching day, under a brief lock. An ffprobe failure (missing/unreadable
/// file) is not fatal: it falls back to the mtime-derived day rather than looping,
/// since the recording is still marked `refined` afterward.
fn refine_recording_date(conn: &Mutex<Connection>, id: i64, path: &str) {
    let embedded = media::probe_capture_date(path).unwrap_or_default();
    match conn.lock() {
        Ok(mut c) => {
            if let Err(e) = db::refine_capture_day(&mut c, id, path, &embedded) {
                log::error!("media worker: could not refine capture day for {path}: {e}");
            }
        }
        Err(e) => log::error!("media worker: db lock poisoned refining date for {path}: {e}"),
    }
}

/// A draft's progress as [`EVENT_ANALYSIS_PROGRESS`] ticks for one recording (issue
/// #81), throttled so a pass decoding thousands of frames does not flood the
/// frontend. Fire-and-forget: a dropped tick only means one skipped UI update, and
/// the next tick (or the row flipping to `ready`) corrects it.
struct AnalysisTicks<'a> {
    app: &'a AppHandle,
    recording_id: i64,
    total_ms: Option<i64>,
    /// When the current pass started: ticks report elapsed time from here.
    started: Instant,
    throttle: Throttle,
}

impl<'a> AnalysisTicks<'a> {
    fn new(app: &'a AppHandle, recording_id: i64, total_ms: Option<i64>) -> AnalysisTicks<'a> {
        let now = Instant::now();
        AnalysisTicks {
            app,
            recording_id,
            total_ms,
            started: now,
            throttle: Throttle::new(now),
        }
    }

    /// Push one tick now, unthrottled.
    fn emit(&self, processed_ms: i64, total_ms: Option<i64>) {
        let _ = self.app.emit(
            EVENT_ANALYSIS_PROGRESS,
            AnalysisProgress {
                recording_id: self.recording_id,
                processed_ms,
                total_ms,
                elapsed_ms: self.started.elapsed().as_millis() as i64,
            },
        );
    }
}

impl draft::Progress for AnalysisTicks<'_> {
    fn pass_started(&mut self) {
        let now = Instant::now();
        self.started = now;
        self.throttle = Throttle::new(now);
    }

    fn processed(&mut self, footage_ms: i64) {
        if self.throttle.admit(Instant::now()) {
            self.emit(footage_ms, self.total_ms);
        }
    }
}

/// Lets one event through per [`Throttle::EVERY`], counted from the last one let
/// through (or from the start).
struct Throttle {
    last: Instant,
}

impl Throttle {
    const EVERY: Duration = Duration::from_millis(500);

    fn new(now: Instant) -> Throttle {
        Throttle { last: now }
    }

    fn admit(&mut self, now: Instant) -> bool {
        if now.saturating_duration_since(self.last) < Self::EVERY {
            return false;
        }
        self.last = now;
        true
    }
}

/// Update a recording's segmentation state under a brief lock, logging on failure.
fn set_segment_state(conn: &Mutex<Connection>, id: i64, state: &str, path: &str) {
    if let Ok(c) = conn.lock() {
        if let Err(e) = db::set_segment_state(&c, id, state) {
            log::error!("media worker: could not update segment state for {path}: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_throttle_lets_one_event_through_per_half_second() {
        let t0 = Instant::now();
        let at = |ms| t0 + Duration::from_millis(ms);
        let mut throttle = Throttle::new(t0);
        let admitted: Vec<u64> = [0, 100, 499, 500, 501, 999, 1000, 1700, 1800, 2200]
            .into_iter()
            .filter(|&ms| throttle.admit(at(ms)))
            .collect();
        assert_eq!(admitted, [500, 1000, 1700, 2200]);
        // Sub-millisecond: 499.9 ms is still inside the window.
        let mut throttle = Throttle::new(t0);
        assert!(!throttle.admit(t0 + Duration::from_micros(499_900)));
        assert!(throttle.admit(t0 + Duration::from_micros(500_000)));
    }
}
