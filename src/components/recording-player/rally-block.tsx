"use client"

import { FlagIcon } from "lucide-react"

import { fileName, formatClock } from "@/lib/format"
import {
  UNCERTAIN_CONFIDENCE,
  type SessionRally,
} from "@/components/recording-player-transport"
import { UNCERTAIN_BORDER, UNCERTAIN_FILL } from "./constants"
import { rallyNumberFits } from "./timeline-ruler"

/**
 * An in-flight rally-edge drag over the timeline strip: which rally and edge is
 * moving, the fixed opposite edge (`anchorGlobalMs`), the live dragged position
 * (`globalMs`), whether that position is currently snapped to the playhead, and
 * the recording bounds the drag is clamped into. Session-global ms throughout.
 * Null when nothing is being dragged.
 */
export interface DragState {
  key: string
  edge: "start" | "end"
  anchorGlobalMs: number
  globalMs: number
  snappedToPlayhead: boolean
  minGlobalMs: number
  maxGlobalMs: number
}

/**
 * A rally's strict bounds as drawn: its stored bounds, or the live position
 * while one of its edges is being dragged. Ascending.
 */
function liveBounds(
  rally: SessionRally,
  drag: DragState | null
): { lo: number; hi: number } {
  const gStart = drag
    ? drag.edge === "start"
      ? drag.globalMs
      : drag.anchorGlobalMs
    : rally.globalStart
  const gEnd = drag
    ? drag.edge === "end"
      ? drag.globalMs
      : drag.anchorGlobalMs
    : rally.globalEnd
  return { lo: Math.min(gStart, gEnd), hi: Math.max(gStart, gEnd) }
}

/**
 * A block's fill and border. Translucent so the waveform under it still reads;
 * the strip's own theme tokens (index.css) so rallies separate from gaps in
 * light as well as dark, violet for an uncertain rally (the shared uncertain
 * hue). The current rally is the strip's strongest mark: a 2px solid edge in
 * the primary colour over a denser fill — the same reading in both themes,
 * violet kept if it's uncertain.
 */
function blockTone(uncertain: boolean, current: boolean): string {
  if (current) {
    return `border-2 border-strip-current-edge ${
      uncertain
        ? "bg-violet-500/60 hover:bg-violet-500/70"
        : "bg-strip-current hover:bg-strip-current"
    }`
  }
  return uncertain
    ? `${UNCERTAIN_BORDER} ${UNCERTAIN_FILL} hover:bg-violet-500/45`
    : "border-strip-rally-edge bg-strip-rally hover:bg-strip-rally-hover"
}

/**
 * An edit-mode edge handle: a 10px grab zone centred on the rally's edge — half
 * outside it, so even a 3px block keeps its own fill instead of being painted
 * over by two handles (the "uncertain loses its violet in edit mode" bug).
 */
const HANDLE = "absolute -inset-y-px z-[2] w-2.5 cursor-ew-resize"

/**
 * The handle's visible grip, a 2px bar on the edge itself: shown on the
 * selected rally and while hovering a block, hidden otherwise so the strip in
 * edit mode reads like the strip in review.
 */
const GRIP =
  "pointer-events-none absolute inset-y-0 w-0.5 rounded-full bg-foreground opacity-0 group-hover:opacity-70 group-data-selected:opacity-100"

/**
 * The selected rally's grab tab: a 6px-wide, 22px-tall pill centred on each
 * edge, ringed in the background colour so it stands off any fill — the
 * unmistakable "drag me" on the rally the edit tools act on. Only on the
 * selected rally, so edit mode doesn't sprout handles on every block.
 */
const TAB =
  "pointer-events-none absolute top-1/2 h-[22px] w-1.5 -translate-y-1/2 rounded-full bg-foreground opacity-0 shadow-[0_0_0_1px_var(--background)] group-data-selected:opacity-100"

/**
 * One rally drawn as a block on the timeline strip: positioned and sized on the
 * session axis, styled for its uncertain (ADR 0002) / flagged (issue #10) /
 * current / selected state, and — in edit mode — carrying the two draggable edge handles
 * that start a boundary adjustment (issue #7). It fills the full height of its
 * play-interval box (the padded span the strip draws behind it), flush inside
 * the outline. While its own edge is being dragged it renders the live drag
 * position instead of its stored bounds.
 *
 * A click jumps to the rally's padded lead-in — where `[`/`]` land too (in edit
 * mode it selects the rally instead); seeking to a point inside a rally is the
 * ruler's job.
 */
export function RallyBlock({
  rally,
  number,
  pxPerSec,
  editing,
  current,
  selected,
  drag,
  minGlobalMs,
  maxGlobalMs,
  onSelect,
  onSeek,
  onStartDrag,
}: {
  rally: SessionRally
  /** Session-wide rally number (0-based), shown 1-based on the block — the rail's numbering. */
  number: number
  pxPerSec: number
  editing: boolean
  /** The rally the playhead is in (or in the padding of) — drawn emphasised. */
  current: boolean
  selected: boolean
  /** The active drag when it targets this rally, else null. */
  drag: DragState | null
  /** The recording bounds a drag of this rally is clamped into (session-global). */
  minGlobalMs: number
  maxGlobalMs: number
  onSelect: () => void
  /** Seek to where the rally plays from (its padded lead-in). */
  onSeek: () => void
  onStartDrag: (
    edge: "start" | "end",
    anchorGlobalMs: number,
    minGlobalMs: number,
    maxGlobalMs: number
  ) => void
}) {
  const { lo, hi } = liveBounds(rally, drag)
  const left = (lo / 1000) * pxPerSec
  const width = ((hi - lo) / 1000) * pxPerSec
  const uncertain = rally.confidence < UNCERTAIN_CONFIDENCE
  const label = String(number + 1)
  const labelFits = rallyNumberFits(width, label)

  return (
    <button
      type="button"
      onClick={(e) => {
        e.stopPropagation()
        if (editing) {
          onSelect()
        } else {
          onSeek()
        }
      }}
      aria-label={`Rally ${label}`}
      aria-current={current || undefined}
      data-selected={selected || undefined}
      className={`group absolute inset-y-px cursor-pointer rounded-[3px] border transition-colors focus-visible:ring-2 focus-visible:ring-ring ${blockTone(
        uncertain,
        current
      )} ${
        // Edit-mode selection: a 2px dashed outline in the same primary
        // colour, standing off the block, so it reads apart from (and
        // alongside) the current rally's solid edge.
        selected
          ? "z-[1] outline-2 outline-offset-2 outline-primary outline-dashed"
          : "outline-none"
      }`}
      style={{
        left: `${left}px`,
        width: `${Math.max(width, 3)}px`,
      }}
      title={`Rally ${number + 1}: ${formatClock(rally.globalStart)}–${formatClock(
        rally.globalEnd
      )}${uncertain ? " (uncertain)" : ""}${rally.flagged ? " · flagged" : ""} · confidence ${Math.round(
        rally.confidence * 100
      )}% · ${fileName(rally.path)}\nClick to play it from its lead-in · click the ruler above to seek inside it`}
    >
      {/* The rally's number, the one the rail lists: plain muted text in the
          block's top-left when it fits, else floated over the block's top on
          hover / focus. Deliberately no chip — the strip's chips are the
          playhead badge (filled) and mark piles (outlined, "×N") — so a
          number reads as a label, never a count. A track-coloured halo keeps
          it legible over the waveform; the current rally's is full strength. */}
      <span
        aria-hidden
        className={`pointer-events-none absolute z-[3] text-[10px] leading-3 whitespace-nowrap tabular-nums [text-shadow:0_0_2px_var(--strip-track),0_0_1px_var(--strip-track)] ${
          current
            ? "font-semibold text-foreground"
            : "font-medium text-muted-foreground"
        } ${
          labelFits
            ? "top-0.5 left-1"
            : "top-0.5 left-1/2 -translate-x-1/2 opacity-0 group-hover:opacity-100 group-focus-visible:opacity-100"
        }`}
      >
        {label}
      </span>
      {rally.flagged ? (
        // Planted on the block's top edge at its start, up in the strip's
        // marker band: a mark, not an outline, so it never competes with the
        // current rally's edge. Sky is the app-wide flag hue.
        <FlagIcon
          aria-hidden
          className="pointer-events-none absolute bottom-full -left-[3px] z-[2] mb-px size-3 fill-sky-400 text-sky-500"
        />
      ) : null}
      {editing ? (
        <>
          <span
            role="separator"
            aria-label="Drag rally start"
            onClick={(e) => e.stopPropagation()}
            onPointerDown={(e) => {
              e.stopPropagation()
              e.preventDefault()
              onSelect()
              onStartDrag("start", rally.globalEnd, minGlobalMs, maxGlobalMs)
            }}
            className={`${HANDLE} -left-[5px]`}
          >
            <span className={`${GRIP} left-[4px]`} />
            <span className={`${TAB} left-[2px]`} />
          </span>
          <span
            role="separator"
            aria-label="Drag rally end"
            onClick={(e) => e.stopPropagation()}
            onPointerDown={(e) => {
              e.stopPropagation()
              e.preventDefault()
              onSelect()
              onStartDrag("end", rally.globalStart, minGlobalMs, maxGlobalMs)
            }}
            className={`${HANDLE} -right-[5px]`}
          >
            <span className={`${GRIP} right-[4px]`} />
            <span className={`${TAB} right-[2px]`} />
          </span>
        </>
      ) : null}
    </button>
  )
}
