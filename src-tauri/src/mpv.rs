//! Embedded libmpv playback (ADR 0008).
//!
//! Recordings are decoded and rendered by **libmpv linked in-process**, drawing
//! into a native `GtkGLArea` overlaid on the Tauri webview — not an HTML
//! `<video>` element. This is the tracer slice (issue #34): one recording,
//! play/pause only. Seeking, frame-step, speed and the session orchestration are
//! later slices.
//!
//! ## Why the OpenGL render API rather than `--wid`
//!
//! mpv can embed into a foreign window via `--wid` on X11, but this host runs
//! Wayland (where `--wid` is unsupported). So we drive mpv's **render API**: mpv
//! decodes and hands us frames to draw with OpenGL into a `GtkGLArea` we own.
//! GTK composites that area *above* the webview (ADR 0008 "Family A": the webview
//! never draws over the video rect), and the GL path stays usable on NVIDIA even
//! though WebKitGTK's own DMA-BUF renderer is disabled here (ADR + issue #33).
//!
//! ## Threads
//!
//! libmpv's client API is thread-safe, so the play/pause/load Tauri commands call
//! it directly from their worker threads ([`MpvState`]). Everything touching GTK
//! (showing, moving and rendering the `GtkGLArea`) must run on the GTK main
//! thread, so it is funnelled through a glib channel ([`SurfaceTx`]) whose
//! receiver is attached to the main loop in [`init`]. mpv's render-update
//! callback (which fires from mpv's own thread when a new frame is ready) posts a
//! `Render` message down the same channel, which calls `queue_render` on the main
//! thread.

#![cfg(target_os = "linux")]

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::ptr;
use std::sync::{Mutex, OnceLock};

use gtk::prelude::*;
use tauri::{AppHandle, Emitter, Manager, State};

// ---------------------------------------------------------------------------
// libmpv FFI (client.h + render_gl.h). Only the handful of entry points this
// slice needs are declared; layouts mirror the system headers exactly.
// ---------------------------------------------------------------------------

#[repr(C)]
struct MpvHandle {
    _private: [u8; 0],
}
#[repr(C)]
struct MpvRenderContext {
    _private: [u8; 0],
}

#[repr(C)]
struct MpvRenderParam {
    id: c_int,
    data: *mut c_void,
}

#[repr(C)]
struct MpvOpenglInitParams {
    get_proc_address: extern "C" fn(*mut c_void, *const c_char) -> *mut c_void,
    get_proc_address_ctx: *mut c_void,
}

#[repr(C)]
struct MpvOpenglFbo {
    fbo: c_int,
    w: c_int,
    h: c_int,
    internal_format: c_int,
}

const MPV_RENDER_PARAM_INVALID: c_int = 0;
const MPV_RENDER_PARAM_API_TYPE: c_int = 1;
const MPV_RENDER_PARAM_OPENGL_INIT_PARAMS: c_int = 2;
const MPV_RENDER_PARAM_OPENGL_FBO: c_int = 3;
const MPV_RENDER_PARAM_FLIP_Y: c_int = 4;

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

const MPV_EVENT_SHUTDOWN: c_int = 1;
const MPV_EVENT_END_FILE: c_int = 7;
const MPV_EVENT_PROPERTY_CHANGE: c_int = 22;

// `mpv_end_file_reason`: the playthrough reached the end of the file (vs. a
// `stop`/`loadfile` replacing it, or a decode error).
const MPV_END_FILE_REASON_EOF: c_int = 0;
const MPV_END_FILE_REASON_ERROR: c_int = 4;

/// `reply_userdata` tag for the `time-pos` observation, distinguishing it from
/// any other observed property in the event stream.
const TIME_POS_USERDATA: u64 = 1;

// libmpv refuses to initialize unless `LC_NUMERIC` is the C locale (it parses
// floats with `.` decimals); GTK sets a localized one, so we pin it before
// creating the handle. `LC_NUMERIC` is 1 on glibc.
const LC_NUMERIC: c_int = 1;
extern "C" {
    fn setlocale(category: c_int, locale: *const c_char) -> *mut c_char;
}

extern "C" {
    fn mpv_create() -> *mut MpvHandle;
    fn mpv_initialize(ctx: *mut MpvHandle) -> c_int;
    fn mpv_set_option_string(ctx: *mut MpvHandle, name: *const c_char, data: *const c_char) -> c_int;
    fn mpv_set_property_string(ctx: *mut MpvHandle, name: *const c_char, data: *const c_char) -> c_int;
    fn mpv_command(ctx: *mut MpvHandle, args: *const *const c_char) -> c_int;
    fn mpv_error_string(error: c_int) -> *const c_char;

    fn mpv_observe_property(
        ctx: *mut MpvHandle,
        reply_userdata: u64,
        name: *const c_char,
        format: c_int,
    ) -> c_int;
    fn mpv_wait_event(ctx: *mut MpvHandle, timeout: f64) -> *mut MpvEvent;

    fn mpv_render_context_create(
        res: *mut *mut MpvRenderContext,
        mpv: *mut MpvHandle,
        params: *mut MpvRenderParam,
    ) -> c_int;
    fn mpv_render_context_set_update_callback(
        ctx: *mut MpvRenderContext,
        callback: extern "C" fn(*mut c_void),
        callback_ctx: *mut c_void,
    );
    fn mpv_render_context_render(ctx: *mut MpvRenderContext, params: *mut MpvRenderParam) -> c_int;
}

// ---------------------------------------------------------------------------
// GL symbol resolution. mpv asks us to resolve GL entry points by name. The
// right resolver is the platform's GL loader: this host is Wayland/EGL, where
// `eglGetProcAddress` resolves both core and extension functions on NVIDIA
// (which advertises EGL_KHR_get_all_proc_addresses). We do *not* dlsym out of
// libepoxy — it exposes the GL names only as header macros over `epoxy_*`
// dispatchers, so `dlsym(libepoxy, "glGetString")` is NULL and mpv would report
// the GL backend unusable. A global `dlsym` is kept as a fallback for any symbol
// EGL misses. Both are resolved out of the already-loaded driver libraries via
// `RTLD_DEFAULT`, so no extra link dependency is needed.
// ---------------------------------------------------------------------------

const GL_FRAMEBUFFER_BINDING: u32 = 0x8CA6;
/// `RTLD_DEFAULT` is the null handle on glibc — search every loaded object.
const RTLD_DEFAULT: *mut c_void = ptr::null_mut();

extern "C" {
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

/// `eglGetProcAddress`, resolved once from the loaded EGL library.
unsafe fn egl_get_proc_address(name: *const c_char) -> *mut c_void {
    type GetProc = unsafe extern "C" fn(*const c_char) -> *mut c_void;
    static ADDR: OnceLock<usize> = OnceLock::new();
    let addr = *ADDR.get_or_init(|| {
        dlsym(RTLD_DEFAULT, c"eglGetProcAddress".as_ptr()) as usize
    });
    if addr == 0 {
        return ptr::null_mut();
    }
    let func: GetProc = std::mem::transmute(addr);
    func(name)
}

/// Resolve a GL function by NUL-terminated C name: EGL loader first, global
/// `dlsym` fallback.
unsafe fn gl_symbol(name: *const c_char) -> *mut c_void {
    let via_egl = egl_get_proc_address(name);
    if !via_egl.is_null() {
        return via_egl;
    }
    dlsym(RTLD_DEFAULT, name)
}

/// Human-readable form of an mpv error code (`mpv_error_string`), for logs.
fn mpv_err(code: c_int) -> String {
    let p = unsafe { mpv_error_string(code) };
    if p.is_null() {
        return format!("error {code}");
    }
    let msg = unsafe { CStr::from_ptr(p) }.to_string_lossy();
    format!("{msg} ({code})")
}

/// mpv's OpenGL `get_proc_address` callback.
extern "C" fn get_proc_address(_ctx: *mut c_void, name: *const c_char) -> *mut c_void {
    if name.is_null() {
        return ptr::null_mut();
    }
    unsafe { gl_symbol(name) }
}

/// Query the FBO id GtkGLArea has bound for this frame, via `glGetIntegerv`.
fn current_framebuffer() -> c_int {
    type GetIntegerv = unsafe extern "C" fn(u32, *mut c_int);
    let sym = unsafe { gl_symbol(c"glGetIntegerv".as_ptr()) };
    if sym.is_null() {
        return 0;
    }
    let func: GetIntegerv = unsafe { std::mem::transmute(sym) };
    let mut value: c_int = 0;
    unsafe { func(GL_FRAMEBUFFER_BINDING, &mut value) };
    value
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

/// Messages to the GTK main thread that manipulate the video surface. The
/// receiver (attached to the main loop in [`init`]) owns the `GtkGLArea`.
enum SurfaceMsg {
    /// mpv has a new frame ready — ask the area to redraw.
    Render,
    /// Slave the surface to the video pane's bounding rect (CSS px, top-left
    /// origin), reported by the frontend's `ResizeObserver`.
    Rect { x: i32, y: i32, w: i32, h: i32 },
    /// Reveal the surface (player view mounted).
    Show,
    /// Hide the surface (back to the session list — no orphan window).
    Hide,
}

/// Sender end of the surface channel, stored in Tauri state. `glib::Sender` is
/// `Send` but not `Sync`, so it is wrapped in a `Mutex` to be shareable state.
pub struct SurfaceTx(Mutex<glib::Sender<SurfaceMsg>>);

impl SurfaceTx {
    fn send(&self, msg: SurfaceMsg) {
        if let Ok(tx) = self.0.lock() {
            let _ = tx.send(msg);
        }
    }
}

/// Boxed-and-leaked sender pointer handed to mpv's render-update callback as its
/// opaque context. Leaked because the callback may fire for the whole process
/// lifetime.
extern "C" fn on_mpv_render_update(ctx: *mut c_void) {
    let tx = unsafe { &*(ctx as *const glib::Sender<SurfaceMsg>) };
    let _ = tx.send(SurfaceMsg::Render);
}

// ---------------------------------------------------------------------------
// Setup: create mpv, build the overlaid GtkGLArea, wire rendering.
// ---------------------------------------------------------------------------

/// Embed libmpv in the main window and register the playback state/commands.
/// Must run on the GTK main thread (Tauri's `setup` does).
///
/// Re-parents the webview's container into a `GtkOverlay` and adds a
/// `GtkGLArea` as an overlay child positioned with margins, so it can be slaved
/// to the frontend-reported video rect and composited above the webview.
pub fn init(app: &AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or("main window not found")?;
    let gtk_window = window.gtk_window().map_err(|e| e.to_string())?;
    let vbox = window.default_vbox().map_err(|e| e.to_string())?;

    // Pin LC_NUMERIC to C for the process — libmpv requires it (see above).
    if let Ok(c) = CString::new("C") {
        unsafe { setlocale(LC_NUMERIC, c.as_ptr()) };
    }

    // Create and initialize mpv. The render API is selected later by creating a
    // render context; here we only set client-level options.
    let handle = unsafe { mpv_create() };
    if handle.is_null() {
        return Err("mpv_create returned null".into());
    }
    // Route video through the render API into our GtkGLArea. `vo=libmpv` is the
    // built-in output that draws via the render context we create below; without
    // it mpv keeps its default `vo=gpu` and opens its *own* window, ignoring our
    // surface (creating a render context alone does not redirect output in
    // libmpv 2.x). `force-window=no` keeps mpv from ever spawning a window.
    set_option(handle, "vo", "libmpv");
    set_option(handle, "force-window", "no");
    // Hardware decoding where available (NVIDIA), and keep mpv off the terminal.
    set_option(handle, "hwdec", "auto-safe");
    set_option(handle, "terminal", "no");
    if unsafe { mpv_initialize(handle) } < 0 {
        return Err("mpv_initialize failed".into());
    }

    // Pull the webview out of Tauri's vbox and make it the base child of a
    // GtkOverlay, with our GLArea as the overlay child:
    //
    //   GtkApplicationWindow → GtkOverlay → { WebKitWebView (base), GLArea }
    //
    // The webview must stay *exactly two levels* below the window: tauri's wry
    // runtime attaches a resize handler that walks two parents up from the
    // webview and `downcast`s to `GtkWindow` with an `unwrap` (it does this on
    // every Linux webview, decorated or not). Wrapping the vbox in the overlay
    // added a third level and panicked on the first click; reparenting the
    // webview directly under the overlay keeps the two-level invariant.
    let webview_widget = vbox
        .children()
        .into_iter()
        .next()
        .ok_or("webview not found in the window's vbox")?;
    gtk_window.remove(&vbox);
    vbox.remove(&webview_widget);

    let overlay = gtk::Overlay::new();
    overlay.add(&webview_widget);

    let gl_area = gtk::GLArea::new();
    gl_area.set_halign(gtk::Align::Start);
    gl_area.set_valign(gtk::Align::Start);
    gl_area.set_size_request(16, 16);
    // The area is shown with the overlay so it realizes as soon as the window
    // maps — which creates the mpv render context *before* any file can load.
    // That ordering is load-bearing: if `loadfile` reaches mpv before the render
    // context exists, mpv has no render output and falls back to its default
    // `vo=gpu`, opening its *own* window instead of drawing into our surface.
    // The realize handler hides it again once the context exists; the player
    // view reveals it via `mpv_show`.
    overlay.add_overlay(&gl_area);

    gtk_window.add(&overlay);
    overlay.show_all();

    // The render context is created once the GL context is realized, and shared
    // with the render handler. Both closures run on the GTK main thread, so an
    // `Rc<Cell<…>>` is sound.
    let render_ctx: std::rc::Rc<std::cell::Cell<*mut MpvRenderContext>> =
        std::rc::Rc::new(std::cell::Cell::new(ptr::null_mut()));

    // Channel for main-thread surface ops. The sender is cloned for mpv's render
    // callback (boxed + leaked so it outlives this scope). glib's sync channel is
    // deprecated in favour of async-channel, but suffices for this slice's
    // fire-and-forget surface messages.
    #[allow(deprecated)]
    let (tx, rx) = glib::MainContext::channel::<SurfaceMsg>(glib::Priority::DEFAULT);
    let render_tx: &'static glib::Sender<SurfaceMsg> = Box::leak(Box::new(tx.clone()));

    {
        let render_ctx = render_ctx.clone();
        gl_area.connect_realize(move |area| {
            // Realize can fire again after a hide/show cycle; the render context
            // is created once and reused, so skip if it already exists (creating
            // it twice on one mpv handle errors and would leak the first).
            if !render_ctx.get().is_null() {
                return;
            }
            area.make_current();
            if let Some(err) = area.error() {
                log::error!("mpv: GLArea realize failed: {err}");
                return;
            }
            let api = CString::new("opengl").expect("static str");
            let mut init_params = MpvOpenglInitParams {
                get_proc_address,
                get_proc_address_ctx: ptr::null_mut(),
            };
            let mut params = [
                MpvRenderParam {
                    id: MPV_RENDER_PARAM_API_TYPE,
                    data: api.as_ptr() as *mut c_void,
                },
                MpvRenderParam {
                    id: MPV_RENDER_PARAM_OPENGL_INIT_PARAMS,
                    data: &mut init_params as *mut _ as *mut c_void,
                },
                MpvRenderParam {
                    id: MPV_RENDER_PARAM_INVALID,
                    data: ptr::null_mut(),
                },
            ];
            let mut ctx: *mut MpvRenderContext = ptr::null_mut();
            let rc = unsafe { mpv_render_context_create(&mut ctx, handle, params.as_mut_ptr()) };
            if rc < 0 || ctx.is_null() {
                log::error!("mpv: render context creation failed: {}", mpv_err(rc));
                return;
            }
            unsafe {
                mpv_render_context_set_update_callback(
                    ctx,
                    on_mpv_render_update,
                    render_tx as *const _ as *mut c_void,
                );
            }
            render_ctx.set(ctx);
            log::info!("mpv: render context created");
            // Realized only to create the context eagerly; start hidden until the
            // player view calls `mpv_show`. Hiding unmaps but does not unrealize,
            // so the context (and this GL context) persist.
            area.hide();
        });
    }

    {
        let render_ctx = render_ctx.clone();
        gl_area.connect_render(move |area, _gl_ctx| {
            let ctx = render_ctx.get();
            if ctx.is_null() {
                return glib::Propagation::Proceed;
            }
            let scale = area.scale_factor();
            let mut fbo = MpvOpenglFbo {
                fbo: current_framebuffer(),
                w: area.allocated_width() * scale,
                h: area.allocated_height() * scale,
                internal_format: 0,
            };
            // GtkGLArea's framebuffer has a top-left origin, opposite mpv's GL
            // default, so flip vertically.
            let mut flip: c_int = 1;
            let mut params = [
                MpvRenderParam {
                    id: MPV_RENDER_PARAM_OPENGL_FBO,
                    data: &mut fbo as *mut _ as *mut c_void,
                },
                MpvRenderParam {
                    id: MPV_RENDER_PARAM_FLIP_Y,
                    data: &mut flip as *mut _ as *mut c_void,
                },
                MpvRenderParam {
                    id: MPV_RENDER_PARAM_INVALID,
                    data: ptr::null_mut(),
                },
            ];
            unsafe { mpv_render_context_render(ctx, params.as_mut_ptr()) };
            glib::Propagation::Stop
        });
    }

    // Drain surface messages on the GTK main thread. The receiver owns `gl_area`.
    rx.attach(None, move |msg| {
        match msg {
            SurfaceMsg::Render => gl_area.queue_render(),
            SurfaceMsg::Rect { x, y, w, h } => {
                gl_area.set_margin_start(x);
                gl_area.set_margin_top(y);
                gl_area.set_size_request(w.max(1), h.max(1));
            }
            SurfaceMsg::Show => gl_area.show(),
            SurfaceMsg::Hide => gl_area.hide(),
        }
        glib::ControlFlow::Continue
    });

    app.manage(MpvState { handle });
    app.manage(SurfaceTx(Mutex::new(tx)));

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

/// Pump mpv's event stream on a dedicated thread for the whole process lifetime,
/// translating mpv events into Tauri events the player listens to:
///
/// - `time-pos` property changes → `mpv:time-pos` carrying the playhead in ms.
/// - end-of-file → `mpv:ended`; a decode error → `mpv:error`.
///
/// `mpv_wait_event` blocks up to the timeout, so this never busy-spins. The mpv
/// client API is thread-safe, so reading the handle here is sound (the pointer is
/// passed as a `usize` because raw pointers are not `Send`).
fn event_loop(handle_addr: usize, app: AppHandle) {
    let handle = handle_addr as *mut MpvHandle;

    // Ask mpv to push `time-pos` changes into the event stream as doubles
    // (seconds). mpv coalesces these to roughly its display rate.
    if let Ok(name) = CString::new("time-pos") {
        let rc =
            unsafe { mpv_observe_property(handle, TIME_POS_USERDATA, name.as_ptr(), MPV_FORMAT_DOUBLE) };
        if rc < 0 {
            log::warn!("mpv: observe time-pos failed: {}", mpv_err(rc));
        }
    }

    loop {
        // Block until the next event (1s timeout just to re-check liveness).
        let event = unsafe { mpv_wait_event(handle, 1.0) };
        if event.is_null() {
            continue;
        }
        let event = unsafe { &*event };
        match event.event_id {
            MPV_EVENT_SHUTDOWN => break,
            MPV_EVENT_PROPERTY_CHANGE if event.reply_userdata == TIME_POS_USERDATA => {
                if event.data.is_null() {
                    continue;
                }
                let prop = unsafe { &*(event.data as *const MpvEventProperty) };
                // A null payload means the property is currently unavailable
                // (e.g. between files); skip it rather than emit a bogus 0.
                if prop.format != MPV_FORMAT_DOUBLE || prop.data.is_null() {
                    continue;
                }
                let secs = unsafe { *(prop.data as *const f64) };
                let _ = app.emit(EVENT_TIME_POS, secs * 1000.0);
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

/// Set an mpv option, logging (but not failing) on error — a missing option
/// (e.g. `hwdec` on a build without it) should not abort startup.
fn set_option(handle: *mut MpvHandle, name: &str, value: &str) {
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
/// loopback HTTP). Replaces the current file if one is already loaded.
#[tauri::command]
pub fn mpv_load(state: State<'_, MpvState>, path: String) -> Result<(), String> {
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
    let cmd = if forward { "frame-step" } else { "frame-back-step" };
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
    set_property(state.handle, "volume", &volume.clamp(0.0, 100.0).to_string())
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
    tx.send(SurfaceMsg::Rect { x, y, w, h });
}

/// Reveal the native surface (player view mounted).
#[tauri::command]
pub fn mpv_show(tx: State<'_, SurfaceTx>) {
    tx.send(SurfaceMsg::Show);
}

/// Hide the native surface and stop playback (back to the session list — no
/// orphan window, and no audio left playing from the unloaded recording).
#[tauri::command]
pub fn mpv_hide(tx: State<'_, SurfaceTx>, state: State<'_, MpvState>) {
    tx.send(SurfaceMsg::Hide);
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
