"use client"

import { FlagIcon } from "lucide-react"

import { fileName, formatClock } from "@/lib/format"
import {
  UNCERTAIN_CONFIDENCE,
  type SessionRally,
} from "@/components/recording-player-transport"

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
 * One rally drawn as a block on the timeline strip: positioned and sized on the
 * session axis, styled for its uncertain (ADR 0002) / flagged (issue #10) /
 * selected state, and — in edit mode — carrying the two draggable edge handles
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
  selected,
  drag,
  minGlobalMs,
  maxGlobalMs,
  onSelect,
  onSeek,
  onStartDrag,
}: {
  rally: SessionRally
  /** Session-wide rally number (0-based), shown 1-based in the tooltip. */
  number: number
  pxPerSec: number
  editing: boolean
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
      className={`absolute inset-y-px cursor-pointer transition-opacity hover:opacity-80 focus:outline-none ${
        uncertain
          ? "border border-amber-500/70 bg-amber-500/40"
          : "bg-primary/85"
      } ${rally.flagged ? "ring-2 ring-sky-400" : ""} ${selected ? "ring-2 ring-foreground ring-offset-1 ring-offset-muted" : ""}`}
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
      {rally.flagged ? (
        <FlagIcon className="pointer-events-none absolute top-0.5 left-0.5 size-2.5 fill-sky-400 text-sky-400" />
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
            className="absolute inset-y-0 left-0 w-1.5 cursor-ew-resize bg-foreground/70 hover:bg-foreground"
          />
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
            className="absolute inset-y-0 right-0 w-1.5 cursor-ew-resize bg-foreground/70 hover:bg-foreground"
          />
        </>
      ) : null}
    </button>
  )
}
