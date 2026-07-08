"use client"

import { useCallback, useEffect, useMemo, useRef, useState } from "react"
import { ArrowLeftIcon } from "lucide-react"

import { Button } from "@/components/ui/button"
import { fileName } from "@/lib/format"
import { formatCaptureDay } from "@/lib/utils"
import {
  SPEED_LADDER,
  UNCERTAIN_CONFIDENCE,
  buildSessionAnnotations,
  buildSessionModel,
  clamp,
  type PlaylistRecording,
  type SessionModel,
  type Verdict,
} from "@/components/recording-player-transport"
import { useMpvSurface } from "@/components/use-mpv-surface"
import {
  LONG_RALLY_MS,
  SESSION_PX_PER_SEC_DEFAULT,
  SESSION_PX_PER_SEC_MAX,
  SESSION_PX_PER_SEC_MIN,
  VERDICT_DOT,
} from "./constants"
import { useAnnotations } from "./use-annotations"
import { buildKeymap, useGlobalKeymap, type Keybinding } from "./keymap"
import { useMpvTransport } from "./use-mpv-transport"
import { useSessionExport } from "./use-session-export"
import { useSessionPlayback } from "./use-session-playback"
import { useSessionTimelines } from "./use-session-timelines"
import { useTimelineEdits } from "./use-timeline-edits"
import { CheatSheet } from "./cheat-sheet"
import { ExportMenu } from "./export-menu"
import { RallyInspector } from "./rally-inspector"
import { RallyRail } from "./rally-rail"
import { SessionTimeline, type SessionTimelineHandle } from "./session-timeline"
import { StatusBar } from "./status-bar"
import { TransportBar } from "./transport-bar"

export type { PlaylistRecording }
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
   * Recording-local time (ms) to open the first recording at — a jump from the
   * cross-session filter to a specific moment (issue #11). Omitted for a normal
   * session review, which starts from the first rally.
   */
  startMs?: number
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
  startMs,
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
    startMs,
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
  const annotate = useCallback(
    (verdict: Verdict) => {
      if (path) addAnnotation(path, currentMs, verdict)
    },
    [path, currentMs, addAnnotation]
  )

  // The rally under the playhead (session-global), driving the rail highlight,
  // the inspector, and the flag hotkey; -1 while the playhead sits in a gap or
  // before placement.
  const currentRallyIndex =
    globalPlayheadMs == null
      ? -1
      : session.rallies.findIndex(
          (r) =>
            globalPlayheadMs >= r.globalStart && globalPlayheadMs < r.globalEnd
        )
  const currentRally =
    currentRallyIndex >= 0 ? session.rallies[currentRallyIndex] : null

  // Flag / unflag the rally under the playhead (issue #10); a no-op in a gap.
  const flagCurrentRally = useCallback(() => {
    if (currentRally) toggleFlag(currentRally)
  }, [currentRally, toggleFlag])

  const toggleCheatSheet = useCallback(() => setShowCheatSheet((s) => !s), [])

  const {
    progress: exportProgress,
    error: exportError,
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
        toggleLoop,
        toggleGapSkip,
        jumpToPlayhead,
        toggleMute,
        stepSpeed,
        resetSpeed,
        annotate,
        flagCurrentRally,
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
      annotate,
      flagCurrentRally,
      toggleCheatSheet,
    ]
  )

  useGlobalKeymap(keymap)

  // Annotations whose timestamp falls inside the rally under the playhead
  // (glossary: a rally owns the annotations in its span), for the inspector.
  const rallyAnnotations = useMemo(() => {
    if (!currentRally) return []
    return sessionAnnotations.filter(
      (a) =>
        a.globalMs >= currentRally.globalStart &&
        a.globalMs < currentRally.globalEnd
    )
  }, [currentRally, sessionAnnotations])

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
        <div className="ml-auto flex shrink-0 items-center">
          <ExportMenu
            progress={exportProgress}
            error={exportError}
            disabled={exportProgress != null || sessionRallyCount === 0}
            onExportSession={() => void exportSession()}
            onExportCondensed={() => void exportCondensed()}
            onExportFlagged={() => void exportFlagged()}
            onExportMistakes={() => void exportMistakes()}
          />
        </div>
      </header>

      <div className="flex min-h-0 flex-1">
        <RallyRail
          session={session}
          recordings={recordings}
          annotations={sessionAnnotations}
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
            <div
              className="shrink-0 px-4 pt-2 text-sm text-destructive"
              role="alert"
            >
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
              annotations={sessionAnnotations}
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
          rally={currentRally}
          rallyNumber={currentRallyIndex + 1}
          annotations={rallyAnnotations}
          onAnnotate={annotate}
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
        unprocessed={unprocessed}
        recordingCount={recordings.length}
        recordingIndex={index}
        pxPerSec={pxPerSec}
        following={following}
        canJumpToPlayhead={globalPlayheadMs !== null}
        reanalyzing={reanalyzing}
        onGoToUncertain={goToUncertain}
        onZoom={zoomTimeline}
        onResetZoom={() => setPxPerSec(SESSION_PX_PER_SEC_DEFAULT)}
        onJumpToPlayhead={jumpToPlayhead}
        onReanalyze={handleReanalyze}
        onShowCheatSheet={() => setShowCheatSheet(true)}
      />
      {showCheatSheet ? (
        <CheatSheet keymap={keymap} onClose={() => setShowCheatSheet(false)} />
      ) : null}
    </div>
  )
}
