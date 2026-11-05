"use client"

import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type SetStateAction,
} from "react"
import { ArrowLeftIcon, ChevronDownIcon } from "lucide-react"

import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog"
import { Button, buttonVariants } from "@/components/ui/button"
import { fileName, formatClock } from "@/lib/format"
import { formatCaptureDay } from "@/lib/utils"
import {
  SPEED_LADDER,
  UNCERTAIN_CONFIDENCE,
  buildSessionAnnotations,
  buildSessionModel,
  clamp,
  describeHandCorrections,
  describeLanding,
  describeMomentPlace,
  editAffordances,
  extendMarkBurst,
  handCorrections,
  leadInMs,
  markedRallyCount,
  momentLanding,
  momentPlace,
  nextUnmarkedIndex,
  pruneMarkBurst,
  rallyContains,
  rallyFocusAt,
  rallyKey,
  rallyOpenMs,
  rallyStepIndex,
  secondsUntil,
  type MarkBurst,
  type RallyFocus,
  type PlaylistRecording,
  type SessionModel,
  type SessionRally,
  type Verdict,
} from "@/components/recording-player-transport"
import { useMpvSurface } from "@/components/use-mpv-surface"
import {
  LONG_RALLY_MS,
  SESSION_PX_PER_SEC_DEFAULT,
  SESSION_PX_PER_SEC_MAX,
  SESSION_PX_PER_SEC_MIN,
  SESSION_ZOOM_FACTOR,
  VERDICT_DOT,
} from "./constants"
import { useAnnotations } from "./use-annotations"
import { buildKeymap, useGlobalKeymap, type Keybinding } from "./keymap"
import { useMpvTransport } from "./use-mpv-transport"
import { useSessionExport } from "./use-session-export"
import { useSessionPlayback } from "./use-session-playback"
import { usePlaybackPadding } from "./use-playback-padding"
import { useSessionTimelines } from "./use-session-timelines"
import { useTimelineEdits } from "./use-timeline-edits"
import { CheatSheet } from "./cheat-sheet"
import { ExportMenu } from "./export-menu"
import {
  RallyInspector,
  type InspectorEmptyReason,
  type Landing,
} from "./rally-inspector"
import { RallyRail } from "./rally-rail"
import { RecordingMenu } from "./recording-menu"
import { SessionTimeline, type SessionTimelineHandle } from "./session-timeline"
import { StatusBar } from "./status-bar"
import { TransportBar } from "./transport-bar"
import {
  EDIT_ZOOM_CONTEXT_MS,
  TimelineViewContext,
  isPlayheadLost,
  zoomToShow,
  type TimelineView,
} from "./use-timeline-view"

export type { PlaylistRecording }

/** How long dropped verdicts stay confirmed (with their Undo) after the last. */
const MARK_CONFIRM_MS = 5000
/**
 * How long the inspector holds the rally just marked after the last mark,
 * though playback has run on into the next one (issue 2) — unless you navigate.
 */
const MARK_HOLD_MS = 4000
/** How long the rail row a verdict landed in flashes. */
const RAIL_FLASH_MS = 600
export {
  LONG_RALLY_MS,
  SESSION_PX_PER_SEC_MAX,
  SESSION_PX_PER_SEC_MIN,
  VERDICT_DOT,
}

interface RecordingPlayerProps {
  /**
   * The session's recordings, ordered by capture time. Their rallies are
   * flattened into one continuous playlist played back-to-back (the North Star).
   */
  recordings: PlaylistRecording[]
  /** Index of the recording to open first (defaults to the session's start). */
  startIndex?: number
  /**
   * Recording-local strict start (ms) of the rally to open the first recording
   * on — a jump from the cross-session filter to a specific moment (issue #11).
   * Playback opens on its padded lead-in (#104). Omitted for a normal session
   * review, which starts from the first rally.
   */
  rallyStartMs?: number
  /** The session's capture day, shown in the top bar. */
  day?: string
  /** Return to the session list. */
  onBack: () => void
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
  rallyStartMs,
  day,
  onBack,
}: RecordingPlayerProps) {
  // Focus host so the keymap's window handler is the only thing on keystrokes.
  const containerRef = useRef<HTMLDivElement>(null)
  // Imperative handle on the timeline strip so the `F` key / button can recenter
  // it on the playhead (and re-arm follow) without lifting its scroll state.
  const timelineRef = useRef<SessionTimelineHandle>(null)

  const [editing, setEditing] = useState(false)
  const [selectedKey, setSelectedKey] = useState<string | null>(null)
  const [showCheatSheet, setShowCheatSheet] = useState(false)
  // Re-analyze is awaiting confirmation (the recording has hand corrections).
  const [confirmReanalyze, setConfirmReanalyze] = useState(false)
  // The header's recording menu is open: it drops over the video pane.
  const [fileMenuOpen, setFileMenuOpen] = useState(false)
  // Timeline zoom and playhead-follow are owned here rather than by the strip,
  // so the status bar (which lives outside the strip) can drive them; the
  // strip's own wheel/scroll handlers mutate them through the setters.
  // `zoom` is null while the strip is fitted to its width (the default, and
  // what Fit returns to); a number once the user zooms by hand.
  const [zoom, setZoom] = useState<number | null>(null)
  const [following, setFollowing] = useState(true)
  // The strip's width, so the fitted zoom shows the whole session at once.
  const [stripWidth, setStripWidth] = useState(0)
  const stripHostRef = useRef<HTMLDivElement>(null)
  useEffect(() => {
    const el = stripHostRef.current
    if (!el) return
    const observer = new ResizeObserver(([entry]) =>
      setStripWidth(entry.contentRect.width)
    )
    observer.observe(el)
    return () => observer.disconnect()
  }, [])

  // Take focus on mount so the keymap acts immediately, without a click first.
  useEffect(() => {
    containerRef.current?.focus()
  }, [])

  // The native mpv surface's whole lifecycle — rect tracking, show/hide on
  // mount/unmount, and suppression under the cheat-sheet or while minimized
  // (ADR 0008) — lives behind this hook; the returned ref marks the empty pane
  // the surface is slaved to.
  // The header's recording menu drops over the video, so the surface steps
  // aside while it's open, as for a dialog (the native surface would draw over
  // it); the status bar's ⋯ opens upward, clear of it.
  const paneRef = useMpvSurface(
    showCheatSheet || confirmReanalyze || fileMenuOpen
  )
  // Lead-in/tail around each strict rally for playback (#104).
  const padding = usePlaybackPadding()

  // Draft timelines for the whole session, polled until segmented (ADR 0002).
  const { timelines, refreshTimeline, reanalyzing, reanalyze } =
    useSessionTimelines(recordings)

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

  // Fit-to-width zoom: the whole session across the strip. A session too long
  // to fit at the minimum zoom fits anyway — the fitted scale becomes the floor
  // the zoom-out button stops at. A pixel of slack keeps rounding from tipping
  // the strip into a scrollbar.
  const fitPxPerSec =
    session.totalMs > 0 && stripWidth > 0
      ? Math.min(
          SESSION_PX_PER_SEC_MAX,
          Math.max(0.05, (stripWidth - 2) / (session.totalMs / 1000))
        )
      : SESSION_PX_PER_SEC_DEFAULT
  const minPxPerSec = Math.min(SESSION_PX_PER_SEC_MIN, fitPxPerSec)
  const pxPerSec = zoom ?? fitPxPerSec
  // The strip's own wheel zoom sets an absolute scale; the fitted scale is
  // read through a ref so this setter stays stable for its listener.
  const fitRef = useRef(fitPxPerSec)
  useEffect(() => {
    fitRef.current = fitPxPerSec
  }, [fitPxPerSec])
  const setPxPerSec = useCallback((action: SetStateAction<number>) => {
    setZoom((z) =>
      typeof action === "function" ? action(z ?? fitRef.current) : action
    )
  }, [])

  // The playback machinery: which recording is loaded, the playhead, gap-free
  // playback, boundary crossings, and session-global seeking/navigation.
  const {
    index,
    path,
    currentMs,
    globalPlayheadMs,
    error,
    looping,
    gapSkipEnabled,
    atFirstRecording,
    atLastRecording,
    toggleLoop: toggleLoopRaw,
    toggleGapSkip,
    seekSession: seekSessionRaw,
    goToRally: goToRallyRaw,
    goToUncertain: goToUncertainRaw,
    seekRelative: seekRelativeRaw,
  } = useSessionPlayback({
    recordings,
    startIndex,
    // Read once, at mount: the jump opens on the rally's lead-in.
    startMs: rallyStartMs != null ? leadInMs(rallyStartMs, padding) : undefined,
    timelines,
    session,
    segmentOffset,
    padding,
  })

  // After a mark the inspector holds the rally it was made on for a moment
  // (issue 2); any navigation of your own lets go of it at once, so these
  // wrap the playback's seeks for every caller — keys, strip, rail, buttons.
  const [hold, setHold] = useState<{ key: string } | null>(null)
  useEffect(() => {
    if (!hold) return
    const timer = setTimeout(() => setHold(null), MARK_HOLD_MS)
    return () => clearTimeout(timer)
  }, [hold])
  const seekSession = useCallback(
    (globalMs: number) => {
      setHold(null)
      seekSessionRaw(globalMs)
    },
    [seekSessionRaw]
  )
  const goToUncertain = useCallback(() => {
    setHold(null)
    goToUncertainRaw()
  }, [goToUncertainRaw])
  const seekRelative = useCallback(
    (deltaMs: number) => {
      setHold(null)
      seekRelativeRaw(deltaMs)
    },
    [seekRelativeRaw]
  )

  // mpv's transport state (reconciled from its events) and commands.
  const {
    paused,
    muted,
    volume,
    speedIndex,
    togglePlay,
    toggleMute,
    changeVolume,
    stepSpeed,
    resetSpeed,
    frameStep,
  } = useMpvTransport(path)

  // Where a rally-targeted jump opens a rally (issue 1): on its lead-in while
  // playing — what clip padding is for — but on its true start while paused
  // or correcting, so the playhead is in the rally: a verdict pressed right
  // away is that rally's, and Split can act. Rail clicks, [ / ], Next
  // unmarked, the inspector's Jump and Loop all go through here.
  const atStrictStart = paused || editing
  const openRally = useCallback(
    (rally: SessionRally) =>
      seekSession(rallyOpenMs(rally, padding, { atStrictStart })),
    [seekSession, padding, atStrictStart]
  )
  const goToRally = useCallback(
    (direction: "next" | "prev") => {
      setHold(null)
      // Paused or correcting, step rally by rally onto true starts; playing,
      // the playback's own walk of the padded play intervals (which also
      // crosses into a recording not yet placed on the axis).
      const step =
        atStrictStart && globalPlayheadMs != null
          ? rallyStepIndex(session, padding, globalPlayheadMs, direction)
          : -1
      if (step >= 0) openRally(session.rallies[step])
      else goToRallyRaw(direction)
    },
    [atStrictStart, globalPlayheadMs, session, padding, openRally, goToRallyRaw]
  )

  // Recenter the timeline strip on the playhead and re-arm follow (the strip
  // stops tracking the playhead once you scroll it away).
  const jumpToPlayhead = useCallback(
    () => timelineRef.current?.scrollToPlayhead(),
    []
  )

  const zoomTimeline = useCallback(
    (factor: number) => {
      setZoom((z) =>
        clamp((z ?? fitPxPerSec) * factor, minPxPerSec, SESSION_PX_PER_SEC_MAX)
      )
    },
    [fitPxPerSec, minPxPerSec]
  )
  // The - / = keys (issue 8), the same steps as the zoom buttons.
  const stepZoom = useCallback(
    (dir: 1 | -1) =>
      zoomTimeline(dir === 1 ? SESSION_ZOOM_FACTOR : 1 / SESSION_ZOOM_FACTOR),
    [zoomTimeline]
  )
  const fitTimeline = useCallback(() => setZoom(null), [])

  // A zoom set by edit mode (entering zooms to the rally, leaving restores the
  // zoom from before) recenters the strip on the playhead once the strip has
  // re-rendered at the new scale — its handle measures at the current one.
  const centerOnZoomRef = useRef(false)
  useEffect(() => {
    if (!centerOnZoomRef.current) return
    centerOnZoomRef.current = false
    timelineRef.current?.scrollToPlayhead()
  }, [pxPerSec])
  // The zoom to restore on leaving edit mode (null: the strip was fitted).
  const preEditZoomRef = useRef<{ zoom: number | null } | null>(null)
  const zoomForEditing = useCallback(
    (next: number | null) => {
      if ((next ?? fitPxPerSec) === pxPerSec) {
        jumpToPlayhead()
        return
      }
      centerOnZoomRef.current = true
      setZoom(next)
    },
    [fitPxPerSec, pxPerSec, jumpToPlayhead]
  )

  // Where the edit actions render while correcting: a slot in the nav row.
  const [editSlot, setEditSlot] = useState<HTMLElement | null>(null)

  // Where the strip's playhead sits against its viewport, once the strip
  // reports it (undefined until then: `isPlayheadLost` infers it instead).
  const [playheadVisible, setPlayheadVisible] = useState<boolean | undefined>(
    undefined
  )

  // The strip's view controls, hosted in its nav row right above it.
  const timelineView = useMemo<TimelineView>(
    () => ({
      canZoomOut: pxPerSec > minPxPerSec * 1.001,
      canZoomIn: pxPerSec < SESSION_PX_PER_SEC_MAX,
      fitted: zoom === null,
      playheadLost:
        globalPlayheadMs !== null &&
        isPlayheadLost({
          fitted: zoom === null,
          following,
          visible: playheadVisible,
        }),
      canJumpToPlayhead: globalPlayheadMs !== null,
      zoomOut: () => zoomTimeline(1 / SESSION_ZOOM_FACTOR),
      zoomIn: () => zoomTimeline(SESSION_ZOOM_FACTOR),
      fit: fitTimeline,
      jumpToPlayhead,
      editSlot,
      setEditSlot,
    }),
    [
      pxPerSec,
      minPxPerSec,
      zoom,
      following,
      playheadVisible,
      globalPlayheadMs,
      zoomTimeline,
      fitTimeline,
      jumpToPlayhead,
      editSlot,
    ]
  )

  // Re-run segmentation for the current recording, then re-fetch timelines.
  // Re-analysis discards every rally (ADR 0002), so it always asks first, as
  // the session list's does ("Re-analyze…" promises the confirmation), and
  // says what would be lost.
  const reanalyzeLoss = handCorrections(path ? timelines[path] : null)
  const reanalyzeLossText = describeHandCorrections(reanalyzeLoss)
  const runReanalyze = useCallback(() => {
    if (path) reanalyze(path)
  }, [path, reanalyze])
  const handleReanalyze = useCallback(() => {
    if (path) setConfirmReanalyze(true)
  }, [path])

  // The five inline corrections (issue #7): boundary math behind the transport
  // seam, persistence + timeline refresh behind this hook.
  const {
    adjustRally,
    addAtPlayhead,
    deleteRally,
    splitRally,
    mergeRallies,
    toggleFlag,
  } = useTimelineEdits({
    session,
    path,
    index,
    currentMs,
    segmentOffset,
    refreshTimeline,
  })

  // Verdict annotations (issue #8): fetched per recording, dropped at the
  // playhead by a hotkey, and stitched onto the session axis for the strip.
  const {
    annotations,
    add: addAnnotation,
    update: updateAnnotation,
    remove: removeAnnotation,
  } = useAnnotations(recordings)
  const sessionAnnotations = useMemo(
    () => buildSessionAnnotations(session, annotations),
    [session, annotations]
  )

  // The rally the rail, inspector, flag and verdict keys are about: the one
  // playing (inside its strict span or its clip padding — a rail click while
  // playing lands on the lead-in, and that's still the rally you picked), else
  // in a gap the most recent one, pinned until the next rally's lead-in so what
  // you just marked stays in view. Only the playhead moves it: toggling Skip
  // gaps or Loop never does (issue 2).
  const focus: RallyFocus =
    globalPlayheadMs == null
      ? { kind: "none", index: -1, nextIndex: -1 }
      : rallyFocusAt(session, padding, globalPlayheadMs)
  const focusRally = focus.index >= 0 ? session.rallies[focus.index] : null
  // The rally actually playing (strict or padding), or -1 in a gap.
  const liveRallyIndex = focus.kind === "live" ? focus.index : -1
  // In a gap (or before the first rally), the rally that starts next — what
  // the inspector counts down to and the rail marks "up next".
  const nextRally =
    focus.nextIndex >= 0 ? session.rallies[focus.nextIndex] : null
  const jumpToNextRally = useCallback(() => {
    if (nextRally) openRally(nextRally)
  }, [nextRally, openRally])

  // The verdicts just dropped, confirmed together with an Undo for a few
  // seconds (pressing 1, 2, 3 acknowledges all three, not only the last), and
  // the rail row they landed in, flashed in the verdict's colour (issue 2: a
  // mark pressed mid-play must visibly arrive). None of it touches playback.
  const [burst, setBurst] = useState<MarkBurst | null>(null)
  const [flash, setFlash] = useState<{ key: string; verdict: Verdict } | null>(
    null
  )
  useEffect(() => {
    if (!burst) return
    const timer = setTimeout(() => setBurst(null), MARK_CONFIRM_MS)
    return () => clearTimeout(timer)
  }, [burst])
  useEffect(() => {
    if (!flash) return
    const timer = setTimeout(() => setFlash(null), RAIL_FLASH_MS)
    return () => clearTimeout(timer)
  }, [flash])

  // A verdict lands at the exact playhead timestamp (README: "pinned to the
  // exact timestamp") — never moved, even in a gap after a rally. The rally
  // the inspector shows at that moment is then held for a few seconds.
  const annotate = useCallback(
    (verdict: Verdict) => {
      if (!path) return
      const globalMs = segmentOffset(index) + currentMs
      const place = momentPlace(session, padding, globalMs)
      const owner = place.kind === "in" ? session.rallies[place.index] : null
      const shown = rallyFocusAt(session, padding, globalMs).index
      const shownKey = shown >= 0 ? rallyKey(session.rallies[shown]) : null
      void addAnnotation(path, currentMs, verdict).then((id) => {
        if (id == null) return
        setBurst((b) =>
          extendMarkBurst(b, describeMomentPlace(place), {
            path,
            id,
            verdict,
            globalMs,
          })
        )
        setHold((h) => (shownKey ? { key: shownKey } : h))
        if (owner) setFlash({ key: rallyKey(owner), verdict })
      })
    },
    [path, index, currentMs, segmentOffset, session, padding, addAnnotation]
  )
  // Only the marks that still exist are confirmed (one may have been deleted
  // from the list, or undone); Undo deletes the latest, through the same path
  // as the inspector's Delete.
  const shownBurst = pruneMarkBurst(
    burst,
    (m) => annotations[m.path]?.some((a) => a.id === m.id) ?? false
  )
  const undoMark = useCallback(() => {
    const last = shownBurst?.marks[shownBurst.marks.length - 1]
    if (last) removeAnnotation(last.path, last.id)
  }, [shownBurst, removeAnnotation])

  // The rally the inspector shows: the focused one, unless a mark was just
  // made on another that's still held (and still has its marks confirmed).
  const heldIndex =
    hold && shownBurst
      ? session.rallies.findIndex((r) => rallyKey(r) === hold.key)
      : -1
  const holding = heldIndex >= 0 && heldIndex !== focus.index
  const followPlayhead = useCallback(() => setHold(null), [])

  const selectedIndex = session.rallies.findIndex(
    (r) => rallyKey(r) === selectedKey
  )
  const selectedRally =
    selectedIndex >= 0 ? session.rallies[selectedIndex] : null

  // The **target**: the one rally the inspector shows, the rail fills, and the
  // flag key acts on (issue 1) — the rally selected for editing, else the one
  // held just after a mark, else the focused one. The playhead's own rally is
  // shown apart from it (the rail's thin bar), never as a second highlight.
  const editSelected = editing && selectedIndex >= 0
  const targetIndex = editSelected
    ? selectedIndex
    : holding
      ? heldIndex
      : focus.index
  const targetRally = targetIndex >= 0 ? session.rallies[targetIndex] : null
  // The hotkeys edit the selected rally, else the one playing — never one
  // merely pinned in a gap, so X in a gap can't delete the rally before it.
  const {
    target: editTarget,
    mergeTarget,
    canSplit: canSplitEditTarget,
    canMerge: canMergeEditTarget,
  } = editAffordances(
    session.rallies,
    selectedIndex >= 0 ? selectedIndex : liveRallyIndex,
    globalPlayheadMs
  )

  // Entering edit mode selects the rally playing, so the toolbar acts on it
  // straight away, and zooms the strip to it with some context either side
  // (issue 7) — at a whole-session zoom a rally's handles are a few pixels
  // wide; leaving drops the selection so it can't go stale, and restores the
  // zoom from before.
  const liveRally = liveRallyIndex >= 0 ? session.rallies[liveRallyIndex] : null
  const toggleTimelineEditing = useCallback(() => {
    if (!editing && session.rallies.length === 0) return
    if (!editing) {
      preEditZoomRef.current = { zoom }
      const around = liveRally ?? targetRally
      if (around && globalPlayheadMs != null && stripWidth > 0) {
        zoomForEditing(
          zoomToShow({
            startMs: around.globalStart - EDIT_ZOOM_CONTEXT_MS,
            endMs: around.globalEnd + EDIT_ZOOM_CONTEXT_MS,
            centerMs: globalPlayheadMs,
            widthPx: stripWidth,
            minPxPerSec,
            maxPxPerSec: SESSION_PX_PER_SEC_MAX,
          })
        )
      }
    } else if (preEditZoomRef.current) {
      zoomForEditing(preEditZoomRef.current.zoom)
      preEditZoomRef.current = null
    }
    setSelectedKey(editing ? null : liveRally ? rallyKey(liveRally) : null)
    setEditing(!editing)
  }, [
    editing,
    session.rallies.length,
    zoom,
    liveRally,
    targetRally,
    globalPlayheadMs,
    stripWidth,
    minPxPerSec,
    zoomForEditing,
  ])

  // A rail click opens the rally (`openRally`: its lead-in while playing, its
  // true start while paused or correcting). In edit mode it also selects it,
  // as clicking its block on the strip does.
  const selectRallyFromRail = useCallback(
    (rally: SessionRally) => {
      openRally(rally)
      if (editing) setSelectedKey(rallyKey(rally))
    },
    [openRally, editing]
  )

  // Next unmarked (N, issue 4): the next rally after the one shown that holds
  // no verdict yet, wrapping round the session.
  const goToNextUnmarked = useCallback(() => {
    const i = nextUnmarkedIndex(
      session.rallies,
      sessionAnnotations,
      targetIndex
    )
    if (i >= 0) openRally(session.rallies[i])
  }, [session.rallies, sessionAnnotations, targetIndex, openRally])
  const markedCount = useMemo(
    () => markedRallyCount(session.rallies, sessionAnnotations),
    [session.rallies, sessionAnnotations]
  )

  // Loop acts on the rally the inspector shows (issue 2) — or, before the
  // first, the one up next — so what it says is what replays: switched on
  // with the playhead outside that rally, it opens the rally first. Toggling
  // it (or Skip gaps) never changes which rally is shown by itself.
  const toggleLoop = useCallback(() => {
    if (!looping) {
      const i = targetIndex >= 0 ? targetIndex : focus.nextIndex
      const inIt = focus.kind === "live" && focus.index === i
      if (i >= 0 && !inIt) openRally(session.rallies[i])
    }
    toggleLoopRaw()
  }, [
    looping,
    targetIndex,
    focus.nextIndex,
    focus.kind,
    focus.index,
    session.rallies,
    openRally,
    toggleLoopRaw,
  ])

  const splitEditTarget = useCallback(() => {
    if (editTarget && canSplitEditTarget && globalPlayheadMs !== null) {
      splitRally(editTarget, globalPlayheadMs)
    }
  }, [editTarget, canSplitEditTarget, globalPlayheadMs, splitRally])

  const mergeEditTarget = useCallback(() => {
    if (editTarget && canMergeEditTarget && mergeTarget) {
      mergeRallies(editTarget, mergeTarget)
    }
  }, [editTarget, canMergeEditTarget, mergeTarget, mergeRallies])

  const deleteEditTarget = useCallback(() => {
    if (!editTarget) return
    deleteRally(editTarget)
    if (selectedRally) setSelectedKey(null)
  }, [editTarget, selectedRally, deleteRally])

  // Flag / unflag the rally the inspector shows (issue #10) — the target.
  const flagCurrentRally = useCallback(() => {
    if (targetRally) toggleFlag(targetRally)
  }, [targetRally, toggleFlag])

  const toggleCheatSheet = useCallback(() => setShowCheatSheet((s) => !s), [])

  const {
    progress: exportProgress,
    error: exportError,
    counts: exportCounts,
    exportCondensed,
    exportSession,
    exportFlagged,
    exportMistakes,
  } = useSessionExport({
    session,
    sessionAnnotations,
    recordings,
    path,
    day,
  })

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
        goToNextUnmarked,
        toggleLoop,
        toggleGapSkip,
        jumpToPlayhead,
        zoomTimeline: stepZoom,
        fitTimeline,
        toggleMute,
        stepSpeed,
        resetSpeed,
        annotate,
        flagCurrentRally,
        editing,
        toggleEditing: toggleTimelineEditing,
        addAtPlayhead,
        splitEditTarget,
        mergeEditTarget,
        deleteEditTarget,
        toggleCheatSheet,
      }),
    [
      togglePlay,
      seekRelative,
      changeVolume,
      frameStep,
      goToRally,
      goToUncertain,
      goToNextUnmarked,
      toggleLoop,
      toggleGapSkip,
      jumpToPlayhead,
      stepZoom,
      fitTimeline,
      toggleMute,
      stepSpeed,
      resetSpeed,
      annotate,
      flagCurrentRally,
      editing,
      toggleTimelineEditing,
      addAtPlayhead,
      splitEditTarget,
      mergeEditTarget,
      deleteEditTarget,
      toggleCheatSheet,
    ]
  )

  useGlobalKeymap(keymap)

  // Annotations whose timestamp falls inside the rally under the playhead
  // (glossary: a rally owns the annotations in its span), for the inspector.
  const rallyAnnotations = useMemo(() => {
    if (!targetRally) return []
    return sessionAnnotations.filter((a) =>
      rallyContains(targetRally, a.globalMs)
    )
  }, [targetRally, sessionAnnotations])

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

  // Why the inspector has no rally to show: "still analyzing" only when the
  // recording under the playhead really is being analyzed.
  const currentState = path ? timelines[path]?.segment_state : undefined
  const analyzingHere =
    reanalyzing || currentState === undefined || currentState === "unknown"
  const inspectorEmpty: InspectorEmptyReason = analyzingHere
    ? "analyzing"
    : currentState === "failed"
      ? "failed"
      : sessionRallyCount === 0
        ? "no-rallies"
        : "between"
  // While this recording is analyzed its rallies aren't known yet, so pinning
  // the previous recording's last one would mislead.
  const inspectorRally =
    editSelected || holding
      ? targetRally
      : focus.kind === "after" && analyzingHere
        ? null
        : focusRally
  // The rail fills the inspector's rally, exactly — none while it has none.
  const railTarget = inspectorRally ? targetIndex : -1

  // Where a verdict pressed now lands — receiver first, at the exact playhead
  // (never moved), in the wording the inspector's slot, buttons and chips share.
  const playheadPlace =
    globalPlayheadMs != null
      ? momentPlace(session, padding, globalPlayheadMs)
      : null
  const landing: Landing | null =
    playheadPlace && globalPlayheadMs != null
      ? {
          ...momentLanding(playheadPlace),
          clock: formatClock(globalPlayheadMs),
          text: describeLanding(playheadPlace, formatClock(globalPlayheadMs)),
          place: describeMomentPlace(playheadPlace),
        }
      : null

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
        {/* The open recording's name opens its actions (issue 11) — the
            same menu as the status bar's ⋯, where you'd look for them first. */}
        {day && path ? (
          <RecordingMenu
            side="bottom"
            align="start"
            recordingPath={path}
            recordingIndex={index}
            recordingCount={recordings.length}
            canReanalyze={!segmentingNow}
            onReanalyze={handleReanalyze}
            onOpenChange={setFileMenuOpen}
            trigger={
              <Button
                variant="ghost"
                size="sm"
                className="-ml-1.5 min-w-0 gap-1 px-1.5 font-normal text-muted-foreground hover:text-foreground"
                onMouseDown={(e) => e.preventDefault()}
                title={`${path} — recording actions`}
              >
                <span className="min-w-0 truncate">{fileName(path)}</span>
                <ChevronDownIcon className="size-3.5 shrink-0" />
              </Button>
            }
          />
        ) : null}
        <div className="ml-auto flex shrink-0 items-center">
          <ExportMenu
            progress={exportProgress}
            error={exportError}
            disabled={exportProgress != null || sessionRallyCount === 0}
            counts={exportCounts}
            onExportSession={() => void exportSession()}
            onExportCondensed={() => void exportCondensed()}
            onExportFlagged={() => void exportFlagged()}
            onExportMistakes={() => void exportMistakes()}
          />
        </div>
      </header>

      {/* A size container, so the side panes narrow with the window (the rail
          to a number column, the inspector to 15rem) below 75rem — at ~1024px
          wide the video gets the room. */}
      <div className="@container/player flex min-h-0 flex-1">
        <RallyRail
          session={session}
          recordings={recordings}
          annotations={sessionAnnotations}
          targetIndex={railTarget}
          playheadIndex={liveRallyIndex}
          nextIndex={focus.nextIndex}
          editingTarget={editSelected}
          flash={flash}
          onSelectRally={selectRallyFromRail}
        />

        <main className="@container/stage flex min-w-0 flex-1 flex-col">
          {/* The stage: the pane — the empty hole the native mpv surface
              composites over (ADR 0008) — takes all the column's height the
              transport and timeline (docked at the bottom) leave, never less
              than a 16:9 sliver would need, so no band of dead space opens
              under the timeline. Where the column is taller or wider than the
              video's shape, mpv letterboxes inside the black pane. The pane
              *is* the stage — it never moves without resizing, so the size
              observer that slaves the surface to it always fires. */}
          <div
            ref={paneRef}
            className="mx-4 mt-3 min-h-40 flex-1 rounded-lg bg-black"
          />
          {error ? (
            <div
              className="shrink-0 px-4 pt-2 text-sm text-destructive"
              role="alert"
            >
              {error}
            </div>
          ) : null}
          <div className="shrink-0 px-4 py-2">
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
          <div ref={stripHostRef} className="shrink-0 border-t px-4 py-3">
            <TimelineViewContext.Provider value={timelineView}>
              <SessionTimeline
                ref={timelineRef}
                session={session}
                annotations={sessionAnnotations}
                globalPlayheadMs={globalPlayheadMs}
                padding={padding}
                pxPerSec={pxPerSec}
                minPxPerSec={minPxPerSec}
                onPlayheadVisibleChange={setPlayheadVisible}
                setPxPerSec={setPxPerSec}
                following={following}
                setFollowing={setFollowing}
                canPrev={!atFirstRecording}
                canNext={!atLastRecording}
                editing={editing}
                selectedKey={selectedKey}
                setSelectedKey={setSelectedKey}
                onSeekGlobal={seekSession}
                onPrevRally={() => goToRally("prev")}
                onNextRally={() => goToRally("next")}
                onNextUncertain={goToUncertain}
                onToggleEditing={toggleTimelineEditing}
                onAdjustRally={adjustRally}
                onAddAtPlayhead={addAtPlayhead}
                onDeleteRally={deleteRally}
                onSplitRally={splitRally}
                onMergeRallies={mergeRallies}
              />
            </TimelineViewContext.Provider>
          </div>
        </main>

        <RallyInspector
          rally={inspectorRally}
          rallyNumber={targetIndex + 1}
          ended={!editSelected && (holding || focus.kind !== "live")}
          held={!editSelected && holding}
          upNext={
            nextRally && globalPlayheadMs != null
              ? {
                  number: focus.nextIndex + 1,
                  inSeconds: secondsUntil(
                    globalPlayheadMs,
                    nextRally.globalStart
                  ),
                }
              : null
          }
          emptyReason={inspectorEmpty}
          annotations={rallyAnnotations}
          landing={landing}
          burst={shownBurst}
          onAnnotate={annotate}
          onUndoMark={undoMark}
          onJumpNext={jumpToNextRally}
          onFollowPlayhead={followPlayhead}
          onToggleFlag={flagCurrentRally}
          onUpdate={updateAnnotation}
          onDelete={removeAnnotation}
        />
      </div>

      {/* The status bar: less important info and actions, and a visual footer
          so the timeline doesn't sit on the window edge. */}
      <StatusBar
        totalMs={session.totalMs}
        segmentingNow={segmentingNow}
        failedCount={failedCount}
        sessionRallyCount={sessionRallyCount}
        uncertainCount={uncertainCount}
        markedCount={markedCount}
        unprocessed={unprocessed}
        recordingCount={recordings.length}
        recordingIndex={index}
        recordingPath={path}
        editing={editing}
        editingRally={selectedIndex >= 0 ? selectedIndex + 1 : null}
        reanalyzing={reanalyzing}
        canReanalyze={path != null && !segmentingNow}
        gapSkipEnabled={gapSkipEnabled}
        loopingRally={looping && focus.index >= 0 ? focus.index + 1 : null}
        onGoToUncertain={goToUncertain}
        onNextUnmarked={goToNextUnmarked}
        onFinishEditing={toggleTimelineEditing}
        onReanalyze={handleReanalyze}
        onShowCheatSheet={() => setShowCheatSheet(true)}
      />
      {/* Re-analyze over hand corrections: confirmed, never one click. The
          mpv surface is suppressed while this is open (it would draw over
          the dialog, ADR 0008). */}
      <AlertDialog open={confirmReanalyze} onOpenChange={setConfirmReanalyze}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Re-analyze this recording?</AlertDialogTitle>
            <AlertDialogDescription>
              Rallies in {path ? fileName(path) : "this recording"} are detected
              again from scratch.{" "}
              {reanalyzeLossText
                ? `This discards your ${reanalyzeLossText} on it.`
                : "It has no corrections or flags to lose."}{" "}
              Verdicts and notes stay, pinned to their timestamps.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>
              {reanalyzeLossText ? "Keep my corrections" : "Cancel"}
            </AlertDialogCancel>
            <AlertDialogAction
              onClick={runReanalyze}
              className={buttonVariants({ variant: "destructive" })}
            >
              Re-analyze
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
      {showCheatSheet ? (
        <CheatSheet keymap={keymap} onClose={() => setShowCheatSheet(false)} />
      ) : null}
    </div>
  )
}
