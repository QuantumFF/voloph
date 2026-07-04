"use client"

import { useCallback, useEffect, useMemo, useRef, useState } from "react"
import { listen } from "@tauri-apps/api/event"
import {
  ArrowLeftIcon,
  CrosshairIcon,
  DownloadIcon,
  KeyboardIcon,
  Loader2Icon,
  RotateCwIcon,
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
import { fileName } from "@/lib/format"
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
  type EditPlan,
  type PlaylistRecording,
  type Resume,
  type SessionModel,
  type SessionRally,
} from "@/components/recording-player-transport"
import { useMpvSurface } from "@/components/use-mpv-surface"
import { buildKeymap, useGlobalKeymap, type Keybinding } from "./keymap"
import { useSessionTimelines } from "./use-session-timelines"
import { CheatSheet } from "./cheat-sheet"
import { RallyInspector } from "./rally-inspector"
import { RallyRail } from "./rally-rail"
import { SessionTimeline, type SessionTimelineHandle } from "./session-timeline"
import { TransportBar } from "./transport-bar"

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

/** Index of `1×` on the speed ladder — the default and the `Ctrl+0` reset target. */
const DEFAULT_SPEED_INDEX = SPEED_LADDER.indexOf(1)

/**
 * Horizontal scale of the session timeline strip in pixels-per-second, so the
 * whole session is one long, horizontally-scrollable strip. The zoom buttons
 * step it between MIN (a whole long session at a glance) and MAX (frame-level
 * detail), each press scaling by `SESSION_ZOOM_FACTOR`.
 */
const SESSION_PX_PER_SEC_DEFAULT = 3
export const SESSION_PX_PER_SEC_MIN = 1
export const SESSION_PX_PER_SEC_MAX = 240
const SESSION_ZOOM_FACTOR = 1.5

/**
 * How close (ms) the playhead must get to a crossing's resume target before its
 * `time-pos` ticks count as "landed" and normal playback resumes. mpv's resume
 * seek is exact but async, so the first settled tick sits at the target; this
 * slack only clears the near-0 pre-seek lead-in the freshly-loaded file emits
 * before the seek takes hold.
 */
const RESUME_TICK_TOL_MS = 250

/**
 * Rally length threshold (CONTEXT.md: every rally is classified long or short
 * by duration, objectively and automatically). UI-only until length filtering
 * lands; 15s reads as a sustained exchange.
 */
export const LONG_RALLY_MS = 15_000

/** How wide a rally Add-at-playhead creates around the playhead (ms each side). */
const ADD_RALLY_HALF_MS = 2000

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

  // Draft timelines for the whole session, polled until segmented (ADR 0002).
  const { timelines, refreshTimeline, reanalyzing, reanalyze } =
    useSessionTimelines(recordings)
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
    reanalyze(path)
  }, [path, reanalyze])

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

  const toggleCheatSheet = useCallback(() => setShowCheatSheet((s) => !s), [])

  // The keymap array is rebuilt only when an action's closure changes; the
  // window key handler lives in `useGlobalKeymap`.
  const keymap = useMemo<Keybinding[]>(
    () =>
      // eslint-disable-next-line react-hooks/refs -- buildKeymap only stores these callbacks in the keymap array; none of them run during render
      buildKeymap({
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
        toggleCheatSheet,
      }),
    [
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
      toggleCheatSheet,
    ]
  )

  useGlobalKeymap(keymap)

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
