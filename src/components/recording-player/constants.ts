import type { Verdict } from "@/components/recording-player-transport"

/**
 * Horizontal scale of the session timeline strip in pixels-per-second, so the
 * whole session is one long, horizontally-scrollable strip. The zoom buttons
 * step it between MIN (a whole long session at a glance) and MAX (frame-level
 * detail), each press scaling by `SESSION_ZOOM_FACTOR`.
 */
export const SESSION_PX_PER_SEC_DEFAULT = 3
export const SESSION_PX_PER_SEC_MIN = 1
export const SESSION_PX_PER_SEC_MAX = 240
export const SESSION_ZOOM_FACTOR = 1.5

/**
 * Rally length threshold (CONTEXT.md: every rally is classified long or short
 * by duration, objectively and automatically). UI-only until length filtering
 * lands; 15s reads as a sustained exchange.
 */
export const LONG_RALLY_MS = 15_000

/**
 * The dot colour per verdict (issue #8), shared by every surface that shows a
 * verdict — the inspector, the rail, the strip markers, and the moment browser —
 * so they can't drift apart.
 */
export const VERDICT_DOT: Record<Verdict, string> = {
  good: "bg-emerald-500",
  bad: "bg-amber-500",
  mistake: "bg-red-500",
}
