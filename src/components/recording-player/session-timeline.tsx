"use client"

import {
  forwardRef,
  memo,
  useCallback,
  useEffect,
  useImperativeHandle,
  useLayoutEffect,
  useRef,
  useState,
  type Dispatch,
  type MouseEvent as ReactMouseEvent,
  type SetStateAction,
} from "react"
import { ChevronLeftIcon, ChevronRightIcon } from "lucide-react"

import { fileName, formatClock, formatTimecode } from "@/lib/format"
import {
  UNCERTAIN_CONFIDENCE,
  clamp,
  editAffordances,
  rallyKey,
  paddedPlayIntervals,
  rallyFocusAt,
  rallyLeadInMs,
  stripScrollTarget,
  type Padding,
  type SessionAnnotation,
  type SessionModel,
  type SessionRally,
} from "@/components/recording-player-transport"
import {
  SESSION_PX_PER_SEC_MAX,
  SESSION_PX_PER_SEC_MIN,
  VERDICT_DOT,
} from "./constants"
import { RallyBlock, type DragState } from "./rally-block"
import { EditToolbar, TimelineNav } from "./timeline-toolbar"
import {
  MARK_HEAD_PX,
  MARK_HIT_PX,
  MARK_PILL_PX,
  VERDICT_ORDER,
  dodgePlayhead,
  groupMarks,
  pillLabel,
  pillWidth,
  type MarkGroup,
} from "./timeline-marks"
import {
  chipSpan,
  edgeChipSpan,
  rulerLabelVisible,
  rulerScale,
  rulerTicks,
  type PxSpan,
} from "./timeline-ruler"

/** How much each Alt+scroll notch over the timeline zooms. */
const ALT_SCROLL_ZOOM_FACTOR = 1.15

/**
 * The share of the strip's width at each side that counts as "near the edge"
 * for follow: a playhead inside it gets recentred.
 */
const FOLLOW_MARGIN = 0.15

/** How close a dragged rally edge must be to magnetically land on the playhead. */
const PLAYHEAD_SNAP_THRESHOLD_MS = 250

/**
 * How far past each side of the visible window ruler ticks are rendered, as a
 * fraction of its width, so a quick scroll doesn't reveal a bare ruler.
 */
const RULER_OVERSCAN = 0.5

/**
 * Heights of the strip's rows (px), top to bottom: the recording lane naming
 * each recording, the time ruler, the marker band (verdict-mark heads and
 * pills, and flags, just above the rallies they belong to), then the track of rally blocks over the
 * waveform. The playhead starts below the lane — its badge sits on the ruler —
 * so it never slices a recording's name. The marker band is sized around a
 * 16px pile pill (MARK_PILL_PX) with a pixel clear above and below. Fixed, so
 * the strip's height doesn't depend on the window: ruler + markers + track
 * come to 120px, with the lane and scrollbar ~146px in all.
 */
const LANE_PX = 18
const RULER_PX = 20
const MARKER_PX = 18
const TRACK_PX = 82

/** Height (px) of a lone verdict mark's head, standing in the marker band. */
const MARK_HEAD_H_PX = 7

/**
 * The rally the strip draws as current: the one playing (strict span or its
 * padded lead-in / tail, #104) — the same rule the rail and inspector use
 * (`rallyFocusAt`), so the three can't disagree. Null in a gap.
 */
function currentRallyKey(
  session: SessionModel,
  globalMs: number | null,
  padding: Padding
): string | null {
  if (globalMs === null) return null
  const focus = rallyFocusAt(session, padding, globalMs)
  return focus.kind === "live" ? rallyKey(session.rallies[focus.index]) : null
}

/** Imperative surface of the timeline strip the player drives (jump-to-playhead). */
export interface SessionTimelineHandle {
  scrollToPlayhead: () => void
}

/**
 * The session timeline strip beneath the player: every recording's draft
 * timeline stitched onto one continuous, horizontally-scrollable axis at a fixed
 * pixels-per-second scale. Each recording's audio waveform fills its span with
 * detected rallies drawn as blocks over it, gaps the empty space between them
 * (ADR 0001), low-confidence rallies styled as uncertain regions (ADR 0002), and
 * recordings marked off by solid dividers, alternating shading and a lane
 * above the ruler naming each one. A time ruler labels the session clock, with
 * gridlines down through the strip at each label. The playhead (a neutral
 * foreground line, with a timecode badge on the ruler) tracks the session
 * position and auto-scrolls into view; when you've scrolled it out of view an
 * edge chip points the way back. Each play interval — rallies padded by the
 * lead-in and tail (#104), merged where they touch — is an outlined box with
 * the stored rallies inset inside; clicking a rally jumps to its padded
 * lead-in, and clicking a box's padding to the box's start, like `[`/`]`;
 * clicking the ruler seeks to that exact point, inside a rally or not (a hover cursor
 * shows the time it would land on); clicking a gap seeks there. Seeks cross
 * recordings as needed.
 *
 * In correction mode (issue #7) each edit is resolved against the recording that
 * owns the rally: drag an edge to adjust, split at the playhead, merge with the
 * next rally in the same recording, add around the playhead, or delete.
 */
export const SessionTimeline = forwardRef<
  SessionTimelineHandle,
  {
    session: SessionModel
    /** Verdict annotations lifted onto the session axis, drawn as markers. */
    annotations: SessionAnnotation[]
    globalPlayheadMs: number | null
    /** Playback padding (#104), drawn around each rally and used by its click. */
    padding: Padding
    /**
     * Zoom and playhead-follow are owned by the player (the status bar drives
     * them from outside the strip); the strip's wheel/scroll handlers mutate
     * them through the setters.
     */
    pxPerSec: number
    /** Lowest zoom Alt+scroll may reach: the fitted scale when that's below
     * SESSION_PX_PER_SEC_MIN, so zooming out never jumps back in. */
    minPxPerSec?: number
    setPxPerSec: Dispatch<SetStateAction<number>>
    following: boolean
    setFollowing: Dispatch<SetStateAction<boolean>>
    canPrev: boolean
    canNext: boolean
    editing: boolean
    selectedKey: string | null
    setSelectedKey: Dispatch<SetStateAction<string | null>>
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
    /**
     * Told whether the playhead lies inside the strip's visible scroll window,
     * fired only when that changes (the strip starts out reporting it visible,
     * so the first call is always `false`). Unmeasured, rally-less or
     * playhead-less strips count as visible — only a playhead actually
     * scrolled out of view is lost. Optional.
     */
    onPlayheadVisibleChange?: (visible: boolean) => void
  }
>(function SessionTimeline(
  {
    session,
    annotations,
    globalPlayheadMs,
    padding,
    pxPerSec,
    minPxPerSec = SESSION_PX_PER_SEC_MIN,
    setPxPerSec,
    following,
    setFollowing,
    canPrev,
    canNext,
    editing,
    selectedKey,
    setSelectedKey,
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
    onPlayheadVisibleChange,
  },
  ref
) {
  const [drag, setDrag] = useState<DragState | null>(null)
  const scrollRef = useRef<HTMLDivElement>(null)
  const contentRef = useRef<HTMLDivElement>(null)
  // `following`: whether the strip auto-scrolls to keep the playhead in view.
  // Scrolling the strip by hand (wheel or scrollbar) disarms it so you can look
  // ahead; the jump-to-playhead control re-arms it. This ref guards our own
  // programmatic scrollLeft writes so they don't read back as a manual scroll.
  const programmaticScrollRef = useRef(false)
  // The strip's visible window (content px), so the ruler renders only the
  // ticks in view and the playhead chip knows when the playhead is off-screen.
  const [viewport, setViewport] = useState({ left: 0, width: 0 })
  // The hover cursor is moved imperatively — re-rendering the strip (and its
  // waveform) on every pointer move would be wasted work.
  const hoverLineRef = useRef<HTMLDivElement>(null)
  const hoverLabelRef = useRef<HTMLSpanElement>(null)
  // An edge drag released off its rally still fires a click on the strip;
  // swallow that one so finishing a drag doesn't also move the playhead.
  const suppressClickRef = useRef(false)

  const totalMs = session.totalMs
  const totalPx = (totalMs / 1000) * pxPerSec
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

  const snapToPlayhead = useCallback(
    (
      globalMs: number,
      minGlobalMs: number,
      maxGlobalMs: number
    ): { globalMs: number; snappedToPlayhead: boolean } => {
      if (
        globalPlayheadMs === null ||
        globalPlayheadMs < minGlobalMs ||
        globalPlayheadMs > maxGlobalMs ||
        Math.abs(globalMs - globalPlayheadMs) > PLAYHEAD_SNAP_THRESHOLD_MS
      ) {
        return { globalMs, snappedToPlayhead: false }
      }
      return { globalMs: globalPlayheadMs, snappedToPlayhead: true }
    },
    [globalPlayheadMs]
  )

  // While dragging a rally edge, follow the pointer and persist on release.
  useEffect(() => {
    if (!drag) return
    const move = (e: PointerEvent) =>
      setDrag((d) => {
        if (!d) return d
        const globalMs = clamp(xToMs(e.clientX), d.minGlobalMs, d.maxGlobalMs)
        return {
          ...d,
          ...snapToPlayhead(globalMs, d.minGlobalMs, d.maxGlobalMs),
        }
      })
    const up = () => {
      suppressClickRef.current = true
      setTimeout(() => {
        suppressClickRef.current = false
      }, 0)
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
  }, [drag, session, xToMs, snapToPlayhead, onAdjustRally])

  // Cursor-anchored zoom. A wheel notch can't scroll the strip itself: the
  // strip hasn't re-rendered at the new width yet, so the browser would clamp
  // the scroll to the old width (zooming in near the end), and a fast burst of
  // notches would each read a `scrollLeft` that belongs to a stale zoom — the
  // view jumping away from the cursor. Instead a notch records *which moment*
  // sits under the cursor and the zoom it asks for; the layout effect below
  // puts that moment back under the cursor once the strip has the new width.
  // `renderedPxRef` is the zoom the DOM currently shows (so `scrollLeft` reads
  // against it), `targetPxRef` the latest zoom asked for (so notches compound).
  const renderedPxRef = useRef(pxPerSec)
  const targetPxRef = useRef(pxPerSec)
  const minPxPerSecRef = useRef(minPxPerSec)
  useEffect(() => {
    minPxPerSecRef.current = minPxPerSec
  }, [minPxPerSec])
  const zoomAnchorRef = useRef<{ ms: number; viewportX: number } | null>(null)
  const playheadRef = useRef(globalPlayheadMs)
  useLayoutEffect(() => {
    playheadRef.current = globalPlayheadMs
  }, [globalPlayheadMs])
  useLayoutEffect(() => {
    renderedPxRef.current = pxPerSec
    targetPxRef.current = pxPerSec
    const anchor = zoomAnchorRef.current
    zoomAnchorRef.current = null
    if (anchor) {
      scrollStripTo((anchor.ms / 1000) * pxPerSec - anchor.viewportX)
    }
  }, [pxPerSec, scrollStripTo])

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
        if (e.deltaY === 0) return
        const factor =
          e.deltaY < 0 ? ALT_SCROLL_ZOOM_FACTOR : 1 / ALT_SCROLL_ZOOM_FACTOR
        const nextPx = clamp(
          targetPxRef.current * factor,
          minPxPerSecRef.current,
          SESSION_PX_PER_SEC_MAX
        )
        if (nextPx === targetPxRef.current) return
        const viewportX = e.clientX - el.getBoundingClientRect().left
        // The moment under the cursor, read against what's on screen.
        const ms = ((el.scrollLeft + viewportX) / renderedPxRef.current) * 1000
        zoomAnchorRef.current = { ms, viewportX }
        targetPxRef.current = nextPx
        // Zooming around the cursor is looking somewhere on purpose: if it
        // pushes the playhead out of the followed band, stop following rather
        // than let follow yank the view back to the playhead.
        const playhead = playheadRef.current
        if (playhead != null) {
          const x = ((playhead - ms) / 1000) * nextPx + viewportX
          const margin = el.clientWidth * FOLLOW_MARGIN
          if (x < margin || x > el.clientWidth - margin) setFollowing(false)
        }
        setPxPerSec(nextPx)
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
  }, [hasRallies, setPxPerSec, setFollowing])

  // A hand scroll (wheel or scrollbar) stops the strip tracking the playhead, so
  // you can look ahead without it snapping back. Our own programmatic writes are
  // flagged so they don't count as a manual scroll.
  useEffect(() => {
    const el = scrollRef.current
    if (!el) return
    const onScroll = () => {
      setViewport({ left: el.scrollLeft, width: el.clientWidth })
      if (programmaticScrollRef.current) {
        programmaticScrollRef.current = false
        return
      }
      setFollowing(false)
    }
    el.addEventListener("scroll", onScroll, { passive: true })
    return () => el.removeEventListener("scroll", onScroll)
  }, [hasRallies, setFollowing])

  // Track the strip's width (and initial offset) so the visible window stays
  // right through window resizes.
  useEffect(() => {
    const el = scrollRef.current
    if (!el) return
    const measure = () =>
      setViewport({ left: el.scrollLeft, width: el.clientWidth })
    measure()
    const observer = new ResizeObserver(measure)
    observer.observe(el)
    return () => observer.disconnect()
  }, [hasRallies])

  const showHover = useCallback(
    (clientX: number) => {
      const line = hoverLineRef.current
      const label = hoverLabelRef.current
      if (!line || !label) return
      const ms = xToMs(clientX)
      line.style.left = `${(ms / 1000) * pxPerSec}px`
      label.textContent = formatTimecode(ms)
      line.hidden = false
    },
    [xToMs, pxPerSec]
  )
  const hideHover = useCallback(() => {
    if (hoverLineRef.current) hoverLineRef.current.hidden = true
  }, [])

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
    const margin = el.clientWidth * FOLLOW_MARGIN
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
  // The toolbar edits only the selected rally (the hotkeys also fall back to
  // the one under the playhead — same rule, see `editAffordances`).
  const {
    target: selected,
    mergeTarget,
    canSplit,
    canMerge,
  } = editAffordances(session.rallies, selectedIndex, globalPlayheadMs)

  const currentKey = currentRallyKey(session, globalPlayheadMs, padding)

  const playheadPx =
    globalPlayheadMs !== null ? (globalPlayheadMs / 1000) * pxPerSec : null
  const snappedToPlayhead = drag?.snappedToPlayhead ?? false

  // The play-interval boxes, from each rally's live bounds so a dragged edge
  // reshapes (and re-merges) its box as it moves.
  const playBoxes = paddedPlayIntervals(
    session.rallies.map((r) => {
      const seg = session.segments.find((s) => s.index === r.recordingIndex)
      const recordingStart = seg?.offsetMs ?? 0
      const live =
        drag?.key === rallyKey(r)
          ? [drag.globalMs, drag.anchorGlobalMs]
          : [r.globalStart, r.globalEnd]
      return {
        recordingIndex: r.recordingIndex,
        globalStart: Math.min(live[0], live[1]),
        globalEnd: Math.max(live[0], live[1]),
        recordingStart,
        recordingEnd: recordingStart + (seg?.durationMs ?? 0),
      }
    }),
    padding
  )

  // Ticks only for the visible window (plus overscan): a long session at deep
  // zoom would otherwise be tens of thousands of nodes.
  const scale = rulerScale(pxPerSec)
  const overscanPx = viewport.width * RULER_OVERSCAN
  const ticks = rulerTicks(
    scale,
    ((viewport.left - overscanPx) / pxPerSec) * 1000,
    ((viewport.left + viewport.width + overscanPx) / pxPerSec) * 1000,
    totalMs
  )
  const majorTicks = ticks.filter((t) => t.major)
  const chipLabel =
    globalPlayheadMs !== null ? formatTimecode(globalPlayheadMs) : null

  // Which side the playhead has gone off to, if you've scrolled it out of view.
  const playheadOffside =
    playheadPx === null || viewport.width === 0
      ? null
      : playheadPx < viewport.left
        ? "left"
        : playheadPx > viewport.left + viewport.width
          ? "right"
          : null

  // Report the playhead leaving / re-entering the visible window to anyone
  // listening (the player's jump-to-playhead control), only on a change.
  const playheadVisible = !hasRallies || playheadOffside === null
  const reportedVisibleRef = useRef(true)
  const onPlayheadVisibleChangeRef = useRef(onPlayheadVisibleChange)
  useLayoutEffect(() => {
    onPlayheadVisibleChangeRef.current = onPlayheadVisibleChange
  }, [onPlayheadVisibleChange])
  useEffect(() => {
    if (reportedVisibleRef.current === playheadVisible) return
    reportedVisibleRef.current = playheadVisible
    onPlayheadVisibleChangeRef.current?.(playheadVisible)
  }, [playheadVisible])

  // A ruler label is drawn only whole: not straddling the strip's edge
  // (":40"), and not under the playhead badge or the off-screen chip ("0:0") —
  // re-checked every render, so a hidden label returns once the badge moves on.
  const rulerCovered: PxSpan[] =
    playheadPx === null || chipLabel === null
      ? []
      : playheadOffside === null
        ? [chipSpan(playheadPx, chipLabel)]
        : [edgeChipSpan(playheadOffside, viewport, chipLabel)]
  const labelShown = (ms: number, label: string) =>
    rulerLabelVisible((ms / 1000) * pxPerSec, label, viewport, rulerCovered)

  // Verdict marks, merged where they'd pile up at this zoom.
  const markGroups = groupMarks(annotations, pxPerSec)

  return (
    <div className="shrink-0 space-y-2">
      <TimelineNav
        hasRallies={hasRallies}
        canPrev={canPrev}
        canNext={canNext}
        uncertainCount={uncertainCount}
        editing={editing}
        onPrevRally={onPrevRally}
        onNextRally={onNextRally}
        onNextUncertain={onNextUncertain}
        onToggleEditing={onToggleEditing}
      />
      {editing ? (
        <EditToolbar
          selected={selected}
          selectedIndex={selectedIndex}
          canSplit={canSplit}
          canMerge={canMerge}
          onAddAtPlayhead={onAddAtPlayhead}
          onSplit={() =>
            selected &&
            globalPlayheadMs !== null &&
            onSplitRally(selected, globalPlayheadMs)
          }
          onMerge={() =>
            selected && mergeTarget && onMergeRallies(selected, mergeTarget)
          }
          onDelete={() => {
            if (selected) {
              onDeleteRally(selected)
              setSelectedKey(null)
            }
          }}
        />
      ) : null}
      {hasRallies ? (
        <div className="relative">
          <div
            ref={scrollRef}
            className="strip-scrollbar w-full overflow-x-scroll rounded-md bg-strip-track"
          >
            <div
              ref={contentRef}
              className="relative cursor-pointer select-none"
              style={{ width: `${Math.max(totalPx, 1)}px` }}
              onClickCapture={(e) => {
                if (suppressClickRef.current) e.stopPropagation()
              }}
              onClick={(e) => {
                if (editing) {
                  setSelectedKey(null)
                  return
                }
                onSeekGlobal(xToMs(e.clientX))
              }}
            >
              {/* The recording lane: each recording's number and file name,
                  pinned to the left edge while you scroll through it, above
                  the ruler so it never covers a rally. */}
              <div
                className="relative border-b border-foreground/10"
                style={{ height: LANE_PX }}
              >
                {session.segments.map((seg) => {
                  if (seg.durationMs == null || !seg.timeline) return null
                  return (
                    <div
                      key={seg.path}
                      className={`absolute inset-y-0 ${
                        seg.index % 2 === 1 ? "bg-foreground/[0.07]" : ""
                      }`}
                      style={{
                        left: `${(seg.offsetMs / 1000) * pxPerSec}px`,
                        width: `${(seg.durationMs / 1000) * pxPerSec}px`,
                      }}
                    >
                      <span
                        className="sticky left-0 inline-block max-w-full truncate px-2 text-[11px] leading-[18px] text-muted-foreground"
                        title={seg.path}
                      >
                        <span className="font-semibold text-foreground/80 tabular-nums">
                          {seg.index + 1}
                        </span>{" "}
                        · {fileName(seg.path)}
                      </span>
                    </div>
                  )
                })}
              </div>

              {/* The time ruler: labelled major ticks, short minor ones. The
                  one place a click seeks to an exact point inside a rally — in
                  edit mode too, keeping the selection, so you can place the
                  playhead for a split. */}
              <div
                className="relative cursor-col-resize overflow-hidden border-b border-foreground/15 bg-background/40 hover:bg-background/70"
                style={{ height: RULER_PX }}
                onPointerMove={(e) => showHover(e.clientX)}
                onPointerLeave={hideHover}
                onClick={(e) => {
                  e.stopPropagation()
                  onSeekGlobal(xToMs(e.clientX))
                }}
              >
                {ticks.map((t) => (
                  <div
                    key={t.ms}
                    className={`pointer-events-none absolute bottom-0 w-px ${
                      t.major
                        ? "h-2.5 bg-foreground/50"
                        : "h-1 bg-foreground/25"
                    }`}
                    style={{ left: `${(t.ms / 1000) * pxPerSec}px` }}
                  >
                    {t.major && labelShown(t.ms, formatTimecode(t.ms)) ? (
                      <span className="absolute bottom-0.5 left-1 text-[11px] leading-none whitespace-nowrap text-muted-foreground tabular-nums">
                        {formatTimecode(t.ms)}
                      </span>
                    ) : null}
                  </div>
                ))}
              </div>

              {/* The marker band over the track: shading, waveform and
                  gridlines run through both; verdict marks stand in the band
                  and drop a tick through their rally, flags are planted on
                  their block's top edge into it. */}
              <div
                className="relative"
                style={{ height: MARKER_PX + TRACK_PX }}
              >
                {session.segments.map((seg) => {
                  if (seg.durationMs == null || !seg.timeline) return null
                  const left = (seg.offsetMs / 1000) * pxPerSec
                  const width = (seg.durationMs / 1000) * pxPerSec
                  return (
                    <div
                      key={seg.path}
                      className={`pointer-events-none absolute inset-y-0 ${
                        seg.index % 2 === 1 ? "bg-foreground/[0.07]" : ""
                      }`}
                      style={{ left: `${left}px`, width: `${width}px` }}
                    >
                      <div
                        className="absolute inset-x-0 bottom-0"
                        style={{ top: MARKER_PX }}
                      >
                        <Waveform peaks={seg.timeline.waveform} />
                      </div>
                    </div>
                  )
                })}
                {/* Gridlines down from each labelled tick, behind the rallies. */}
                {majorTicks.map((t) => (
                  <div
                    key={t.ms}
                    className="pointer-events-none absolute inset-y-0 w-px bg-foreground/10"
                    style={{ left: `${(t.ms / 1000) * pxPerSec}px` }}
                  />
                ))}
                {/* Each play interval as a box: the stored rallies padded by
                    the lead-in and tail and merged where they touch — exactly
                    what playback and [ / ] step through. Clicking a box's
                    padding opens it where playback would. */}
                <div
                  className="absolute inset-x-0 bottom-0"
                  style={{ height: TRACK_PX }}
                >
                  {playBoxes.map((box, i) => (
                    <div
                      key={i}
                      aria-hidden
                      onClick={(e) => {
                        if (editing) return
                        e.stopPropagation()
                        onSeekGlobal(box.globalStart)
                      }}
                      className="absolute inset-y-0 cursor-pointer rounded-sm border border-dashed border-strip-pad-edge bg-strip-pad hover:bg-strip-rally"
                      style={{
                        left: `${(box.globalStart / 1000) * pxPerSec}px`,
                        width: `${((box.globalEnd - box.globalStart) / 1000) * pxPerSec}px`,
                      }}
                    />
                  ))}
                  {session.rallies.map((rally, i) => {
                    const key = rallyKey(rally)
                    const seg = session.segments.find(
                      (s) => s.index === rally.recordingIndex
                    )
                    const minGlobalMs = seg?.offsetMs ?? 0
                    const maxGlobalMs = minGlobalMs + (seg?.durationMs ?? 0)
                    return (
                      <RallyBlock
                        key={key}
                        rally={rally}
                        number={i}
                        pxPerSec={pxPerSec}
                        editing={editing}
                        current={key === currentKey}
                        selected={editing && key === selectedKey}
                        drag={drag?.key === key ? drag : null}
                        minGlobalMs={minGlobalMs}
                        maxGlobalMs={maxGlobalMs}
                        onSelect={() => setSelectedKey(key)}
                        onSeek={() =>
                          onSeekGlobal(rallyLeadInMs(rally, padding))
                        }
                        onStartDrag={(edge, anchorGlobalMs, minMs, maxMs) =>
                          setDrag({
                            key,
                            edge,
                            anchorGlobalMs,
                            globalMs:
                              edge === "start"
                                ? rally.globalStart
                                : rally.globalEnd,
                            snappedToPlayhead: false,
                            minGlobalMs: minMs,
                            maxGlobalMs: maxMs,
                          })
                        }
                      />
                    )
                  })}
                </div>
                {markGroups.map((group) => (
                  <VerdictMarkGroup
                    key={group.marks[0].id}
                    group={group}
                    pillCentre={
                      group.marks.length === 1
                        ? group.centre
                        : dodgePlayhead(
                            group.centre,
                            pillWidth(group.marks.length),
                            playheadPx
                          )
                    }
                    onSeek={onSeekGlobal}
                  />
                ))}
              </div>

              {/* Recording boundaries: a solid divider through lane, ruler
                  and strip, with a notch at the top. */}
              {session.segments.map((seg) => {
                if (seg.index === 0 || seg.durationMs == null || !seg.timeline)
                  return null
                return (
                  <div
                    key={seg.path}
                    className="pointer-events-none absolute inset-y-0 z-[5]"
                    style={{ left: `${(seg.offsetMs / 1000) * pxPerSec}px` }}
                  >
                    <div className="absolute inset-y-0 left-0 w-0.5 -translate-x-1/2 bg-foreground/70" />
                    <div className="absolute top-0 left-0 size-0 -translate-x-1/2 border-x-[5px] border-t-[6px] border-x-transparent border-t-foreground/70" />
                  </div>
                )
              })}

              {/* Hover cursor: where a click would land, and when. */}
              <div
                ref={hoverLineRef}
                hidden
                className="pointer-events-none absolute bottom-0 z-10 w-0 border-l border-dashed border-foreground/60"
                style={{ top: LANE_PX }}
              >
                <span
                  ref={hoverLabelRef}
                  className="absolute top-px left-1 rounded-sm border border-foreground/30 bg-background px-1 text-[11px] leading-4 whitespace-nowrap text-foreground tabular-nums shadow-sm"
                />
              </div>

              {playheadPx !== null && globalPlayheadMs !== null ? (
                <div
                  className="pointer-events-none absolute bottom-0 z-10"
                  style={{ left: `${playheadPx}px`, top: LANE_PX }}
                  title={
                    snappedToPlayhead
                      ? "Dragged edge snapped to playhead"
                      : undefined
                  }
                >
                  <div
                    className={`absolute inset-y-0 -translate-x-1/2 bg-foreground shadow-[0_0_0_1px_var(--background)] transition-[width,box-shadow] ${
                      snappedToPlayhead
                        ? "w-1 ring-4 ring-foreground/30"
                        : "w-0.5"
                    }`}
                  />
                  <span className="absolute top-0 -translate-x-1/2 rounded-sm bg-foreground px-1.5 text-[11px] leading-5 font-semibold whitespace-nowrap text-background tabular-nums shadow-[0_0_0_1px_var(--background)]">
                    {chipLabel}
                  </span>
                </div>
              ) : null}
            </div>
          </div>

          {/* The playhead is scrolled out of view: point the way back. */}
          {playheadOffside !== null && globalPlayheadMs !== null ? (
            <button
              type="button"
              onClick={scrollToPlayhead}
              style={{ top: LANE_PX }}
              className={`absolute z-20 flex h-5 items-center gap-0.5 rounded-full bg-foreground text-[11px] font-semibold text-background tabular-nums shadow-md hover:bg-foreground/85 ${
                playheadOffside === "left"
                  ? "left-1 pr-2 pl-1"
                  : "right-1 pr-1 pl-2"
              }`}
              title="The playhead is off-screen — jump back to it (F)."
            >
              {playheadOffside === "left" ? (
                <ChevronLeftIcon className="size-3" />
              ) : null}
              {formatTimecode(globalPlayheadMs)}
              {playheadOffside === "right" ? (
                <ChevronRightIcon className="size-3" />
              ) : null}
            </button>
          ) : null}
        </div>
      ) : null}
    </div>
  )
})

const VERDICT_LABEL = { good: "Good", bad: "Bad", mistake: "Mistake" } as const

/**
 * The verdict stripes of a merged pile, in verdict order, each as long as its
 * share of the pile — so a pile reads "mostly mistakes" at a glance.
 */
function VerdictStripes({
  counts,
  direction,
  className = "inset-0",
}: {
  counts: MarkGroup<SessionAnnotation>["counts"]
  direction: "row" | "col"
  /** Where the stripes sit in their (positioned) parent. */
  className?: string
}) {
  return (
    <span
      className={`absolute flex overflow-hidden ${className} ${
        direction === "row" ? "flex-row" : "flex-col"
      }`}
    >
      {VERDICT_ORDER.filter((v) => counts[v] > 0).map((v) => (
        <span
          key={v}
          className={VERDICT_DOT[v]}
          style={{ flexGrow: counts[v], flexBasis: 0 }}
        />
      ))}
    </span>
  )
}

/**
 * One verdict mark — or a pile of marks too close to tell apart at this zoom —
 * on the strip. A lone mark is a 2px verdict-coloured tick down through its
 * rally with a small head standing in the marker band; the head's hit area is
 * wider than it (MARK_HIT_PX) and the full band tall, and it's drawn over the
 * playhead line, so a mark just made at the playhead stays visible and
 * grabbable. A pile is an outlined pill in the band — track-coloured fill, a
 * "×N" count and its verdict stripes along its foot, each as long as its
 * share — over one tick striped the same way: never a solid block hiding its
 * rally, and a different object from the playhead's filled badge and the
 * rallies' plain numbers. A pill the playhead line would slice moves just
 * aside (`pillCentre`, see `dodgePlayhead`); its tick stays put. Zooming in
 * splits a pile into its lone ticks. Both outline in the strip's track colour
 * so they hold up over any rally fill in either theme. The head / pill is the
 * click target: it seeks to the mark (a pile's first); its tooltip lists every
 * mark in it.
 */
const VerdictMarkGroup = memo(function VerdictMarkGroup({
  group,
  pillCentre,
  onSeek,
}: {
  group: MarkGroup<SessionAnnotation>
  /** Where the head / pill is centred: the group's own centre, or a pile's dodged one. */
  pillCentre: number
  onSeek: (globalMs: number) => void
}) {
  const n = group.marks.length
  const lone = n === 1
  const list = group.marks
    .map((m) => `${VERDICT_LABEL[m.verdict]} · ${formatClock(m.globalMs)}`)
    .join("\n")
  const summary = lone
    ? `${VERDICT_LABEL[group.marks[0].verdict]} at ${formatClock(group.marks[0].globalMs)}`
    : `${n} verdicts, ${formatClock(group.marks[0].globalMs)}–${formatClock(
        group.marks[n - 1].globalMs
      )}`
  const title = lone
    ? `${summary}\nClick to jump to it`
    : `${summary} — zoom in to separate them\n${list}\nClick to jump to the first`
  const ring = "shadow-[0_0_0_1px_var(--strip-track)]"
  const onClick = (e: ReactMouseEvent) => {
    e.stopPropagation()
    onSeek(group.marks[0].globalMs)
  }
  return (
    <>
      {/* The tick: through the rally, under the head / pill. */}
      <span
        aria-hidden
        className={`pointer-events-none absolute z-[3] rounded-[1px] ${ring} ${
          lone ? VERDICT_DOT[group.marks[0].verdict] : ""
        }`}
        style={{
          top: MARKER_PX,
          bottom: 1,
          left: `${group.centre - 1}px`,
          width: 2,
        }}
      >
        {lone ? null : <VerdictStripes counts={group.counts} direction="col" />}
      </span>
      {lone ? (
        <button
          type="button"
          aria-label={summary}
          title={title}
          onClick={onClick}
          className="group/mark absolute top-0 z-[11] cursor-pointer focus-visible:outline-none"
          style={{
            left: `${pillCentre - MARK_HIT_PX / 2}px`,
            width: MARK_HIT_PX,
            height: MARKER_PX,
          }}
        >
          <span
            className={`absolute bottom-0 left-1/2 -translate-x-1/2 rounded-t-[2px] ${ring} transition-shadow group-hover/mark:shadow-[0_0_0_1.5px_var(--foreground)] group-focus-visible/mark:shadow-[0_0_0_2px_var(--ring)] ${VERDICT_DOT[group.marks[0].verdict]}`}
            style={{ width: MARK_HEAD_PX, height: MARK_HEAD_H_PX }}
          />
        </button>
      ) : (
        <button
          type="button"
          aria-label={summary}
          title={title}
          onClick={onClick}
          className={`absolute z-[3] cursor-pointer overflow-hidden rounded-full border border-foreground/55 bg-strip-track text-foreground ${ring} transition-shadow hover:border-foreground hover:shadow-[0_0_0_1px_var(--strip-track),0_0_0_2px_var(--foreground)] focus-visible:shadow-[0_0_0_1px_var(--strip-track),0_0_0_3px_var(--ring)] focus-visible:outline-none`}
          style={{
            left: `${pillCentre - pillWidth(n) / 2}px`,
            width: pillWidth(n),
            top: (MARKER_PX - MARK_PILL_PX) / 2,
            height: MARK_PILL_PX,
          }}
        >
          <span className="block text-center text-[11px] leading-3 font-semibold tabular-nums">
            {pillLabel(n)}
          </span>
          <VerdictStripes
            counts={group.counts}
            direction="row"
            className="inset-x-0 bottom-0 h-[2px]"
          />
        </button>
      )}
    </>
  )
})

/**
 * The audio waveform under the rally blocks: each downsampled peak is a vertical
 * bar centred on the strip, so shuttle hits read as spikes and rally boundaries
 * can be eyeballed where the blocks overlay them. Drawn behind the blocks at low
 * contrast and stretched to fill its recording's span.
 */
const Waveform = memo(function Waveform({ peaks }: { peaks: number[] }) {
  if (peaks.length === 0) return null
  return (
    <svg
      className="pointer-events-none absolute inset-0 size-full text-strip-wave"
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
})
