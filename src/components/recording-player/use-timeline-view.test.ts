import { describe, expect, it } from "vitest"

import { isPlayheadLost, zoomToShow } from "./use-timeline-view"

describe("isPlayheadLost", () => {
  it("trusts the strip's own report once it has one", () => {
    // Zoomed and scrolled by hand, but the playhead is still on screen.
    expect(
      isPlayheadLost({ fitted: false, following: false, visible: true })
    ).toBe(false)
    expect(
      isPlayheadLost({ fitted: false, following: true, visible: false })
    ).toBe(true)
  })

  it("never calls a fitted strip lost — the whole session is in view", () => {
    expect(
      isPlayheadLost({ fitted: true, following: false, visible: undefined })
    ).toBe(false)
  })

  it("infers lost only for a zoomed strip that stopped following", () => {
    expect(
      isPlayheadLost({ fitted: false, following: true, visible: undefined })
    ).toBe(false)
    expect(
      isPlayheadLost({ fitted: false, following: false, visible: undefined })
    ).toBe(true)
  })
})

describe("zoomToShow", () => {
  const range = { minPxPerSec: 1, maxPxPerSec: 240 }

  it("fits a rally ±10s across the strip, centred on the playhead", () => {
    // Rally 215–222 s, playhead just inside its start: the far edge is
    // 222 + 10 = 232 s, 16.95 s from the centre, so 33.9 s span 800 px.
    const px = zoomToShow({
      startMs: 205_000,
      endMs: 232_000,
      centerMs: 215_050,
      widthPx: 800,
      ...range,
    })
    expect(px).toBeCloseTo(800 / 33.9, 5)
    // Both ends are on screen with the playhead in the middle.
    expect(215.05 - 400 / px).toBeLessThanOrEqual(205)
    expect(215.05 + 400 / px).toBeGreaterThanOrEqual(232)
  })

  it("stays within the strip's zoom range", () => {
    expect(
      zoomToShow({
        startMs: 0,
        endMs: 10_000_000,
        centerMs: 0,
        widthPx: 800,
        ...range,
      })
    ).toBe(1)
    expect(
      zoomToShow({
        startMs: 1000,
        endMs: 1000,
        centerMs: 1000,
        widthPx: 1_000_000,
        ...range,
      })
    ).toBe(240)
  })
})
