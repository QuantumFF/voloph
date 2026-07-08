mod db;
mod export;
mod media;
mod segment;
mod staging;

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
    pub fn mpv_load(_path: String, _start_ms: Option<f64>) -> Result<(), String> {
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

/// Where analysis stages recordings copied off a network-declared mount (ADR
/// 0011). A per-run local scratch area, wiped on launch so an interrupted run
/// leaves no orphaned copies; empty until the first stage.
struct StagingDir(std::path::PathBuf);

/// The active library's folder on this device (ADR 0011), or `None` when the
/// active kind has not been designated yet — the frontend prompts for designation
/// before the first scan. This and every other DB command below is `async` so
/// Tauri runs it on the async runtime's thread pool rather than inline on the GTK
/// main thread.
#[tauri::command]
async fn active_library(db: State<'_, Db>) -> Result<Option<String>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::library_path(&conn).map_err(|e| e.to_string())
}

/// The switcher's state (ADR 0011): every designated library (kind, per-device
/// mount path, declared locality) and which kind is currently active.
#[tauri::command]
async fn library_state(db: State<'_, Db>) -> Result<(Vec<db::Library>, String), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::library_state(&conn).map_err(|e| e.to_string())
}

/// Designate (or re-designate) `folder` as the library of `kind` ('local' or
/// 'shared'), declaring its `mount` locality ('local' or 'network'; ADR 0011).
/// `kind` becomes active. Runs the adoption pass — every already-known recording
/// of this kind under `folder` converts to library-relative identity with its
/// review state intact — then scans the folder so new files appear, and kicks off
/// background media work for anything new.
///
/// A library scan (tree walk + per-file content hashing under the DB lock) would
/// freeze the webview if run inline on the GTK main thread, hence `async`.
#[tauri::command]
async fn designate_library(
    app: AppHandle,
    db: State<'_, Db>,
    kind: String,
    folder: String,
    mount: String,
) -> Result<db::ScanResult, String> {
    let result = {
        let mut conn = db.0.lock().map_err(|e| e.to_string())?;
        db::designate_library(&mut conn, &kind, std::path::Path::new(&folder), &mount)
            .map_err(|e| e.to_string())?;
        db::scan_library(&mut conn).map_err(|e| e.to_string())?
    };
    spawn_media_worker(&app);
    Ok(result)
}

/// Switch the active library to `kind` (ADR 0011). The session list, filters, and
/// review scope to it; switching back and forth loses nothing. Re-scans the newly
/// active library so newly added files appear and starts background media work for
/// it (the worker follows the active library).
#[tauri::command]
async fn switch_library(
    app: AppHandle,
    db: State<'_, Db>,
    kind: String,
) -> Result<db::ScanResult, String> {
    let result = {
        let mut conn = db.0.lock().map_err(|e| e.to_string())?;
        db::set_active_kind(&conn, &kind).map_err(|e| e.to_string())?;
        db::scan_library(&mut conn).map_err(|e| e.to_string())?
    };
    spawn_media_worker(&app);
    Ok(result)
}

/// All sessions with their recordings nested under them.
#[tauri::command]
async fn list_sessions(db: State<'_, Db>) -> Result<Vec<db::Session>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::list_sessions(&conn).map_err(|e| e.to_string())
}

/// Cross-library carry-over offers (ADR 0011): the same content exists in both
/// libraries (a copy) and exactly one side has hand-touched review state. The app
/// offers — never silently — to carry that review to the other copy. Surfaced to
/// the user, who accepts via [`carry_review`] or declines (leaving both untouched).
#[tauri::command]
async fn carry_offers(db: State<'_, Db>) -> Result<Vec<db::CarryOffer>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::carry_offers(&conn).map_err(|e| e.to_string())
}

/// Accept a cross-library carry-over offer (ADR 0011): copy the review state from
/// the copy at `from_path` onto the other-library copy at `to_path`. Returns
/// whether anything was carried.
#[tauri::command]
async fn carry_review(
    db: State<'_, Db>,
    from_path: String,
    to_path: String,
) -> Result<bool, String> {
    let mut conn = db.0.lock().map_err(|e| e.to_string())?;
    db::carry_review(&mut conn, &from_path, &to_path).map_err(|e| e.to_string())
}

/// The sharer label this device signs its bundles with (ADR 0012) — the name the
/// user gives themselves once. `None` until they name themselves. Persisted per
/// device in `meta`.
#[tauri::command]
async fn sharer_label(db: State<'_, Db>) -> Result<Option<String>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::meta_get(&conn, "sharer_label").map_err(|e| e.to_string())
}

/// Share a session as a metadata-only bundle written into the **shared** library
/// (ADR 0012, issue #65). The file lands in the shared root, named by the
/// session's capture day + `sharer_label`, so it is identifiable by both and
/// re-sharing the same session overwrites only this sharer's previous bundle —
/// another sharer's bundle for the same session is untouched. The label is
/// remembered so the user names themselves only once. Refused while the local
/// library is active (recipients cannot reach local files). Returns the bundle's
/// absolute path.
#[tauri::command]
async fn share_session_bundle(
    db: State<'_, Db>,
    session_id: i64,
    sharer_label: String,
) -> Result<String, String> {
    let label = sharer_label.trim().to_string();
    if label.is_empty() {
        return Err("please enter a name to share under".into());
    }
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    if db::active_kind(&conn).map_err(|e| e.to_string())? != "shared" {
        return Err("switch to the shared library to share a session".into());
    }
    let root = db::library_path(&conn)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "the shared library is not designated".to_string())?;
    let bundle = db::build_session_bundle(&conn, session_id, &label)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "session not found".to_string())?;
    db::meta_set(&conn, "sharer_label", &label).map_err(|e| e.to_string())?;

    let file = db::bundle_file_name(&bundle.capture_day, &label);
    let path = std::path::Path::new(&root).join(&file);
    let json = serde_json::to_vec_pretty(&bundle).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| format!("could not write bundle: {e}"))?;
    Ok(path.to_string_lossy().into_owned())
}

/// "Save bundle as…": write the identical session bundle artifact to an arbitrary
/// path the user picks (ADR 0012 fallback). Same bytes as [`share_session_bundle`];
/// available regardless of the active library since it does not touch the shared
/// root.
#[tauri::command]
async fn save_session_bundle_as(
    db: State<'_, Db>,
    session_id: i64,
    sharer_label: String,
    output: String,
) -> Result<(), String> {
    let label = sharer_label.trim().to_string();
    if label.is_empty() {
        return Err("please enter a name to share under".into());
    }
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let bundle = db::build_session_bundle(&conn, session_id, &label)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "session not found".to_string())?;
    db::meta_set(&conn, "sharer_label", &label).map_err(|e| e.to_string())?;
    let json = serde_json::to_vec_pretty(&bundle).map_err(|e| e.to_string())?;
    std::fs::write(&output, json).map_err(|e| format!("could not write bundle: {e}"))?;
    Ok(())
}

/// Receive a session bundle (ADR 0012, issue #66): read the `.vbundle` file at
/// `bundle_path` and apply its review state against this device's shared library.
/// Unknown recordings are registered straight from the bundle after verifying
/// their file (quick hash + size); machine-only local state is replaced silently;
/// hand-touched recordings are returned as `conflicts` for a keep-mine-or-take-
/// theirs choice (nothing changed for them). Files that fail verification are
/// `refused`, named, while the rest of the bundle still applies. Receiving the
/// same bundle twice is a no-op. Refused while the local library is active
/// (bundles live in and resolve against the shared library).
#[tauri::command]
async fn receive_session_bundle(
    db: State<'_, Db>,
    bundle_path: String,
) -> Result<db::ReceiveResult, String> {
    let json = std::fs::read_to_string(&bundle_path)
        .map_err(|e| format!("could not read bundle: {e}"))?;
    let mut conn = db.0.lock().map_err(|e| e.to_string())?;
    if db::active_kind(&conn).map_err(|e| e.to_string())? != "shared" {
        return Err("switch to the shared library to receive a bundle".into());
    }
    db::receive_session_bundle(&mut conn, &json)
}

/// Resolve one keep-mine-or-take-theirs conflict from a received bundle (ADR
/// 0012, issue #66). `path` is the recording's library-relative path (as returned
/// in [`db::ReceiveResult::conflicts`]); `take_theirs` replaces the recipient's
/// whole timeline + annotations with the bundle's, keep-mine leaves it untouched.
/// The bundle is re-read from `bundle_path` so no server-side state is held
/// between the offer and the choice.
#[tauri::command]
async fn resolve_bundle_conflict(
    db: State<'_, Db>,
    bundle_path: String,
    path: String,
    take_theirs: bool,
) -> Result<bool, String> {
    let json = std::fs::read_to_string(&bundle_path)
        .map_err(|e| format!("could not read bundle: {e}"))?;
    let mut conn = db.0.lock().map_err(|e| e.to_string())?;
    db::resolve_bundle_conflict(&mut conn, &json, &path, take_theirs)
}

/// The aspect vocabulary present in the active library's annotations (issue #66),
/// so the moment browser's aspect filter can offer aspects a received bundle
/// imported alongside its seeded list. A user-editable vocabulary, not a fixed
/// enum (CONTEXT.md).
#[tauri::command]
async fn aspect_vocabulary(db: State<'_, Db>) -> Result<Vec<String>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::aspect_vocabulary(&conn).map_err(|e| e.to_string())
}

/// Discover shared bundles dropped into the shared library by other people (ADR
/// 0012, issue #67), each offered by session + sharer label. Your own bundle is
/// never offered back; a declined bundle stops appearing until its sharer
/// re-shares (then it is offered again as an update). The media worker holds the
/// recordings a pending offer covers out of analysis, so accepting the offer's
/// receive skips their probe/segmentation/staging entirely. Called on every
/// scan/refresh.
#[tauri::command]
async fn discover_bundles(db: State<'_, Db>) -> Result<Vec<db::BundleOffer>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    Ok(db::discover_bundles(&conn))
}

/// Decline a discovered bundle offer (ADR 0012, issue #67): record its current
/// on-disk signature so it stops being offered until the sharer re-shares it.
/// The recordings it covered are released back to the analysis queue — the user
/// chose not to take the shared review, so the app segments them itself.
#[tauri::command]
async fn decline_bundle(app: AppHandle, db: State<'_, Db>, bundle_path: String) -> Result<(), String> {
    {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        db::decline_bundle(&conn, &bundle_path)?;
    }
    // Recordings this offer had held back may now need analysis; wake the worker.
    spawn_media_worker(&app);
    Ok(())
}

/// Resolve the draft timeline (rallies + per-region confidence) for the
/// recording at `path` (ADR 0002). While segmentation is still running the
/// `segment_state` is `unknown` and `rallies` is empty; the player polls until
/// it turns `ready`. An unregistered path reports `unknown` with no rallies.
#[tauri::command]
async fn recording_timeline(db: State<'_, Db>, path: String) -> Result<db::Timeline, String> {
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
async fn reanalyze_recording(
    app: AppHandle,
    db: State<'_, Db>,
    path: String,
) -> Result<(), String> {
    {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        db::reset_segmentation(&conn, &path).map_err(|e| e.to_string())?;
    }
    spawn_media_worker(&app);
    Ok(())
}

/// Re-walk the active library for recordings added to it since the last scan
/// (ADR 0011). Idempotent like a fresh scan, and kicks off background media work
/// for anything new. Backs the Refresh action.
#[tauri::command]
async fn rescan_library(app: AppHandle, db: State<'_, Db>) -> Result<db::ScanResult, String> {
    let result = {
        let mut conn = db.0.lock().map_err(|e| e.to_string())?;
        db::scan_library(&mut conn).map_err(|e| e.to_string())?
    };
    spawn_media_worker(&app);
    Ok(result)
}

/// Re-analyze every recording (ADR 0002) — the bulk counterpart of
/// [`reanalyze_recording`], for re-segmenting the whole library after a segmenter
/// change. Discards manual corrections, as a re-analyze does.
#[tauri::command]
async fn reanalyze_all(app: AppHandle, db: State<'_, Db>) -> Result<(), String> {
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
async fn update_rally(
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
async fn add_rally(
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
async fn delete_rally(db: State<'_, Db>, path: String, rally_id: i64) -> Result<bool, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::delete_rally(&conn, &path, rally_id).map_err(|e| e.to_string())
}

/// Set a rally's flag (issue #10 — "this rally matters", the source material for
/// an export reel). Scoped to the recording at `path` like the inline edits;
/// persists immediately so it survives restart. Returns whether a rally was found.
#[tauri::command]
async fn set_rally_flag(
    db: State<'_, Db>,
    path: String,
    rally_id: i64,
    flagged: bool,
) -> Result<bool, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::set_rally_flag(&conn, &path, rally_id, flagged).map_err(|e| e.to_string())
}

/// Drop a verdict annotation at `time_ms` (recording-local) on the recording at
/// `path` (issue #8 — the fast capture path: `good`/`bad`/`mistake`, no pause).
/// Persists immediately, pinned to absolute time so it survives restart; returns
/// the new annotation's id, or `None` when `path` is not a registered recording.
#[tauri::command]
async fn add_annotation(
    db: State<'_, Db>,
    path: String,
    time_ms: i64,
    verdict: String,
) -> Result<Option<i64>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::add_annotation(&conn, &path, time_ms, &verdict).map_err(|e| e.to_string())
}

/// Every verdict annotation on the recording at `path`, in timestamp order, so
/// the player can lay their markers over the timeline strip (issue #8).
#[tauri::command]
async fn recording_annotations(
    db: State<'_, Db>,
    path: String,
) -> Result<Vec<db::Annotation>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::recording_annotations(&conn, &path).map_err(|e| e.to_string())
}

/// Enrich or re-classify one annotation (issue #9): set its verdict, structured
/// aspect, and free-text note. Scoped to the recording at `path`; `aspect`/`note`
/// as given (`None` clears). Returns `false` when the annotation is not found.
#[tauri::command]
async fn update_annotation(
    db: State<'_, Db>,
    path: String,
    id: i64,
    verdict: String,
    aspect: Option<String>,
    note: Option<String>,
) -> Result<bool, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::update_annotation(&conn, &path, id, &verdict, aspect.as_deref(), note.as_deref())
        .map_err(|e| e.to_string())
}

/// Remove one annotation (issue #9). Scoped to the recording at `path`. Returns
/// `false` when the annotation is not found.
#[tauri::command]
async fn delete_annotation(db: State<'_, Db>, path: String, id: i64) -> Result<bool, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::delete_annotation(&conn, &path, id).map_err(|e| e.to_string())
}

/// Cross-session filter over rallies and their annotations (issue #11 — the
/// payoff of the structured data). Every argument is optional and they combine
/// with AND; `verdict`/`aspect` keep rallies containing a matching moment (and
/// attach those moments), `length` (`Some(true)` = long, derived from duration)
/// and `flagged` filter the rally itself. Returns the matching rallies newest
/// session first, each carrying enough context to open the right recording at its
/// timestamp.
#[tauri::command]
async fn filter_moments(
    db: State<'_, Db>,
    verdict: Option<String>,
    aspect: Option<String>,
    length: Option<bool>,
    flagged: Option<bool>,
) -> Result<Vec<db::FilteredRally>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    db::filter_moments(
        &conn,
        verdict.as_deref(),
        aspect.as_deref(),
        length,
        flagged,
    )
    .map_err(|e| e.to_string())
}

/// Render one new MP4 at `output` from a selection of the rallies of the
/// recording at `path` (issue #12 — the Export engine). `rally_ids` picks which
/// rallies; `None` exports **all** of them (the condensed-recording case). The
/// rallies are cut from the source and concatenated in timeline order; the source
/// is never modified. Progress is emitted on [`export::EVENT_PROGRESS`]. The slow
/// ffmpeg run happens off the DB lock (only the timeline read holds it).
#[tauri::command]
async fn export_rallies(
    app: AppHandle,
    db: State<'_, Db>,
    path: String,
    output: String,
    rally_ids: Option<Vec<i64>>,
) -> Result<(), String> {
    let timeline = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        db::recording_timeline(&conn, &path)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "recording is not registered".to_string())?
    };
    // Timeline order is already start_ms-ascending; keep only the selected rallies.
    let cuts: Vec<export::Cut> = timeline
        .rallies
        .iter()
        .filter(|r| rally_ids.as_ref().is_none_or(|ids| ids.contains(&r.id)))
        .map(|r| export::Cut {
            src: 0,
            start_ms: r.start_ms,
            end_ms: r.end_ms,
        })
        .collect();
    export::export(&app, &[&path], &output, &cuts)
}

/// Render one new MP4 at `output` from a selection of a whole session's rallies —
/// the rallies of `paths` (the session's recordings, given in capture order),
/// gaps removed, concatenated across file boundaries into one portable file. Each
/// recording becomes an ffmpeg input; its rallies are cut from it and stitched in,
/// in the order the paths arrive. `rally_ids` picks which rallies (by their unique
/// row id); `None` exports **every** rally (issue #13's condensed session), a
/// selection drives a targeted reel (issue #14 — flagged rallies, rallies with
/// mistakes). Sources are never modified. Progress is emitted on
/// [`export::EVENT_PROGRESS`]. The DB lock is held only for the timeline reads.
#[tauri::command]
async fn export_session(
    app: AppHandle,
    db: State<'_, Db>,
    paths: Vec<String>,
    output: String,
    rally_ids: Option<Vec<i64>>,
) -> Result<(), String> {
    let cuts: Vec<export::Cut> = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        let mut cuts = Vec::new();
        for (src, path) in paths.iter().enumerate() {
            let timeline = db::recording_timeline(&conn, path)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("recording is not registered: {path}"))?;
            // Timeline order is already start_ms-ascending, so appending each
            // recording's rallies in turn yields capture-then-timeline order.
            cuts.extend(
                timeline
                    .rallies
                    .iter()
                    .filter(|r| rally_ids.as_ref().is_none_or(|ids| ids.contains(&r.id)))
                    .map(|r| export::Cut {
                        src,
                        start_ms: r.start_ms,
                        end_ms: r.end_ms,
                    }),
            );
        }
        cuts
    };
    let srcs: Vec<&str> = paths.iter().map(String::as_str).collect();
    export::export(&app, &srcs, &output, &cuts)
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
            // Wipe any staged copies an interrupted run left behind (ADR 0011); the
            // staging area holds only in-flight copies, never durable state.
            let staging_dir = dir.join("staging");
            staging::clean(&staging_dir);
            app.manage(StagingDir(staging_dir));
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
            active_library,
            library_state,
            designate_library,
            switch_library,
            list_sessions,
            carry_offers,
            carry_review,
            sharer_label,
            share_session_bundle,
            save_session_bundle_as,
            receive_session_bundle,
            resolve_bundle_conflict,
            discover_bundles,
            decline_bundle,
            aspect_vocabulary,
            recording_timeline,
            reanalyze_recording,
            rescan_library,
            reanalyze_all,
            update_rally,
            add_rally,
            delete_rally,
            set_rally_flag,
            add_annotation,
            recording_annotations,
            update_annotation,
            delete_annotation,
            filter_moments,
            export_rallies,
            export_session,
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
