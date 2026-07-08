//! The background media worker: it drains the media-work queue (ADR 0002) —
//! refine each recording's capture day, probe it for playability, then segment
//! it — without holding the DB lock across the slow ffmpeg/segmentation work.
//!
//! On a network-declared mount (ADR 0011) the probe+segment phases route through
//! [`run_staged_analysis`], copying each file into a local scratch area once and
//! analyzing the copy; a local mount analyzes in place.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use tauri::{AppHandle, Manager};

use crate::{db, media, segment, staging};

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
    std::thread::spawn(move || {
        run_media_worker(&conn, &staging_dir);
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
fn run_media_worker(conn: &Mutex<Connection>, staging_dir: &std::path::Path) {
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
                    run_staged_analysis(conn, staging_dir);
                } else {
                    match work {
                        db::MediaWork::Probe(id, path) => {
                            probe_recording(conn, id, &path);
                        }
                        db::MediaWork::Segment(id, path) => segment_recording(conn, id, &path),
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
fn run_staged_analysis(conn: &Mutex<Connection>, staging_dir: &std::path::Path) {
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
    for i in 0..pending.len() {
        let item = &pending[i];
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
            segment_recording(conn, item.id, &local);
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

/// Extract the recording's audio and motion tracks and run the hybrid segmenter
/// (ADR 0006), persisting the draft timeline. The slow extraction + segmentation
/// happen unlocked; only the final persist (and the failure mark) takes the lock.
fn segment_recording(conn: &Mutex<Connection>, id: i64, path: &str) {
    let samples = match media::extract_pcm(path) {
        Ok(s) => s,
        Err(e) => {
            log::warn!("media worker: audio extraction failed for {path}: {e}");
            set_segment_state(conn, id, "failed", path);
            return;
        }
    };
    let motion = match media::extract_motion(path) {
        Ok(energy) => segment::MotionTrack {
            fps: f64::from(media::MOTION_FPS),
            energy,
        },
        Err(e) => {
            log::warn!("media worker: motion extraction failed for {path}: {e}");
            set_segment_state(conn, id, "failed", path);
            return;
        }
    };
    let rallies = segment::segment(&samples, media::SEGMENT_SAMPLE_RATE, &motion);
    // The displayed waveform for the timeline strip (issue #6), reduced from the
    // same samples while we still hold them in memory.
    let waveform = segment::waveform(&samples);
    let duration_ms = (samples.len() as f64 / media::SEGMENT_SAMPLE_RATE as f64 * 1000.0) as i64;
    log::info!(
        "media worker: segmented {path} into {} rallies ({duration_ms} ms)",
        rallies.len()
    );

    match conn.lock() {
        Ok(mut c) => {
            if let Err(e) = db::save_rallies(&mut c, id, duration_ms, &rallies, &waveform) {
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

/// Update a recording's segmentation state under a brief lock, logging on failure.
fn set_segment_state(conn: &Mutex<Connection>, id: i64, state: &str, path: &str) {
    if let Ok(c) = conn.lock() {
        if let Err(e) = db::set_segment_state(&c, id, state) {
            log::error!("media worker: could not update segment state for {path}: {e}");
        }
    }
}
