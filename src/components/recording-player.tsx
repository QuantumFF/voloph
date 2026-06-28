"use client"

import { useCallback, useEffect, useRef, useState } from "react"
import { ArrowLeftIcon, PauseIcon, PlayIcon } from "lucide-react"

import { Button } from "@/components/ui/button"
import { trackedInvoke } from "@/lib/tauri"

/** One recording in the session playlist, in capture-time order. */
export interface PlaylistRecording {
  /** Absolute on-disk path of the recording. */
  path: string
}

interface RecordingPlayerProps {
  /** The session's recordings, ordered by capture time. */
  recordings: PlaylistRecording[]
  /** Index of the recording to open first (defaults to the session's start). */
  startIndex?: number
  /** Return to the session list. */
  onBack: () => void
}

function fileName(path: string): string {
  const parts = path.split(/[\\/]/)
  return parts[parts.length - 1] || path
}

/**
 * Plays one recording with embedded libmpv (ADR 0008) — the tracer slice for
 * native playback (issue #34): one recording, play/pause only. Seeking,
 * frame-step, speed and the session orchestration (gap-skip, the stitched
 * session axis, the five inline edits) are later slices.
 *
 * There is **no** `<video>` element and no loopback HTTP. libmpv is linked into
 * the Rust process and decodes the file directly from disk, rendering into a
 * native `GtkGLArea` that GTK composites *above* the webview (Family A — the
 * webview never draws over the video rect). The pane below is therefore an empty
 * `<div>`: a hole the native surface fills. A `ResizeObserver` reports the pane's
 * bounding rectangle to Rust (`mpv_set_rect`), which slaves the surface to it.
 * The surface is shown on mount (`mpv_show`) and hidden on unmount (`mpv_hide`)
 * so no orphan native window floats over the session list.
 */
export function RecordingPlayer({
  recordings,
  startIndex = 0,
  onBack,
}: RecordingPlayerProps) {
  const index = Math.min(
    Math.max(startIndex, 0),
    Math.max(recordings.length - 1, 0)
  )
  const path = recordings[index]?.path ?? null
  // The empty pane the native mpv surface is slaved to.
  const paneRef = useRef<HTMLDivElement>(null)
  const [paused, setPaused] = useState(false)

  // Report the pane's bounding rect to Rust so it can position the native
  // surface over it (ADR 0008). Fires on mount and whenever the pane resizes or
  // the window reflows; a brief trailing during a window resize is acceptable.
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

  // Reveal the surface while the player is mounted; hide it on unmount (back to
  // the session list) so no orphan native window lingers (ADR 0008).
  useEffect(() => {
    void trackedInvoke("mpv_show").catch(() => {})
    return () => {
      void trackedInvoke("mpv_hide").catch(() => {})
    }
  }, [])

  // Load the recording directly from disk and start playing it.
  useEffect(() => {
    if (!path) return
    void trackedInvoke("mpv_load", { path })
      .then(() => setPaused(false))
      .catch(() => {})
  }, [path])

  const togglePlay = useCallback(() => {
    setPaused((prev) => {
      const next = !prev
      void trackedInvoke("mpv_set_pause", { paused: next }).catch(() => {})
      return next
    })
  }, [])

  return (
    <div className="flex h-full min-h-0 flex-col gap-4">
      <div className="flex shrink-0 items-center gap-3">
        <Button variant="outline" size="sm" onClick={onBack}>
          <ArrowLeftIcon className="size-4" />
          Sessions
        </Button>
        <span className="truncate font-medium" title={path ?? undefined}>
          {path ? fileName(path) : "No recordings"}
        </span>
        {recordings.length > 1 ? (
          <span className="shrink-0 text-sm text-muted-foreground tabular-nums">
            Recording {index + 1} of {recordings.length}
          </span>
        ) : null}
      </div>
      {/* The video pane: an empty hole the native mpv surface composites over. */}
      <div ref={paneRef} className="min-h-0 w-full flex-1 rounded-lg bg-black" />
      <div className="flex shrink-0 items-center gap-3">
        <Button
          variant="outline"
          size="sm"
          onClick={togglePlay}
          disabled={!path}
          aria-label={paused ? "Play" : "Pause"}
        >
          {paused ? (
            <PlayIcon className="size-4" />
          ) : (
            <PauseIcon className="size-4" />
          )}
          {paused ? "Play" : "Pause"}
        </Button>
      </div>
    </div>
  )
}
