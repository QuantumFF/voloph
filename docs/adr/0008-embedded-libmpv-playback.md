# Play recordings with embedded libmpv, not the webview `<video>`

Recordings are decoded and rendered by **libmpv embedded in-process**, drawing into a **native child surface tiled beside the webview UI** — not an HTML `<video>` element. This is a deliberate retreat from the webview's media stack: on this WebKitGTK/GStreamer build, `currentTime` seeks are silently dropped (ADR 0007), and every workaround built on top — seek-by-reloading a positioned ffmpeg stream, a double-buffered pair of `<video>` elements, a JPEG-overlay frame-step, and forcing ~1s keyframes into every recording at import — was load-bearing complexity that existed only to route around that one bug. libmpv seeks natively (sparse GOPs included), so all of it goes.

The frontend keeps the playback *orchestration* (gap-skip, rally-loop, the session-global axis stitched across recordings, cross-file boundary crossing, next-uncertain, free-play, the five inline edits) and drives mpv as a thin controllable surface — play/seek/frame-step/speed — observing `time-pos` over a Tauri event stream where the old `timeupdate` handler used to read. The frontend also owns layout: a `ResizeObserver` on the video pane reports its rectangle to Rust, which slaves the mpv child window to it and hides it when a full-area modal opens, the user returns to the session list, or the window is minimized.

## Considered Options

- **Stay in the webview, optimize the workarounds** (`react-player`, a different HTTP server like axum/tower-http) — neither touches the dropped-seek bug, which is the actual disease.
- **mpv EDL for gap-free playback** — elegant for pure rally concatenation, but fights free-play (watching gaps on purpose) and live inline edits (every adjust/split/merge would rebuild and reload the EDL mid-review).
- **Render-to-texture / zero-copy DMA-BUF into the DOM** — per-frame cost, and the webview-compositing path is already unreliable here (ADR 0007 notes video composites in a separate layer where canvas capture comes out black; the DMA-BUF renderer was disabled on NVIDIA).
- **Transparent webview composited over a native video widget** (one GTK window, `GtkOverlay` of an mpv widget under a transparent WebKitWebView) — would let HTML overlay the video everywhere and remove the modal/surface-hide constraint, but bets on the same fragile accelerated-GTK compositing; deferred to a possible future spike rather than taken blind.

## Consequences

- The import **transcode is eliminated** (ADR 0005 superseded): libmpv decodes HEVC/AV1/10-bit and seeks sparse GOPs, so there is no codec or keyframe reason to re-encode. Originals are never modified again — ADR 0001 is restored, and the capture-date-loss class (issue #25) cannot occur. Import is now just probe + segmentation; a recording is playable immediately.
- The **loopback HTTP playback server, per-launch token, range serving, `ChildStream`, and the ffmpeg seek/frame streams are removed** — mpv opens the recording directly from disk, so there is no `http://` source to serve.
- On the frontend the **double-buffer (`live`/`incoming`/promote), the `seekBaseMs + currentTime` mapping, and the JPEG frame-step path delete**; `currentMs` becomes mpv's real `time-pos` and frame-step uses mpv's native frame-stepping (fixes the out-of-sync frame-step).
- The webview **cannot draw over the video rect** (a native surface composites above it): a full-area modal must hide the surface first, and any in-video HUD uses mpv's OSD rather than HTML.
- The frontend slaving the surface to a reported rectangle means a window resize can **briefly trail the DOM** — accepted for a desktop review tool.
- **libmpv is bundled** (extends ADR 0004); the bundle carries ffmpeg code twice (libmpv's internal libav plus the CLI sidecars). Licensing is unchanged — a GPL ffmpeg build is already bundled.
- ffmpeg/ffprobe **sidecars remain** for audio-PCM extraction, motion analysis, capture-date probing, and export.
