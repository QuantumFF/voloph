"use client"

import { useCallback, useEffect, useMemo, useRef, useState } from "react"
import { listen } from "@tauri-apps/api/event"
import {
  ArrowLeftIcon,
  PauseIcon,
  PlayIcon,
  Volume2Icon,
  VolumeXIcon,
} from "lucide-react"

import { Button } from "@/components/ui/button"
import { trackedInvoke } from "@/lib/tauri"
import {
  SPEED_LADDER,
  clampVolume,
  seekTarget,
  stepSpeedIndex,
} from "./recording-player-transport"

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

/** Relative seek distance for the arrow keys, in milliseconds. */
const SEEK_STEP_MS = 5000
const DEFAULT_SPEED_INDEX = SPEED_LADDER.indexOf(1)
/** Volume step (0–100) for the up/down arrows. */
const VOLUME_STEP = 10

function fileName(path: string): string {
  const parts = path.split(/[\\/]/)
  return parts[parts.length - 1] || path
}

function formatTime(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000))
  const m = Math.floor(total / 60)
  const s = total % 60
  return `${m}:${s.toString().padStart(2, "0")}`
}

/**
 * Plays one recording with embedded libmpv (ADR 0008), driving mpv's native
 * transport (issue #35): seek, frame-step, speed, and mute/volume go straight to
 * mpv, and the playhead is mpv's observed `time-pos` over a Tauri event stream —
 * not the webview's `timeupdate` handler or any `seekBaseMs + currentTime` math.
 * There is no `<video>` element, no loopback HTTP, no double-buffer, and no
 * JPEG-overlay frame-step: libmpv seeks sparse GOPs natively, so all of that is
 * gone. Cross-file session orchestration is a later slice.
 *
 * libmpv renders into a native `GtkGLArea` that GTK composites *above* the
 * webview (Family A — the webview never draws over the video rect), so the pane
 * below is an empty `<div>`: a hole the surface fills. A `ResizeObserver` reports
 * the pane's rect to Rust (`mpv_set_rect`); the surface is shown on mount and
 * hidden on unmount so no orphan native window floats over the session list.
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
  // The playhead, driven by mpv's `time-pos` events (ms).
  const [currentMs, setCurrentMs] = useState(0)
  const [ended, setEnded] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [speedIndex, setSpeedIndex] = useState(DEFAULT_SPEED_INDEX)
  const [muted, setMuted] = useState(false)
  const [volume, setVolume] = useState(100)
  const speed = SPEED_LADDER[speedIndex]

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

  // The playhead, end, and error UI states all come from mpv's event stream
  // (ADR 0008), replacing the webview's `timeupdate`/`ended`/`error` handlers.
  useEffect(() => {
    const unlisten: Array<() => void> = []
    void listen<number>("mpv:time-pos", (event) => {
      setCurrentMs(event.payload)
      setEnded(false)
    }).then((u) => unlisten.push(u))
    void listen("mpv:ended", () => setEnded(true)).then((u) => unlisten.push(u))
    void listen<string>("mpv:error", (event) =>
      setError(event.payload ?? "playback failed")
    ).then((u) => unlisten.push(u))
    return () => {
      for (const u of unlisten) u()
    }
  }, [])

  // Load the recording directly from disk and start playing it, resetting the
  // per-recording transport state once the load resolves (the resets live in the
  // promise callbacks so the effect body never calls setState synchronously).
  useEffect(() => {
    if (!path) return
    void trackedInvoke("mpv_load", { path })
      .then(() => {
        setError(null)
        setEnded(false)
        setCurrentMs(0)
        setPaused(false)
      })
      .catch((e) => setError(String(e)))
  }, [path])

  const togglePlay = useCallback(() => {
    setPaused((prev) => {
      const next = !prev
      void trackedInvoke("mpv_set_pause", { paused: next }).catch(() => {})
      return next
    })
  }, [])

  const seekBy = useCallback((deltaMs: number) => {
    setCurrentMs((prev) => {
      const next = seekTarget(prev, deltaMs)
      void trackedInvoke("mpv_seek", { ms: next }).catch(() => {})
      return next
    })
  }, [])

  const frameStep = useCallback((forward: boolean) => {
    void trackedInvoke("mpv_frame_step", { forward }).catch(() => {})
  }, [])

  const stepSpeed = useCallback((dir: 1 | -1) => {
    setSpeedIndex((prev) => {
      const next = stepSpeedIndex(prev, dir)
      void trackedInvoke("mpv_set_speed", { speed: SPEED_LADDER[next] }).catch(
        () => {}
      )
      return next
    })
  }, [])

  const toggleMute = useCallback(() => {
    setMuted((prev) => {
      const next = !prev
      void trackedInvoke("mpv_set_mute", { muted: next }).catch(() => {})
      return next
    })
  }, [])

  const changeVolume = useCallback((delta: number) => {
    setVolume((prev) => {
      const next = clampVolume(prev + delta)
      void trackedInvoke("mpv_set_volume", { volume: next }).catch(() => {})
      return next
    })
  }, [])

  // The transport keymap. mpv handles each command natively (ADR 0008).
  useEffect(() => {
    if (!path) return
    const onKeyDown = (e: KeyboardEvent) => {
      // Don't hijack keys while typing into an input.
      const target = e.target as HTMLElement | null
      if (
        target &&
        (target.isContentEditable ||
          ["INPUT", "TEXTAREA", "SELECT"].includes(target.tagName))
      ) {
        return
      }
      switch (e.key) {
        case " ":
          e.preventDefault()
          togglePlay()
          break
        case "ArrowLeft":
          e.preventDefault()
          seekBy(-SEEK_STEP_MS)
          break
        case "ArrowRight":
          e.preventDefault()
          seekBy(SEEK_STEP_MS)
          break
        case ",":
          e.preventDefault()
          frameStep(false)
          break
        case ".":
          e.preventDefault()
          frameStep(true)
          break
        case "[":
          e.preventDefault()
          stepSpeed(-1)
          break
        case "]":
          e.preventDefault()
          stepSpeed(1)
          break
        case "ArrowUp":
          e.preventDefault()
          changeVolume(VOLUME_STEP)
          break
        case "ArrowDown":
          e.preventDefault()
          changeVolume(-VOLUME_STEP)
          break
        case "m":
        case "M":
          e.preventDefault()
          toggleMute()
          break
        default:
          break
      }
    }
    window.addEventListener("keydown", onKeyDown)
    return () => window.removeEventListener("keydown", onKeyDown)
  }, [
    path,
    togglePlay,
    seekBy,
    frameStep,
    stepSpeed,
    changeVolume,
    toggleMute,
  ])

  const speedLabel = useMemo(() => `${speed}x`, [speed])

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
      {error ? (
        <div className="shrink-0 text-sm text-destructive" role="alert">
          {error}
        </div>
      ) : null}
      <div className="flex shrink-0 flex-wrap items-center gap-3">
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
        <span className="text-sm text-muted-foreground tabular-nums">
          {formatTime(currentMs)}
          {ended ? " (ended)" : ""}
        </span>
        <div className="ml-auto flex items-center gap-2">
          <Button
            variant="outline"
            size="sm"
            onClick={() => stepSpeed(-1)}
            disabled={!path || speedIndex === 0}
            aria-label="Slower"
          >
            −
          </Button>
          <span className="min-w-12 text-center text-sm tabular-nums">
            {speedLabel}
          </span>
          <Button
            variant="outline"
            size="sm"
            onClick={() => stepSpeed(1)}
            disabled={!path || speedIndex === SPEED_LADDER.length - 1}
            aria-label="Faster"
          >
            +
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={toggleMute}
            disabled={!path}
            aria-label={muted ? "Unmute" : "Mute"}
          >
            {muted ? (
              <VolumeXIcon className="size-4" />
            ) : (
              <Volume2Icon className="size-4" />
            )}
          </Button>
          <span className="min-w-10 text-right text-sm text-muted-foreground tabular-nums">
            {muted ? "—" : `${volume}%`}
          </span>
        </div>
      </div>
    </div>
  )
}
