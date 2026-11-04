import { describe, expect, it } from "vitest"

import {
  SESSION_PX_PER_SEC_DEFAULT,
  SESSION_PX_PER_SEC_MAX,
  SESSION_PX_PER_SEC_MIN,
} from "./constants"
import { rulerScale, rulerTicks } from "./timeline-ruler"

describe("rulerScale", () => {
  it("labels every 30s at the default zoom, with 5s minor ticks", () => {
    expect(rulerScale(SESSION_PX_PER_SEC_DEFAULT)).toEqual({
      majorMs: 30_000,
      minorMs: 5_000,
    })
  })

  it("labels every second at the deepest zoom, with 100ms minor ticks", () => {
    expect(rulerScale(SESSION_PX_PER_SEC_MAX)).toEqual({
      majorMs: 1_000,
      minorMs: 100,
    })
  })

  it("keeps labels apart and minor ticks dividing them at every zoom", () => {
    for (
      let px = SESSION_PX_PER_SEC_MIN;
      px <= SESSION_PX_PER_SEC_MAX;
      px *= 1.15
    ) {
      const { majorMs, minorMs } = rulerScale(px)
      expect((majorMs / 1000) * px).toBeGreaterThanOrEqual(72)
      expect(majorMs % minorMs).toBe(0)
      expect(majorMs / minorMs).toBeLessThanOrEqual(10)
    }
  })
})

describe("rulerTicks", () => {
  const scale = { majorMs: 30_000, minorMs: 5_000 }

  it("emits ticks inside the window, marking the major ones", () => {
    expect(rulerTicks(scale, 22_000, 41_000, 100_000)).toEqual([
      { ms: 25_000, major: false },
      { ms: 30_000, major: true },
      { ms: 35_000, major: false },
      { ms: 40_000, major: false },
    ])
  })

  it("clips to the session", () => {
    const ticks = rulerTicks(scale, -10_000, 1_000_000, 12_000)
    expect(ticks.map((t) => t.ms)).toEqual([0, 5_000, 10_000])
    expect(ticks[0].major).toBe(true)
  })

  it("is empty when the window misses the session", () => {
    expect(rulerTicks(scale, 50_000, 60_000, 12_000)).toEqual([])
  })
})
