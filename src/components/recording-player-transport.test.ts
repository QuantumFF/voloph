import { describe, expect, it } from "vitest"

import {
  SPEED_LADDER,
  addAtPlayheadEdit,
  adjustRallyEdit,
  annotationsByRally,
  buildSessionAnnotations,
  buildSessionModel,
  clampVolume,
  editAffordances,
  gapSkipAction,
  inPlayInterval,
  leadInMs,
  mergeRallyEdit,
  nextRallyMs,
  nextUncertainMs,
  prevRallyAction,
  rallyAt,
  rallyContains,
  rallyIndexAt,
  rallyKey,
  rallyLeadInMs,
  NO_PADDING,
  paddedSpanMs,
  paddedPlayIntervals,
  resumeStartMs,
  resumeTickLanded,
  seekTarget,
  sessionPlayIntervals,
  speedIndexForValue,
  splitRallyEdit,
  stepSpeedIndex,
  stripScrollTarget,
  type Rally,
  type SessionAnnotation,
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
  return { id, start_ms, end_ms, confidence, flagged: false }
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

describe("speedIndexForValue", () => {
  it("maps each ladder value back to its index", () => {
    SPEED_LADDER.forEach((speed, i) => {
      expect(speedIndexForValue(speed)).toBe(i)
    })
  })

  it("snaps an off-ladder speed to the nearest index", () => {
    expect(speedIndexForValue(0.6)).toBe(SPEED_LADDER.indexOf(0.5))
    expect(speedIndexForValue(1.2)).toBe(SPEED_LADDER.indexOf(1))
    expect(speedIndexForValue(1.4)).toBe(SPEED_LADDER.indexOf(1.5))
  })

  it("clamps beyond the ends of the ladder", () => {
    expect(speedIndexForValue(0.1)).toBe(0)
    expect(speedIndexForValue(4)).toBe(SPEED_LADDER.length - 1)
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

  it("lands on the padded lead-in and moves on from it on the next jump", () => {
    const model = buildSessionModel([{ path: "a.mp4" }], {
      "a.mp4": timeline(10000, [
        rally(1, 4000, 5000, 0.2),
        rally(2, 7000, 8000, 0.3),
      ]),
    })
    const pad = { lead_ms: 1000, tail_ms: 1500 }
    expect(nextUncertainMs(model.rallies, 0, pad)).toBe(3000)
    // The playhead now sits on rally 1's lead-in, before its strict start.
    expect(nextUncertainMs(model.rallies, 3000, pad)).toBe(6000)
  })
})

describe("rallyLeadInMs", () => {
  it("pads the start, but never back into the previous recording", () => {
    const model = buildSessionModel([{ path: "a.mp4" }, { path: "b.mp4" }], {
      "a.mp4": timeline(10000, [rally(1, 5000, 6000)]),
      "b.mp4": timeline(10000, [rally(2, 300, 2000)]),
    })
    const pad = { lead_ms: 1000, tail_ms: 1500 }
    const [first, second] = model.rallies
    expect(rallyLeadInMs(first, pad)).toBe(4000)
    // b.mp4 starts at 10 s on the session axis; its rally starts 0.3 s in.
    expect(second.globalStart).toBe(10300)
    expect(rallyLeadInMs(second, pad)).toBe(10000)
  })
})

describe("paddedSpanMs", () => {
  const pad = { lead_ms: 1000, tail_ms: 1500 }

  it("widens a rally by its lead-in and tail", () => {
    expect(paddedSpanMs(5000, 6000, pad, 0, 10000)).toEqual({
      start: 4000,
      end: 7500,
    })
  })

  it("never reaches past its own recording", () => {
    // A recording spanning 10–20 s on the session axis.
    expect(paddedSpanMs(10300, 19000, pad, 10000, 20000)).toEqual({
      start: 10000,
      end: 20000,
    })
  })

  it("is the strict rally with no padding", () => {
    expect(paddedSpanMs(5000, 6000, NO_PADDING, 0, 10000)).toEqual({
      start: 5000,
      end: 6000,
    })
  })
})

describe("paddedPlayIntervals", () => {
  const pad = { lead_ms: 1000, tail_ms: 1500 }
  const span = (
    recordingIndex: number,
    globalStart: number,
    globalEnd: number,
    recordingStart = 0,
    recordingEnd = 100_000
  ) => ({
    recordingIndex,
    globalStart,
    globalEnd,
    recordingStart,
    recordingEnd,
  })

  it("merges rallies whose padding touches into one box", () => {
    expect(
      paddedPlayIntervals(
        [span(0, 5000, 6000), span(0, 8000, 9000), span(0, 20000, 21000)],
        pad
      )
    ).toEqual([
      { recordingIndex: 0, globalStart: 4000, globalEnd: 10500 },
      { recordingIndex: 0, globalStart: 19000, globalEnd: 22500 },
    ])
  })

  it("never merges across a recording boundary", () => {
    expect(
      paddedPlayIntervals(
        [span(0, 8000, 9500, 0, 10000), span(1, 10300, 12000, 10000, 20000)],
        pad
      )
    ).toEqual([
      { recordingIndex: 0, globalStart: 7000, globalEnd: 10000 },
      { recordingIndex: 1, globalStart: 10000, globalEnd: 13500 },
    ])
  })

  it("draws exactly the session's play intervals", () => {
    const model = buildSessionModel([{ path: "a.mp4" }, { path: "b.mp4" }], {
      "a.mp4": timeline(10000, [rally(1, 2000, 3000), rally(2, 4000, 9500)]),
      "b.mp4": timeline(10000, [rally(3, 300, 2000), rally(4, 7000, 8000)]),
    })
    const spans = model.rallies.map((r) => {
      const seg = model.segments[r.recordingIndex]
      return {
        recordingIndex: r.recordingIndex,
        globalStart: r.globalStart,
        globalEnd: r.globalEnd,
        recordingStart: seg.offsetMs,
        recordingEnd: seg.offsetMs + (seg.durationMs ?? 0),
      }
    })
    expect(paddedPlayIntervals(spans, pad)).toEqual(
      sessionPlayIntervals(model, pad)
    )
  })
})

describe("resumeStartMs", () => {
  const rallies = [rally(1, 1000, 2000), rally(2, 5000, 6000)]

  // The regression seam for "jumping to the next recording starts from the
  // beginning instead of where I clicked": a click that crosses a boundary
  // carries `{ atMs }`, which must resolve to that exact local time so the load
  // can bake it in (applied on FILE_LOADED) rather than race a dropped seek.
  it("resolves a clicked local time straight through", () => {
    expect(resumeStartMs({ atMs: 4200 }, rallies)).toBe(4200)
    // Resolves even before the crossed-into recording is segmented.
    expect(resumeStartMs({ atMs: 4200 }, [])).toBe(4200)
    // Zero is a real target, not "no resume".
    expect(resumeStartMs({ atMs: 0 }, rallies)).toBe(0)
  })

  it("resumes forward crossings at the first rally's start", () => {
    expect(resumeStartMs("start", rallies)).toBe(1000)
  })

  it("resumes backward crossings at the last rally's start", () => {
    expect(resumeStartMs("end", rallies)).toBe(5000)
  })

  it("defers a rally resume until the timeline arrives", () => {
    // No rallies yet → null, so the load can't carry it and the recording plays
    // from the top until the deferred resume seeks once its draft lands.
    expect(resumeStartMs("start", [])).toBeNull()
    expect(resumeStartMs("end", [])).toBeNull()
  })
})

describe("resumeTickLanded", () => {
  // The second crossing gate (the position gate). The identity gate
  // (`mpv:file-loaded`) has already dropped the outgoing recording's stale ticks,
  // so this only decides when the freshly-loaded file's pre-seek near-0 ticks give
  // way to its settled position — without this, gap-skip reads the transient ~0 as
  // "before the first rally" and yanks the playhead there (always starts the
  // crossed-into recording at its first rally instead of where the click landed).
  it("drops the pre-seek near-zero ticks of a deep resume", () => {
    expect(resumeTickLanded(0, 42_000, 250)).toBe(false)
    expect(resumeTickLanded(80, 42_000, 250)).toBe(false)
  })

  it("lands once the playhead reaches the target (exact seek)", () => {
    expect(resumeTickLanded(42_000, 42_000, 250)).toBe(true)
    // A frame shy of the target still counts as landed (within tolerance).
    expect(resumeTickLanded(41_900, 42_000, 250)).toBe(true)
    // And anything past it (an overshooting first tick, or playback advancing
    // after the seek) — one-sided so an overshoot can't strand the gate.
    expect(resumeTickLanded(42_500, 42_000, 250)).toBe(true)
  })

  it("treats a near-start resume as immediately landed (no gating to strand)", () => {
    expect(resumeTickLanded(0, 0, 250)).toBe(true)
    expect(resumeTickLanded(0, 100, 250)).toBe(true)
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

describe("buildSessionAnnotations", () => {
  it("lifts each recording's annotations onto the session axis, ordered", () => {
    const session = buildSessionModel([{ path: "a.mp4" }, { path: "b.mp4" }], {
      "a.mp4": timeline(10000, [rally(1, 1000, 2000)]),
      "b.mp4": timeline(8000, [rally(2, 500, 1500)]),
    })
    const marks = buildSessionAnnotations(session, {
      "b.mp4": [
        { id: 3, time_ms: 700, verdict: "mistake", aspect: null, note: null },
      ],
      "a.mp4": [
        {
          id: 2,
          time_ms: 1500,
          verdict: "good",
          aspect: "selection",
          note: "clean",
        },
      ],
    })
    // Ordered by global time: a's at 1500, b's lifted by a's 10000 → 10700.
    expect(marks.map((m) => [m.id, m.globalMs, m.verdict])).toEqual([
      [2, 1500, "good"],
      [3, 10700, "mistake"],
    ])
    expect(marks[1].recordingIndex).toBe(1)
    // Aspect and note ride along onto the session axis.
    expect(marks[0].aspect).toBe("selection")
    expect(marks[0].note).toBe("clean")
  })

  it("skips annotations on an unplaced recording", () => {
    const session = buildSessionModel([{ path: "a.mp4" }, { path: "b.mp4" }], {
      "a.mp4": {
        segment_state: "unknown",
        duration_ms: null,
        rallies: [],
        waveform: [],
      },
      "b.mp4": timeline(8000, []),
    })
    // a isn't placed (no duration) → b isn't either; both drop out.
    const marks = buildSessionAnnotations(session, {
      "a.mp4": [
        { id: 1, time_ms: 100, verdict: "good", aspect: null, note: null },
      ],
      "b.mp4": [
        { id: 2, time_ms: 100, verdict: "bad", aspect: null, note: null },
      ],
    })
    expect(marks).toEqual([])
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
    flagged: false,
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
        {
          command: "update_rally",
          path: "b.mp4",
          rallyId: 7,
          startMs: 1200,
          endMs: 1800,
        },
      ],
    })
  })

  it("orders flipped drag endpoints (start past end)", () => {
    const plan = adjustRallyEdit(
      sessionRally(7, 1000, 2000),
      11800,
      11200,
      10000,
      8000
    )
    expect(plan).toEqual({
      kind: "ops",
      ops: [
        {
          command: "update_rally",
          path: "b.mp4",
          rallyId: 7,
          startMs: 1200,
          endMs: 1800,
        },
      ],
    })
  })

  it("clamps to the recording's bounds", () => {
    const plan = adjustRallyEdit(
      sessionRally(7, 1000, 2000),
      9000,
      99999,
      10000,
      8000
    )
    expect(plan).toEqual({
      kind: "ops",
      ops: [
        {
          command: "update_rally",
          path: "b.mp4",
          rallyId: 7,
          startMs: 0,
          endMs: 8000,
        },
      ],
    })
  })

  it("rejects a zero-or-negative-length result", () => {
    // Both endpoints clamp to 0 (before the recording start) → empty rally.
    const plan = adjustRallyEdit(
      sessionRally(7, 1000, 2000),
      9000,
      9500,
      10000,
      8000
    )
    expect(plan).toEqual({ kind: "reject" })
  })
})

describe("addAtPlayheadEdit", () => {
  it("centres a rally on the playhead spanning halfMs each side", () => {
    const plan = addAtPlayheadEdit("b.mp4", 5000, 8000, 2000)
    expect(plan).toEqual({
      kind: "ops",
      ops: [
        { command: "add_rally", path: "b.mp4", startMs: 3000, endMs: 7000 },
      ],
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
    const plan = addAtPlayheadEdit(
      "b.mp4",
      5000,
      Number.POSITIVE_INFINITY,
      2000
    )
    expect(plan).toEqual({
      kind: "ops",
      ops: [
        { command: "add_rally", path: "b.mp4", startMs: 3000, endMs: 7000 },
      ],
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
        {
          command: "update_rally",
          path: "b.mp4",
          rallyId: 7,
          startMs: 1000,
          endMs: 1500,
        },
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
        {
          command: "update_rally",
          path: "b.mp4",
          rallyId: 7,
          startMs: 1000,
          endMs: 3000,
        },
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

describe("playIntervals (presentation padding, #104)", () => {
  const r = (id: number, start_ms: number, end_ms: number) => ({
    id,
    start_ms,
    end_ms,
    confidence: 0.9,
    flagged: false,
  })
  const pad = { lead_ms: 1000, tail_ms: 1500 }

  it("widens each strict rally by the lead-in and tail", async () => {
    const { playIntervals } = await import("./recording-player-transport")
    expect(playIntervals([r(1, 5000, 6000)], pad)).toEqual([r(1, 4000, 7500)])
  })

  it("clamps to the recording and merges rallies the padding makes overlap", async () => {
    const { playIntervals } = await import("./recording-player-transport")
    const got = playIntervals(
      [
        r(1, 500, 3000),
        r(2, 8000, 9000),
        r(3, 11000, 12000),
        r(4, 19000, 19500),
      ],
      pad,
      20000
    )
    expect(got.map((x) => [x.id, x.start_ms, x.end_ms])).toEqual([
      [1, 0, 4500],
      [2, 7000, 13500],
      [4, 18000, 20000],
    ])
  })

  it("leaves the stored edges alone with no padding", async () => {
    const { playIntervals, NO_PADDING } =
      await import("./recording-player-transport")
    expect(playIntervals([r(1, 5000, 6000)], NO_PADDING)).toEqual([
      r(1, 5000, 6000),
    ])
  })
})

describe("leadInMs", () => {
  it.each([
    // [strict start, lead-in, opens at]
    [5000, 1000, 4000],
    [600, 1000, 0], // clamped to the recording's start
    [5000, 0, 5000],
  ])("a rally at %i with a %i ms lead-in opens at %i", (start, lead, want) => {
    expect(leadInMs(start, { lead_ms: lead, tail_ms: 0 })).toBe(want)
  })
})

describe("sessionPlayIntervals (#104)", () => {
  const pad = { lead_ms: 1000, tail_ms: 1500 }
  const edges = (model: ReturnType<typeof buildSessionModel>) =>
    sessionPlayIntervals(model, pad).map((i) => [
      i.recordingIndex,
      i.globalStart,
      i.globalEnd,
    ])

  it.each([
    {
      name: "a tail stops at its own recording's end, never spilling into the next",
      a: [rally(1, 2000, 9500)],
      b: [rally(2, 5000, 6000)],
      want: [
        [0, 1000, 10000],
        [1, 14000, 17500],
      ],
    },
    {
      name: "a lead-in stops at its own recording's start, never reaching back",
      a: [rally(1, 2000, 3000)],
      b: [rally(2, 300, 2000)],
      want: [
        [0, 1000, 4500],
        [1, 10000, 13500],
      ],
    },
    {
      name: "the first lead-in and the last tail clamp to the session's ends",
      a: [rally(1, 400, 3000)],
      b: [rally(2, 5000, 9800)],
      want: [
        [0, 0, 4500],
        [1, 14000, 20000],
      ],
    },
    {
      name: "rallies the padding makes overlap merge within a recording",
      a: [rally(1, 2000, 3000), rally(2, 5000, 6000)],
      b: [rally(3, 5000, 6000)],
      want: [
        [0, 1000, 7500],
        [1, 14000, 17500],
      ],
    },
  ])("$name", ({ a, b, want }) => {
    const model = buildSessionModel([{ path: "a.mp4" }, { path: "b.mp4" }], {
      "a.mp4": timeline(10000, a),
      "b.mp4": timeline(10000, b),
    })
    expect(edges(model)).toEqual(want)
  })

  it("skips a recording that isn't placed yet", () => {
    const model = buildSessionModel([{ path: "a.mp4" }, { path: "b.mp4" }], {
      "a.mp4": timeline(10000, [rally(1, 2000, 3000)]),
    })
    expect(edges(model)).toEqual([[0, 1000, 4500]])
  })

  it("is half-open: a moment just past a recording's end is the next one's gap", () => {
    const model = buildSessionModel([{ path: "a.mp4" }, { path: "b.mp4" }], {
      "a.mp4": timeline(10000, [rally(1, 2000, 9500)]),
      "b.mp4": timeline(10000, [rally(2, 5000, 6000)]),
    })
    const intervals = sessionPlayIntervals(model, pad)
    expect(inPlayInterval(intervals, 9999)).toBe(true)
    expect(inPlayInterval(intervals, 10000)).toBe(false)
    expect(inPlayInterval(intervals, 10500)).toBe(false)
    expect(inPlayInterval(intervals, 14000)).toBe(true)
  })
})

describe("rally containing a moment", () => {
  const model = buildSessionModel([{ path: "a.mp4" }, { path: "b.mp4" }], {
    "a.mp4": timeline(10000, [rally(1, 1000, 2000), rally(2, 2000, 3000)]),
    "b.mp4": timeline(10000, [rally(1, 500, 1500)]),
  })

  it.each([
    // [session-global ms, owning rally index or -1]
    [999, -1],
    [1000, 0], // a rally's start is inside it
    [1999, 0],
    [2000, 1], // its end is not — it belongs to whatever comes next
    [3000, -1],
    [10500, 2],
    [11500, -1],
  ])("at %i ms the rally index is %i", (ms, want) => {
    expect(rallyIndexAt(model.rallies, ms)).toBe(want)
    expect(rallyAt(model.rallies, ms)).toBe(model.rallies[want] ?? null)
    for (const [i, r] of model.rallies.entries()) {
      expect(rallyContains(r, ms)).toBe(i === want)
    }
  })

  it("keys a rally by recording and id, since ids repeat across recordings", () => {
    expect(model.rallies.map(rallyKey)).toEqual([
      "a.mp4:1",
      "a.mp4:2",
      "b.mp4:1",
    ])
  })

  it("groups annotations under the rally that owns them, dropping gap ones", () => {
    const note = (id: number, globalMs: number): SessionAnnotation => ({
      id,
      recordingIndex: 0,
      path: "a.mp4",
      verdict: "good",
      aspect: null,
      note: null,
      globalMs,
    })
    const groups = annotationsByRally(model.rallies, [
      note(1, 1500),
      note(2, 2000),
      note(3, 5000),
      note(4, 1999),
      note(5, 11000),
    ])
    expect(
      [...groups].map(([key, owned]) => [key, owned.map((a) => a.id)])
    ).toEqual([
      ["a.mp4:1", [1, 4]],
      ["a.mp4:2", [2]],
      ["b.mp4:1", [5]],
    ])
  })
})

describe("editAffordances", () => {
  // Two rallies in a.mp4, one in b.mp4 (b starts at 10 s on the session axis).
  const model = buildSessionModel([{ path: "a.mp4" }, { path: "b.mp4" }], {
    "a.mp4": timeline(10000, [rally(1, 1000, 2000), rally(2, 4000, 5000)]),
    "b.mp4": timeline(10000, [rally(3, 1000, 2000)]),
  })
  const [first, second, third] = model.rallies

  it("affords nothing without a target", () => {
    expect(editAffordances(model.rallies, -1, 1500)).toEqual({
      target: null,
      mergeTarget: null,
      canSplit: false,
      canMerge: false,
    })
  })

  it.each([
    // [playhead, can split] — strictly inside the rally, as splitRallyEdit guards
    [null, false],
    [1000, false],
    [1001, true],
    [1999, true],
    [2000, false],
    [4500, false],
  ])("splits the first rally with the playhead at %s: %s", (ms, want) => {
    expect(editAffordances(model.rallies, 0, ms).canSplit).toBe(want)
  })

  it("splits a later recording's rally against its own offset", () => {
    expect(editAffordances(model.rallies, 2, 11500).canSplit).toBe(true)
    expect(editAffordances(model.rallies, 2, 1500).canSplit).toBe(false)
  })

  it("merges only with the next rally in the same recording", () => {
    const within = editAffordances(model.rallies, 0, null)
    expect(within.target).toBe(first)
    expect(within.mergeTarget).toBe(second)
    expect(within.canMerge).toBe(true)

    const across = editAffordances(model.rallies, 1, null)
    expect(across.target).toBe(second)
    expect(across.mergeTarget).toBeNull()
    expect(across.canMerge).toBe(false)

    const last = editAffordances(model.rallies, 2, null)
    expect(last.target).toBe(third)
    expect(last.canMerge).toBe(false)
  })
})
