/**
 * Pure transport math for the recording player, factored out of the component so
 * it can be unit-tested without WebKitGTK or a native surface (ADR 0008). The
 * component owns the side effects (the `mpv_*` invokes and the `time-pos` event
 * stream); this only computes the next value a transport action lands on.
 */

/** The playback speed ladder; `[`/`]` step down/up it. */
export const SPEED_LADDER = [0.25, 0.5, 0.75, 1, 1.5, 2] as const

/** Clamp a ladder index into range after a step. */
export function stepSpeedIndex(index: number, dir: 1 | -1): number {
  return Math.min(Math.max(index + dir, 0), SPEED_LADDER.length - 1)
}

/**
 * The absolute playhead a relative seek lands on, never before zero. The clamp
 * to the recording's end is left to mpv (it caps a seek past EOF itself).
 */
export function seekTarget(currentMs: number, deltaMs: number): number {
  return Math.max(0, currentMs + deltaMs)
}

/** Clamp a volume (0–100) after a step. */
export function clampVolume(volume: number): number {
  return Math.min(Math.max(volume, 0), 100)
}
