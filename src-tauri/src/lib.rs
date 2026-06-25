mod db;
mod media;
mod segment;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use serde::Serialize;
use tauri::webview::PageLoadEvent;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_log::{Target, TargetKind};
use tauri_plugin_opener::OpenerExt;

/// The single SQLite metadata connection. An `Arc` so the background transcode
/// worker can share it with the Tauri commands.
struct Db(Arc<Mutex<Connection>>);

/// Guard so at most one background media worker runs at a time. A worker drains
/// every pending unit of media work — probe, transcode (ADR 0005), then segment
/// (ADR 0002) — before exiting, so anything registered while it runs is picked
/// up on the same pass; a scan that arrives after it exits starts a fresh worker.
struct MediaWorker(Arc<AtomicBool>);

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

/// Register the video files under `folder` as recordings, grouped into
/// sessions by capture day. Idempotent across re-scans. Kicks off background
/// media work (transcode then segment) for any newly registered recordings.
#[tauri::command]
fn scan_folder(app: AppHandle, db: State<'_, Db>, folder: String) -> Result<db::ScanResult, String> {
    let result = {
        let mut conn = db.0.lock().map_err(|e| e.to_string())?;
        db::scan_folder(&mut conn, std::path::Path::new(&folder)).map_err(|e| e.to_string())?
    };
    spawn_media_worker(&app);
    Ok(result)
}

/// All sessions with their recordings nested under them.
#[tauri::command]
fn list_sessions(db: State<'_, Db>) -> Result<Vec<db::Session>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::list_sessions(&conn).map_err(|e| e.to_string())
}

/// The loopback origin + token the player builds playback URLs from. The
/// frontend requests `${origin}/play?path=…&token=…` for a ready recording.
#[tauri::command]
fn playback_endpoint(endpoint: State<'_, media::PlaybackEndpoint>) -> media::PlaybackEndpoint {
    endpoint.inner().clone()
}

/// What the player needs to decide how to render a recording: which file to load
/// and where it is in the transcode lifecycle (ADR 0005).
#[derive(Serialize)]
struct PlaybackSource {
    /// Absolute path to load. With in-place transcoding this is always the
    /// recording's own path — there is no separate proxy file.
    path: String,
    /// Transcode state: `ready` is playable now; `unknown`/`pending` mean a
    /// transcode is still in progress (the player should wait rather than load
    /// the as-yet-undecodable original); `failed` is an error.
    state: String,
}

/// Resolve how to play the recording at `path`. When `ready` the file is
/// playable now and the frontend streams it from the playback server for native
/// seeking. While a transcode is still running (`unknown`/`pending`) the file
/// would not decode in the webview, so the frontend waits on the state instead.
#[tauri::command]
fn resolve_playback(db: State<'_, Db>, path: String) -> Result<PlaybackSource, String> {
    let state = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        db::recording_transcode_state(&conn, &path).map_err(|e| e.to_string())?
    };
    Ok(PlaybackSource {
        // Not registered (e.g. played straight from a dialog) — assume playable.
        state: state.unwrap_or_else(|| "ready".to_string()),
        path,
    })
}

/// Resolve the draft timeline (rallies + per-region confidence) for the
/// recording at `path` (ADR 0002). While segmentation is still running the
/// `segment_state` is `unknown` and `rallies` is empty; the player polls until
/// it turns `ready`. An unregistered path reports `unknown` with no rallies.
#[tauri::command]
fn recording_timeline(db: State<'_, Db>, path: String) -> Result<db::Timeline, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let timeline = db::recording_timeline(&conn, &path).map_err(|e| e.to_string())?;
    Ok(timeline.unwrap_or_else(|| db::Timeline {
        segment_state: "unknown".to_string(),
        duration_ms: None,
        rallies: Vec::new(),
    }))
}

/// Re-run segmentation for the recording at `path`: discard its draft timeline,
/// return it to the queue, and wake the media worker (ADR 0002). Backs the
/// Re-analyze action for the human tuning step — re-segment without re-transcoding.
#[tauri::command]
fn reanalyze_recording(app: AppHandle, db: State<'_, Db>, path: String) -> Result<(), String> {
    {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        db::reset_segmentation(&conn, &path).map_err(|e| e.to_string())?;
    }
    spawn_media_worker(&app);
    Ok(())
}

/// Start the background media worker unless one is already running. It drains
/// every pending unit of work — probe each `unknown` recording, transcode each
/// `pending` one in place (ADR 0005), then segment each playable-but-unsegmented
/// one (ADR 0002) — without holding the DB lock across the slow ffmpeg/segment
/// work, so playback and timeline queries stay responsive.
fn spawn_media_worker(app: &AppHandle) {
    let running = app.state::<MediaWorker>().0.clone();
    if running
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return; // a worker is already draining the queue
    }

    let conn = app.state::<Db>().0.clone();
    std::thread::spawn(move || {
        run_media_worker(&conn);
        running.store(false, Ordering::SeqCst);
    });
}

/// Drain the media-work queue (see [`db::next_media_work`]) until none remains.
/// Each DB touch takes the lock only briefly; the ffmpeg probe/transcode and the
/// audio extraction + segmentation run unlocked.
fn run_media_worker(conn: &Mutex<Connection>) {
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
            db::MediaWork::Probe(id, path) => {
                let next = match media::probe(&path) {
                    Ok(probe) if probe.passthrough => "ready",
                    Ok(_) => "pending",
                    Err(e) => {
                        log::warn!("media worker: probe failed for {path}: {e}");
                        "failed"
                    }
                };
                set_transcode_state(conn, id, next, &path);
            }
            db::MediaWork::Transcode(id, path) => match media::transcode_in_place(&path) {
                Ok(()) => {
                    log::info!("media worker: transcoded {path} in place");
                    if let Ok(c) = conn.lock() {
                        if let Err(e) = db::mark_transcoded(&c, id, &path) {
                            log::error!("media worker: could not finalize {path}: {e}");
                        }
                    }
                }
                Err(e) => {
                    log::warn!("media worker: transcode failed for {path}: {e}");
                    set_transcode_state(conn, id, "failed", &path);
                }
            },
            db::MediaWork::Segment(id, path) => segment_recording(conn, id, &path),
        }
    }
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
    let duration_ms =
        (samples.len() as f64 / media::SEGMENT_SAMPLE_RATE as f64 * 1000.0) as i64;
    log::info!(
        "media worker: segmented {path} into {} rallies ({duration_ms} ms)",
        rallies.len()
    );

    match conn.lock() {
        Ok(mut c) => {
            if let Err(e) = db::save_rallies(&mut c, id, duration_ms, &rallies) {
                log::error!("media worker: could not save timeline for {path}: {e}");
            }
        }
        Err(e) => log::error!("media worker: db lock poisoned saving {path}: {e}"),
    }
}

/// Update a recording's transcode state under a brief lock, logging on failure.
fn set_transcode_state(conn: &Mutex<Connection>, id: i64, state: &str, path: &str) {
    if let Ok(c) = conn.lock() {
        if let Err(e) = db::set_transcode_state(&c, id, state) {
            log::error!("media worker: could not update transcode state for {path}: {e}");
        }
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

fn external_navigation_plugin<R: tauri::Runtime>() -> tauri::plugin::TauriPlugin<R> {
    tauri::plugin::Builder::<R>::new("external-navigation")
        .on_navigation(|webview, url| {
            let is_internal_host = matches!(
                url.host_str(),
                Some("localhost") | Some("127.0.0.1") | Some("tauri.localhost") | Some("::1")
            );

            let is_internal = url.scheme() == "tauri" || is_internal_host;

            if is_internal {
                return true;
            }

            let is_external_link = matches!(url.scheme(), "http" | "https" | "mailto" | "tel");

            if is_external_link {
                log::info!("opening external link in system browser: {}", url);
                let _ = webview.opener().open_url(url.as_str(), None::<&str>);
                return false;
            }

            true
        })
        .build()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(
            tauri_plugin_log::Builder::new()
                .targets([
                    Target::new(TargetKind::Stdout),
                    Target::new(TargetKind::LogDir { file_name: None }),
                    Target::new(TargetKind::Webview),
                ])
                .build(),
        )
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(external_navigation_plugin())
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            // Remove the obsolete proxy cache from the superseded design (ADR
            // 0005); transcoding is now done in place, so this dir is dead.
            let proxies = dir.join("proxies");
            if proxies.exists() {
                let _ = std::fs::remove_dir_all(&proxies);
            }
            let conn = db::open(&dir.join("voloph.db"))?;
            app.manage(Db(Arc::new(Mutex::new(conn))));
            app.manage(MediaWorker(Arc::new(AtomicBool::new(false))));
            // Loopback HTTP server the player streams recordings from (ADR 0005).
            let endpoint = media::start()?;
            log::info!("playback server listening at {}", endpoint.origin);
            app.manage(endpoint);
            // Resume any media work left unfinished by a previous run (recordings
            // still needing a probe, transcode, or segmentation) so every
            // recording becomes playable and gets its draft timeline.
            spawn_media_worker(app.handle());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            greet,
            scan_folder,
            list_sessions,
            resolve_playback,
            recording_timeline,
            reanalyze_recording,
            playback_endpoint
        ])
        .on_page_load(|webview, payload| {
            if webview.label() == "main" && matches!(payload.event(), PageLoadEvent::Finished) {
                log::info!("main webview finished loading");
                let _ = webview.window().show();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
