"use client"

import {
  forwardRef,
  useCallback,
  useEffect,
  useImperativeHandle,
  useMemo,
  useRef,
  useState,
  type Dispatch,
  type SetStateAction,
} from "react"
import { listen } from "@tauri-apps/api/event"
import {
  ArrowLeftIcon,
  ChevronLeftIcon,
  ChevronRightIcon,
  CrosshairIcon,
  DownloadIcon,
  FastForwardIcon,
  FlagIcon,
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
  TimerIcon,
  Trash2Icon,
  TriangleAlertIcon,
  Volume2Icon,
  VolumeXIcon,
  XIcon,
  ZoomInIcon,
  ZoomOutIcon,
} from "lucide-react"

import { Button } from "@/components/ui/button"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import { trackedInvoke } from "@/lib/tauri"
import { formatCaptureDay } from "@/lib/utils"
import {
  SPEED_LADDER,
  UNCERTAIN_CONFIDENCE,
  addAtPlayheadEdit,
  adjustRallyEdit,
  buildSessionModel,
  clamp,
  clampVolume,
  gapSkipAction,
  mergeRallyEdit,
  nextRallyMs,
  nextUncertainMs,
  prevRallyAction,
  resumeStartMs,
  resumeTickLanded,
  seekTarget,
  speedIndexForValue,
  splitRallyEdit,
  stepSpeedIndex,
  stripScrollTarget,
  type EditPlan,
  type PlaylistRecording,
  type Resume,
  type SessionModel,
  type SessionRally,
  type Timeline,
} from "./recording-player-transport"
import { useMpvSurface } from "./use-mpv-surface"

export type { PlaylistRecording }

interface RecordingPlayerProps {
  /**
   * The session's recordings, ordered by capture time. Their rallies are
   * flattened into one continuous playlist played back-to-back (the North Star).
   */
  recordings: PlaylistRecording[]
  /** Index of the recording to open first (defaults to the session's start). */
  startIndex?: number
  /** The session's capture day, shown in the top bar. */
  day?: string
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

/**
 * How close (ms) the playhead must get to a crossing's resume target before its
 * `time-pos` ticks count as "landed" and normal playback resumes. mpv's resume
 * seek is exact but async, so the first settled tick sits at the target; this
 * slack only clears the near-0 pre-seek lead-in the freshly-loaded file emits
 * before the seek takes hold.
 */
const RESUME_TICK_TOL_MS = 250

/** How much each Alt+scroll notch over the timeline zooms. */
const ALT_SCROLL_ZOOM_FACTOR = 1.15

/**
 * Rally length threshold (CONTEXT.md: every rally is classified long or short
 * by duration, objectively and automatically). UI-only until length filtering
 * lands; 15s reads as a sustained exchange.
 */
const LONG_RALLY_MS = 15_000

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
  day,
  onBack,
}: RecordingPlayerProps) {
  // Gap-free playback is the default (the North Star), but a manual playhead move
  // opts out: dragging into a gap, or clicking an empty part of the session
  // strip, flips `freePlayRef` true and gap-skipping stands down so footage
  // between rallies can be watched. A rally-targeted action restores gap-free
  // playback. It's a ref, not state, so the `time-pos` listener reads the latest
  // value synchronously without re-subscribing on every change.
  const freePlayRef = useRef(false)
  // Focus host so the keymap's window handler is the only thing on keystrokes.
  const containerRef = useRef<HTMLDivElement>(null)
  // Imperative handle on the timeline strip so the `F` key / button can recenter
  // it on the playhead (and re-arm follow) without lifting its scroll state.
  const timelineRef = useRef<SessionTimelineHandle>(null)
  // True from the moment a load is initiated until mpv confirms the new file is
  // open (the `mpv:file-loaded` event, fired after its baked-in resume seek has
  // landed). While true the `time-pos` listener drops every tick, because a tick
  // carries no file identity: the *outgoing* recording keeps emitting ticks at
  // its own (often large) playhead position after a `loadfile`, and the
  // freshly-loaded file emits near-0 pre-seek ticks before the resume takes hold.
  // Acting on either runs gap-skip against the wrong position — a stale far-past
  // tick reads as "past the last rally" and yanks the playhead into the *next*
  // recording, overriding a click that crossed into this one. Set synchronously
  // at the crossing (not in the load effect) so no tick slips through the
  // render→effect gap; a ref so the listener reads it without re-subscribing.
  // Starts true so the initial mount load is gated the same way.
  const awaitingLoadRef = useRef(true)
  // The second, position gate, working with `awaitingLoadRef`: the recording-local
  // time the pending crossing resumes at, while the (async) resume seek lands. The
  // identity gate above drops every tick until `mpv:file-loaded`, but mpv applies
  // the seek a moment *after* the file opens, so the file's first ticks are still
  // near 0 — this drops them until the playhead reaches the target, so gap-skip
  // can't read a transient ~0 as "before the first rally" and yank there. Because
  // the identity gate has already filtered out the outgoing recording's stale
  // (often far-past) ticks, a plain "have we reached the target?" check is enough
  // here. Null once landed, or when a crossing has no specific target yet.
  const resumeTargetRef = useRef<number | null>(null)

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
  // Whether gap-free playback is on (the North Star default). When off, the
  // playhead runs straight through the gaps between rallies — a manual "watch
  // everything" mode toggled from the transport bar or the `G` key.
  const [gapSkipEnabled, setGapSkipEnabled] = useState(true)
  const [editing, setEditing] = useState(false)
  const [showCheatSheet, setShowCheatSheet] = useState(false)
  const [error, setError] = useState<string | null>(null)
  // Bumped by Re-analyze to re-trigger the timeline fetch/poll; `reanalyzing`
  // guards the button (ADR 0002, the tuning loop).
  const [reanalyzeNonce, setReanalyzeNonce] = useState(0)
  const [reanalyzing, setReanalyzing] = useState(false)
  // Timeline zoom and playhead-follow are owned here rather than by the strip,
  // so the status bar (which lives outside the strip) can drive them; the
  // strip's own wheel/scroll handlers mutate them through the setters.
  const [pxPerSec, setPxPerSec] = useState(SESSION_PX_PER_SEC_DEFAULT)
  const [following, setFollowing] = useState(true)

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

  // The native mpv surface's whole lifecycle — rect tracking, show/hide on
  // mount/unmount, and suppression under the cheat-sheet or while minimized
  // (ADR 0008) — lives behind this hook; the returned ref marks the empty pane
  // the surface is slaved to.
  const paneRef = useMpvSurface(showCheatSheet)

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
    // Gate the playhead immediately: stale ticks from the recording we're leaving
    // must be dropped until the new file confirms loaded, or gap-skip acts on the
    // old position and crosses on past where we meant to land. Set here (not in
    // the load effect, which is deferred) so no tick slips through in between.
    awaitingLoadRef.current = true
    setCurrentMs(0)
    setIndex(next)
    setPendingSeek(resume)
  }, [])

  // Load the current recording directly from disk and start playing it (ADR
  // 0008). A boundary crossing re-runs this on the new `path`, carrying the
  // resume position *into the load*: mpv applies `startMs` on `FILE_LOADED`,
  // atomic with opening the file, so a click that lands in another recording
  // resumes where it landed instead of racing a separate seek against the
  // still-loading file (which mpv drops, leaving playback at 0). `pendingSeek`
  // and `rallies` are read from the crossing commit, not subscribed — re-running
  // this on either would reload the file mid-playback — hence the deps lint-off.
  useEffect(() => {
    if (!path) return
    const startMs =
      pendingSeek != null ? resumeStartMs(pendingSeek, rallies) : null
    // Re-assert both gates (the mount load reaches here without a crossing, and
    // re-running this effect means a fresh file is opening either way): drop every
    // tick until `mpv:file-loaded`, then drop the new file's pre-seek ticks until
    // the playhead reaches `startMs`. A null target means no specific resume (a
    // deferred `start`/`end` whose rallies haven't arrived) — play from the top.
    awaitingLoadRef.current = true
    resumeTargetRef.current = startMs
    void trackedInvoke("mpv_load", { path, startMs })
      .then(() => {
        // `mpv_load` unpauses; `paused` reconciles from the `mpv:pause` event.
        setError(null)
      })
      .catch((e) => setError(String(e)))
    if (startMs != null) {
      // Resolved here → the deferred resume effect below has nothing left to do.
      /* eslint-disable-next-line react-hooks/set-state-in-effect -- carrying the resume into the load is the point of the effect */
      setCurrentMs(startMs)
      setPendingSeek(null)
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps -- resume/rallies are the crossing-commit values, deliberately not deps (they'd reload the file)
  }, [path])

  // Re-apply the user's speed/volume/mute after a load. mpv resets these to its
  // defaults when a new file opens and *reports those defaults* through the
  // property events (issue #42) — reporting alone can't carry the user's
  // session-wide preference across a recording boundary, so this re-push stays
  // load-bearing. Its job is preference persistence, not UI sync (the events do
  // that); the events then confirm the re-applied values. Reads the current
  // transport state at the crossing commit, hence the path-only deps.
  useEffect(() => {
    if (!path) return
    void trackedInvoke("mpv_set_speed", {
      speed: SPEED_LADDER[speedIndex],
    }).catch(() => {})
    void trackedInvoke("mpv_set_volume", { volume }).catch(() => {})
    void trackedInvoke("mpv_set_mute", { muted }).catch(() => {})
    // Re-applied per recording (the `path` dep); changes within a recording are
    // commanded by the transport actions themselves.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [path])

  // The deferred half of a boundary crossing: a `start`/`end` resume whose
  // recording wasn't segmented yet at load time (so `resumeStartMs` returned
  // null and the load couldn't carry it). Once its draft arrives the file is
  // already loaded, so a plain seek lands cleanly — no race. A specific `{ atMs }`
  // and an already-segmented `start`/`end` are resolved by the load effect above,
  // which clears `pendingSeek`, so they never reach here.
  useEffect(() => {
    /* eslint-disable react-hooks/set-state-in-effect -- resuming a crossed-into recording seeks (which sets state), the whole point of the effect */
    if (pendingSeek == null || typeof pendingSeek === "object") return
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
      // Gap-skipping off → play straight through the gaps (mpv's `ended` event
      // still crosses recordings at EOF). Looping a rally is independent of the
      // toggle, so it's left to run.
      if (!gapSkipEnabled && !looping) return
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
          // `paused` reconciles from the `mpv:pause` event (issue #42).
          void trackedInvoke("mpv_set_pause", { paused: true }).catch(() => {})
          break
        case "none":
          break
      }
    },
    [
      rallies,
      looping,
      gapSkipEnabled,
      atLastRecording,
      seekTo,
      goToRecording,
      index,
    ]
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

  // The playhead, end, error, and transport UI states all come from mpv's event
  // stream (ADR 0008, issue #35). Each `time-pos` tick drives the playhead and
  // runs gap-skip (issue #36), where the old webview `timeUpdate` handler used
  // to; the pause/speed/volume/mute events reconcile the transport controls from
  // the player's real state, so a silently-failed `mpv_set_*` can't leave the UI
  // out of sync (issue #42) — the controls are never set optimistically.
  useEffect(() => {
    const unlisten: Array<() => void> = []
    void listen<number>("mpv:time-pos", (event) => {
      // Identity gate: drop every tick until the new file confirms loaded. A tick
      // carries no file identity, so before `mpv:file-loaded` it's a stale (often
      // far-past) position from the recording we're leaving — acting on it runs
      // gap-skip against the wrong position and crosses on past the resume target.
      if (awaitingLoadRef.current) return
      const ms = event.payload
      // Position gate: the file is open but mpv applies the resume seek a moment
      // later, so its first ticks are still near 0. Drop them until the playhead
      // reaches the target, then resume normally — otherwise gap-skip reads the
      // transient ~0 as "before the first rally" and yanks the playhead there.
      const target = resumeTargetRef.current
      if (target != null) {
        if (!resumeTickLanded(ms, target, RESUME_TICK_TOL_MS)) return
        resumeTargetRef.current = null
      }
      setCurrentMs(ms)
      handlersRef.current.skipGaps(ms)
    }).then((u) => unlisten.push(u))
    // The new file is open and its baked-in resume seek has landed: the next
    // `time-pos` reflects the resumed position, so reopen the playhead gate.
    void listen("mpv:file-loaded", () => {
      awaitingLoadRef.current = false
    }).then((u) => unlisten.push(u))
    void listen("mpv:ended", () => handlersRef.current.handleEnded()).then(
      (u) => unlisten.push(u)
    )
    void listen<string>("mpv:error", (event) => {
      // A load that errors never fires `mpv:file-loaded`; reopen both gates so a
      // failed crossing can't leave the playhead frozen.
      awaitingLoadRef.current = false
      resumeTargetRef.current = null
      setError(event.payload ?? "playback failed")
    }).then((u) => unlisten.push(u))
    // Transport reconciliation (issue #42): mpv reports its own pause/speed/
    // volume/mute, so the controls reflect the player rather than a hopeful
    // optimistic write. mpv reports speed/volume as raw numbers; the UI tracks a
    // speed *ladder index* and a rounded volume.
    void listen<boolean>("mpv:pause", (event) =>
      setPaused(event.payload)
    ).then((u) => unlisten.push(u))
    void listen<number>("mpv:speed", (event) =>
      setSpeedIndex(speedIndexForValue(event.payload))
    ).then((u) => unlisten.push(u))
    void listen<number>("mpv:volume", (event) =>
      setVolume(Math.round(event.payload))
    ).then((u) => unlisten.push(u))
    void listen<boolean>("mpv:mute", (event) =>
      setMuted(event.payload)
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

  // The transport actions only *command* mpv; the resulting state lands via the
  // `mpv:pause`/`speed`/`volume`/`mute` events above (issue #42), so a command
  // that silently fails leaves no optimistic value the player never reached.

  const togglePlay = useCallback(() => {
    void trackedInvoke("mpv_set_pause", { paused: !paused }).catch(() => {})
  }, [paused])

  const toggleMute = useCallback(() => {
    void trackedInvoke("mpv_set_mute", { muted: !muted }).catch(() => {})
  }, [muted])

  const toggleLoop = useCallback(() => setLooping((l) => !l), [])

  const toggleGapSkip = useCallback(() => setGapSkipEnabled((g) => !g), [])

  // Recenter the timeline strip on the playhead and re-arm follow (the strip
  // stops tracking the playhead once you scroll it away).
  const jumpToPlayhead = useCallback(
    () => timelineRef.current?.scrollToPlayhead(),
    []
  )

  const zoomTimeline = useCallback((factor: number) => {
    setPxPerSec((p) =>
      clamp(p * factor, SESSION_PX_PER_SEC_MIN, SESSION_PX_PER_SEC_MAX)
    )
  }, [])

  const changeVolume = useCallback(
    (delta: number) => {
      void trackedInvoke("mpv_set_volume", {
        volume: clampVolume(volume + delta),
      }).catch(() => {})
    },
    [volume]
  )

  const stepSpeed = useCallback(
    (dir: 1 | -1) => {
      void trackedInvoke("mpv_set_speed", {
        speed: SPEED_LADDER[stepSpeedIndex(speedIndex, dir)],
      }).catch(() => {})
    },
    [speedIndex]
  )

  const resetSpeed = useCallback(() => {
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

  // The five inline corrections (issue #7). The boundary math and integrity
  // guards that decide what reaches SQLite live behind the transport seam (so
  // they're unit-tested without mpv); here each callback resolves a plan and
  // hands it to `runEdit`, which persists its ops in order then re-reads the
  // affected recording's timeline so playback and the strip reflect it at once.

  // Recording-local duration of a recording, or +Infinity until it's segmented
  // (the edit math leaves an unknown-duration recording uncapped).
  const recordingDuration = useCallback(
    (recordingIndex: number) =>
      session.segments.find((s) => s.index === recordingIndex)?.durationMs ??
      Number.POSITIVE_INFINITY,
    [session]
  )

  const runEdit = useCallback(
    (plan: EditPlan) => {
      if (plan.kind !== "ops" || plan.ops.length === 0) return
      const recordingPath = plan.ops[0].path
      let chain: Promise<unknown> = Promise.resolve()
      for (const op of plan.ops) {
        const { command, ...args } = op
        chain = chain.then(() => trackedInvoke(command, args))
      }
      void chain.then(() => refreshTimeline(recordingPath)).catch(() => {})
    },
    [refreshTimeline]
  )

  const adjustRally = useCallback(
    (rally: SessionRally, globalStart: number, globalEnd: number) => {
      runEdit(
        adjustRallyEdit(
          rally,
          globalStart,
          globalEnd,
          segmentOffset(rally.recordingIndex),
          recordingDuration(rally.recordingIndex)
        )
      )
    },
    [segmentOffset, recordingDuration, runEdit]
  )

  const addAtPlayhead = useCallback(() => {
    if (!path) return
    runEdit(
      addAtPlayheadEdit(
        path,
        currentMs,
        recordingDuration(index),
        ADD_RALLY_HALF_MS
      )
    )
  }, [path, index, currentMs, recordingDuration, runEdit])

  const deleteRally = useCallback(
    (rally: SessionRally) => {
      runEdit({
        kind: "ops",
        ops: [{ command: "delete_rally", path: rally.path, rallyId: rally.id }],
      })
    },
    [runEdit]
  )

  const splitRally = useCallback(
    (rally: SessionRally, atGlobalMs: number) => {
      runEdit(
        splitRallyEdit(rally, atGlobalMs, segmentOffset(rally.recordingIndex))
      )
    },
    [segmentOffset, runEdit]
  )

  const mergeRallies = useCallback(
    (first: SessionRally, second: SessionRally) => {
      runEdit(mergeRallyEdit(first, second))
    },
    [runEdit]
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
        keys: ["G"],
        label: "Toggle skipping gaps",
        match: (e) => plain(e) && e.key.toLowerCase() === "g",
        run: toggleGapSkip,
      },
      {
        keys: ["F"],
        label: "Jump to playhead",
        match: (e) => plain(e) && e.key.toLowerCase() === "f",
        run: jumpToPlayhead,
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
        keys: ["Scroll over timeline"],
        label: "Scroll the timeline",
        match: () => false,
        run: () => {},
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
    toggleGapSkip,
    jumpToPlayhead,
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

  // The rally under the playhead (session-global), driving the rail highlight
  // and the inspector; -1 while the playhead sits in a gap or before placement.
  const currentRallyIndex =
    globalPlayheadMs == null
      ? -1
      : session.rallies.findIndex(
          (r) =>
            globalPlayheadMs >= r.globalStart && globalPlayheadMs < r.globalEnd
        )

  // Status-bar readouts: how segmentation stands across the whole session.
  const segmentingNow =
    reanalyzing ||
    session.segments.some((s) => s.timeline?.segment_state === "unknown")
  const failedCount = session.segments.filter(
    (s) => s.timeline?.segment_state === "failed"
  ).length
  const sessionRallyCount = session.rallies.length
  const uncertainCount = session.rallies.filter(
    (r) => r.confidence < UNCERTAIN_CONFIDENCE
  ).length
  const unprocessed = recordings.length - session.placedCount

  // The studio layout (issue #48): a thin top bar over three panes — the rally
  // rail (the session's table of contents), the player column (video, transport,
  // docked timeline), and the inspector for the rally under the playhead.
  return (
    <div
      ref={containerRef}
      tabIndex={-1}
      className="flex h-full min-h-0 flex-col outline-none"
    >
      <header className="flex h-11 shrink-0 items-center gap-3 border-b px-4">
        <Button variant="ghost" size="sm" onClick={onBack}>
          <ArrowLeftIcon className="size-4" />
          Sessions
        </Button>
        <span className="shrink-0 font-medium">
          {day
            ? formatCaptureDay(day)
            : path
              ? fileName(path)
              : "No recordings"}
        </span>
        <span
          className="min-w-0 truncate text-sm text-muted-foreground"
          title={path ?? undefined}
        >
          {day && path ? fileName(path) : null}
        </span>
        <div className="ml-auto shrink-0">
          {/* Export stub: the selection-driven render (CONTEXT.md) isn't built
              yet, but its entry point lives here in the studio design. */}
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button
                variant="outline"
                size="sm"
                title="Render one new video from a selection of rallies."
              >
                <DownloadIcon className="size-4" />
                Export
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              <DropdownMenuLabel>Export — coming soon</DropdownMenuLabel>
              <DropdownMenuItem disabled>
                Condensed session (gaps removed)
              </DropdownMenuItem>
              <DropdownMenuItem disabled>Flagged rallies</DropdownMenuItem>
              <DropdownMenuItem disabled>
                Rallies with mistakes
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      </header>

      <div className="flex min-h-0 flex-1">
        <RallyRail
          session={session}
          recordings={recordings}
          currentRallyIndex={currentRallyIndex}
          onSelectRally={(rally) => seekSession(rally.globalStart)}
        />

        <main className="flex min-w-0 flex-1 flex-col">
          {/* The video pane: an empty hole the native mpv surface composites over. */}
          <div
            ref={paneRef}
            className="mx-4 mt-3 mb-1 min-h-0 flex-1 rounded-lg bg-black"
          />
          {error ? (
            <div className="shrink-0 px-4 pt-2 text-sm text-destructive" role="alert">
              {error}
            </div>
          ) : null}
          <div className="shrink-0 px-4 py-3">
            <TransportBar
              paused={paused}
              muted={muted}
              looping={looping}
              gapSkipEnabled={gapSkipEnabled}
              volume={volume}
              speed={SPEED_LADDER[speedIndex]}
              positionMs={globalPlayheadMs ?? currentMs}
              durationMs={session.totalMs}
              onTogglePlay={togglePlay}
              onFrameStep={frameStep}
              onToggleMute={toggleMute}
              onToggleLoop={toggleLoop}
              onToggleGapSkip={toggleGapSkip}
              onStepSpeed={stepSpeed}
              onResetSpeed={resetSpeed}
            />
          </div>
          <div className="shrink-0 border-t px-4 py-3">
            <SessionTimeline
              ref={timelineRef}
              session={session}
              globalPlayheadMs={globalPlayheadMs}
              pxPerSec={pxPerSec}
              setPxPerSec={setPxPerSec}
              following={following}
              setFollowing={setFollowing}
              canPrev={!atFirstRecording}
              canNext={!atLastRecording}
              editing={editing}
              onSeekGlobal={seekSession}
              onPrevRally={() => goToRally("prev")}
              onNextRally={() => goToRally("next")}
              onNextUncertain={goToUncertain}
              onToggleEditing={() => setEditing((e) => !e)}
              onAdjustRally={adjustRally}
              onAddAtPlayhead={addAtPlayhead}
              onDeleteRally={deleteRally}
              onSplitRally={splitRally}
              onMergeRallies={mergeRallies}
            />
          </div>
        </main>

        <RallyInspector
          rally={
            currentRallyIndex >= 0 ? session.rallies[currentRallyIndex] : null
          }
          rallyNumber={currentRallyIndex + 1}
        />
      </div>

      {/* The status bar: less important info and actions, and a visual footer
          so the timeline doesn't sit on the window edge. */}
      <footer className="flex h-9 shrink-0 items-center gap-3 border-t px-4 text-xs text-muted-foreground">
        {session.totalMs === 0 && segmentingNow ? (
          <span className="flex items-center gap-1.5">
            <Loader2Icon className="size-3.5 animate-spin" />
            Detecting rallies…
          </span>
        ) : sessionRallyCount === 0 && failedCount > 0 ? (
          <span>Couldn&apos;t detect rallies for this session.</span>
        ) : sessionRallyCount === 0 ? (
          <span>No rallies detected.</span>
        ) : (
          <span className="tabular-nums">
            {sessionRallyCount}{" "}
            {sessionRallyCount === 1 ? "rally" : "rallies"} across the session
            {uncertainCount > 0 ? (
              <>
                {" · "}
                <button
                  type="button"
                  onClick={goToUncertain}
                  className="text-amber-600 hover:underline dark:text-amber-500"
                  title="Low-confidence spans the segmenter is unsure about — click to jump to the next one (U)."
                >
                  {uncertainCount} uncertain
                </button>
              </>
            ) : null}
          </span>
        )}
        {unprocessed > 0 ? (
          <span className="flex items-center gap-1.5">
            <Loader2Icon className="size-3.5 animate-spin" />
            {unprocessed} more{" "}
            {unprocessed === 1 ? "recording" : "recordings"} preparing
          </span>
        ) : null}
        {recordings.length > 1 ? (
          <span className="tabular-nums">
            Recording {index + 1} of {recordings.length}
          </span>
        ) : null}
        <div className="ml-auto flex items-center gap-0.5">
          <Button
            variant="ghost"
            size="icon-sm"
            onClick={() => zoomTimeline(1 / SESSION_ZOOM_FACTOR)}
            disabled={pxPerSec <= SESSION_PX_PER_SEC_MIN}
            title="Zoom the timeline out — fit more of the session on screen."
          >
            <ZoomOutIcon className="size-3.5" />
          </Button>
          <Button
            variant="ghost"
            size="icon-sm"
            onClick={() => zoomTimeline(SESSION_ZOOM_FACTOR)}
            disabled={pxPerSec >= SESSION_PX_PER_SEC_MAX}
            title="Zoom the timeline in — see finer detail around the playhead."
          >
            <ZoomInIcon className="size-3.5" />
          </Button>
          <Button
            variant="ghost"
            size="sm"
            className="text-xs"
            onClick={() => setPxPerSec(SESSION_PX_PER_SEC_DEFAULT)}
            disabled={pxPerSec === SESSION_PX_PER_SEC_DEFAULT}
            title="Reset the timeline zoom."
          >
            Reset
          </Button>
          <Button
            variant={following ? "ghost" : "outline"}
            size="sm"
            className="text-xs"
            onClick={jumpToPlayhead}
            disabled={globalPlayheadMs === null}
            title="Scroll the timeline back to the playhead and follow it again (F)."
          >
            <CrosshairIcon className="size-3.5" />
            Playhead
          </Button>
          <Button
            variant="ghost"
            size="sm"
            className="text-xs"
            onClick={handleReanalyze}
            disabled={segmentingNow}
            title="Re-run rally detection for the current recording in place (for tuning the segmenter)."
          >
            <RotateCwIcon
              className={`size-3.5 ${reanalyzing ? "animate-spin" : ""}`}
            />
            Re-analyze
          </Button>
          <Button
            variant="ghost"
            size="icon-sm"
            onClick={() => setShowCheatSheet(true)}
            title="Keyboard shortcuts (?)"
          >
            <KeyboardIcon className="size-3.5" />
          </Button>
        </div>
      </footer>
      {showCheatSheet ? (
        <CheatSheet keymap={keymap} onClose={() => setShowCheatSheet(false)} />
      ) : null}
    </div>
  )
}

/**
 * The left rail of the studio layout (issue #48): the session's table of
 * contents — every rally in play order, grouped by recording, with its
 * duration, long-rally marker, and uncertain-region marker. Clicking a rally
 * seeks the session to its start; the row under the playhead stays highlighted
 * and scrolled into view.
 */
function RallyRail({
  session,
  recordings,
  currentRallyIndex,
  onSelectRally,
}: {
  session: SessionModel
  recordings: PlaylistRecording[]
  currentRallyIndex: number
  onSelectRally: (rally: SessionRally) => void
}) {
  // Keep the playhead's rally visible as playback walks the session.
  const activeRef = useRef<HTMLLIElement>(null)
  useEffect(() => {
    activeRef.current?.scrollIntoView({ block: "nearest" })
  }, [currentRallyIndex])

  // Session-wide rally numbers, matching the strip's and inspector's numbering.
  const numbered = session.rallies.map((rally, number) => ({ rally, number }))

  return (
    <aside className="w-60 shrink-0 overflow-y-auto border-r">
      {recordings.map((rec, recordingIndex) => {
        const seg = session.segments.find((s) => s.index === recordingIndex)
        const state = seg?.timeline?.segment_state
        const rows = numbered.filter(
          ({ rally }) => rally.recordingIndex === recordingIndex
        )
        return (
          <div key={rec.path}>
            <div
              className="sticky top-0 z-10 truncate border-b bg-background px-3 py-1.5 text-xs font-medium text-muted-foreground"
              title={rec.path}
            >
              {fileName(rec.path)}
            </div>
            {rows.length > 0 ? (
              <ul>
                {rows.map(({ rally, number }) => {
                  const active = number === currentRallyIndex
                  const durationMs = rally.localEnd - rally.localStart
                  return (
                    <li
                      key={`${rally.path}:${rally.id}`}
                      ref={active ? activeRef : undefined}
                    >
                      <button
                        type="button"
                        onClick={() => onSelectRally(rally)}
                        className={`flex w-full items-center gap-2 px-3 py-1.5 text-left text-sm ${
                          active ? "bg-accent" : "hover:bg-accent/50"
                        }`}
                        title={`Rally ${number + 1}: ${formatClock(rally.globalStart)}–${formatClock(rally.globalEnd)}`}
                      >
                        <span className="w-9 shrink-0 text-muted-foreground tabular-nums">
                          {number + 1}
                        </span>
                        <span className="font-mono text-xs text-muted-foreground tabular-nums">
                          {formatClock(durationMs)}
                        </span>
                        {durationMs >= LONG_RALLY_MS ? (
                          <TimerIcon
                            className="size-3.5 shrink-0 text-muted-foreground"
                            aria-label="Long rally"
                          />
                        ) : null}
                        {rally.confidence < UNCERTAIN_CONFIDENCE ? (
                          <TriangleAlertIcon
                            className="ml-auto size-3.5 shrink-0 text-amber-500"
                            aria-label="Uncertain region — worth checking"
                          />
                        ) : null}
                      </button>
                    </li>
                  )
                })}
              </ul>
            ) : state === "failed" ? (
              <p className="flex items-center gap-1.5 px-3 py-2 text-sm text-amber-600 dark:text-amber-500">
                <TriangleAlertIcon className="size-3.5 shrink-0" />
                No timeline
              </p>
            ) : state === "ready" ? (
              <p className="px-3 py-2 text-sm text-muted-foreground">
                No rallies detected.
              </p>
            ) : (
              <p className="flex items-center gap-1.5 px-3 py-2 text-sm text-muted-foreground">
                <Loader2Icon className="size-3.5 shrink-0 animate-spin" />
                Detecting rallies…
              </p>
            )}
          </div>
        )
      })}
    </aside>
  )
}

/** Seeded aspect vocabulary (CONTEXT.md), previewed in the inspector stub. */
const STUB_ASPECTS = [
  "selection",
  "execution",
  "deception",
  "footwork",
  "positioning",
]

const VERDICT_DOT = {
  good: "bg-emerald-500",
  bad: "bg-amber-500",
  mistake: "bg-red-500",
} as const

/**
 * The right inspector of the studio layout (issue #48): everything about the
 * rally under the playhead. Its identity, bounds, length class, and uncertainty
 * are real; the capture surfaces — flag, verdict, aspect, note, annotation
 * list — are visual stubs until annotations and flags are implemented.
 */
function RallyInspector({
  rally,
  rallyNumber,
}: {
  rally: SessionRally | null
  rallyNumber: number
}) {
  return (
    <aside className="flex w-72 shrink-0 flex-col overflow-y-auto border-l">
      {rally === null ? (
        <div className="p-4 text-sm text-muted-foreground">
          <p className="font-medium text-foreground">
            No rally at the playhead
          </p>
          <p className="mt-1">
            You&apos;re in a gap, or this recording is still being analyzed.
            Play into a rally to inspect it.
          </p>
        </div>
      ) : (
        <>
          <div className="border-b p-4">
            <div className="flex items-center justify-between">
              <h2 className="font-medium">Rally {rallyNumber}</h2>
              <Button
                variant="outline"
                size="sm"
                disabled
                title="Flags are coming soon — one keystroke to mark a rally for the export reel."
              >
                <FlagIcon className="size-4" />
                Flag
              </Button>
            </div>
            <p className="mt-1 text-sm text-muted-foreground tabular-nums">
              {formatClock(rally.globalStart)}–{formatClock(rally.globalEnd)}
              {" · "}
              {formatClock(rally.globalEnd - rally.globalStart)}
              {" · "}
              {rally.globalEnd - rally.globalStart >= LONG_RALLY_MS
                ? "long"
                : "short"}
            </p>
            {rally.confidence < UNCERTAIN_CONFIDENCE ? (
              <p className="mt-2 flex items-center gap-1.5 rounded-md bg-amber-500/10 px-2 py-1.5 text-xs text-amber-600 dark:text-amber-400">
                <TriangleAlertIcon className="size-3.5 shrink-0" />
                Uncertain boundaries — worth a check
              </p>
            ) : null}
          </div>

          {/* Annotation capture stub: verdict → aspect → note (CONTEXT.md). */}
          <div className="border-b p-4">
            <div className="mb-2 flex items-baseline justify-between">
              <h3 className="text-xs font-medium text-muted-foreground">
                Verdict at playhead
              </h3>
              <span className="text-xs text-muted-foreground/70">
                coming soon
              </span>
            </div>
            <div className="grid grid-cols-3 gap-1.5">
              {(["good", "bad", "mistake"] as const).map((verdict) => (
                <Button
                  key={verdict}
                  variant="outline"
                  size="sm"
                  disabled
                  className="capitalize"
                >
                  <span
                    className={`size-2 rounded-full ${VERDICT_DOT[verdict]}`}
                  />
                  {verdict}
                </Button>
              ))}
            </div>
            <div className="mt-2 flex flex-wrap gap-1">
              {STUB_ASPECTS.map((aspect) => (
                <span
                  key={aspect}
                  className="rounded-full border px-2 py-0.5 text-xs text-muted-foreground/70"
                >
                  {aspect}
                </span>
              ))}
            </div>
            <textarea
              disabled
              placeholder="Note (optional) — shot type goes here"
              rows={2}
              className="mt-2 w-full resize-none rounded-md border bg-transparent px-2 py-1.5 text-sm placeholder:text-muted-foreground/70 disabled:cursor-not-allowed"
            />
          </div>

          <div className="p-4">
            <div className="mb-2 flex items-baseline justify-between">
              <h3 className="text-xs font-medium text-muted-foreground">
                Annotations
              </h3>
              <span className="text-xs text-muted-foreground/70">
                coming soon
              </span>
            </div>
            <p className="text-sm text-muted-foreground">
              Moments you mark during playback will collect here, pinned to
              their timestamps.
            </p>
          </div>
        </>
      )}
    </aside>
  )
}

/**
 * The transport-only control bar beneath the player: play/pause, exact
 * frame-step, a session-global timecode mirroring the session playhead and total
 * duration, a playback-speed indicator, a loop toggle, mute, and a volume
 * readout. It deliberately has **no scrubber**: the session timeline strip below
 * remains the single scrub/seek surface.
 */
function TransportBar({
  paused,
  muted,
  looping,
  gapSkipEnabled,
  volume,
  speed,
  positionMs,
  durationMs,
  onTogglePlay,
  onFrameStep,
  onToggleMute,
  onToggleLoop,
  onToggleGapSkip,
  onStepSpeed,
  onResetSpeed,
}: {
  paused: boolean
  muted: boolean
  looping: boolean
  gapSkipEnabled: boolean
  volume: number
  speed: number
  positionMs: number
  durationMs: number
  onTogglePlay: () => void
  onFrameStep: (forward: boolean) => void
  onToggleMute: () => void
  onToggleLoop: () => void
  onToggleGapSkip: () => void
  onStepSpeed: (dir: -1 | 1) => void
  onResetSpeed: () => void
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
          variant={gapSkipEnabled ? "default" : "outline"}
          size="icon"
          onClick={onToggleGapSkip}
          title={
            gapSkipEnabled
              ? "Skipping gaps between rallies — click to play everything (G)"
              : "Playing through gaps — click to skip to the next rally (G)"
          }
        >
          <FastForwardIcon className="size-4" />
        </Button>
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

/** Imperative surface of the timeline strip the player drives (jump-to-playhead). */
interface SessionTimelineHandle {
  scrollToPlayhead: () => void
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
const SessionTimeline = forwardRef<
  SessionTimelineHandle,
  {
    session: SessionModel
    globalPlayheadMs: number | null
    /**
     * Zoom and playhead-follow are owned by the player (the status bar drives
     * them from outside the strip); the strip's wheel/scroll handlers mutate
     * them through the setters.
     */
    pxPerSec: number
    setPxPerSec: Dispatch<SetStateAction<number>>
    following: boolean
    setFollowing: Dispatch<SetStateAction<boolean>>
    canPrev: boolean
    canNext: boolean
    editing: boolean
    onSeekGlobal: (globalMs: number) => void
    onPrevRally: () => void
    onNextRally: () => void
    onNextUncertain: () => void
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
  }
>(function SessionTimeline(
  {
    session,
    globalPlayheadMs,
    pxPerSec,
    setPxPerSec,
    following,
    setFollowing,
    canPrev,
    canNext,
    editing,
    onSeekGlobal,
    onPrevRally,
    onNextRally,
    onNextUncertain,
    onToggleEditing,
    onAdjustRally,
    onAddAtPlayhead,
    onDeleteRally,
    onSplitRally,
    onMergeRallies,
  },
  ref
) {
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
  // `following`: whether the strip auto-scrolls to keep the playhead in view.
  // Scrolling the strip by hand (wheel or scrollbar) disarms it so you can look
  // ahead; the jump-to-playhead control re-arms it. This ref guards our own
  // programmatic scrollLeft writes so they don't read back as a manual scroll.
  const programmaticScrollRef = useRef(false)

  const totalMs = session.totalMs
  const totalPx = (totalMs / 1000) * pxPerSec
  const rallyKey = (r: SessionRally) => `${r.path}:${r.id}`
  // The strip only renders once a recording has rallies; the wheel/scroll
  // listeners below re-attach when it appears (deps include this).
  const hasRallies = totalMs > 0 && session.rallies.length > 0

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

  // Programmatically scroll the strip to a target offset, arming the guard so
  // the resulting `scroll` event isn't mistaken for a manual scroll. Skips the
  // write when it wouldn't move `scrollLeft` (a no-op assignment fires no event,
  // which would otherwise strand the guard true and eat the next manual scroll —
  // the "can't scroll past the playhead at a recording's start" bug). The clamp
  // lives in `stripScrollTarget` so the no-op case is unit-tested without a DOM.
  const scrollStripTo = useCallback((targetPx: number) => {
    const el = scrollRef.current
    if (!el) return
    const next = stripScrollTarget(
      targetPx,
      el.scrollLeft,
      el.clientWidth,
      el.scrollWidth
    )
    if (next === null) return
    programmaticScrollRef.current = true
    el.scrollLeft = next
  }, [])

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

  // The wheel over the strip: Alt+scroll zooms centered on the cursor; a plain
  // scroll pans the strip horizontally (so a vertical mouse wheel scrubs the
  // timeline without having to grab the scrollbar). Re-attaches when the strip
  // mounts (`hasRallies`), and uses `passive: false` so it can preventDefault.
  useEffect(() => {
    const el = scrollRef.current
    if (!el) return
    const onWheel = (e: WheelEvent) => {
      if (e.altKey) {
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
          scrollStripTo(cursorContentPx * scale - (e.clientX - rect.left))
          return nextPx
        })
        return
      }
      // Pan horizontally. Trackpads send horizontal intent as deltaX; a plain
      // mouse wheel only has deltaY, so fold that in too.
      const delta = e.deltaX !== 0 ? e.deltaX : e.deltaY
      if (delta === 0) return
      e.preventDefault()
      el.scrollLeft += delta
    }
    el.addEventListener("wheel", onWheel, { passive: false })
    return () => el.removeEventListener("wheel", onWheel)
  }, [hasRallies, scrollStripTo, setPxPerSec])

  // A hand scroll (wheel or scrollbar) stops the strip tracking the playhead, so
  // you can look ahead without it snapping back. Our own programmatic writes are
  // flagged so they don't count as a manual scroll.
  useEffect(() => {
    const el = scrollRef.current
    if (!el) return
    const onScroll = () => {
      if (programmaticScrollRef.current) {
        programmaticScrollRef.current = false
        return
      }
      setFollowing(false)
    }
    el.addEventListener("scroll", onScroll, { passive: true })
    return () => el.removeEventListener("scroll", onScroll)
  }, [hasRallies, setFollowing])

  // Recenter the strip on the playhead and re-arm follow — the jump-to-playhead
  // control, also reached by the `F` key through this imperative handle.
  const scrollToPlayhead = useCallback(() => {
    const el = scrollRef.current
    if (!el || globalPlayheadMs == null || totalMs === 0) return
    scrollStripTo((globalPlayheadMs / 1000) * pxPerSec - el.clientWidth / 2)
    setFollowing(true)
  }, [globalPlayheadMs, totalMs, pxPerSec, scrollStripTo, setFollowing])
  useImperativeHandle(ref, () => ({ scrollToPlayhead }), [scrollToPlayhead])

  // While following, keep the playhead in view as playback advances, crosses
  // recordings, or zooms. Once you scroll the strip away it stands down until
  // jump-to-playhead re-arms it.
  useEffect(() => {
    const el = scrollRef.current
    if (!el || !following || globalPlayheadMs == null || totalMs === 0) return
    const x = (globalPlayheadMs / 1000) * pxPerSec
    const margin = el.clientWidth * 0.15
    if (
      x < el.scrollLeft + margin ||
      x > el.scrollLeft + el.clientWidth - margin
    ) {
      scrollStripTo(x - el.clientWidth / 2)
    }
  }, [globalPlayheadMs, totalMs, pxPerSec, following, scrollStripTo])

  const uncertainCount = session.rallies.filter(
    (r) => r.confidence < UNCERTAIN_CONFIDENCE
  ).length

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
      <div className="flex flex-wrap items-center justify-end gap-2">
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
              className="relative h-20 cursor-pointer"
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
        </>
      ) : null}
    </div>
  )
})

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
