# Embed mpv on Windows via a `--wid` child HWND, not the render API

Windows gets the same embedded-libmpv playback as Linux (ADR 0008), but through the **simpler embedding mpv itself offers where it works**: a bare child `HWND` of the main Tauri window is handed to mpv with `--wid`, and mpv creates its own D3D11 swapchain inside it and renders there itself. The Linux backend drives mpv's OpenGL render API only because `--wid` is unsupported on Wayland; none of that machinery (GL proc-address plumbing, per-frame render callbacks, a `GtkGLArea` to composite) has a reason to exist on Windows, so the Windows backend is ~150 lines of window management.

The playback module splits accordingly: the libmpv *client* API — load/seek/pause/speed commands, the property-observation event loop, the Tauri events — is platform-neutral and lives in `src/mpv/mod.rs`; only the *surface* (where video lands, slaved to the frontend-reported rect) is per-platform (`mpv/linux.rs`, `mpv/windows.rs`). The frontend contract is byte-identical across platforms: same commands, same `mpv:*` events, same CSS-px rect reports (the Windows backend converts to physical px by the window's DPI).

Linking and shipping: the extern block links `libmpv-2.dll` with Rust's `raw-dylib`, so no import library (`.lib`/`.dll.a`) is generated or vendored — the only artifact is the DLL itself, fetched by `scripts/fetch-libmpv.sh` (from the zhongfly/mpv-winbuild shinchiro-style builds, mirroring the BtbN ffmpeg fetch) and bundled as a resource that lands beside the exe.

## Considered Options

- **Port the render API path (mirror Linux)** — works, but on Windows it buys nothing: `--wid` is first-class there, and the render path would add a GL/D3D interop surface, proc-address resolution, and a render-callback thread hop to maintain for zero behavioral difference.
- **Use the WebView2 `<video>` element on Windows only** — Chromium's media stack seeks correctly (the WebKitGTK dropped-seek bug of ADR 0010 is Linux-specific), but HEVC decode in WebView2 hinges on the user having Microsoft's paid HEVC extension, which would reintroduce the import transcode ADR 0008 eliminated — and fork the player into two divergent frontends.
- **Import library instead of raw-dylib** — generating `mpv.lib` from the DLL (gendef/dlltool) adds a toolchain step and a vendored artifact for no benefit; `raw-dylib` is stable and makes the linker synthesize the stubs.

## Consequences

- The "webview cannot draw over the video rect" constraint (ADR 0008) holds identically on Windows: the mpv child window is a sibling of WebView2's child window, kept above it in the z-order (re-asserted on every show, `WS_CLIPSIBLINGS` both ways). Full-area modals must keep hiding the surface; in-video HUD stays on mpv's OSD.
- The ADR 0009 wry patch (IPC response priority) is glib-specific and has no Windows counterpart — the starvation it fixes cannot occur outside the GTK main loop. The vendored wry still builds on Windows; the patch is simply inert there.
- `libmpv-2.dll` is fetched, not committed (like the ffmpeg sidecars, ADR 0004); the release workflow fetches it on the Windows runner before bundling. Licensing is unchanged in kind — a GPL mpv build ships alongside the already-GPL ffmpeg sidecars.
- For `cargo tauri dev` on Windows the DLL must be findable at process start (raw-dylib resolves at load): beside the dev exe or on `PATH`; `fetch-libmpv.sh` documents this.
- mpv paints the child window itself, so a rect update during a live resize can briefly show stale video edges — the same trail-the-DOM tolerance ADR 0008 already accepts.
- Non-Linux/Windows targets keep the inert stubs (macOS would need its own surface backend and is out of scope).
