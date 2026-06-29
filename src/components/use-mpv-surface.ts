import { useEffect, useRef, useState } from "react"
import { getCurrentWindow } from "@tauri-apps/api/window"

import { trackedInvoke } from "@/lib/tauri"

/**
 * Owns the native mpv surface's lifecycle (ADR 0008), keeping it off the
 * playback orchestration. libmpv draws into a native child surface tiled beside
 * the webview, so the frontend's only job is to keep that surface positioned over
 * an empty pane and hidden when it must not show. This hook concentrates all of
 * that:
 *
 * - slaves the surface to the rect of the pane the returned ref is attached to,
 *   re-reporting on pane resize and window reflow;
 * - reveals the surface while mounted and hides it on unmount (no orphan window);
 * - suppresses it (without stopping playback) while `modalOpen` is true or the
 *   window is minimized, restoring it once both clear.
 *
 * The single input is `modalOpen` (a full-area HTML overlay that must not be
 * drawn over by the native surface — the webview can't composite over the video
 * rect). The single output is the ref to attach to the empty pane.
 */
export function useMpvSurface(modalOpen: boolean) {
  // The empty pane the native mpv surface is slaved to.
  const paneRef = useRef<HTMLDivElement>(null)
  // True while the window is minimized; the surface is suppressed so it leaves no
  // stray window (ADR 0008), and restored on un-minimize.
  const [minimized, setMinimized] = useState(false)

  // Report the pane's bounding rect to Rust so it can position the native
  // surface over it. Fires on mount and whenever the pane resizes or the window
  // reflows; a brief trailing during a window resize is acceptable.
  useEffect(() => {
    const pane = paneRef.current
    if (!pane) return
    const report = () => {
      const r = pane.getBoundingClientRect()
      void trackedInvoke("mpv_set_rect", {
        x: Math.round(r.left),
        y: Math.round(r.top),
        w: Math.round(r.width),
        h: Math.round(r.height),
      }).catch(() => {})
    }
    report()
    const observer = new ResizeObserver(report)
    observer.observe(pane)
    window.addEventListener("resize", report)
    return () => {
      observer.disconnect()
      window.removeEventListener("resize", report)
    }
  }, [])

  // Reveal the surface while mounted; hide it on unmount (back to the session
  // list) so no orphan native window lingers.
  useEffect(() => {
    void trackedInvoke("mpv_show").catch(() => {})
    return () => {
      void trackedInvoke("mpv_hide").catch(() => {})
    }
  }, [])

  // The one constraint of the tiled native surface: the webview cannot draw over
  // the video rect, so a full-area HTML overlay must hide the surface first.
  // Suppress it whenever a full-area modal is open or the window is minimized,
  // and restore it once both clear. Playback continues underneath — this only
  // toggles the surface's visibility, unlike the unmount teardown above. Any
  // in-video HUD (e.g. a verdict flash) must use mpv's OSD, not HTML over the
  // video rect, so it stays visible under this hide.
  const suppressed = modalOpen || minimized
  useEffect(() => {
    void trackedInvoke("mpv_suppress_surface", { suppressed }).catch(() => {})
  }, [suppressed])

  // Track the window's minimized state from its resize events (a minimize is a
  // resize on GTK) so the surface can be suppressed while minimized and restored
  // on un-minimize, leaving no stray or mispositioned surface.
  useEffect(() => {
    const appWindow = getCurrentWindow()
    let unlisten: (() => void) | undefined
    let cancelled = false
    void appWindow
      .onResized(() => {
        void appWindow.isMinimized().then((m) => setMinimized(m))
      })
      .then((fn) => {
        if (cancelled) fn()
        else unlisten = fn
      })
    return () => {
      cancelled = true
      unlisten?.()
    }
  }, [])

  return paneRef
}
