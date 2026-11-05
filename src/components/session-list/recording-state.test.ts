import { describe, expect, it } from "vitest"

import {
  formatFootage,
  groupByMonth,
  plural,
  sessionStats,
  shortPath,
} from "./recording-state"
import type { Recording, Session } from "./types"

function rec(over: Partial<Recording>): Recording {
  return {
    id: 1,
    path: "/lib/a.mov",
    file_size: 1,
    quick_hash: "h",
    capture_day: "2026-06-21",
    probe_state: "ready",
    segment_state: "ready",
    duration_ms: 60_000,
    rally_count: 3,
    ...over,
  }
}

describe("sessionStats", () => {
  it("counts only ready rallies and flags background work", () => {
    const session: Session = {
      id: 1,
      capture_day: "2026-06-21",
      recordings: [
        rec({ id: 1 }),
        rec({ id: 2, segment_state: "unknown", rally_count: 0 }),
        rec({ id: 3, segment_state: "failed", rally_count: 9 }),
      ],
    }
    expect(sessionStats(session)).toEqual({
      recordings: 3,
      rallies: 3,
      durationMs: 180_000,
      processing: 1,
    })
  })

  it("sums across sessions", () => {
    const a: Session = {
      id: 1,
      capture_day: "2026-06-21",
      recordings: [rec({})],
    }
    const b: Session = {
      id: 2,
      capture_day: "2026-06-18",
      recordings: [rec({})],
    }
    expect(sessionStats([a, b]).rallies).toBe(6)
  })
})

describe("groupByMonth", () => {
  it("keeps order and splits on month changes", () => {
    const groups = groupByMonth([
      { capture_day: "2026-07-02" },
      { capture_day: "2026-06-21" },
      { capture_day: "2026-06-15" },
    ])
    expect(groups.map((g) => [g.key, g.sessions.length])).toEqual([
      ["2026-07", 1],
      ["2026-06", 2],
    ])
  })
})

describe("formatters", () => {
  it("pluralizes", () => {
    expect(plural(1, "rally", "rallies")).toBe("1 rally")
    expect(plural(2, "rally", "rallies")).toBe("2 rallies")
  })
  it("shows sub-minute footage in seconds", () => {
    expect(formatFootage(42_000)).toBe("42s")
    expect(formatFootage(360_000)).toBe("6m")
  })
})

describe("shortPath", () => {
  it("abbreviates home and keeps the last two folders of a deep path", () => {
    expect(shortPath("/home/qdes/Videos/badminton")).toBe("~/Videos/badminton")
    expect(shortPath("/tmp/claude-1000/a/b/scratchpad/lib")).toBe(
      "…/scratchpad/lib"
    )
    expect(shortPath("/home/qdes/a/b/c/lib")).toBe("~/…/c/lib")
    expect(shortPath("/mnt/nas")).toBe("/mnt/nas")
  })
})
