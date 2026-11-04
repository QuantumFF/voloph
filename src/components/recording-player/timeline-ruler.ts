/**
 * The time ruler over the session timeline strip: which tick spacing suits the
 * current zoom, and which ticks fall inside the visible window. Pure so the
 * scale choice is unit-tested without a DOM.
 */

/** Spacings a labelled (major) tick may use, in ms — whole seconds and up. */
const MAJOR_STEPS_MS = [
  1_000, 2_000, 5_000, 10_000, 15_000, 30_000, 60_000, 120_000, 300_000,
  600_000, 900_000, 1_800_000, 3_600_000,
]

/** Spacings an unlabelled (minor) tick may use, in ms — sub-second allowed. */
const MINOR_STEPS_MS = [100, 200, 500, ...MAJOR_STEPS_MS]

/** Labels need this much room between them to stay legible. */
const MIN_MAJOR_PX = 72

/** Minor ticks closer than this read as a solid smear. */
const MIN_MINOR_PX = 10

/** No more than this many minor subdivisions per major interval. */
const MAX_SUBDIVISIONS = 10

export interface RulerScale {
  majorMs: number
  minorMs: number
}

/**
 * The tick spacing for a zoom level: the finest major step whose labels have
 * room, and the finest minor step that divides it evenly, has room, and doesn't
 * split a major interval into more than `MAX_SUBDIVISIONS` pieces.
 */
export function rulerScale(pxPerSec: number): RulerScale {
  const px = (ms: number) => (ms / 1000) * pxPerSec
  const majorMs =
    MAJOR_STEPS_MS.find((ms) => px(ms) >= MIN_MAJOR_PX) ??
    MAJOR_STEPS_MS[MAJOR_STEPS_MS.length - 1]
  const minorMs =
    MINOR_STEPS_MS.find(
      (ms) =>
        ms < majorMs &&
        majorMs % ms === 0 &&
        majorMs / ms <= MAX_SUBDIVISIONS &&
        px(ms) >= MIN_MINOR_PX
    ) ?? majorMs
  return { majorMs, minorMs }
}

export interface RulerTick {
  ms: number
  major: boolean
}

/**
 * Every tick of `scale` within `[fromMs, toMs]`, clipped to the session
 * `[0, totalMs]`. Indexed by integer step so long sessions don't drift.
 */
export function rulerTicks(
  scale: RulerScale,
  fromMs: number,
  toMs: number,
  totalMs: number
): RulerTick[] {
  const lo = Math.max(0, fromMs)
  const hi = Math.min(totalMs, toMs)
  if (hi < lo) return []
  const ticks: RulerTick[] = []
  for (let i = Math.ceil(lo / scale.minorMs); i * scale.minorMs <= hi; i++) {
    const ms = i * scale.minorMs
    ticks.push({ ms, major: ms % scale.majorMs === 0 })
  }
  return ticks
}
