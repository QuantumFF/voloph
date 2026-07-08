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
 * (`globalMs`), and the recording bounds the drag is clamped into. Session-global
 * ms throughout. Null when nothing is being dragged.
 */
export interface DragState {
  key: string
  edge: "start" | "end"
  anchorGlobalMs: number
  globalMs: number
  minGlobalMs: number
  maxGlobalMs: number
}

/**
 * One rally drawn as a block on the timeline strip: positioned and sized on the
 * session axis, styled for its uncertain (ADR 0002) / flagged (issue #10) /
 * selected state, and — in edit mode — carrying the two draggable edge handles
 * that start a boundary adjustment (issue #7). While its own edge is being
 * dragged it renders the live drag position instead of its stored bounds.
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
  onSeek: () => void
  onStartDrag: (
    edge: "start" | "end",
    anchorGlobalMs: number,
    minGlobalMs: number,
    maxGlobalMs: number
  ) => void
}) {
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
  const lo = Math.min(gStart, gEnd)
  const hi = Math.max(gStart, gEnd)
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
      className={`absolute inset-y-0 rounded-sm transition-opacity hover:opacity-80 focus:outline-none ${
        uncertain
          ? "border border-amber-500/70 bg-amber-500/40"
          : "bg-primary/70"
      } ${rally.flagged ? "ring-2 ring-sky-400" : ""} ${selected ? "ring-2 ring-foreground ring-offset-1 ring-offset-muted" : ""}`}
      style={{
        left: `${left}px`,
        width: `${Math.max(width, 3)}px`,
      }}
      title={`Rally ${number + 1}: ${formatClock(rally.globalStart)}–${formatClock(
        rally.globalEnd
      )}${uncertain ? " (uncertain)" : ""}${rally.flagged ? " · flagged" : ""} · confidence ${Math.round(
        rally.confidence * 100
      )}% · ${fileName(rally.path)}`}
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
            className="absolute inset-y-0 left-0 w-1.5 cursor-ew-resize rounded-l-sm bg-foreground/70 hover:bg-foreground"
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
            className="absolute inset-y-0 right-0 w-1.5 cursor-ew-resize rounded-r-sm bg-foreground/70 hover:bg-foreground"
          />
        </>
      ) : null}
    </button>
  )
}
