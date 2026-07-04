"use client"

import { useCallback, useEffect, useMemo, useRef, useState } from "react"
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
import { formatCaptureDay } from "@/lib/utils"
import {
  SPEED_LADDER,
  UNCERTAIN_CONFIDENCE,
  buildSessionModel,
  clamp,
  type PlaylistRecording,
  type SessionModel,
} from "@/components/recording-player-transport"
import { useMpvSurface } from "@/components/use-mpv-surface"
import { buildKeymap, useGlobalKeymap, type Keybinding } from "./keymap"
import { useMpvTransport } from "./use-mpv-transport"
import { useSessionPlayback } from "./use-session-playback"
import { useSessionTimelines } from "./use-session-timelines"
import { useTimelineEdits } from "./use-timeline-edits"
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
 * Rally length threshold (CONTEXT.md: every rally is classified long or short
 * by duration, objectively and automatically). UI-only until length filtering
 * lands; 15s reads as a sustained exchange.
 */
export const LONG_RALLY_MS = 15_000

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
  // Focus host so the keymap's window handler is the only thing on keystrokes.
  const containerRef = useRef<HTMLDivElement>(null)
  // Imperative handle on the timeline strip so the `F` key / button can recenter
  // it on the playhead (and re-arm follow) without lifting its scroll state.
  const timelineRef = useRef<SessionTimelineHandle>(null)

  const [editing, setEditing] = useState(false)
  const [showCheatSheet, setShowCheatSheet] = useState(false)
  // Timeline zoom and playhead-follow are owned here rather than by the strip,
  // so the status bar (which lives outside the strip) can drive them; the
  // strip's own wheel/scroll handlers mutate them through the setters.
  const [pxPerSec, setPxPerSec] = useState(SESSION_PX_PER_SEC_DEFAULT)
  const [following, setFollowing] = useState(true)

  // Take focus on mount so the keymap acts immediately, without a click first.
  useEffect(() => {
    containerRef.current?.focus()
  }, [])

  // The native mpv surface's whole lifecycle — rect tracking, show/hide on
  // mount/unmount, and suppression under the cheat-sheet or while minimized
  // (ADR 0008) — lives behind this hook; the returned ref marks the empty pane
  // the surface is slaved to.
  const paneRef = useMpvSurface(showCheatSheet)

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
    toggleLoop,
    toggleGapSkip,
    seekSession,
    goToRally,
    goToUncertain,
    seekRelative,
  } = useSessionPlayback({
    recordings,
    startIndex,
    timelines,
    session,
    segmentOffset,
  })

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

  // Re-run segmentation for the current recording, then re-fetch timelines.
  const handleReanalyze = useCallback(() => {
    if (!path) return
    reanalyze(path)
  }, [path, reanalyze])

  // The five inline corrections (issue #7): boundary math behind the transport
  // seam, persistence + timeline refresh behind this hook.
  const { adjustRally, addAtPlayhead, deleteRally, splitRally, mergeRallies } =
    useTimelineEdits({
      session,
      path,
      index,
      currentMs,
      segmentOffset,
      refreshTimeline,
    })

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
