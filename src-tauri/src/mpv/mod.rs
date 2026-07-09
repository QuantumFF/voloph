//! Embedded libmpv playback (ADR 0008, ADR 0014).
//!
//! Recordings are decoded and rendered by **libmpv linked in-process**, drawing
//! into a native surface tiled beside the webview UI — not an HTML `<video>`
//! element. This module is the platform-neutral core: the libmpv *client* API
//! (load/seek/pause/speed and the property-change event stream) is identical on
//! every platform, so the Tauri commands and the event loop live here. Only the
//! *surface* — where mpv's video actually lands and how it is slaved to the
//! frontend-reported rect — is platform work, delegated to one backend:
//!
//! - [`linux`]: mpv's OpenGL render API drawing into a `GtkGLArea` overlaid on
//!   the webview (ADR 0008 — `--wid` is unsupported on Wayland).
//! - [`windows`]: a child `HWND` mpv adopts via `--wid` and renders into
//!   itself (ADR 0014 — no render plumbing needed where `--wid` works).
//!
//! Each backend exposes the same two hooks: `init` (create the surface and the
//! configured mpv handle) and [`SurfaceTx`] (rect/show/hide, marshalled to the
//! platform's UI thread).
//!
//! ## Threads
//!
//! libmpv's client API is thread-safe, so the play/pause/load Tauri commands
//! call it directly from their worker threads ([`MpvState`]). Surface
//! manipulation must run on the platform UI thread; [`SurfaceTx`] hides the
//! marshalling (a glib channel on Linux, `run_on_main_thread` on Windows).

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::ptr;
use std::sync::Mutex;

use tauri::{AppHandle, Emitter, Manager, State};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as platform;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
use windows as platform;

pub use platform::SurfaceTx;

// ---------------------------------------------------------------------------
// libmpv FFI (client.h). Only the handful of entry points this app needs are
// declared; layouts mirror the system headers exactly. The render API
// (render_gl.h) is Linux-only and declared in the linux backend.
// ---------------------------------------------------------------------------

#[repr(C)]
pub(crate) struct MpvHandle {
    _private: [u8; 0],
}

// --- Event API (client.h). Only what the playhead/end stream needs. ---

/// `mpv_event` — what `mpv_wait_event` returns. `data` points at an event-specific
/// struct (here either `mpv_event_property` or `mpv_event_end_file`).
#[repr(C)]
struct MpvEvent {
    event_id: c_int,
    error: c_int,
    reply_userdata: u64,
    data: *mut c_void,
}

/// `mpv_event_property` — the payload of a `PROPERTY_CHANGE` event. `data` points
/// at the value in the requested format (a `f64` for `MPV_FORMAT_DOUBLE`).
#[repr(C)]
struct MpvEventProperty {
    name: *const c_char,
    format: c_int,
    data: *mut c_void,
}

/// `mpv_event_end_file` — the payload of an `END_FILE` event.
#[repr(C)]
struct MpvEventEndFile {
    reason: c_int,
    error: c_int,
}

const MPV_FORMAT_DOUBLE: c_int = 5;
// `pause`/`mute` come back as a flag (a `c_int` 0/1), not a double.
const MPV_FORMAT_FLAG: c_int = 3;

const MPV_EVENT_SHUTDOWN: c_int = 1;
const MPV_EVENT_FILE_LOADED: c_int = 8;
const MPV_EVENT_END_FILE: c_int = 7;
const MPV_EVENT_PLAYBACK_RESTART: c_int = 21;
const MPV_EVENT_PROPERTY_CHANGE: c_int = 22;

// `mpv_end_file_reason`: the playthrough reached the end of the file (vs. a
// `stop`/`loadfile` replacing it, or a decode error).
const MPV_END_FILE_REASON_EOF: c_int = 0;
const MPV_END_FILE_REASON_ERROR: c_int = 4;

/// `reply_userdata` tags identifying which observed property a `PROPERTY_CHANGE`
/// event carries. `time-pos` drives the playhead; the other four reconcile the
/// transport controls — the frontend sets `paused`/`speed`/`volume`/`mute` from
/// these events rather than optimistically, so a silently-failed `mpv_set_*`
/// leaves no UI/player divergence (issue #42). Mirrors the `time-pos` loop.
const TIME_POS_USERDATA: u64 = 1;
const PAUSE_USERDATA: u64 = 2;
const SPEED_USERDATA: u64 = 3;
const VOLUME_USERDATA: u64 = 4;
const MUTE_USERDATA: u64 = 5;

/// Recording-local position (ms) the *next* `loadfile` should resume at, applied
/// by the event loop when mpv fires `FILE_LOADED`. Crossing into a recording at a
/// specific point (a click on the session strip, or rally-to-rally navigation)
/// sets this before loading; seeking *with* the load rather than as a separate
/// command avoids racing the seek against the still-loading file (libmpv's
/// `loadfile` is async and drops a seek issued before the file is ready, leaving
/// playback at 0). `None` opens the file at its start. The last load wins.
static PENDING_START_MS: Mutex<Option<f64>> = Mutex::new(None);

// On Linux this resolves via the `-lmpv` emitted by build.rs. On Windows,
// `raw-dylib` makes the linker synthesize the import stubs for libmpv-2.dll
// directly — no import library (.lib/.dll.a) is needed; the DLL itself ships
// beside the exe (fetched by scripts/fetch-libmpv.sh, bundled as a resource).
#[cfg_attr(target_os = "windows", link(name = "libmpv-2", kind = "raw-dylib"))]
extern "C" {
    pub(crate) fn mpv_create() -> *mut MpvHandle;
    pub(crate) fn mpv_initialize(ctx: *mut MpvHandle) -> c_int;
    fn mpv_set_option_string(
        ctx: *mut MpvHandle,
        name: *const c_char,
        data: *const c_char,
    ) -> c_int;
    fn mpv_set_property_string(
        ctx: *mut MpvHandle,
        name: *const c_char,
        data: *const c_char,
    ) -> c_int;
    fn mpv_command(ctx: *mut MpvHandle, args: *const *const c_char) -> c_int;
    fn mpv_error_string(error: c_int) -> *const c_char;

    fn mpv_observe_property(
        ctx: *mut MpvHandle,
        reply_userdata: u64,
        name: *const c_char,
        format: c_int,
    ) -> c_int;
    fn mpv_wait_event(ctx: *mut MpvHandle, timeout: f64) -> *mut MpvEvent;
}

/// Human-readable form of an mpv error code (`mpv_error_string`), for logs.
pub(crate) fn mpv_err(code: c_int) -> String {
    let p = unsafe { mpv_error_string(code) };
    if p.is_null() {
        return format!("error {code}");
    }
    let msg = unsafe { CStr::from_ptr(p) }.to_string_lossy();
    format!("{msg} ({code})")
}

// ---------------------------------------------------------------------------
// State shared with Tauri commands.
// ---------------------------------------------------------------------------

/// The mpv client handle, shared with the play/pause/load commands. libmpv's
/// client API is thread-safe, so the raw pointer is sound to use from the
/// command worker threads.
pub struct MpvState {
    handle: *mut MpvHandle,
}
unsafe impl Send for MpvState {}
unsafe impl Sync for MpvState {}

// ---------------------------------------------------------------------------
// Setup: platform surface + mpv handle, then the shared event loop.
// ---------------------------------------------------------------------------

/// Embed libmpv in the main window and register the playback state/commands.
/// Must run on the platform's UI thread (Tauri's `setup` does).
///
/// The platform backend creates the native surface, creates and configures the
/// mpv handle to render into it, and manages its [`SurfaceTx`]; the shared core
/// then owns the handle ([`MpvState`]) and pumps mpv's event stream.
pub fn init(app: &AppHandle) -> Result<(), String> {
    let handle = platform::init(app)?;

    app.manage(MpvState { handle });

    // Drive the playhead and end/error UI states from mpv's own event stream
    // (ADR 0008): observe `time-pos` and forward each tick — plus end-of-file and
    // decode errors — to the frontend as Tauri events, replacing the webview's
    // `timeupdate` handler and the `seekBaseMs + currentTime` mapping.
    let handle_addr = handle as usize;
    let app_for_events = app.clone();
    std::thread::spawn(move || event_loop(handle_addr, app_for_events));

    log::info!("mpv: embedded surface initialized");
    Ok(())
}

/// Names of the Tauri events the playback event loop emits.
const EVENT_TIME_POS: &str = "mpv:time-pos";
const EVENT_ENDED: &str = "mpv:ended";
const EVENT_ERROR: &str = "mpv:error";
// Transport property-change events the frontend reconciles its controls from
// (issue #42). `pause`/`mute` carry a bool; `speed`/`volume` carry a f64.
const EVENT_PAUSE: &str = "mpv:pause";
const EVENT_SPEED: &str = "mpv:speed";
const EVENT_VOLUME: &str = "mpv:volume";
const EVENT_MUTE: &str = "mpv:mute";
// A new file is open and any resume seek carried by its load has been applied,
// so the next `time-pos` reflects the resumed position. The frontend drops every
// tick between issuing a load and this event, because a tick carries no file
// identity — without the signal a stale tick from the outgoing recording (or a
// pre-seek near-0 from the new one) trips gap-skip against the wrong position.
const EVENT_FILE_LOADED: &str = "mpv:file-loaded";
// mpv finished applying a seek (or started a file) and playback resumed at the
// new position — every `time-pos` from here on reflects the post-seek playhead.
// The frontend drops ticks between issuing `mpv_seek` and this signal, because
// in-flight ticks still carry the *pre-seek* position: acting on one runs
// gap-skip against the spot the user just scrubbed away from (a stale tick in a
// gap yanks the playhead to the rally after the old position; one past the
// session's last rally pauses mid-scrub).
const EVENT_PLAYBACK_RESTART: &str = "mpv:playback-restart";

/// Pump mpv's event stream on a dedicated thread for the whole process lifetime,
/// translating mpv events into Tauri events the player listens to:
///
/// - `time-pos` property changes → `mpv:time-pos` carrying the playhead in ms.
/// - `pause`/`speed`/`volume`/`mute` changes → `mpv:pause`/`…`, so the frontend
///   reconciles its transport controls from the player's real state rather than
///   optimistically (issue #42).
/// - end-of-file → `mpv:ended`; a decode error → `mpv:error`.
///
/// `mpv_wait_event` blocks up to the timeout, so this never busy-spins. The mpv
/// client API is thread-safe, so reading the handle here is sound (the pointer is
/// passed as a `usize` because raw pointers are not `Send`).
fn event_loop(handle_addr: usize, app: AppHandle) {
    let handle = handle_addr as *mut MpvHandle;

    // Ask mpv to push these property changes into the event stream. mpv emits the
    // current value once on observe and on every change after (coalesced to ~its
    // display rate for `time-pos`). `pause`/`mute` are flags, the rest doubles.
    observe(handle, TIME_POS_USERDATA, "time-pos", MPV_FORMAT_DOUBLE);
    observe(handle, PAUSE_USERDATA, "pause", MPV_FORMAT_FLAG);
    observe(handle, SPEED_USERDATA, "speed", MPV_FORMAT_DOUBLE);
    observe(handle, VOLUME_USERDATA, "volume", MPV_FORMAT_DOUBLE);
    observe(handle, MUTE_USERDATA, "mute", MPV_FORMAT_FLAG);

    loop {
        // Block until the next event (1s timeout just to re-check liveness).
        let event = unsafe { mpv_wait_event(handle, 1.0) };
        if event.is_null() {
            continue;
        }
        let event = unsafe { &*event };
        match event.event_id {
            MPV_EVENT_SHUTDOWN => break,
            MPV_EVENT_FILE_LOADED => {
                // The file is now open and seekable, so a resume position carried
                // by the load lands cleanly here — atomic with the load, unlike a
                // seek raced against the still-loading file (which mpv drops).
                let start = PENDING_START_MS.lock().ok().and_then(|mut g| g.take());
                if let Some(ms) = start {
                    let secs = (ms / 1000.0).max(0.0);
                    if let Err(e) =
                        run_command(handle, &["seek", &secs.to_string(), "absolute+exact"])
                    {
                        log::warn!("mpv: resume seek on file-loaded failed: {e}");
                    }
                }
                // Tell the frontend the file is up and its resume seek (if any)
                // has landed, so it can reopen the playhead gate. Emitted after
                // the seek and from this single event-loop thread, so it always
                // reaches the frontend ahead of the new file's first `time-pos`.
                let _ = app.emit(EVENT_FILE_LOADED, ());
            }
            MPV_EVENT_PLAYBACK_RESTART => {
                let _ = app.emit(EVENT_PLAYBACK_RESTART, ());
            }
            MPV_EVENT_PROPERTY_CHANGE => {
                if event.data.is_null() {
                    continue;
                }
                let prop = unsafe { &*(event.data as *const MpvEventProperty) };
                // A null payload means the property is currently unavailable
                // (e.g. between files); skip it rather than emit a bogus value.
                if prop.data.is_null() {
                    continue;
                }
                match event.reply_userdata {
                    TIME_POS_USERDATA => {
                        if let Some(secs) = prop_f64(prop) {
                            let _ = app.emit(EVENT_TIME_POS, secs * 1000.0);
                        }
                    }
                    SPEED_USERDATA => {
                        if let Some(speed) = prop_f64(prop) {
                            let _ = app.emit(EVENT_SPEED, speed);
                        }
                    }
                    VOLUME_USERDATA => {
                        if let Some(volume) = prop_f64(prop) {
                            let _ = app.emit(EVENT_VOLUME, volume);
                        }
                    }
                    PAUSE_USERDATA => {
                        if let Some(paused) = prop_flag(prop) {
                            let _ = app.emit(EVENT_PAUSE, paused);
                        }
                    }
                    MUTE_USERDATA => {
                        if let Some(muted) = prop_flag(prop) {
                            let _ = app.emit(EVENT_MUTE, muted);
                        }
                    }
                    _ => {}
                }
            }
            MPV_EVENT_END_FILE => {
                let reason = if event.data.is_null() {
                    MPV_END_FILE_REASON_EOF
                } else {
                    unsafe { (*(event.data as *const MpvEventEndFile)).reason }
                };
                match reason {
                    MPV_END_FILE_REASON_EOF => {
                        let _ = app.emit(EVENT_ENDED, ());
                    }
                    MPV_END_FILE_REASON_ERROR => {
                        let _ = app.emit(EVENT_ERROR, "playback failed");
                    }
                    // STOP/QUIT/REDIRECT: a deliberate stop or a `loadfile`
                    // replacing the file — not an end state the UI reacts to.
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

/// Ask mpv to push a property's changes into the event stream under `userdata`,
/// logging (but not failing) if the property can't be observed.
fn observe(handle: *mut MpvHandle, userdata: u64, name: &str, format: c_int) {
    let Ok(name_c) = CString::new(name) else {
        return;
    };
    let rc = unsafe { mpv_observe_property(handle, userdata, name_c.as_ptr(), format) };
    if rc < 0 {
        log::warn!("mpv: observe {name} failed: {}", mpv_err(rc));
    }
}

/// Read a `PROPERTY_CHANGE` payload as a `f64`, or `None` if it isn't a double.
fn prop_f64(prop: &MpvEventProperty) -> Option<f64> {
    (prop.format == MPV_FORMAT_DOUBLE).then(|| unsafe { *(prop.data as *const f64) })
}

/// Read a `PROPERTY_CHANGE` payload as a bool flag, or `None` if it isn't a flag.
fn prop_flag(prop: &MpvEventProperty) -> Option<bool> {
    (prop.format == MPV_FORMAT_FLAG).then(|| unsafe { *(prop.data as *const c_int) } != 0)
}

/// Set an mpv option, logging (but not failing) on error — a missing option
/// (e.g. `hwdec` on a build without it) should not abort startup.
pub(crate) fn set_option(handle: *mut MpvHandle, name: &str, value: &str) {
    let (Ok(name_c), Ok(value_c)) = (CString::new(name), CString::new(value)) else {
        return;
    };
    let rc = unsafe { mpv_set_option_string(handle, name_c.as_ptr(), value_c.as_ptr()) };
    if rc < 0 {
        log::warn!("mpv: could not set option {name}={value} ({rc})");
    }
}

// ---------------------------------------------------------------------------
// Tauri commands.
// ---------------------------------------------------------------------------

/// Open a recording directly from disk and start playing it (ADR 0008 — no
/// loopback HTTP). Replaces the current file if one is already loaded. When
/// `start_ms` is given the recording resumes there: it's recorded for the event
/// loop to apply on `FILE_LOADED` (seeking *with* the load, not as a racing
/// follow-up command — see [`PENDING_START_MS`]), so a click that crosses into
/// another recording lands where it clicked instead of at the file's start.
#[tauri::command]
pub fn mpv_load(
    state: State<'_, MpvState>,
    path: String,
    start_ms: Option<f64>,
) -> Result<(), String> {
    // Record the resume position before issuing the load so the `FILE_LOADED`
    // this load triggers sees it. A `None` clears any stale pending start.
    if let Ok(mut pending) = PENDING_START_MS.lock() {
        *pending = start_ms;
    }
    let cmd = CString::new("loadfile").map_err(|e| e.to_string())?;
    let file = CString::new(path).map_err(|e| e.to_string())?;
    let args: [*const c_char; 3] = [cmd.as_ptr(), file.as_ptr(), ptr::null()];
    let rc = unsafe { mpv_command(state.handle, args.as_ptr()) };
    if rc < 0 {
        return Err(format!("mpv loadfile failed: {}", mpv_err(rc)));
    }
    // loadfile starts paused-less; make the intent explicit.
    set_property(state.handle, "pause", "no")
}

/// Drive play/pause from the UI.
#[tauri::command]
pub fn mpv_set_pause(state: State<'_, MpvState>, paused: bool) -> Result<(), String> {
    set_property(state.handle, "pause", if paused { "yes" } else { "no" })
}

/// Seek to an absolute position in the recording (ADR 0008 — native mpv seek, no
/// stream reload). libmpv seeks sparse GOPs without a dense-keyframe requirement,
/// so this works on HEVC originals untouched since import.
#[tauri::command]
pub fn mpv_seek(state: State<'_, MpvState>, ms: f64) -> Result<(), String> {
    let secs = (ms / 1000.0).max(0.0);
    run_command(state.handle, &["seek", &secs.to_string(), "absolute+exact"])
}

/// Step one frame forward (`true`) or back (`false`) using mpv's native
/// frame-stepping — frame-accurate and in sync with the playhead, replacing the
/// deleted JPEG-overlay path.
#[tauri::command]
pub fn mpv_frame_step(state: State<'_, MpvState>, forward: bool) -> Result<(), String> {
    let cmd = if forward {
        "frame-step"
    } else {
        "frame-back-step"
    };
    run_command(state.handle, &[cmd])
}

/// Set the playback speed multiplier (the speed ladder, e.g. 0.25–2.0).
#[tauri::command]
pub fn mpv_set_speed(state: State<'_, MpvState>, speed: f64) -> Result<(), String> {
    set_property(state.handle, "speed", &speed.to_string())
}

/// Set the output volume (0–100).
#[tauri::command]
pub fn mpv_set_volume(state: State<'_, MpvState>, volume: f64) -> Result<(), String> {
    set_property(
        state.handle,
        "volume",
        &volume.clamp(0.0, 100.0).to_string(),
    )
}

/// Mute or unmute the audio.
#[tauri::command]
pub fn mpv_set_mute(state: State<'_, MpvState>, muted: bool) -> Result<(), String> {
    set_property(state.handle, "mute", if muted { "yes" } else { "no" })
}

/// Slave the native surface to the video pane's bounding rect (CSS px), reported
/// by the frontend on mount and on resize.
#[tauri::command]
pub fn mpv_set_rect(tx: State<'_, SurfaceTx>, x: i32, y: i32, w: i32, h: i32) {
    tx.rect(x, y, w, h);
}

/// Reveal the native surface (player view mounted, or a full-area modal closed /
/// the window restored — see [`mpv_suppress_surface`]). Playback is untouched.
#[tauri::command]
pub fn mpv_show(tx: State<'_, SurfaceTx>) {
    tx.show();
}

/// Hide the native surface *without* stopping playback, for the one constraint of
/// the tiled surface (ADR 0008): the webview cannot draw over the video rect, so a
/// full-area HTML overlay (the cheat-sheet) must hide the surface first, and a
/// minimized window must not leave a stray surface. Restore with [`mpv_show`].
///
/// Distinct from [`mpv_hide`], which also `stop`s playback because it is the
/// leave-the-player teardown — here playback continues underneath, paused or not.
/// In-video HUD (e.g. an annotation verdict flash) belongs on mpv's OSD, never an
/// HTML overlay over the video rect, precisely so it does not trip this hide.
#[tauri::command]
pub fn mpv_suppress_surface(tx: State<'_, SurfaceTx>, suppressed: bool) {
    if suppressed {
        tx.hide();
    } else {
        tx.show();
    }
}

/// Hide the native surface and stop playback (back to the session list — no
/// orphan window, and no audio left playing from the unloaded recording).
#[tauri::command]
pub fn mpv_hide(tx: State<'_, SurfaceTx>, state: State<'_, MpvState>) {
    tx.hide();
    let cmd = c"stop";
    let args: [*const c_char; 2] = [cmd.as_ptr(), ptr::null()];
    unsafe { mpv_command(state.handle, args.as_ptr()) };
}

/// Issue an mpv command from string arguments (a NULL-terminated `argv`).
fn run_command(handle: *mut MpvHandle, args: &[&str]) -> Result<(), String> {
    let c_args: Vec<CString> = args
        .iter()
        .map(|a| CString::new(*a))
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    let mut argv: Vec<*const c_char> = c_args.iter().map(|a| a.as_ptr()).collect();
    argv.push(ptr::null());
    let rc = unsafe { mpv_command(handle, argv.as_ptr()) };
    if rc < 0 {
        return Err(format!("mpv command {:?} failed: {}", args, mpv_err(rc)));
    }
    Ok(())
}

fn set_property(handle: *mut MpvHandle, name: &str, value: &str) -> Result<(), String> {
    let (Ok(name_c), Ok(value_c)) = (CString::new(name), CString::new(value)) else {
        return Err("invalid property name/value".into());
    };
    let rc = unsafe { mpv_set_property_string(handle, name_c.as_ptr(), value_c.as_ptr()) };
    if rc < 0 {
        return Err(format!("mpv set {name}={value} failed ({rc})"));
    }
    Ok(())
}
