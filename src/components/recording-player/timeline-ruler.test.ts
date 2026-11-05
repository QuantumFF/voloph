import { describe, expect, it } from "vitest"

import {
  SESSION_PX_PER_SEC_DEFAULT,
  SESSION_PX_PER_SEC_MAX,
  SESSION_PX_PER_SEC_MIN,
} from "./constants"
import {
  chipSpan,
  edgeChipSpan,
  labelUnderChip,
  rallyNumberFits,
  rulerLabelVisible,
  rulerScale,
  rulerTicks,
} from "./timeline-ruler"

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

describe("labelUnderChip", () => {
  it("hides a label the chip lands on, including one starting just left of it", () => {
    // Playhead at 3:34 sitting just right of the "3:30" tick (the "3:3" bug).
    expect(labelUnderChip(100, "3:30", 112, "3:34")).toBe(true)
    // A label starting under the chip's right half.
    expect(labelUnderChip(115, "4:00", 112, "3:34")).toBe(true)
  })

  it("keeps labels clear of the chip", () => {
    expect(labelUnderChip(0, "0:00", 200, "3:34")).toBe(false)
    expect(labelUnderChip(300, "5:00", 200, "3:34")).toBe(false)
  })

  it("widens with an hour-long chip label", () => {
    expect(labelUnderChip(120, "1:00:30", 100, "0:59")).toBe(false)
    expect(labelUnderChip(120, "1:00:30", 100, "1:00:20")).toBe(true)
  })
})

describe("rulerLabelVisible", () => {
  const view = { left: 1_000, width: 600 }

  it("drops a label that would be sliced by the window's left edge", () => {
    // The tick sits just off-screen left: its label would read ":40".
    expect(rulerLabelVisible(990, "2:40", view, [])).toBe(false)
    // Its tick is in view but the label starts a hair before the edge.
    expect(rulerLabelVisible(995, "2:40", view, [])).toBe(false)
    expect(rulerLabelVisible(996, "2:40", view, [])).toBe(true)
  })

  it("drops a label that would run past the window's right edge", () => {
    expect(rulerLabelVisible(1_580, "4:20", view, [])).toBe(false)
    expect(rulerLabelVisible(1_560, "4:20", view, [])).toBe(true)
  })

  it("clips nothing before the window is measured", () => {
    expect(rulerLabelVisible(-2, "0:00", { left: 0, width: 0 }, [])).toBe(true)
  })

  it("hides only the label under the playhead chip, and restores it once the chip moves on", () => {
    const at = (playheadPx: number) => [chipSpan(playheadPx, "3:34")]
    expect(rulerLabelVisible(1_100, "3:30", view, at(1_112))).toBe(false)
    expect(rulerLabelVisible(1_172, "3:40", view, at(1_112))).toBe(true)
    expect(rulerLabelVisible(1_100, "3:30", view, at(1_160))).toBe(true)
  })

  it("keeps labels out from under the off-screen playhead chip", () => {
    const left = [edgeChipSpan("left", view, "0:19")]
    expect(rulerLabelVisible(1_010, "2:50", view, left)).toBe(false)
    expect(rulerLabelVisible(1_080, "3:00", view, left)).toBe(true)
    const right = [edgeChipSpan("right", view, "5:10")]
    expect(rulerLabelVisible(1_540, "4:10", view, right)).toBe(false)
    expect(rulerLabelVisible(1_480, "4:00", view, right)).toBe(true)
  })
})

describe("rallyNumberFits", () => {
  it("needs at least 14px for any number", () => {
    expect(rallyNumberFits(13, "1")).toBe(false)
    expect(rallyNumberFits(14, "1")).toBe(true)
  })

  it("needs more room the more digits the number has", () => {
    expect(rallyNumberFits(19, "12")).toBe(false)
    expect(rallyNumberFits(20, "12")).toBe(true)
    expect(rallyNumberFits(25, "120")).toBe(false)
    expect(rallyNumberFits(26, "120")).toBe(true)
  })
})
