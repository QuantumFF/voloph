import { describe, expect, it } from "vitest"

import {
  SPEED_LADDER,
  clampVolume,
  seekTarget,
  stepSpeedIndex,
} from "./recording-player-transport"

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
