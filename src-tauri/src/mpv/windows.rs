//! Windows surface backend: a child `HWND` mpv adopts via `--wid` (ADR 0014).
//!
//! ## Why `--wid` rather than the render API
//!
//! The Linux backend drives mpv's OpenGL render API only because `--wid` is
//! unsupported on Wayland (ADR 0008). On Windows `--wid` *is* supported: mpv
//! creates its own D3D11 swapchain inside the window we hand it and renders
//! there itself — no GL context, no proc-address plumbing, no per-frame render
//! callback. So this backend is just window management: create a child window
//! of the main Tauri window, tell mpv to embed into it before `mpv_initialize`,
//! and slave its rect/visibility to the frontend's reports.
//!
//! The child is a sibling of WebView2's own child window, kept above it in the
//! z-order (re-asserted on every show) with `WS_CLIPSIBLINGS` so the two never
//! paint over each other — the same "native surface above the webview, the
//! webview cannot draw over the video rect" model as the GTK overlay
//! (ADR 0008 "Family A"). Frontend contract, commands and events are identical
//! across backends.
//!
//! ## Threads
//!
//! The child window is created on the main thread (Tauri's `setup`), which owns
//! its message queue; [`SurfaceTx`] marshals every subsequent window operation
//! back there via `run_on_main_thread`, mirroring the Linux glib channel. The
//! `HWND`s are stored as `isize` because raw pointers are not `Send`.

use std::ptr;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use tauri::{AppHandle, Manager};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, MoveWindow, SetWindowPos, ShowWindow, HWND_TOP, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, SW_HIDE, SW_SHOWNA, WS_CHILD, WS_CLIPSIBLINGS,
};

use super::{mpv_create, mpv_initialize, set_option, MpvHandle};

/// Create the mpv handle and its child-window surface; must run on the main
/// thread (Tauri's `setup` does) so the window's message queue lives on the
/// thread that pumps it. Returns the configured, initialized handle for the
/// shared core to own; manages this backend's [`SurfaceTx`].
pub fn init(app: &AppHandle) -> Result<*mut MpvHandle, String> {
    let window = app
        .get_webview_window("main")
        .ok_or("main window not found")?;
    let parent = match window.window_handle().map_err(|e| e.to_string())?.as_raw() {
        RawWindowHandle::Win32(h) => h.hwnd.get() as HWND,
        _ => return Err("unexpected window handle kind".into()),
    };

    // The surface mpv embeds into: a bare child of the main window, using the
    // predefined STATIC class so no window class registration or window proc is
    // needed (mpv creates its own inner window and does all painting; a STATIC
    // control also hit-tests transparent, so clicks on the video are inert —
    // matching the GLArea). Created without WS_VISIBLE: hidden until the player
    // view calls `mpv_show`, like the GLArea after realize.
    let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
    let child = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            ptr::null(),
            WS_CHILD | WS_CLIPSIBLINGS,
            0,
            0,
            16,
            16,
            parent,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null(),
        )
    };
    if child.is_null() {
        return Err("CreateWindowExW for the mpv surface failed".into());
    }

    // Create and initialize mpv, embedding into the child. `wid` must be set
    // before `mpv_initialize` — mpv reads it when it creates its VO. With a wid
    // the default `vo=gpu` renders into our window instead of opening its own
    // (the whole reason the Linux backend needs `vo=libmpv` + a render context
    // disappears here).
    let handle = unsafe { mpv_create() };
    if handle.is_null() {
        return Err("mpv_create returned null".into());
    }
    set_option(handle, "wid", &(child as isize).to_string());
    // Hardware decoding where available (D3D11VA), and keep mpv off the console.
    set_option(handle, "hwdec", "auto-safe");
    set_option(handle, "terminal", "no");
    if unsafe { mpv_initialize(handle) } < 0 {
        return Err("mpv_initialize failed".into());
    }

    app.manage(SurfaceTx {
        app: app.clone(),
        parent: parent as isize,
        child: child as isize,
    });

    Ok(handle)
}

/// Surface handle stored in Tauri state: the main window and the mpv child
/// window, plus the `AppHandle` used to marshal every window operation onto the
/// main thread (window ops from worker threads risk synchronous cross-thread
/// sends). `HWND`s live as `isize` so the struct is `Send + Sync`.
pub struct SurfaceTx {
    app: AppHandle,
    parent: isize,
    child: isize,
}

impl SurfaceTx {
    fn on_main(&self, f: impl FnOnce(HWND, HWND) + Send + 'static) {
        let (parent, child) = (self.parent, self.child);
        let _ = self
            .app
            .run_on_main_thread(move || f(parent as HWND, child as HWND));
    }

    /// Slave the surface to the frontend-reported rect. The rect arrives in CSS
    /// px (WebView2 device-independent px); `MoveWindow` wants physical px, so
    /// scale by the window's current DPI — re-read on every call so a move to a
    /// different-DPI monitor corrects itself on the next report.
    pub fn rect(&self, x: i32, y: i32, w: i32, h: i32) {
        self.on_main(move |parent, child| unsafe {
            let scale = GetDpiForWindow(parent) as f64 / 96.0;
            let px = |v: i32| (v as f64 * scale).round() as i32;
            MoveWindow(child, px(x), px(y), px(w).max(1), px(h).max(1), 1);
        });
    }

    pub fn show(&self) {
        self.on_main(|_, child| unsafe {
            // Re-assert top-of-siblings before revealing: WebView2 can spawn
            // sibling windows after ours, and a surface below the webview would
            // be invisible. SW_SHOWNA keeps focus in the webview.
            SetWindowPos(
                child,
                HWND_TOP,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
            ShowWindow(child, SW_SHOWNA);
        });
    }

    pub fn hide(&self) {
        self.on_main(|_, child| unsafe {
            ShowWindow(child, SW_HIDE);
        });
    }
}
