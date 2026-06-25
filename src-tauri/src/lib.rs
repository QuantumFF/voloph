mod db;
mod media;

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

/// Guard so at most one background transcode worker runs at a time. A worker
/// drains every `unknown`/`pending` recording before exiting, so anything
/// registered while it runs is picked up on the same pass; a scan that arrives
/// after it exits simply starts a fresh worker.
struct TranscodeWorker(Arc<AtomicBool>);

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

/// Register the video files under `folder` as recordings, grouped into
/// sessions by capture day. Idempotent across re-scans. Kicks off background
/// transcoding for any newly registered web-incompatible recordings.
#[tauri::command]
fn scan_folder(app: AppHandle, db: State<'_, Db>, folder: String) -> Result<db::ScanResult, String> {
    let result = {
        let mut conn = db.0.lock().map_err(|e| e.to_string())?;
        db::scan_folder(&mut conn, std::path::Path::new(&folder)).map_err(|e| e.to_string())?
    };
    spawn_transcode_worker(&app);
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

/// Start the background transcode worker unless one is already running. Probes
/// each `unknown` recording (marking it `ready` when already web-playable, else
/// `pending`) and transcodes each `pending` one in place (marking it `ready` or
/// `failed`), without holding the DB lock across the slow ffmpeg work.
fn spawn_transcode_worker(app: &AppHandle) {
    let running = app.state::<TranscodeWorker>().0.clone();
    if running
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return; // a worker is already draining the queue
    }

    let conn = app.state::<Db>().0.clone();
    std::thread::spawn(move || {
        run_transcode_worker(&conn);
        running.store(false, Ordering::SeqCst);
    });
}

/// Drain the transcode queue: probe `unknown` recordings, transcode `pending`
/// ones in place, until none remain. Each DB touch takes the lock only briefly;
/// the ffmpeg probe/transcode runs unlocked so playback queries stay responsive.
fn run_transcode_worker(conn: &Mutex<Connection>) {
    loop {
        let work = match conn.lock() {
            Ok(c) => db::next_transcode_work(&c),
            Err(e) => {
                log::error!("transcode worker: db lock poisoned: {e}");
                return;
            }
        };
        let Some((id, path, state)) = (match work {
            Ok(w) => w,
            Err(e) => {
                log::error!("transcode worker: query failed: {e}");
                return;
            }
        }) else {
            return; // queue empty
        };

        match state.as_str() {
            "unknown" => {
                let next = match media::probe(&path) {
                    Ok(probe) if probe.passthrough => "ready",
                    Ok(_) => "pending",
                    Err(e) => {
                        log::warn!("transcode worker: probe failed for {path}: {e}");
                        "failed"
                    }
                };
                update_state(conn, id, next, &path);
            }
            "pending" => match media::transcode_in_place(&path) {
                Ok(()) => {
                    log::info!("transcode worker: transcoded {path} in place");
                    if let Ok(c) = conn.lock() {
                        if let Err(e) = db::mark_transcoded(&c, id, &path) {
                            log::error!("transcode worker: could not finalize {path}: {e}");
                        }
                    }
                }
                Err(e) => {
                    log::warn!("transcode worker: transcode failed for {path}: {e}");
                    update_state(conn, id, "failed", &path);
                }
            },
            other => {
                log::error!("transcode worker: unexpected state {other} for {path}");
                return;
            }
        }
    }
}

/// Update a recording's transcode state under a brief lock, logging on failure.
fn update_state(conn: &Mutex<Connection>, id: i64, state: &str, path: &str) {
    if let Ok(c) = conn.lock() {
        if let Err(e) = db::set_transcode_state(&c, id, state) {
            log::error!("transcode worker: could not update state for {path}: {e}");
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
            app.manage(TranscodeWorker(Arc::new(AtomicBool::new(false))));
            // Loopback HTTP server the player streams recordings from (ADR 0005).
            let endpoint = media::start()?;
            log::info!("playback server listening at {}", endpoint.origin);
            app.manage(endpoint);
            // Resume any transcode work left unfinished by a previous run
            // (recordings still `unknown`/`pending`) so they are ready to review.
            spawn_transcode_worker(&app.handle());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            greet,
            scan_folder,
            list_sessions,
            resolve_playback,
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
