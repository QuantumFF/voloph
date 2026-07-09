import { describe, expect, it } from "vitest"

import {
  estimateRemainingMs,
  formatEta,
  remainingByRecording,
  type AnalysisProgress,
} from "./analysis-progress"

function tick(over: Partial<AnalysisProgress> & { recording_id: number }): AnalysisProgress {
  return { processed_ms: 30_000, total_ms: 120_000, elapsed_ms: 20_000, ...over }
}

describe("estimateRemainingMs", () => {
  // The estimate is derived from measured progress through the recording:
  // how much footage has been processed vs its total, paced by the wall-clock
  // time that took. Speed = processed / elapsed; remaining footage / speed.
  it("scales the unprocessed footage by the measured processing speed", () => {
    // Processed 30s of a 120s recording in 20s of wall clock → 1.5× real-time.
    // 90s of footage left ÷ 1.5 = 60s of wall clock remaining.
    expect(estimateRemainingMs(30_000, 120_000, 20_000)).toBe(60_000)
  })

  it("handles slower-than-real-time analysis", () => {
    // Processed 30s in 60s of wall clock → 0.5× real-time. 90s left ÷ 0.5 = 180s.
    expect(estimateRemainingMs(30_000, 120_000, 60_000)).toBe(180_000)
  })

  it("returns null before any measurable progress", () => {
    // No footage processed, or no elapsed time yet: speed is undefined, so we
    // cannot estimate — the caller shows a bare spinner instead of a bad number.
    expect(estimateRemainingMs(0, 120_000, 20_000)).toBeNull()
    expect(estimateRemainingMs(30_000, 120_000, 0)).toBeNull()
  })

  it("returns null when the total is unknown", () => {
    expect(estimateRemainingMs(30_000, 0, 20_000)).toBeNull()
    expect(estimateRemainingMs(30_000, null, 20_000)).toBeNull()
  })

  it("floors at zero once processing has caught up to the total", () => {
    expect(estimateRemainingMs(120_000, 120_000, 40_000)).toBe(0)
    expect(estimateRemainingMs(130_000, 120_000, 40_000)).toBe(0)
  })
})

describe("remainingByRecording", () => {
  it("keeps an estimate only for recordings still analyzing", () => {
    const progress = new Map<number, AnalysisProgress>([
      [1, tick({ recording_id: 1 })],
      [2, tick({ recording_id: 2 })],
    ])
    // Recording 2 has finished (not in the analyzing set) — its tick is dropped.
    const out = remainingByRecording(new Set([1]), progress)
    expect(out.get(1)).toBe(60_000)
    expect(out.has(2)).toBe(false)
  })

  it("omits recordings too early to estimate from", () => {
    const progress = new Map<number, AnalysisProgress>([
      [1, tick({ recording_id: 1, processed_ms: 0 })],
    ])
    const out = remainingByRecording(new Set([1]), progress)
    expect(out.has(1)).toBe(false)
  })
})

describe("formatEta", () => {
  it("rounds up to whole seconds under a minute", () => {
    expect(formatEta(0)).toBe("~0s left")
    expect(formatEta(1)).toBe("~1s left")
    expect(formatEta(4_200)).toBe("~5s left")
    expect(formatEta(59_000)).toBe("~59s left")
  })

  it("reads minutes (rounded up) past a minute", () => {
    expect(formatEta(60_000)).toBe("~1m left")
    expect(formatEta(61_000)).toBe("~2m left")
    expect(formatEta(600_000)).toBe("~10m left")
  })
})
