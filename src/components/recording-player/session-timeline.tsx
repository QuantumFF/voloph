"use client"

import {
  forwardRef,
  useCallback,
  useEffect,
  useImperativeHandle,
  useRef,
  useState,
  type Dispatch,
  type SetStateAction,
} from "react"
import {
  ChevronLeftIcon,
  ChevronRightIcon,
  PencilIcon,
  PlusIcon,
  ScissorsIcon,
  Trash2Icon,
  TriangleAlertIcon,
} from "lucide-react"

import { Button } from "@/components/ui/button"
import { fileName, formatClock } from "@/lib/format"
import {
  UNCERTAIN_CONFIDENCE,
  clamp,
  stripScrollTarget,
  type SessionModel,
  type SessionRally,
} from "@/components/recording-player-transport"
import { SESSION_PX_PER_SEC_MAX, SESSION_PX_PER_SEC_MIN } from "./index"

/** How much each Alt+scroll notch over the timeline zooms. */
const ALT_SCROLL_ZOOM_FACTOR = 1.15

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
 * faint dividers marking recording boundaries. The playhead tracks the session
 * position and auto-scrolls into view. Clicking the strip seeks the session
 * (crossing recordings as needed); a rally block seeks to its start.
 *
 * In correction mode (issue #7) each edit is resolved against the recording that
 * owns the rally: drag an edge to adjust, split at the playhead, merge with the
 * next rally in the same recording, add around the playhead, or delete.
 */
export const SessionTimeline = forwardRef<
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
