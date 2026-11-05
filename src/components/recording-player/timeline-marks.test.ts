import { describe, expect, it } from "vitest"

import type { Verdict } from "@/components/recording-player-transport"
import {
  MARK_HEAD_PX,
  MARK_HIT_PX,
  dodgePlayhead,
  groupMarks,
  pillCount,
  pillLabel,
  pillWidth,
} from "./timeline-marks"

let nextId = 1
const mark = (globalMs: number, verdict: Verdict = "good") => ({
  id: nextId++,
  globalMs,
  verdict,
})

describe("groupMarks", () => {
  it("keeps marks that stand apart as lone ticks", () => {
    // 10px/s: 1s apart is 10px.
    const groups = groupMarks([mark(1_000), mark(2_000), mark(3_000)], 10)
    expect(groups.map((g) => g.marks.length)).toEqual([1, 1, 1])
    expect(groups.map((g) => g.centre)).toEqual([10, 20, 30])
  })

  it("merges a burst too close to tell apart into one counted group", () => {
    // Nine marks over 4s at the default 3px/s: 12px of strip.
    const burst = [
      ...[20_000, 20_200, 20_400].map((t) => mark(t, "good")),
      ...[22_000, 22_200, 22_400].map((t) => mark(t, "bad")),
      ...[24_000, 24_200, 24_400].map((t) => mark(t, "mistake")),
    ]
    const groups = groupMarks(burst, 3)
    expect(groups).toHaveLength(1)
    expect(groups[0].counts).toEqual({ good: 3, bad: 3, mistake: 3 })
    expect(groups[0].lo).toBeCloseTo(60)
    expect(groups[0].hi).toBeCloseTo(73.2)
  })

  it("splits the burst back into ticks once zoomed in", () => {
    const burst = [20_000, 22_000, 24_000].map((t) => mark(t))
    expect(groupMarks(burst, 3)).toHaveLength(1)
    expect(groupMarks(burst, 20)).toHaveLength(3)
  })

  it("sorts marks into time order whatever order they arrive in", () => {
    const [a, b] = [mark(5_000), mark(1_000)]
    const groups = groupMarks([a, b], 10)
    expect(groups.map((g) => g.marks[0])).toEqual([b, a])
  })

  it("merges a lone tick into a pill it would touch", () => {
    // A pair 1px apart forms an 18px pill centred at 100.5; a lone mark 8px
    // past it would sit under the pill's edge, so it joins.
    const groups = groupMarks([mark(10_000), mark(10_100), mark(10_900)], 10)
    expect(groups).toHaveLength(1)
    expect(groups[0].marks).toHaveLength(3)
  })

  it("leaves every drawn head and pill clear of its neighbours", () => {
    const marks = Array.from({ length: 60 }, (_, i) =>
      mark(i * 700 + ((i * 37) % 11) * 90)
    )
    for (const px of [1, 3, 8, 20, 60]) {
      const groups = groupMarks(marks, px)
      for (let i = 1; i < groups.length; i++) {
        const w = (g: (typeof groups)[number]) =>
          g.marks.length === 1 ? MARK_HEAD_PX : pillWidth(g.marks.length)
        const prev = groups[i - 1]
        const g = groups[i]
        expect(g.centre - w(g) / 2).toBeGreaterThanOrEqual(
          prev.centre + w(prev) / 2
        )
      }
      expect(groups.reduce((n, g) => n + g.marks.length, 0)).toBe(60)
    }
  })

  it("is empty without marks", () => {
    expect(groupMarks([], 3)).toEqual([])
  })
})

describe("pillWidth", () => {
  it("fits its × and count, never narrower than a round pill", () => {
    expect(pillWidth(2)).toBe(23) // "×2"
    expect(pillWidth(15)).toBe(30) // "×15"
    expect(pillWidth(140)).toBe(37) // "×99+"
  })
})

describe("pillCount", () => {
  it("caps the count so the pill stays small", () => {
    expect(pillCount(9)).toBe("9")
    expect(pillCount(140)).toBe("99+")
  })
})

describe("pillLabel", () => {
  it("reads as a multiplier, not a bare number", () => {
    expect(pillLabel(18)).toBe("×18")
    expect(pillLabel(140)).toBe("×99+")
  })
})

describe("MARK_HIT_PX", () => {
  it("gives a lone head a grab area at least 8px wider than its tick", () => {
    expect(MARK_HIT_PX).toBeGreaterThanOrEqual(MARK_HEAD_PX + 8)
  })
})

describe("dodgePlayhead", () => {
  // A 30px pill: the line must stay 15 + 4 = 19px from its centre.
  it("leaves a pill the playhead misses where it is", () => {
    expect(dodgePlayhead(100, 30, 150)).toBe(100)
    expect(dodgePlayhead(100, 30, 81)).toBe(100)
    expect(dodgePlayhead(100, 30, null)).toBe(100)
  })

  it("moves a pill the playhead would slice just aside, on its own side", () => {
    expect(dodgePlayhead(100, 30, 90)).toBe(109)
    expect(dodgePlayhead(100, 30, 110)).toBe(91)
  })

  it("moves a pill dead on the playhead to its left, over the marks behind it", () => {
    expect(dodgePlayhead(100, 30, 100)).toBe(81)
  })

  it("always leaves the pill clear of the line", () => {
    for (let p = 70; p <= 130; p += 0.5) {
      const c = dodgePlayhead(100, 30, p)
      expect(Math.abs(c - p)).toBeGreaterThanOrEqual(19)
    }
  })
})
