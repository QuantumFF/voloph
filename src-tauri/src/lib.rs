mod db;
mod media;

use std::sync::Mutex;

use tauri::webview::PageLoadEvent;
use tauri::{Manager, State};
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_log::{Target, TargetKind};

/// The single SQLite metadata connection, guarded for cross-command access.
struct Db(Mutex<rusqlite::Connection>);

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

/// Register the video files under `folder` as recordings, grouped into
/// sessions by capture day. Idempotent across re-scans.
#[tauri::command]
fn scan_folder(db: State<'_, Db>, folder: String) -> Result<db::ScanResult, String> {
    let mut conn = db.0.lock().map_err(|e| e.to_string())?;
    db::scan_folder(&mut conn, std::path::Path::new(&folder)).map_err(|e| e.to_string())
}

/// All sessions with their recordings nested under them.
#[tauri::command]
fn list_sessions(db: State<'_, Db>) -> Result<Vec<db::Session>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::list_sessions(&conn).map_err(|e| e.to_string())
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
        .plugin(tauri_plugin_shell::init())
        .plugin(external_navigation_plugin())
        // Player playback source: probe + passthrough-or-transcode via ffmpeg.
        .register_asynchronous_uri_scheme_protocol(media::SCHEME, |ctx, request, responder| {
            media::handle(ctx.app_handle().clone(), request, responder);
        })
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            let conn = db::open(&dir.join("voloph.db"))?;
            app.manage(Db(Mutex::new(conn)));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![greet, scan_folder, list_sessions])
        .on_page_load(|webview, payload| {
            if webview.label() == "main" && matches!(payload.event(), PageLoadEvent::Finished) {
                log::info!("main webview finished loading");
                let _ = webview.window().show();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
