import { describe, expect, it } from "vitest"

import {
  SPEED_LADDER,
  addAtPlayheadEdit,
  adjustRallyEdit,
  buildSessionModel,
  clampVolume,
  gapSkipAction,
  mergeRallyEdit,
  nextRallyMs,
  nextUncertainMs,
  prevRallyAction,
  seekTarget,
  splitRallyEdit,
  stepSpeedIndex,
  stripScrollTarget,
  type Rally,
  type SessionRally,
  type Timeline,
} from "./recording-player-transport"

/** A rally with a default-certain confidence unless overridden. */
function rally(
  id: number,
  start_ms: number,
  end_ms: number,
  confidence = 1
): Rally {
  return { id, start_ms, end_ms, confidence }
}

/** A ready timeline of the given duration and rallies. */
function timeline(duration_ms: number, rallies: Rally[]): Timeline {
  return { segment_state: "ready", duration_ms, rallies, waveform: [] }
}

describe("stepSpeedIndex", () => {
  it("steps up and down the ladder", () => {
    expect(stepSpeedIndex(3, 1)).toBe(4)
    expect(stepSpeedIndex(3, -1)).toBe(2)
  })

  it("clamps at the slow end", () => {
    expect(stepSpeedIndex(0, -1)).toBe(0)
  })

  it("clamps at the fast end", () => {
    expect(stepSpeedIndex(SPEED_LADDER.length - 1, 1)).toBe(
      SPEED_LADDER.length - 1
    )
  })
})

describe("seekTarget", () => {
  it("adds a relative delta", () => {
    expect(seekTarget(10_000, 5_000)).toBe(15_000)
    expect(seekTarget(10_000, -5_000)).toBe(5_000)
  })

  it("never lands before zero", () => {
    expect(seekTarget(2_000, -5_000)).toBe(0)
  })
})

describe("clampVolume", () => {
  it("clamps into 0–100", () => {
    expect(clampVolume(110)).toBe(100)
    expect(clampVolume(-10)).toBe(0)
    expect(clampVolume(55)).toBe(55)
  })
})

describe("gapSkipAction", () => {
  const rallies = [rally(1, 1000, 2000), rally(2, 5000, 6000)]

  it("does nothing with no rallies (plays straight through)", () => {
    expect(gapSkipAction([], 1234, false, false, false)).toEqual({
      kind: "none",
    })
  })

  it("does nothing inside a rally", () => {
    expect(gapSkipAction(rallies, 1500, false, false, false)).toEqual({
      kind: "none",
    })
  })

  it("jumps to the next rally when in a gap", () => {
    expect(gapSkipAction(rallies, 3000, false, false, false)).toEqual({
      kind: "seek",
      ms: 5000,
    })
  })

  it("jumps from the head gap to the first rally", () => {
    expect(gapSkipAction(rallies, 0, false, false, false)).toEqual({
      kind: "seek",
      ms: 1000,
    })
  })

  it("crosses into the next recording past the last rally", () => {
    expect(gapSkipAction(rallies, 6500, false, false, false)).toEqual({
      kind: "next-recording",
    })
  })

  it("stops past the last rally of the last recording", () => {
    expect(gapSkipAction(rallies, 6500, false, false, true)).toEqual({
      kind: "stop",
    })
  })

  it("plays a gap through under free-play", () => {
    expect(gapSkipAction(rallies, 3000, false, true, false)).toEqual({
      kind: "none",
    })
  })

  it("restarts the current rally at its end when looping", () => {
    expect(gapSkipAction(rallies, 2000, true, false, false)).toEqual({
      kind: "seek",
      ms: 1000,
    })
  })

  it("does not loop while free-play is on", () => {
    // Free-play wins: the gap past the rally's end plays through.
    expect(gapSkipAction(rallies, 2000, true, true, false)).toEqual({
      kind: "none",
    })
  })
})

describe("nextRallyMs", () => {
  const rallies = [rally(1, 1000, 2000), rally(2, 5000, 6000)]

  it("finds the first rally after the playhead", () => {
    expect(nextRallyMs(rallies, 0)).toBe(1000)
    expect(nextRallyMs(rallies, 1500)).toBe(5000)
  })

  it("returns null past the last rally (caller crosses the boundary)", () => {
    expect(nextRallyMs(rallies, 5500)).toBeNull()
  })
})

describe("prevRallyAction", () => {
  const rallies = [rally(1, 1000, 2000), rally(2, 5000, 6000)]

  it("restarts the rally when played well into it", () => {
    // Past rally 2's start by more than the restart slack → rewind to its start.
    expect(prevRallyAction(rallies, 6500, false)).toEqual({
      kind: "seek",
      ms: 5000,
    })
  })

  it("steps to the previous rally when at the current's start", () => {
    expect(prevRallyAction(rallies, 5000, false)).toEqual({
      kind: "seek",
      ms: 1000,
    })
  })

  it("crosses into the previous recording from the first rally", () => {
    expect(prevRallyAction(rallies, 1000, false)).toEqual({
      kind: "prev-recording",
    })
  })

  it("snaps to the first rally on the first recording", () => {
    expect(prevRallyAction(rallies, 1000, true)).toEqual({
      kind: "seek",
      ms: 1000,
    })
  })

  it("reports none with no rallies", () => {
    expect(prevRallyAction([], 1000, true)).toEqual({ kind: "none" })
  })
})

describe("nextUncertainMs", () => {
  it("finds the next low-confidence rally on the session axis, wrapping", () => {
    const model = buildSessionModel([{ path: "a.mp4" }], {
      "a.mp4": timeline(10000, [
        rally(1, 1000, 2000, 0.9),
        rally(2, 4000, 5000, 0.2),
        rally(3, 7000, 8000, 0.3),
      ]),
    })
    expect(nextUncertainMs(model.rallies, 0)).toBe(4000)
    expect(nextUncertainMs(model.rallies, 4500)).toBe(7000)
    // Past the last uncertain region → wrap to the first.
    expect(nextUncertainMs(model.rallies, 9000)).toBe(4000)
  })

  it("returns null when nothing is uncertain", () => {
    const model = buildSessionModel([{ path: "a.mp4" }], {
      "a.mp4": timeline(10000, [rally(1, 1000, 2000, 0.9)]),
    })
    expect(nextUncertainMs(model.rallies, 0)).toBeNull()
  })
})

describe("buildSessionModel", () => {
  it("stitches recordings onto one axis, offsetting rallies", () => {
    const model = buildSessionModel([{ path: "a.mp4" }, { path: "b.mp4" }], {
      "a.mp4": timeline(10000, [rally(1, 1000, 2000)]),
      "b.mp4": timeline(8000, [rally(2, 500, 1500)]),
    })
    expect(model.placedCount).toBe(2)
    expect(model.totalMs).toBe(18000)
    expect(model.rallies).toHaveLength(2)
    // b.mp4's rally is lifted by a.mp4's duration.
    expect(model.rallies[1].globalStart).toBe(10500)
    expect(model.rallies[1].globalEnd).toBe(11500)
    expect(model.rallies[1].recordingIndex).toBe(1)
  })

  it("stops placing at the first recording with an unknown duration", () => {
    const model = buildSessionModel(
      [{ path: "a.mp4" }, { path: "b.mp4" }, { path: "c.mp4" }],
      {
        "a.mp4": timeline(10000, [rally(1, 1000, 2000)]),
        // b has no duration yet → it's included but nothing after it is placed.
        "b.mp4": {
          segment_state: "unknown",
          duration_ms: null,
          rallies: [],
          waveform: [],
        },
        "c.mp4": timeline(5000, [rally(2, 0, 1000)]),
      }
    )
    expect(model.placedCount).toBe(1)
    expect(model.totalMs).toBe(10000)
    expect(model.segments).toHaveLength(2) // a (placed) + b (the unknown one)
    expect(model.rallies).toHaveLength(1) // only a's rally is placed
  })
})

/** A session rally on b.mp4 at recording index 1, with the given local bounds. */
function sessionRally(
  id: number,
  localStart: number,
  localEnd: number,
  offset = 10000
): SessionRally {
  return {
    recordingIndex: 1,
    path: "b.mp4",
    id,
    localStart,
    localEnd,
    globalStart: offset + localStart,
    globalEnd: offset + localEnd,
    confidence: 1,
  }
}

describe("adjustRallyEdit", () => {
  it("maps global drag bounds to recording-local and updates the rally", () => {
    const plan = adjustRallyEdit(
      sessionRally(7, 1000, 2000),
      11200, // global start
      11800, // global end
      10000, // offset
      8000 // duration
    )
    expect(plan).toEqual({
      kind: "ops",
      ops: [
        { command: "update_rally", path: "b.mp4", rallyId: 7, startMs: 1200, endMs: 1800 },
      ],
    })
  })

  it("orders flipped drag endpoints (start past end)", () => {
    const plan = adjustRallyEdit(sessionRally(7, 1000, 2000), 11800, 11200, 10000, 8000)
    expect(plan).toEqual({
      kind: "ops",
      ops: [
        { command: "update_rally", path: "b.mp4", rallyId: 7, startMs: 1200, endMs: 1800 },
      ],
    })
  })

  it("clamps to the recording's bounds", () => {
    const plan = adjustRallyEdit(sessionRally(7, 1000, 2000), 9000, 99999, 10000, 8000)
    expect(plan).toEqual({
      kind: "ops",
      ops: [
        { command: "update_rally", path: "b.mp4", rallyId: 7, startMs: 0, endMs: 8000 },
      ],
    })
  })

  it("rejects a zero-or-negative-length result", () => {
    // Both endpoints clamp to 0 (before the recording start) → empty rally.
    const plan = adjustRallyEdit(sessionRally(7, 1000, 2000), 9000, 9500, 10000, 8000)
    expect(plan).toEqual({ kind: "reject" })
  })
})

describe("addAtPlayheadEdit", () => {
  it("centres a rally on the playhead spanning halfMs each side", () => {
    const plan = addAtPlayheadEdit("b.mp4", 5000, 8000, 2000)
    expect(plan).toEqual({
      kind: "ops",
      ops: [{ command: "add_rally", path: "b.mp4", startMs: 3000, endMs: 7000 }],
    })
  })

  it("clamps the start at zero near the recording head", () => {
    const plan = addAtPlayheadEdit("b.mp4", 1000, 8000, 2000)
    expect(plan).toEqual({
      kind: "ops",
      ops: [{ command: "add_rally", path: "b.mp4", startMs: 0, endMs: 3000 }],
    })
  })

  it("leaves the end uncapped when the duration is unknown (Infinity)", () => {
    const plan = addAtPlayheadEdit("b.mp4", 5000, Number.POSITIVE_INFINITY, 2000)
    expect(plan).toEqual({
      kind: "ops",
      ops: [{ command: "add_rally", path: "b.mp4", startMs: 3000, endMs: 7000 }],
    })
  })
})

describe("splitRallyEdit", () => {
  it("shrinks the rally to the cut and adds the remainder", () => {
    // Cut at global 11500 → local 1500, inside [1000, 2000].
    const plan = splitRallyEdit(sessionRally(7, 1000, 2000), 11500, 10000)
    expect(plan).toEqual({
      kind: "ops",
      ops: [
        { command: "update_rally", path: "b.mp4", rallyId: 7, startMs: 1000, endMs: 1500 },
        { command: "add_rally", path: "b.mp4", startMs: 1500, endMs: 2000 },
      ],
    })
  })

  it("rejects a cut at or outside the rally's bounds", () => {
    expect(splitRallyEdit(sessionRally(7, 1000, 2000), 11000, 10000)).toEqual({
      kind: "reject",
    })
    expect(splitRallyEdit(sessionRally(7, 1000, 2000), 12000, 10000)).toEqual({
      kind: "reject",
    })
  })
})

describe("mergeRallyEdit", () => {
  it("stretches the first over both and deletes the second", () => {
    const plan = mergeRallyEdit(
      sessionRally(7, 1000, 2000),
      sessionRally(8, 2500, 3000)
    )
    expect(plan).toEqual({
      kind: "ops",
      ops: [
        { command: "update_rally", path: "b.mp4", rallyId: 7, startMs: 1000, endMs: 3000 },
        { command: "delete_rally", path: "b.mp4", rallyId: 8 },
      ],
    })
  })

  it("rejects a cross-recording merge", () => {
    const first = sessionRally(7, 1000, 2000)
    const second: SessionRally = { ...sessionRally(8, 500, 800), path: "c.mp4" }
    expect(mergeRallyEdit(first, second)).toEqual({ kind: "reject" })
  })
})

describe("stripScrollTarget", () => {
  // The regression seam for the "can't scroll past the playhead at a recording's
  // start" bug: centring on a playhead at the far left clamps the target to 0,
  // and the strip is already at 0, so the helper must report "skip" (null) — the
  // caller then never arms its programmatic-scroll guard, so the next manual
  // scroll is free to disarm follow.
  it("returns null when centring on the far-left playhead is already a no-op", () => {
    // target = playheadPx - clientWidth/2 = 0 - 400 = -400, clamps to 0 == current.
    expect(stripScrollTarget(-400, 0, 800, 5000)).toBeNull()
  })

  it("returns null when the clamped target equals the current offset", () => {
    expect(stripScrollTarget(100, 100, 800, 5000)).toBeNull()
    // Past the max scroll, but already pinned to the max → still a no-op.
    expect(stripScrollTarget(9000, 4200, 800, 5000)).toBeNull()
  })

  it("returns the clamped offset when a real move is needed", () => {
    expect(stripScrollTarget(1200, 0, 800, 5000)).toBe(1200)
    // Negative target clamps up to 0 (a real move away from a scrolled position).
    expect(stripScrollTarget(-50, 300, 800, 5000)).toBe(0)
    // Beyond the scrollable range clamps down to max (scrollWidth - clientWidth).
    expect(stripScrollTarget(9000, 0, 800, 5000)).toBe(4200)
  })

  it("treats content narrower than the viewport as a single 0 position", () => {
    expect(stripScrollTarget(500, 0, 800, 300)).toBeNull()
    expect(stripScrollTarget(500, 10, 800, 300)).toBe(0)
  })

  it("rounds so sub-pixel scrollLeft drift doesn't force a spurious write", () => {
    expect(stripScrollTarget(100.4, 100.1, 800, 5000)).toBeNull()
    expect(stripScrollTarget(102, 100, 800, 5000)).toBe(102)
  })
})
