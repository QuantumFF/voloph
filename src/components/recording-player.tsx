"use client"

import { useCallback, useEffect, useMemo, useRef, useState } from "react"
import { listen } from "@tauri-apps/api/event"
import { getCurrentWindow } from "@tauri-apps/api/window"
import {
  ArrowLeftIcon,
  ChevronLeftIcon,
  ChevronRightIcon,
  KeyboardIcon,
  Loader2Icon,
  PauseIcon,
  PencilIcon,
  PlayIcon,
  PlusIcon,
  RepeatIcon,
  RotateCwIcon,
  ScissorsIcon,
  StepBackIcon,
  StepForwardIcon,
  Trash2Icon,
  TriangleAlertIcon,
  Volume2Icon,
  VolumeXIcon,
  XIcon,
  ZoomInIcon,
  ZoomOutIcon,
} from "lucide-react"

import { Button } from "@/components/ui/button"
import { trackedInvoke } from "@/lib/tauri"
import {
  SPEED_LADDER,
  UNCERTAIN_CONFIDENCE,
  buildSessionModel,
  clamp,
  clampVolume,
  gapSkipAction,
  nextRallyMs,
  nextUncertainMs,
  prevRallyAction,
  seekTarget,
  stepSpeedIndex,
  type PlaylistRecording,
  type SessionModel,
  type SessionRally,
  type Timeline,
} from "./recording-player-transport"

export type { PlaylistRecording }

interface RecordingPlayerProps {
  /**
   * The session's recordings, ordered by capture time. Their rallies are
   * flattened into one continuous playlist played back-to-back (the North Star).
   */
  recordings: PlaylistRecording[]
  /** Index of the recording to open first (defaults to the session's start). */
  startIndex?: number
  /** Return to the session list. */
  onBack: () => void
}

/** How long to wait before re-checking a recording whose timeline is still being produced. */
const SEGMENT_POLL_MS = 2000

/** Index of `1×` on the speed ladder — the default and the `Ctrl+0` reset target. */
const DEFAULT_SPEED_INDEX = SPEED_LADDER.indexOf(1)

/**
 * Horizontal scale of the session timeline strip in pixels-per-second, so the
 * whole session is one long, horizontally-scrollable strip. The zoom buttons
 * step it between MIN (a whole long session at a glance) and MAX (frame-level
 * detail), each press scaling by `SESSION_ZOOM_FACTOR`.
 */
const SESSION_PX_PER_SEC_DEFAULT = 3
const SESSION_PX_PER_SEC_MIN = 1
const SESSION_PX_PER_SEC_MAX = 240
const SESSION_ZOOM_FACTOR = 1.5

/** Arrow-seek step sizes in ms (session-global), by modifier. */
const SEEK_FINE_MS = 2500 // Ctrl+←/→
const SEEK_DEFAULT_MS = 5000 // ←/→
const SEEK_COARSE_MS = 10000 // Shift+←/→

/** Volume step (0–100) for the up/down arrows. */
const VOLUME_STEP = 10

/** How much each Alt+scroll notch over the timeline zooms. */
const ALT_SCROLL_ZOOM_FACTOR = 1.15

/** How wide a rally Add-at-playhead creates around the playhead (ms each side). */
const ADD_RALLY_HALF_MS = 2000

function formatClock(ms: number): string {
  const total = Math.round(ms / 1000)
  const m = Math.floor(total / 60)
  const s = total % 60
  return `${m}:${s.toString().padStart(2, "0")}`
}

/**
 * Session-global timecode for the transport bar: `mm:ss` under an hour,
 * `h:mm:ss` past it, so the position display reads naturally for both a short
 * clip and a long session.
 */
function formatTimecode(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000))
  const h = Math.floor(total / 3600)
  const m = Math.floor((total % 3600) / 60)
  const s = total % 60
  if (h > 0) {
    return `${h}:${m.toString().padStart(2, "0")}:${s.toString().padStart(2, "0")}`
  }
  return `${m}:${s.toString().padStart(2, "0")}`
}

function fileName(path: string): string {
  const parts = path.split(/[\\/]/)
  return parts[parts.length - 1] || path
}

/**
 * Where to resume once the playlist crosses into a recording: its first rally
 * (advancing forward), its last rally (stepping back via Prev), or a specific
 * recording-local time (a click on the session strip that landed in another
 * recording).
 */
type Resume = "start" | "end" | { atMs: number }

/**
 * One entry in the single keymap definition: its display form (`keys`/`label`
 * for the cheat-sheet), the predicate that decides whether a keydown matches it,
 * and the action to run. One array backs both the live key handler and the `?`
 * cheat-sheet, so they cannot drift.
 */
interface Keybinding {
  keys: string[]
  label: string
  match: (e: KeyboardEvent) => boolean
  run: (e: KeyboardEvent) => void
}

/**
 * Plays a whole **session** as one continuous playlist (the North Star) on
 * embedded libmpv (ADR 0008): the rallies of every recording, in capture-time
 * order, played back-to-back with gaps skipped. mpv decodes one recording at a
 * time; when the playhead runs past the last rally of the current recording the
 * player advances to the next recording (`mpv_load`) and resumes from its first
 * rally, so file boundaries are invisible. Rally-to-rally navigation likewise
 * crosses boundaries (issue #36).
 *
 * The playhead is mpv's observed `time-pos` over a Tauri event stream (issue
 * #35), where the old webview `timeupdate` handler used to read; seeks go to
 * mpv's native absolute seek and frame-step to mpv's native frame-stepping. The
 * orchestration — gap-skip, rally-loop, the session-global axis stitched across
 * recordings, cross-file crossing, next-uncertain, free-play, and the five inline
 * edits — stays frontend-side and drives mpv as a thin controllable surface (no
 * mpv EDL).
 *
 * Beneath the player a single **session timeline** stitches every recording's
 * draft timeline onto one continuous axis. libmpv renders into a native surface
 * GTK composites *above* the webview, so the video pane is an empty `<div>` (a
 * hole the surface fills); a `ResizeObserver` reports its rect to Rust and the
 * surface is shown on mount, hidden on unmount.
 */
export function RecordingPlayer({
  recordings,
  startIndex = 0,
  onBack,
}: RecordingPlayerProps) {
  // Gap-free playback is the default (the North Star), but a manual playhead move
  // opts out: dragging into a gap, or clicking an empty part of the session
  // strip, flips `freePlayRef` true and gap-skipping stands down so footage
  // between rallies can be watched. A rally-targeted action restores gap-free
  // playback. It's a ref, not state, so the `time-pos` listener reads the latest
  // value synchronously without re-subscribing on every change.
  const freePlayRef = useRef(false)
  // The empty pane the native mpv surface is slaved to.
  const paneRef = useRef<HTMLDivElement>(null)
  // Focus host so the keymap's window handler is the only thing on keystrokes.
  const containerRef = useRef<HTMLDivElement>(null)

  // Every recording's draft timeline, keyed by path so it survives switching
  // recordings (and so the whole session can be stitched into one strip). Each
  // unsegmented recording is polled until its rallies arrive (ADR 0002).
  const [timelines, setTimelines] = useState<Record<string, Timeline>>({})
  // The playhead within the current recording (ms), from mpv's `time-pos`.
  const [currentMs, setCurrentMs] = useState(0)
  const [paused, setPaused] = useState(false)
  const [muted, setMuted] = useState(false)
  const [volume, setVolume] = useState(100)
  const [speedIndex, setSpeedIndex] = useState(DEFAULT_SPEED_INDEX)
  const [looping, setLooping] = useState(false)
  const [editing, setEditing] = useState(false)
  const [showCheatSheet, setShowCheatSheet] = useState(false)
  // True while the window is minimized; the native surface is suppressed so it
  // leaves no stray window (ADR 0008), and restored on un-minimize.
  const [minimized, setMinimized] = useState(false)
  const [error, setError] = useState<string | null>(null)
  // Bumped by Re-analyze to re-trigger the timeline fetch/poll; `reanalyzing`
  // guards the button (ADR 0002, the tuning loop).
  const [reanalyzeNonce, setReanalyzeNonce] = useState(0)
  const [reanalyzing, setReanalyzing] = useState(false)

  // Which recording in the playlist is loaded, and where to resume once its
  // timeline arrives after a boundary crossing.
  const [index, setIndex] = useState(() =>
    Math.min(Math.max(startIndex, 0), Math.max(recordings.length - 1, 0))
  )
  const [pendingSeek, setPendingSeek] = useState<Resume | null>(null)
  const path = recordings[index]?.path ?? null
  const timeline = path ? (timelines[path] ?? null) : null

  const atFirstRecording = index <= 0
  const atLastRecording = index >= recordings.length - 1

  // Take focus on mount so the keymap acts immediately, without a click first.
  useEffect(() => {
    containerRef.current?.focus()
  }, [])

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

  // The one constraint of the tiled native surface (ADR 0008): the webview cannot
  // draw over the video rect, so a full-area HTML overlay must hide the surface
  // first. Suppress it whenever a full-area modal (currently only the cheat-sheet)
  // is open or the window is minimized, and restore it once both clear. Playback
  // continues underneath — this only toggles the surface's visibility, unlike the
  // unmount teardown above. Any in-video HUD (e.g. a verdict flash) must use mpv's
  // OSD, not HTML over the video rect, so it stays visible under this hide.
  const surfaceSuppressed = showCheatSheet || minimized
  useEffect(() => {
    void trackedInvoke("mpv_suppress_surface", {
      suppressed: surfaceSuppressed,
    }).catch(() => {})
  }, [surfaceSuppressed])

  // Track the window's minimized state from its resize events (a minimize is a
  // resize on GTK) so the surface can be suppressed while minimized and restored
  // on un-minimize, leaving no stray or mispositioned surface (ADR 0008).
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

  // Fetch every recording's draft timeline so the whole session can be stitched
  // into one strip, polling the recordings still being segmented so their
  // rallies appear as soon as the worker finishes (ADR 0002). Re-runs on
  // Re-analyze so the re-segmented recording is re-polled to ready.
  useEffect(() => {
    let cancelled = false
    const timers: ReturnType<typeof setTimeout>[] = []
    const loadOne = (recordingPath: string) => {
      trackedInvoke<Timeline>("recording_timeline", { path: recordingPath })
        .then((result) => {
          if (cancelled) return
          setTimelines((prev) => ({ ...prev, [recordingPath]: result }))
          if (result.segment_state === "unknown") {
            timers.push(
              setTimeout(() => loadOne(recordingPath), SEGMENT_POLL_MS)
            )
          }
        })
        .catch(() => {
          // A timeline failure is non-fatal — playback still works without it.
        })
    }
    recordings.forEach((rec) => loadOne(rec.path))
    return () => {
      cancelled = true
      timers.forEach(clearTimeout)
    }
  }, [recordings, reanalyzeNonce])

  // Re-fetch a single recording's saved timeline after an inline correction
  // (issue #7) without disturbing the rest of the session.
  const refreshTimeline = useCallback((recordingPath: string) => {
    trackedInvoke<Timeline>("recording_timeline", { path: recordingPath })
      .then((result) =>
        setTimelines((prev) => ({ ...prev, [recordingPath]: result }))
      )
      .catch(() => {})
  }, [])

  // Rallies for the current recording, ascending by start (sorted in segment.rs).
  // The empty list when there's no timeline means the recording plays straight
  // through until its draft arrives.
  const rallies = useMemo(() => timeline?.rallies ?? [], [timeline])

  // The whole session stitched onto one continuous axis.
  const session = useMemo<SessionModel>(
    () => buildSessionModel(recordings, timelines),
    [recordings, timelines]
  )

  // Offset of a recording on the session axis (0 if it isn't placed yet).
  const segmentOffset = useCallback(
    (recordingIndex: number) =>
      session.segments.find((s) => s.index === recordingIndex)?.offsetMs ?? 0,
    [session]
  )

  // Session-global playhead: the current recording's offset plus the local time.
  // Null until the current recording is placed (its predecessors are segmented).
  const globalPlayheadMs =
    index < session.placedCount ? segmentOffset(index) + currentMs : null

  // Seek the current recording to a local position via mpv's native absolute
  // seek (ADR 0008). `currentMs` is set optimistically so rally-to-rally math
  // (e.g. Prev twice) chains off the target immediately rather than waiting on
  // the next `time-pos` tick.
  const seekTo = useCallback((ms: number) => {
    const target = Math.max(0, Math.round(ms))
    setCurrentMs(target)
    void trackedInvoke("mpv_seek", { ms: target }).catch(() => {})
  }, [])

  // Move the playlist to another recording, remembering where to resume once it
  // loads (and reset the optimistic playhead so the resume math is clean).
  const goToRecording = useCallback((next: number, resume: Resume) => {
    setCurrentMs(0)
    setIndex(next)
    setPendingSeek(resume)
  }, [])

  // Load the current recording directly from disk and start playing it (ADR
  // 0008). A boundary crossing re-runs this on the new `path`; the resume effect
  // below seeks to the right rally once its timeline is known.
  useEffect(() => {
    if (!path) return
    void trackedInvoke("mpv_load", { path })
      .then(() => {
        setError(null)
        setPaused(false)
      })
      .catch((e) => setError(String(e)))
  }, [path])

  // Re-apply the user's speed/volume/mute after a load (mpv resets them when a
  // new file opens), so a boundary crossing inherits the current transport state.
  useEffect(() => {
    if (!path) return
    void trackedInvoke("mpv_set_speed", {
      speed: SPEED_LADDER[speedIndex],
    }).catch(() => {})
    void trackedInvoke("mpv_set_volume", { volume }).catch(() => {})
    void trackedInvoke("mpv_set_mute", { muted }).catch(() => {})
    // Re-applied per recording (the `path` dep); changes within a recording are
    // pushed by the transport actions themselves.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [path])

  // After a boundary crossing, once the new recording's timeline is known, seek
  // to the requested resume point. A specific time seeks straight there; the
  // first/last rally waits for the timeline (with no rallies yet there is nothing
  // to seek to, so the recording plays from the top until its draft arrives).
  useEffect(() => {
    /* eslint-disable react-hooks/set-state-in-effect -- resuming a crossed-into recording seeks (which sets state), the whole point of the effect */
    if (pendingSeek == null) return
    if (typeof pendingSeek === "object") {
      seekTo(pendingSeek.atMs)
      setPendingSeek(null)
      return
    }
    if (rallies.length === 0) {
      // Wait for this recording's draft; until then it plays from the top.
      return
    }
    const target =
      pendingSeek === "start" ? rallies[0] : rallies[rallies.length - 1]
    seekTo(target.start_ms)
    setPendingSeek(null)
    /* eslint-enable react-hooks/set-state-in-effect */
  }, [pendingSeek, rallies, seekTo])

  // Gap-free playback (the North Star): as the playhead crosses out of a rally
  // into a gap, jump straight to the next rally's start so only play is watched
  // (ADR 0001). Past the final rally of this recording, advance into the next
  // recording; only the session's very last rally ends playback. The decision is
  // a pure helper (issue #36) so it stays testable without mpv.
  const skipGaps = useCallback(
    (ms: number) => {
      const action = gapSkipAction(
        rallies,
        ms,
        looping,
        freePlayRef.current,
        atLastRecording
      )
      switch (action.kind) {
        case "seek":
          seekTo(action.ms)
          break
        case "next-recording":
          freePlayRef.current = false
          goToRecording(index + 1, "start")
          break
        case "stop":
          setPaused(true)
          void trackedInvoke("mpv_set_pause", { paused: true }).catch(() => {})
          break
        case "none":
          break
      }
    },
    [rallies, looping, atLastRecording, seekTo, goToRecording, index]
  )

  // When the current recording ends naturally (its trailing gap was short enough
  // that no later rally triggered a skip), cross into the next so the playlist
  // keeps flowing; the session's last recording just stops.
  const handleEnded = useCallback(() => {
    freePlayRef.current = false
    if (!atLastRecording) goToRecording(index + 1, "start")
  }, [atLastRecording, goToRecording, index])

  // The mpv-event handlers, mirrored into a ref so the listeners below subscribe
  // once yet always run the latest closures (which capture the current rallies,
  // loop/free-play state, and crossing index) — without re-subscribing the
  // `time-pos` stream on every playhead tick. Synced in an effect, not during
  // render (the codebase forbids writing a ref while rendering).
  const handlersRef = useRef({ skipGaps, handleEnded })
  useEffect(() => {
    handlersRef.current = { skipGaps, handleEnded }
  }, [skipGaps, handleEnded])

  // The playhead, end, and error UI states come from mpv's event stream (ADR
  // 0008, issue #35). Each `time-pos` tick drives the playhead and runs gap-skip
  // (issue #36), where the old webview `timeUpdate` handler used to.
  useEffect(() => {
    const unlisten: Array<() => void> = []
    void listen<number>("mpv:time-pos", (event) => {
      setCurrentMs(event.payload)
      handlersRef.current.skipGaps(event.payload)
    }).then((u) => unlisten.push(u))
    void listen("mpv:ended", () => handlersRef.current.handleEnded()).then(
      (u) => unlisten.push(u)
    )
    void listen<string>("mpv:error", (event) =>
      setError(event.payload ?? "playback failed")
    ).then((u) => unlisten.push(u))
    return () => {
      for (const u of unlisten) u()
    }
  }, [])

  // Seek the session to a global position: find the recording that owns it and
  // either seek within the current recording or cross into it. A seek landing in
  // a gap means "let me watch from here" → free play; one landing inside a rally
  // keeps gap-free.
  const seekSession = useCallback(
    (globalMs: number) => {
      const target = clamp(globalMs, 0, session.totalMs)
      const seg =
        session.segments.find(
          (s) =>
            s.durationMs != null &&
            target >= s.offsetMs &&
            target < s.offsetMs + s.durationMs
        ) ?? session.segments[session.placedCount - 1]
      if (!seg) return
      const localMs = clamp(target - seg.offsetMs, 0, seg.durationMs ?? target)
      freePlayRef.current = !session.rallies.some(
        (r) => target >= r.globalStart && target < r.globalEnd
      )
      if (seg.index === index) {
        seekTo(localMs)
      } else {
        goToRecording(seg.index, { atMs: localMs })
      }
    },
    [session, index, seekTo, goToRecording]
  )

  // Manual rally-to-rally navigation, across recording boundaries. A jump is an
  // explicit gap-free intent — it leaves free play (issue #36).
  const goToRally = useCallback(
    (direction: "next" | "prev") => {
      freePlayRef.current = false
      const ms = currentMs
      if (direction === "next") {
        const target = nextRallyMs(rallies, ms)
        if (target != null) {
          seekTo(target)
        } else if (!atLastRecording) {
          goToRecording(index + 1, "start")
        }
      } else {
        const action = prevRallyAction(rallies, ms, atFirstRecording)
        if (action.kind === "seek") {
          seekTo(action.ms)
        } else if (action.kind === "prev-recording") {
          goToRecording(index - 1, "end")
        }
      }
    },
    [
      currentMs,
      rallies,
      seekTo,
      atFirstRecording,
      atLastRecording,
      goToRecording,
      index,
    ]
  )

  // Jump to the next uncertain region across the whole session (ADR 0002).
  const goToUncertain = useCallback(() => {
    const target = nextUncertainMs(session.rallies, globalPlayheadMs ?? 0)
    if (target != null) seekSession(target)
  }, [session, globalPlayheadMs, seekSession])

  // --- Transport: the custom bar and keymap drive these via mpv (issue #35). ---

  const togglePlay = useCallback(() => {
    setPaused((prev) => {
      const next = !prev
      void trackedInvoke("mpv_set_pause", { paused: next }).catch(() => {})
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

  const toggleLoop = useCallback(() => setLooping((l) => !l), [])

  const changeVolume = useCallback((delta: number) => {
    setVolume((prev) => {
      const next = clampVolume(prev + delta)
      void trackedInvoke("mpv_set_volume", { volume: next }).catch(() => {})
      return next
    })
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

  const resetSpeed = useCallback(() => {
    setSpeedIndex(DEFAULT_SPEED_INDEX)
    void trackedInvoke("mpv_set_speed", { speed: 1 }).catch(() => {})
  }, [])

  const frameStep = useCallback((forward: boolean) => {
    void trackedInvoke("mpv_frame_step", { forward }).catch(() => {})
  }, [])

  // Seek the session by a signed offset (session-global, so it crosses recording
  // boundaries). A relative seek is a manual move, so `seekSession` opts it out
  // of gap-free playback like a scrubber drag. Inert until placed on the axis.
  const seekRelative = useCallback(
    (deltaMs: number) => {
      if (globalPlayheadMs == null) return
      seekSession(seekTarget(globalPlayheadMs, deltaMs))
    },
    [globalPlayheadMs, seekSession]
  )

  // Re-run segmentation for the current recording, then re-fetch timelines.
  const handleReanalyze = useCallback(() => {
    if (!path) return
    setReanalyzing(true)
    trackedInvoke("reanalyze_recording", { path })
      .then(() => setReanalyzeNonce((n) => n + 1))
      .catch(() => {})
      .finally(() => setReanalyzing(false))
  }, [path])

  // The five inline corrections (issue #7), resolved against the recording that
  // owns the rally. Each persists immediately to SQLite and re-reads that
  // recording's timeline so playback and the strip reflect it at once.

  const adjustRally = useCallback(
    (rally: SessionRally, globalStart: number, globalEnd: number) => {
      const offset = segmentOffset(rally.recordingIndex)
      const duration =
        session.segments.find((s) => s.index === rally.recordingIndex)
          ?.durationMs ?? Number.POSITIVE_INFINITY
      const startMs = Math.round(
        clamp(Math.min(globalStart, globalEnd) - offset, 0, duration)
      )
      const endMs = Math.round(
        clamp(Math.max(globalStart, globalEnd) - offset, 0, duration)
      )
      if (endMs <= startMs) return
      void trackedInvoke("update_rally", {
        path: rally.path,
        rallyId: rally.id,
        startMs,
        endMs,
      })
        .then(() => refreshTimeline(rally.path))
        .catch(() => {})
    },
    [segmentOffset, session, refreshTimeline]
  )

  const addAtPlayhead = useCallback(() => {
    if (!path) return
    const duration =
      session.segments.find((s) => s.index === index)?.durationMs ??
      Number.POSITIVE_INFINITY
    const start = Math.max(0, Math.round(currentMs - ADD_RALLY_HALF_MS))
    const end = Math.round(
      clamp(currentMs + ADD_RALLY_HALF_MS, start + 1, duration)
    )
    void trackedInvoke("add_rally", { path, startMs: start, endMs: end })
      .then(() => refreshTimeline(path))
      .catch(() => {})
  }, [path, index, currentMs, session, refreshTimeline])

  const deleteRally = useCallback(
    (rally: SessionRally) => {
      void trackedInvoke("delete_rally", {
        path: rally.path,
        rallyId: rally.id,
      })
        .then(() => refreshTimeline(rally.path))
        .catch(() => {})
    },
    [refreshTimeline]
  )

  // Split a rally at the global playhead: shrink it to end at the cut, then add a
  // new rally from the cut to the old end.
  const splitRally = useCallback(
    (rally: SessionRally, atGlobalMs: number) => {
      const offset = segmentOffset(rally.recordingIndex)
      const atLocal = Math.round(atGlobalMs - offset)
      if (atLocal <= rally.localStart || atLocal >= rally.localEnd) return
      void trackedInvoke("update_rally", {
        path: rally.path,
        rallyId: rally.id,
        startMs: rally.localStart,
        endMs: atLocal,
      })
        .then(() =>
          trackedInvoke("add_rally", {
            path: rally.path,
            startMs: atLocal,
            endMs: rally.localEnd,
          })
        )
        .then(() => refreshTimeline(rally.path))
        .catch(() => {})
    },
    [segmentOffset, refreshTimeline]
  )

  // Merge a rally with the next one in the same recording: stretch the first to
  // cover both, then delete the second.
  const mergeRallies = useCallback(
    (first: SessionRally, second: SessionRally) => {
      if (first.path !== second.path) return
      void trackedInvoke("update_rally", {
        path: first.path,
        rallyId: first.id,
        startMs: Math.min(first.localStart, second.localStart),
        endMs: Math.max(first.localEnd, second.localEnd),
      })
        .then(() =>
          trackedInvoke("delete_rally", {
            path: first.path,
            rallyId: second.id,
          })
        )
        .then(() => refreshTimeline(first.path))
        .catch(() => {})
    },
    [refreshTimeline]
  )

  // The single keymap definition: the one source of truth behind both the global
  // key handler and the `?` cheat-sheet, so the two can never drift.
  const keymap = useMemo<Keybinding[]>(() => {
    const plain = (e: KeyboardEvent) =>
      !e.ctrlKey && !e.metaKey && !e.altKey && !e.shiftKey
    return [
      {
        keys: ["Space", "K"],
        label: "Play / pause",
        match: (e) =>
          plain(e) && (e.code === "Space" || e.key.toLowerCase() === "k"),
        run: togglePlay,
      },
      {
        keys: ["Ctrl+←", "Ctrl+→"],
        label: "Seek ∓ 2.5s",
        match: (e) =>
          (e.ctrlKey || e.metaKey) &&
          !e.shiftKey &&
          !e.altKey &&
          (e.key === "ArrowLeft" || e.key === "ArrowRight"),
        run: (e) =>
          seekRelative(e.key === "ArrowLeft" ? -SEEK_FINE_MS : SEEK_FINE_MS),
      },
      {
        keys: ["←", "→"],
        label: "Seek ∓ 5s",
        match: (e) =>
          plain(e) && (e.key === "ArrowLeft" || e.key === "ArrowRight"),
        run: (e) =>
          seekRelative(
            e.key === "ArrowLeft" ? -SEEK_DEFAULT_MS : SEEK_DEFAULT_MS
          ),
      },
      {
        keys: ["Shift+←", "Shift+→"],
        label: "Seek ∓ 10s",
        match: (e) =>
          e.shiftKey &&
          !e.ctrlKey &&
          !e.metaKey &&
          !e.altKey &&
          (e.key === "ArrowLeft" || e.key === "ArrowRight"),
        run: (e) =>
          seekRelative(
            e.key === "ArrowLeft" ? -SEEK_COARSE_MS : SEEK_COARSE_MS
          ),
      },
      {
        keys: ["↑", "↓"],
        label: "Volume up / down",
        match: (e) =>
          plain(e) && (e.key === "ArrowUp" || e.key === "ArrowDown"),
        run: (e) =>
          changeVolume(e.key === "ArrowUp" ? VOLUME_STEP : -VOLUME_STEP),
      },
      {
        keys: [",", "."],
        label: "Frame step back / forward",
        match: (e) => plain(e) && (e.key === "," || e.key === "."),
        run: (e) => frameStep(e.key === "."),
      },
      {
        keys: ["[", "]"],
        label: "Prev / Next rally",
        match: (e) => plain(e) && (e.key === "[" || e.key === "]"),
        run: (e) => goToRally(e.key === "[" ? "prev" : "next"),
      },
      {
        keys: ["U"],
        label: "Next uncertain region",
        match: (e) => plain(e) && e.key.toLowerCase() === "u",
        run: goToUncertain,
      },
      {
        keys: ["L"],
        label: "Loop current rally",
        match: (e) => plain(e) && e.key.toLowerCase() === "l",
        run: toggleLoop,
      },
      {
        keys: ["M"],
        label: "Mute",
        match: (e) => plain(e) && e.key.toLowerCase() === "m",
        run: toggleMute,
      },
      {
        keys: ["Ctrl+-", "Ctrl+="],
        label: "Playback speed down / up",
        match: (e) =>
          (e.ctrlKey || e.metaKey) &&
          !e.altKey &&
          (e.key === "-" || e.key === "=" || e.key === "+" || e.key === "_"),
        run: (e) => stepSpeed(e.key === "-" || e.key === "_" ? -1 : 1),
      },
      {
        keys: ["Ctrl+0"],
        label: "Reset speed to 1×",
        match: (e) => (e.ctrlKey || e.metaKey) && !e.altKey && e.key === "0",
        run: resetSpeed,
      },
      {
        keys: ["?"],
        label: "Toggle this cheat-sheet",
        match: (e) => e.key === "?",
        run: () => setShowCheatSheet((s) => !s),
      },
      {
        keys: ["Alt + scroll over timeline"],
        label: "Zoom timeline at the cursor",
        match: () => false,
        run: () => {},
      },
    ]
  }, [
    togglePlay,
    seekRelative,
    changeVolume,
    frameStep,
    goToRally,
    goToUncertain,
    toggleLoop,
    toggleMute,
    stepSpeed,
    resetSpeed,
  ])

  // The one global key handler, at window capture so it can `preventDefault`
  // page-zoom. Ignores keystrokes while typing in an input/textarea.
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement | null
      if (
        target &&
        (target.isContentEditable ||
          target.tagName === "INPUT" ||
          target.tagName === "TEXTAREA" ||
          target.tagName === "SELECT")
      ) {
        return
      }
      const binding = keymap.find((b) => b.match(e))
      if (!binding) return
      e.preventDefault()
      e.stopPropagation()
      binding.run(e)
    }
    window.addEventListener("keydown", onKeyDown, { capture: true })
    return () =>
      window.removeEventListener("keydown", onKeyDown, { capture: true })
  }, [keymap])

  return (
    <div
      ref={containerRef}
      tabIndex={-1}
      className="flex h-full min-h-0 flex-col gap-4 outline-none"
    >
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
      <div
        ref={paneRef}
        className="min-h-0 w-full flex-1 rounded-lg bg-black"
      />
      {error ? (
        <div className="shrink-0 text-sm text-destructive" role="alert">
          {error}
        </div>
      ) : null}
      <TransportBar
        paused={paused}
        muted={muted}
        looping={looping}
        volume={volume}
        speed={SPEED_LADDER[speedIndex]}
        positionMs={globalPlayheadMs ?? currentMs}
        durationMs={session.totalMs}
        onTogglePlay={togglePlay}
        onFrameStep={frameStep}
        onToggleMute={toggleMute}
        onToggleLoop={toggleLoop}
        onStepSpeed={stepSpeed}
        onResetSpeed={resetSpeed}
        onShowKeys={() => setShowCheatSheet(true)}
      />
      <SessionTimeline
        session={session}
        recordingCount={recordings.length}
        globalPlayheadMs={globalPlayheadMs}
        reanalyzing={reanalyzing}
        canPrev={!atFirstRecording}
        canNext={!atLastRecording}
        editing={editing}
        onSeekGlobal={seekSession}
        onPrevRally={() => goToRally("prev")}
        onNextRally={() => goToRally("next")}
        onNextUncertain={goToUncertain}
        onReanalyze={handleReanalyze}
        onToggleEditing={() => setEditing((e) => !e)}
        onAdjustRally={adjustRally}
        onAddAtPlayhead={addAtPlayhead}
        onDeleteRally={deleteRally}
        onSplitRally={splitRally}
        onMergeRallies={mergeRallies}
      />
      {showCheatSheet ? (
        <CheatSheet keymap={keymap} onClose={() => setShowCheatSheet(false)} />
      ) : null}
    </div>
  )
}

/**
 * The transport-only control bar beneath the player: play/pause, exact
 * frame-step, a session-global timecode mirroring the session playhead and total
 * duration, a playback-speed indicator, a loop toggle, mute, and a volume
 * readout. It deliberately has **no scrubber**: the session timeline strip below
 * remains the single scrub/seek surface. The `?` button opens the cheat-sheet.
 */
function TransportBar({
  paused,
  muted,
  looping,
  volume,
  speed,
  positionMs,
  durationMs,
  onTogglePlay,
  onFrameStep,
  onToggleMute,
  onToggleLoop,
  onStepSpeed,
  onResetSpeed,
  onShowKeys,
}: {
  paused: boolean
  muted: boolean
  looping: boolean
  volume: number
  speed: number
  positionMs: number
  durationMs: number
  onTogglePlay: () => void
  onFrameStep: (forward: boolean) => void
  onToggleMute: () => void
  onToggleLoop: () => void
  onStepSpeed: (dir: -1 | 1) => void
  onResetSpeed: () => void
  onShowKeys: () => void
}) {
  return (
    <div className="flex shrink-0 flex-wrap items-center gap-2">
      <Button
        variant="outline"
        size="icon"
        onClick={onTogglePlay}
        title={paused ? "Play (Space)" : "Pause (Space)"}
      >
        {paused ? (
          <PlayIcon className="size-4" />
        ) : (
          <PauseIcon className="size-4" />
        )}
      </Button>
      <Button
        variant="outline"
        size="icon"
        onClick={() => onFrameStep(false)}
        title="Step back one frame (,)"
      >
        <StepBackIcon className="size-4" />
      </Button>
      <Button
        variant="outline"
        size="icon"
        onClick={() => onFrameStep(true)}
        title="Step forward one frame (.)"
      >
        <StepForwardIcon className="size-4" />
      </Button>
      <span className="ml-1 font-mono text-sm text-muted-foreground tabular-nums">
        {formatTimecode(positionMs)} / {formatTimecode(durationMs)}
      </span>
      <div className="ml-auto flex items-center gap-2">
        <div
          className="flex items-center"
          title="Playback speed (Ctrl+- / Ctrl+= , Ctrl+0 to reset)"
        >
          <Button
            variant="outline"
            size="icon"
            onClick={() => onStepSpeed(-1)}
            title="Slower (Ctrl+-)"
          >
            <span className="text-sm">−</span>
          </Button>
          <button
            type="button"
            onClick={onResetSpeed}
            title="Reset speed to 1× (Ctrl+0)"
            className="min-w-12 px-1 text-center font-mono text-sm text-muted-foreground tabular-nums hover:text-foreground"
          >
            {speed}×
          </button>
          <Button
            variant="outline"
            size="icon"
            onClick={() => onStepSpeed(1)}
            title="Faster (Ctrl+=)"
          >
            <span className="text-sm">+</span>
          </Button>
        </div>
        <Button
          variant={looping ? "default" : "outline"}
          size="icon"
          onClick={onToggleLoop}
          title={looping ? "Stop looping (L)" : "Loop the current rally (L)"}
        >
          <RepeatIcon className="size-4" />
        </Button>
        <Button
          variant="outline"
          size="icon"
          onClick={onToggleMute}
          title={muted ? "Unmute (M)" : "Mute (M)"}
        >
          {muted ? (
            <VolumeXIcon className="size-4" />
          ) : (
            <Volume2Icon className="size-4" />
          )}
        </Button>
        <span className="min-w-10 text-right font-mono text-sm text-muted-foreground tabular-nums">
          {muted ? "—" : `${volume}%`}
        </span>
        <Button
          variant="outline"
          size="icon"
          onClick={onShowKeys}
          title="Keyboard shortcuts (?)"
        >
          <KeyboardIcon className="size-4" />
        </Button>
      </div>
    </div>
  )
}

/**
 * The `?` cheat-sheet overlay: a modal listing every keybinding, rendered
 * straight from the single keymap definition so it can never drift from what the
 * keys actually do.
 */
function CheatSheet({
  keymap,
  onClose,
}: {
  keymap: Keybinding[]
  onClose: () => void
}) {
  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4"
      onClick={onClose}
    >
      <div
        className="max-h-full w-full max-w-md overflow-y-auto rounded-lg border bg-background p-5 shadow-lg"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="mb-3 flex items-center justify-between">
          <h2 className="font-medium">Keyboard shortcuts</h2>
          <Button
            variant="ghost"
            size="icon"
            onClick={onClose}
            title="Close (?)"
          >
            <XIcon className="size-4" />
          </Button>
        </div>
        <dl className="space-y-1.5 text-sm">
          {keymap.map((b) => (
            <div
              key={b.label}
              className="flex items-center justify-between gap-4"
            >
              <dt className="text-muted-foreground">{b.label}</dt>
              <dd className="flex flex-wrap justify-end gap-1">
                {b.keys.map((k) => (
                  <kbd
                    key={k}
                    className="rounded border bg-muted px-1.5 py-0.5 font-mono text-xs"
                  >
                    {k}
                  </kbd>
                ))}
              </dd>
            </div>
          ))}
        </dl>
      </div>
    </div>
  )
}

/**
 * The session timeline strip beneath the player: every recording's draft
 * timeline stitched onto one continuous, horizontally-scrollable axis at a fixed
 * pixels-per-second scale. Each recording's audio waveform fills its span with
 * detected rallies drawn as blocks over it, gaps the empty space between them
 * (ADR 0001), low-confidence rallies styled as uncertain regions (ADR 0002), and
 * faint dividers marking recording boundaries. The playhead tracks the session
 * position and auto-scrolls into view. Clicking the strip seeks the session
 * (crossing recordings as needed); a rally block seeks to its start.
 *
 * In correction mode (issue #7) each edit is resolved against the recording that
 * owns the rally: drag an edge to adjust, split at the playhead, merge with the
 * next rally in the same recording, add around the playhead, or delete.
 */
function SessionTimeline({
  session,
  recordingCount,
  globalPlayheadMs,
  reanalyzing,
  canPrev,
  canNext,
  editing,
  onSeekGlobal,
  onPrevRally,
  onNextRally,
  onNextUncertain,
  onReanalyze,
  onToggleEditing,
  onAdjustRally,
  onAddAtPlayhead,
  onDeleteRally,
  onSplitRally,
  onMergeRallies,
}: {
  session: SessionModel
  recordingCount: number
  globalPlayheadMs: number | null
  reanalyzing: boolean
  canPrev: boolean
  canNext: boolean
  editing: boolean
  onSeekGlobal: (globalMs: number) => void
  onPrevRally: () => void
  onNextRally: () => void
  onNextUncertain: () => void
  onReanalyze: () => void
  onToggleEditing: () => void
  onAdjustRally: (
    rally: SessionRally,
    globalStart: number,
    globalEnd: number
  ) => void
  onAddAtPlayhead: () => void
  onDeleteRally: (rally: SessionRally) => void
  onSplitRally: (rally: SessionRally, atGlobalMs: number) => void
  onMergeRallies: (first: SessionRally, second: SessionRally) => void
}) {
  // The selected rally, by "path:id" so a row id shared across recordings can
  // never be ambiguous.
  const [selectedKey, setSelectedKey] = useState<string | null>(null)
  const [drag, setDrag] = useState<{
    key: string
    edge: "start" | "end"
    anchorGlobalMs: number
    globalMs: number
    minGlobalMs: number
    maxGlobalMs: number
  } | null>(null)
  const scrollRef = useRef<HTMLDivElement>(null)
  const contentRef = useRef<HTMLDivElement>(null)
  const [pxPerSec, setPxPerSec] = useState(SESSION_PX_PER_SEC_DEFAULT)

  const totalMs = session.totalMs
  const totalPx = (totalMs / 1000) * pxPerSec
  const rallyKey = (r: SessionRally) => `${r.path}:${r.id}`

  const canZoomIn = pxPerSec < SESSION_PX_PER_SEC_MAX
  const canZoomOut = pxPerSec > SESSION_PX_PER_SEC_MIN
  const zoomBy = useCallback((factor: number) => {
    setPxPerSec((p) =>
      clamp(p * factor, SESSION_PX_PER_SEC_MIN, SESSION_PX_PER_SEC_MAX)
    )
  }, [])

  // Map a client x over the strip content to a session-global time (ms, clamped).
  const xToMs = useCallback(
    (clientX: number): number => {
      const rect = contentRef.current?.getBoundingClientRect()
      if (!rect || rect.width === 0) return 0
      const frac = (clientX - rect.left) / rect.width
      return Math.round(clamp(frac, 0, 1) * totalMs)
    },
    [totalMs]
  )

  // While dragging a rally edge, follow the pointer and persist on release.
  useEffect(() => {
    if (!drag) return
    const move = (e: PointerEvent) =>
      setDrag((d) =>
        d
          ? {
              ...d,
              globalMs: clamp(xToMs(e.clientX), d.minGlobalMs, d.maxGlobalMs),
            }
          : d
      )
    const up = () => {
      setDrag((d) => {
        if (d) {
          const rally = session.rallies.find((r) => rallyKey(r) === d.key)
          if (rally) {
            const start = d.edge === "start" ? d.globalMs : d.anchorGlobalMs
            const end = d.edge === "end" ? d.globalMs : d.anchorGlobalMs
            onAdjustRally(rally, start, end)
          }
        }
        return null
      })
    }
    window.addEventListener("pointermove", move)
    window.addEventListener("pointerup", up)
    return () => {
      window.removeEventListener("pointermove", move)
      window.removeEventListener("pointerup", up)
    }
  }, [drag, session, xToMs, onAdjustRally])

  // Alt+scroll over the strip zooms centered on the cursor.
  useEffect(() => {
    const el = scrollRef.current
    if (!el) return
    const onWheel = (e: WheelEvent) => {
      if (!e.altKey) return
      e.preventDefault()
      const factor =
        e.deltaY < 0 ? ALT_SCROLL_ZOOM_FACTOR : 1 / ALT_SCROLL_ZOOM_FACTOR
      const rect = el.getBoundingClientRect()
      const cursorContentPx = e.clientX - rect.left + el.scrollLeft
      setPxPerSec((p) => {
        const nextPx = clamp(
          p * factor,
          SESSION_PX_PER_SEC_MIN,
          SESSION_PX_PER_SEC_MAX
        )
        const scale = nextPx / p
        el.scrollLeft = cursorContentPx * scale - (e.clientX - rect.left)
        return nextPx
      })
    }
    el.addEventListener("wheel", onWheel, { passive: false })
    return () => el.removeEventListener("wheel", onWheel)
  }, [])

  // Keep the playhead in view as playback advances, crosses recordings, or zooms.
  useEffect(() => {
    const el = scrollRef.current
    if (!el || globalPlayheadMs == null || totalMs === 0) return
    const x = (globalPlayheadMs / 1000) * pxPerSec
    const margin = el.clientWidth * 0.15
    if (
      x < el.scrollLeft + margin ||
      x > el.scrollLeft + el.clientWidth - margin
    ) {
      el.scrollLeft = x - el.clientWidth / 2
    }
  }, [globalPlayheadMs, totalMs, pxPerSec])

  const segmentingNow =
    reanalyzing ||
    session.segments.some((s) => s.timeline?.segment_state === "unknown")
  const failedCount = session.segments.filter(
    (s) => s.timeline?.segment_state === "failed"
  ).length
  const rallyCount = session.rallies.length
  const uncertainCount = session.rallies.filter(
    (r) => r.confidence < UNCERTAIN_CONFIDENCE
  ).length
  const unprocessed = recordingCount - session.placedCount
  const hasRallies = totalMs > 0 && rallyCount > 0

  let summary
  if (totalMs === 0 && segmentingNow) {
    summary = (
      <span className="flex items-center gap-2">
        <Loader2Icon className="size-4 animate-spin" />
        Detecting rallies…
      </span>
    )
  } else if (rallyCount === 0 && failedCount > 0) {
    summary = <span>Couldn&apos;t detect rallies for this session.</span>
  } else if (!hasRallies) {
    summary = <span>No rallies detected.</span>
  } else {
    summary = (
      <span>
        {rallyCount} {rallyCount === 1 ? "rally" : "rallies"} across the session
        {uncertainCount > 0 ? (
          <span
            className="text-amber-600 dark:text-amber-500"
            title="Low-confidence spans the segmenter is unsure about — worth checking."
          >
            {" "}
            · {uncertainCount} uncertain
          </span>
        ) : null}
      </span>
    )
  }

  const selectedIndex = session.rallies.findIndex(
    (r) => rallyKey(r) === selectedKey
  )
  const selected = selectedIndex >= 0 ? session.rallies[selectedIndex] : null
  const next =
    selectedIndex >= 0 ? (session.rallies[selectedIndex + 1] ?? null) : null
  const mergeTarget =
    selected && next && next.recordingIndex === selected.recordingIndex
      ? next
      : null
  const canSplit =
    selected !== null &&
    globalPlayheadMs !== null &&
    globalPlayheadMs > selected.globalStart &&
    globalPlayheadMs < selected.globalEnd
  const canMerge = mergeTarget !== null

  const playheadPx =
    globalPlayheadMs !== null ? (globalPlayheadMs / 1000) * pxPerSec : null

  return (
    <div className="shrink-0 space-y-2">
      <div className="flex flex-wrap items-center justify-between gap-2 text-sm text-muted-foreground">
        {summary}
        <div className="flex items-center gap-2">
          <Button
            variant="outline"
            size="sm"
            onClick={onPrevRally}
            disabled={!hasRallies && !canPrev}
            title="Jump to the previous rally."
          >
            <ChevronLeftIcon className="size-4" />
            Prev rally
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={onNextRally}
            disabled={!hasRallies && !canNext}
            title="Jump to the next rally."
          >
            Next rally
            <ChevronRightIcon className="size-4" />
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={onNextUncertain}
            disabled={uncertainCount === 0}
            title="Jump to the next uncertain region — a span the segmenter doubts, worth checking."
          >
            <TriangleAlertIcon className="size-4" />
            Next uncertain
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={onReanalyze}
            disabled={segmentingNow}
            title="Re-run rally detection for the current recording in place (for tuning the segmenter)."
          >
            <RotateCwIcon
              className={`size-4 ${reanalyzing ? "animate-spin" : ""}`}
            />
            Re-analyze
          </Button>
          <Button
            variant={editing ? "default" : "outline"}
            size="sm"
            onClick={onToggleEditing}
            disabled={!hasRallies}
            title="Correct the draft timeline: drag rally edges, split, merge, add, or delete."
          >
            <PencilIcon className="size-4" />
            {editing ? "Done editing" : "Edit timeline"}
          </Button>
        </div>
      </div>
      {editing ? (
        <div className="flex flex-wrap items-center gap-2 rounded-md border border-dashed bg-muted/40 px-3 py-2 text-sm text-muted-foreground">
          <span>
            {selected
              ? `Rally ${selectedIndex + 1} selected (${formatClock(
                  selected.globalStart
                )}–${formatClock(selected.globalEnd)} · ${fileName(selected.path)})`
              : "Drag a rally's edge to adjust it, or click a rally to select it."}
          </span>
          <div className="ml-auto flex flex-wrap items-center gap-2">
            <Button
              variant="outline"
              size="sm"
              onClick={onAddAtPlayhead}
              title="Add a rally over a span the segmenter missed (around the playhead, in the current recording)."
            >
              <PlusIcon className="size-4" />
              Add at playhead
            </Button>
            <Button
              variant="outline"
              size="sm"
              onClick={() =>
                selected &&
                globalPlayheadMs !== null &&
                onSplitRally(selected, globalPlayheadMs)
              }
              disabled={!canSplit}
              title="Split the selected rally in two at the playhead."
            >
              <ScissorsIcon className="size-4" />
              Split at playhead
            </Button>
            <Button
              variant="outline"
              size="sm"
              onClick={() =>
                selected && mergeTarget && onMergeRallies(selected, mergeTarget)
              }
              disabled={!canMerge}
              title="Merge the selected rally with the next one in the same recording."
            >
              Merge with next
            </Button>
            <Button
              variant="outline"
              size="sm"
              onClick={() => {
                if (selected) {
                  onDeleteRally(selected)
                  setSelectedKey(null)
                }
              }}
              disabled={!selected}
              title="Delete the selected rally (its span becomes a gap)."
            >
              <Trash2Icon className="size-4" />
              Delete
            </Button>
          </div>
        </div>
      ) : null}
      {hasRallies ? (
        <>
          <div
            ref={scrollRef}
            className="w-full overflow-x-auto rounded-md bg-muted"
          >
            <div
              ref={contentRef}
              className="relative h-16 cursor-pointer"
              style={{ width: `${Math.max(totalPx, 1)}px` }}
              onClick={(e) => {
                if (editing) {
                  setSelectedKey(null)
                  return
                }
                onSeekGlobal(xToMs(e.clientX))
              }}
            >
              {session.segments.map((seg) => {
                if (seg.durationMs == null || !seg.timeline) return null
                const left = (seg.offsetMs / 1000) * pxPerSec
                const width = (seg.durationMs / 1000) * pxPerSec
                return (
                  <div
                    key={seg.path}
                    className="pointer-events-none absolute inset-y-0"
                    style={{ left: `${left}px`, width: `${width}px` }}
                  >
                    <Waveform peaks={seg.timeline.waveform} />
                    {seg.index > 0 ? (
                      <div className="absolute inset-y-0 left-0 w-px bg-foreground/30" />
                    ) : null}
                    <span className="absolute top-0.5 left-1 max-w-full truncate text-[10px] text-muted-foreground/70">
                      {fileName(seg.path)}
                    </span>
                  </div>
                )
              })}
              {session.rallies.map((rally, i) => {
                const key = rallyKey(rally)
                const dragging = drag?.key === key ? drag : null
                const gStart = dragging
                  ? dragging.edge === "start"
                    ? dragging.globalMs
                    : dragging.anchorGlobalMs
                  : rally.globalStart
                const gEnd = dragging
                  ? dragging.edge === "end"
                    ? dragging.globalMs
                    : dragging.anchorGlobalMs
                  : rally.globalEnd
                const lo = Math.min(gStart, gEnd)
                const hi = Math.max(gStart, gEnd)
                const left = (lo / 1000) * pxPerSec
                const width = ((hi - lo) / 1000) * pxPerSec
                const uncertain = rally.confidence < UNCERTAIN_CONFIDENCE
                const isSelected = editing && key === selectedKey
                const seg = session.segments.find(
                  (s) => s.index === rally.recordingIndex
                )
                const minGlobalMs = seg?.offsetMs ?? 0
                const maxGlobalMs = minGlobalMs + (seg?.durationMs ?? 0)
                return (
                  <button
                    key={key}
                    type="button"
                    onClick={(e) => {
                      e.stopPropagation()
                      if (editing) {
                        setSelectedKey(key)
                      } else {
                        onSeekGlobal(rally.globalStart)
                      }
                    }}
                    className={`absolute inset-y-0 rounded-sm transition-opacity hover:opacity-80 focus:outline-none ${
                      uncertain
                        ? "border border-amber-500/70 bg-amber-500/40"
                        : "bg-primary/70"
                    } ${isSelected ? "ring-2 ring-foreground ring-offset-1 ring-offset-muted" : ""}`}
                    style={{
                      left: `${left}px`,
                      width: `${Math.max(width, 3)}px`,
                    }}
                    title={`Rally ${i + 1}: ${formatClock(rally.globalStart)}–${formatClock(
                      rally.globalEnd
                    )}${uncertain ? " (uncertain)" : ""} · confidence ${Math.round(
                      rally.confidence * 100
                    )}% · ${fileName(rally.path)}`}
                  >
                    {editing ? (
                      <>
                        <span
                          role="separator"
                          aria-label="Drag rally start"
                          onClick={(e) => e.stopPropagation()}
                          onPointerDown={(e) => {
                            e.stopPropagation()
                            e.preventDefault()
                            setSelectedKey(key)
                            setDrag({
                              key,
                              edge: "start",
                              anchorGlobalMs: rally.globalEnd,
                              globalMs: rally.globalStart,
                              minGlobalMs,
                              maxGlobalMs,
                            })
                          }}
                          className="absolute inset-y-0 left-0 w-1.5 cursor-ew-resize rounded-l-sm bg-foreground/70 hover:bg-foreground"
                        />
                        <span
                          role="separator"
                          aria-label="Drag rally end"
                          onClick={(e) => e.stopPropagation()}
                          onPointerDown={(e) => {
                            e.stopPropagation()
                            e.preventDefault()
                            setSelectedKey(key)
                            setDrag({
                              key,
                              edge: "end",
                              anchorGlobalMs: rally.globalStart,
                              globalMs: rally.globalEnd,
                              minGlobalMs,
                              maxGlobalMs,
                            })
                          }}
                          className="absolute inset-y-0 right-0 w-1.5 cursor-ew-resize rounded-r-sm bg-foreground/70 hover:bg-foreground"
                        />
                      </>
                    ) : null}
                  </button>
                )
              })}
              {playheadPx !== null ? (
                <div
                  className="pointer-events-none absolute inset-y-0 w-0.5 bg-foreground"
                  style={{ left: `${playheadPx}px` }}
                />
              ) : null}
            </div>
          </div>
          <div className="flex items-center gap-2 text-sm text-muted-foreground">
            <span className="mr-1">Zoom</span>
            <Button
              variant="outline"
              size="sm"
              onClick={() => zoomBy(1 / SESSION_ZOOM_FACTOR)}
              disabled={!canZoomOut}
              title="Zoom out — fit more of the session on screen."
            >
              <ZoomOutIcon className="size-4" />
            </Button>
            <Button
              variant="outline"
              size="sm"
              onClick={() => zoomBy(SESSION_ZOOM_FACTOR)}
              disabled={!canZoomIn}
              title="Zoom in — see finer detail around the playhead."
            >
              <ZoomInIcon className="size-4" />
            </Button>
            <Button
              variant="outline"
              size="sm"
              onClick={() => setPxPerSec(SESSION_PX_PER_SEC_DEFAULT)}
              disabled={pxPerSec === SESSION_PX_PER_SEC_DEFAULT}
              title="Reset the timeline zoom."
            >
              Reset
            </Button>
          </div>
          {unprocessed > 0 ? (
            <p className="text-xs text-muted-foreground">
              {unprocessed} more{" "}
              {unprocessed === 1 ? "recording" : "recordings"} still being
              prepared — they’ll join the timeline once segmented.
            </p>
          ) : null}
        </>
      ) : null}
    </div>
  )
}

/**
 * The audio waveform under the rally blocks: each downsampled peak is a vertical
 * bar centred on the strip, so shuttle hits read as spikes and rally boundaries
 * can be eyeballed where the blocks overlay them. Drawn behind the blocks at low
 * contrast and stretched to fill its recording's span.
 */
function Waveform({ peaks }: { peaks: number[] }) {
  if (peaks.length === 0) return null
  return (
    <svg
      className="pointer-events-none absolute inset-0 size-full text-muted-foreground/50"
      viewBox={`0 0 ${peaks.length} 1`}
      preserveAspectRatio="none"
      aria-hidden
    >
      {peaks.map((peak, i) => {
        const h = Math.max(peak, 0.02)
        return (
          <rect
            key={i}
            x={i + 0.1}
            y={(1 - h) / 2}
            width={0.8}
            height={h}
            fill="currentColor"
          />
        )
      })}
    </svg>
  )
}
