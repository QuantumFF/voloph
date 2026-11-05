import { describe, expect, it } from "vitest"

import type {
  Annotation,
  FilteredRally,
  Verdict,
} from "@/components/recording-player-transport"

import {
  markedFraction,
  nextToReview,
  sessionReview,
  tallyReview,
} from "./review-progress"
import type { Recording, Session } from "./types"

function rally(
  recordingId: number,
  rallyId: number,
  over: Partial<FilteredRally> = {}
): FilteredRally {
  return {
    session_id: 1,
    capture_day: "2026-06-21",
    recording_id: recordingId,
    recording_path: `/lib/${recordingId}.mov`,
    rally_id: rallyId,
    start_ms: rallyId * 10_000,
    end_ms: rallyId * 10_000 + 5_000,
    long: false,
    flagged: false,
    annotations: [],
    ...over,
  }
}

function marks(...verdicts: Verdict[]): Annotation[] {
  return verdicts.map((verdict, i) => ({
    id: i,
    time_ms: 0,
    verdict,
    aspect: null,
    note: null,
  }))
}

function session(id: number, recordingIds: number[]): Session {
  return {
    id,
    capture_day: "2026-06-21",
    recordings: recordingIds.map((rid): Recording => ({
      id: rid,
      path: `/lib/${rid}.mov`,
      file_size: 1,
      quick_hash: "h",
      capture_day: "2026-06-21",
      probe_state: "ready",
      segment_state: "ready",
      duration_ms: 60_000,
      rally_count: 0,
    })),
  }
}

describe("tallyReview", () => {
  it("counts rallies, flags, marks, and rallies with any mark per recording", () => {
    const all = [
      rally(1, 1, { flagged: true }),
      rally(1, 2),
      rally(1, 3),
      rally(2, 4),
    ]
    // Rally 1 carries a good and a mistake: it shows up in both verdict runs
    // but is one marked rally with two marks.
    const good = [rally(1, 1, { annotations: marks("good") })]
    const bad = [rally(1, 2, { annotations: marks("bad", "bad") })]
    const mistake = [rally(1, 1, { annotations: marks("mistake") })]
    const tallies = tallyReview(all, [good, bad, mistake])
    expect(tallies.get(1)).toEqual({
      rallies: 3,
      marked: 2,
      marks: 4,
      flagged: 1,
    })
    expect(tallies.get(2)).toEqual({
      rallies: 1,
      marked: 0,
      marks: 0,
      flagged: 0,
    })
  })
})

describe("sessionReview", () => {
  it("sums its recordings and treats an absent recording as empty", () => {
    const tallies = tallyReview(
      [rally(1, 1), rally(2, 2, { flagged: true })],
      [[rally(2, 2, { annotations: marks("good") })]]
    )
    expect(sessionReview(session(1, [1, 2, 3]), tallies)).toEqual({
      rallies: 2,
      marked: 1,
      marks: 1,
      flagged: 1,
    })
  })
})

describe("nextToReview", () => {
  it("picks the most recent session that still has unmarked rallies", () => {
    const tallies = tallyReview(
      [rally(1, 1), rally(2, 2), rally(3, 3)],
      [[rally(1, 1, { annotations: marks("good") })]]
    )
    // Newest first: session 10 is fully marked, 20 is still analyzing (no
    // rallies), 30 has an unmarked rally.
    const sessions = [session(10, [1]), session(20, [9]), session(30, [2, 3])]
    expect(nextToReview(sessions, tallies)).toBe(30)
  })

  it("is null while review state is unknown or everything is marked", () => {
    expect(nextToReview([session(1, [1])], null)).toBeNull()
    const tallies = tallyReview(
      [rally(1, 1)],
      [[rally(1, 1, { annotations: marks("bad") })]]
    )
    expect(nextToReview([session(1, [1])], tallies)).toBeNull()
  })
})

describe("markedFraction", () => {
  it("is 0 with no rallies and clamps to 1", () => {
    expect(
      markedFraction({ rallies: 0, marked: 0, marks: 0, flagged: 0 })
    ).toBe(0)
    expect(
      markedFraction({ rallies: 4, marked: 1, marks: 3, flagged: 0 })
    ).toBe(0.25)
  })
})
