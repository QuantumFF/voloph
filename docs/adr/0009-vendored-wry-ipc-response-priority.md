# Vendor wry with an IPC response-priority patch

Tauri's Linux IPC delivers every `invoke()` result as the response to a `fetch()`
on the custom `ipc://` protocol, and wry finishes that response on the GTK main
loop via `MainContext::invoke(...)` — a glib **idle source** (`G_PRIORITY_DEFAULT_IDLE`,
200). An idle source is dispatched only in a main-loop iteration where no
higher-priority source is ready. While a recording plays, that never happens:
the embedded mpv `GtkGLArea` (ADR 0008) queues a repaint for every video frame
and the webview animates the playhead every vsync (paint runs at
`GDK_PRIORITY_REDRAW`, 120; the tao/glib event channels at `G_PRIORITY_DEFAULT`, 0).

The observed disease: **every `await invoke(...)` stalls for as long as playback
runs uninterrupted**. Requests still execute immediately (the request direction
doesn't wait on idle — annotation writes hit SQLite in real time), but their
responses buffer until frame delivery hiccups. Verdict markers and flag rings
dropped during playback only appeared on pause or on the decode stall of a
gap-skip seek; the same starvation silently affects timeline refreshes after
inline edits and any future read issued mid-playback. Events (`emit` →
`evaluate_javascript`) ride the priority-0 channels and keep flowing, which is
why the playhead never froze and the bug looked annotation-specific.

The fix is one line, but it lives in wry, not here: deliver the response with
`invoke_with_priority(Priority::DEFAULT, …)` — the same band as the event
channels that demonstrably keep up. Upstream `dev` still schedules at idle as
of wry 0.55.

## Decision

Vendor wry 0.55.1 at `src-tauri/vendor/wry`, patched at the one call site
(marked `VOLOPH PATCH` in `src/webkitgtk/web_context.rs`), and wire it via
`[patch.crates-io]` in `src-tauri/Cargo.toml`.

## Considered Options

- **Fork wry on GitHub + `[patch]` to the fork** — keeps the repo slim but adds
  an external remote to keep alive; the vendored copy is self-contained and
  pinned by the lockfile the same way.
- **App-level workaround: emit events carrying updated data after each
  mutation** — sidesteps the starved responses for annotations/flags only,
  leaves the whole `invoke` surface broken during playback, and taxes every
  future feature with a second data path.
- **Wait for an upstream fix** — the bug class (idle-priority work starved by
  continuous GTK rendering) has precedent in WebKitGTK itself and no open fix
  in wry; review during playback is this app's core loop, so it can't wait.

## Consequences

- **Bumping wry (or tauri, which pins it) requires re-applying the patch**:
  copy the new crate over `vendor/wry`, grep for the old call site, re-add the
  `VOLOPH PATCH` block. The Cargo.toml comment points here.
- No regression test guards this: the failure needs a real GTK main loop with a
  continuously-rendering GLArea plus the webview IPC round trip — no seam in
  this codebase exercises that. The manual check is: while a recording plays,
  press a verdict key; the marker must appear on the session strip within a
  beat, **without pausing**.
- If a future wry release delivers responses at default priority (worth
  checking each bump), the vendor dir and the `[patch.crates-io]` entry can be
  deleted wholesale.
