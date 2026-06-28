mod db;
mod media;
mod segment;

// Embedded libmpv playback (ADR 0008). Linux-only — it links libmpv and drives
// GTK directly; other targets get inert stubs so the crate still builds.
#[cfg(target_os = "linux")]
mod mpv;
#[cfg(not(target_os = "linux"))]
mod mpv {
    pub fn init(_app: &tauri::AppHandle) -> Result<(), String> {
        Ok(())
    }
    #[tauri::command]
    pub fn mpv_load(_path: String) -> Result<(), String> {
        Err("embedded playback is only available on Linux".into())
    }
    #[tauri::command]
    pub fn mpv_set_pause(_paused: bool) -> Result<(), String> {
        Err("embedded playback is only available on Linux".into())
    }
    #[tauri::command]
    pub fn mpv_set_rect(_x: i32, _y: i32, _w: i32, _h: i32) {}
    #[tauri::command]
    pub fn mpv_show() {}
    #[tauri::command]
    pub fn mpv_hide() {}
    #[tauri::command]
    pub fn mpv_suppress_surface(_suppressed: bool) {}
    #[tauri::command]
    pub fn mpv_seek(_ms: f64) -> Result<(), String> {
        Err("embedded playback is only available on Linux".into())
    }
    #[tauri::command]
    pub fn mpv_frame_step(_forward: bool) -> Result<(), String> {
        Err("embedded playback is only available on Linux".into())
    }
    #[tauri::command]
    pub fn mpv_set_speed(_speed: f64) -> Result<(), String> {
        Err("embedded playback is only available on Linux".into())
    }
    #[tauri::command]
    pub fn mpv_set_volume(_volume: f64) -> Result<(), String> {
        Err("embedded playback is only available on Linux".into())
    }
    #[tauri::command]
    pub fn mpv_set_mute(_muted: bool) -> Result<(), String> {
        Err("embedded playback is only available on Linux".into())
    }
}

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use serde::Serialize;
use tauri::webview::PageLoadEvent;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_log::{Target, TargetKind};
use tauri_plugin_opener::OpenerExt;

/// The single SQLite metadata connection. An `Arc` so the background media
/// worker can share it with the Tauri commands.
struct Db(Arc<Mutex<Connection>>);

/// Guard so at most one background media worker runs at a time. A worker drains
/// every pending unit of media work — probe (capture date + fps), then segment
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
/// media work (probe then segment) for any newly registered recordings.
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

/// What the player needs to render a recording: the file to load (always the
/// recording's own path — libmpv opens originals directly, ADR 0008) and its
/// playability state.
#[derive(Serialize)]
struct PlaybackSource {
    /// Absolute path to load. The recording's own path — originals are never
    /// modified or proxied (ADR 0008).
    path: String,
    /// Playability state: `ready` is playable now (every recording becomes ready
    /// as soon as it is probed, since libmpv decodes any codec); `unknown` is the
    /// brief window before its probe runs; `failed` means the probe could not read
    /// the file.
    state: String,
    /// Probed frame rate (issue #19) so the player can frame-step exactly, even
    /// before segmentation. `null` when unknown; the player defaults to 30 fps.
    fps: Option<f64>,
}

/// Resolve how to play the recording at `path`. libmpv opens the original
/// directly and decodes any codec (ADR 0008), so a recording is playable as soon
/// as it has been probed; the `state` only reflects whether the probe has run.
#[tauri::command]
fn resolve_playback(db: State<'_, Db>, path: String) -> Result<PlaybackSource, String> {
    let (state, fps) = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        let state = db::recording_transcode_state(&conn, &path).map_err(|e| e.to_string())?;
        let fps = db::recording_fps(&conn, &path).map_err(|e| e.to_string())?;
        (state, fps)
    };
    Ok(PlaybackSource {
        // Not registered (e.g. played straight from a dialog) — assume playable.
        state: state.unwrap_or_else(|| "ready".to_string()),
        fps,
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
        waveform: Vec::new(),
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

/// Re-walk every folder a previous scan registered, picking up recordings added
/// to them since (see [`db::scanned_folders`]). Idempotent like a fresh scan, and
/// kicks off background media work for anything new. Backs the Refresh action.
#[tauri::command]
fn rescan_folders(app: AppHandle, db: State<'_, Db>) -> Result<db::ScanResult, String> {
    let result = {
        let mut conn = db.0.lock().map_err(|e| e.to_string())?;
        let folders = db::scanned_folders(&conn).map_err(|e| e.to_string())?;
        let mut total = db::ScanResult {
            registered: 0,
            skipped: 0,
        };
        for folder in folders {
            let r = db::scan_folder(&mut conn, std::path::Path::new(&folder))
                .map_err(|e| e.to_string())?;
            total.registered += r.registered;
            total.skipped += r.skipped;
        }
        total
    };
    spawn_media_worker(&app);
    Ok(result)
}

/// Re-analyze every recording (ADR 0002) — the bulk counterpart of
/// [`reanalyze_recording`], for re-segmenting the whole library after a segmenter
/// change. Discards manual corrections, as a re-analyze does.
#[tauri::command]
fn reanalyze_all(app: AppHandle, db: State<'_, Db>) -> Result<(), String> {
    {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        db::reset_all_segmentation(&conn).map_err(|e| e.to_string())?;
    }
    spawn_media_worker(&app);
    Ok(())
}

/// Move a rally's boundaries in the draft timeline (issue #7). Backs the
/// adjust-boundary correction directly, and the split and merge corrections
/// indirectly (the frontend composes those from update + add/delete). Persists
/// immediately so gap-free playback reflects the corrected timeline on its next
/// read, with no reload. Returns whether a rally was actually updated.
#[tauri::command]
fn update_rally(
    db: State<'_, Db>,
    path: String,
    rally_id: i64,
    start_ms: i64,
    end_ms: i64,
) -> Result<bool, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::update_rally(&conn, &path, rally_id, start_ms, end_ms).map_err(|e| e.to_string())
}

/// Create a rally over a span the segmenter missed (issue #7 — the add
/// correction, and the new half of a split). Persists immediately; returns the
/// new rally's id, or `None` when `path` is not a registered recording.
#[tauri::command]
fn add_rally(
    db: State<'_, Db>,
    path: String,
    start_ms: i64,
    end_ms: i64,
) -> Result<Option<i64>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::add_rally(&conn, &path, start_ms, end_ms).map_err(|e| e.to_string())
}

/// Remove a rally from the draft timeline (issue #7 — delete a false positive,
/// whose span then becomes a derived gap; also the discarded half of a merge).
/// Persists immediately. Returns whether a rally was actually removed.
#[tauri::command]
fn delete_rally(db: State<'_, Db>, path: String, rally_id: i64) -> Result<bool, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::delete_rally(&conn, &path, rally_id).map_err(|e| e.to_string())
}

/// Start the background media worker unless one is already running. It drains
/// every pending unit of work — probe each `unknown` recording for its frame
/// rate, then segment each unsegmented one (ADR 0002) — without holding the DB
/// lock across the slow ffmpeg/segment work, so playback and timeline queries
/// stay responsive.
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
/// Each DB touch takes the lock only briefly; the ffmpeg probe and the audio
/// extraction + segmentation run unlocked.
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
            db::MediaWork::CaptureDate(id, path) => refine_recording_date(conn, id, &path),
            db::MediaWork::Probe(id, path) => {
                // libmpv plays any codec and seeks sparse GOPs (ADR 0008), so the
                // recording is playable immediately — the probe only captures the
                // frame rate the player frame-steps by (issue #19) and marks the
                // recording `ready`. A probe failure marks it `failed` instead.
                let (next, fps) = match media::probe(&path) {
                    Ok(probe) => ("ready", probe.fps),
                    Err(e) => {
                        log::warn!("media worker: probe failed for {path}: {e}");
                        ("failed", None)
                    }
                };
                if let Ok(c) = conn.lock() {
                    if let Err(e) = db::set_probe_result(&c, id, next, fps) {
                        log::error!("media worker: could not record probe for {path}: {e}");
                    }
                }
            }
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
    // The displayed waveform for the timeline strip (issue #6), reduced from the
    // same samples while we still hold them in memory.
    let waveform = segment::waveform(&samples);
    let duration_ms =
        (samples.len() as f64 / media::SEGMENT_SAMPLE_RATE as f64 * 1000.0) as i64;
    log::info!(
        "media worker: segmented {path} into {} rallies ({duration_ms} ms)",
        rallies.len()
    );

    match conn.lock() {
        Ok(mut c) => {
            if let Err(e) = db::save_rallies(&mut c, id, duration_ms, &rallies, &waveform) {
                log::error!("media worker: could not save timeline for {path}: {e}");
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
    // WebKitGTK's DMA-BUF renderer is broken on the NVIDIA proprietary driver
    // under Wayland: GPU-composited frames lose their pacing, so continuous CSS
    // animations (e.g. the "Converting…"/"Analyzing…" spinners) visibly stutter.
    // Forcing the renderer off restores smooth frame pacing. Gated on the nvidia
    // kernel module being loaded so Intel/AMD users keep the accelerated path,
    // and skipped if the user already set the variable themselves. Must run
    // before any GTK/WebKit thread starts, hence the very top of run().
    #[cfg(target_os = "linux")]
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none()
        && std::path::Path::new("/sys/module/nvidia").exists()
    {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }

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
            // still needing a probe or segmentation) so every recording gets its
            // frame rate and draft timeline.
            spawn_media_worker(app.handle());
            // Embed libmpv in the main window for native playback (ADR 0008).
            // Must run on the GTK main thread, which `setup` is.
            if let Err(e) = mpv::init(app.handle()) {
                log::error!("mpv: embedding failed: {e}");
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            greet,
            scan_folder,
            list_sessions,
            resolve_playback,
            recording_timeline,
            reanalyze_recording,
            rescan_folders,
            reanalyze_all,
            update_rally,
            add_rally,
            delete_rally,
            playback_endpoint,
            mpv::mpv_load,
            mpv::mpv_set_pause,
            mpv::mpv_set_rect,
            mpv::mpv_show,
            mpv::mpv_hide,
            mpv::mpv_suppress_surface,
            mpv::mpv_seek,
            mpv::mpv_frame_step,
            mpv::mpv_set_speed,
            mpv::mpv_set_volume,
            mpv::mpv_set_mute
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
