import { createContext, useContext } from "react"

import { clamp } from "@/components/recording-player-transport"

/**
 * The strip's view controls — zoom, fit, and back-to-playhead — as the player
 * owns them (zoom and follow state live in `RecordingPlayer`, which the
 * strip's own wheel/scroll handlers also drive). Provided around the strip so
 * its nav row can host the buttons right above the strip they act on, without
 * threading them through the strip's props.
 */
export interface TimelineView {
  canZoomOut: boolean
  canZoomIn: boolean
  /** The strip is fitted to its width: Fit has nothing to do. */
  fitted: boolean
  /** The playhead is off-screen, so "Playhead" is the way back to it. */
  playheadLost: boolean
  canJumpToPlayhead: boolean
  zoomOut: () => void
  zoomIn: () => void
  fit: () => void
  jumpToPlayhead: () => void
  /**
   * Where the edit toolbar renders, in correction mode: a slot the nav row
   * hosts, so the edit actions join the existing row rather than adding one
   * (which would shrink the video as edit mode toggles). Null outside it.
   */
  editSlot: HTMLElement | null
  setEditSlot: (el: HTMLElement | null) => void
}

/** How much context either side of a rally edit mode zooms in to show. */
export const EDIT_ZOOM_CONTEXT_MS = 10_000

/**
 * The zoom (px/s) at which a strip `widthPx` wide, centred on `centerMs`,
 * shows all of `[startMs, endMs]` — for entering edit mode, zoomed to the
 * selected rally and `EDIT_ZOOM_CONTEXT_MS` either side, with the playhead in
 * the middle. Wide enough to reach the farther end from the centre (at least
 * a second each way), and kept within the strip's zoom range.
 */
export function zoomToShow({
  startMs,
  endMs,
  centerMs,
  widthPx,
  minPxPerSec,
  maxPxPerSec,
}: {
  startMs: number
  endMs: number
  centerMs: number
  widthPx: number
  minPxPerSec: number
  maxPxPerSec: number
}): number {
  const halfMs = Math.max(centerMs - startMs, endMs - centerMs, 1000)
  return clamp(widthPx / ((2 * halfMs) / 1000), minPxPerSec, maxPxPerSec)
}

/**
 * Whether the strip's playhead is off-screen. Exact once the strip has
 * reported `visible` (its `onPlayheadVisibleChange`); until
 * then inferred: a fitted strip shows the whole session, and a following one
 * scrolls to keep the playhead in view — only a zoomed strip scrolled away by
 * hand can have lost it.
 */
export function isPlayheadLost({
  fitted,
  following,
  visible,
}: {
  fitted: boolean
  following: boolean
  /** Whether the strip shows its playhead; undefined until it reports. */
  visible: boolean | undefined
}): boolean {
  if (visible !== undefined) return !visible
  return !fitted && !following
}

export const TimelineViewContext = createContext<TimelineView | null>(null)

/** The strip's view controls, or null outside a player. */
export function useTimelineView(): TimelineView | null {
  return useContext(TimelineViewContext)
}
