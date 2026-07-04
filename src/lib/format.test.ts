import { describe, expect, it } from "vitest"

import {
  fileName,
  formatClock,
  formatDuration,
  formatSize,
  formatTimecode,
} from "./format"

describe("formatClock", () => {
  it("renders minutes and zero-padded seconds", () => {
    expect(formatClock(0)).toBe("0:00")
    expect(formatClock(65_000)).toBe("1:05")
    expect(formatClock(600_000)).toBe("10:00")
  })

  it("rounds to the nearest second", () => {
    expect(formatClock(1_499)).toBe("0:01")
    expect(formatClock(1_500)).toBe("0:02")
  })
})

describe("formatTimecode", () => {
  it("reads m:ss under an hour", () => {
    expect(formatTimecode(0)).toBe("0:00")
    expect(formatTimecode(59_000)).toBe("0:59")
    expect(formatTimecode(3_599_999)).toBe("59:59")
  })

  it("switches to h:mm:ss past an hour", () => {
    expect(formatTimecode(3_600_000)).toBe("1:00:00")
    expect(formatTimecode(3_661_000)).toBe("1:01:01")
  })

  it("clamps negative positions to zero", () => {
    expect(formatTimecode(-500)).toBe("0:00")
  })
})

describe("formatDuration", () => {
  it("reads minutes under an hour and h/mm past it", () => {
    expect(formatDuration(60_000)).toBe("1m")
    expect(formatDuration(3_600_000)).toBe("1h 00m")
    expect(formatDuration(5_400_000)).toBe("1h 30m")
  })

  it("carries rounded-up minutes into the hour", () => {
    // Regression: 59m50s used to render "60m", and 1h59m30s "1h 60m".
    expect(formatDuration(3_590_000)).toBe("1h 00m")
    expect(formatDuration(7_170_000)).toBe("2h 00m")
  })
})

describe("formatSize", () => {
  it("scales through the unit ladder", () => {
    expect(formatSize(0)).toBe("0 B")
    expect(formatSize(512)).toBe("512 B")
    expect(formatSize(1024)).toBe("1.0 KB")
    expect(formatSize(1_572_864)).toBe("1.5 MB")
    expect(formatSize(1024 ** 3)).toBe("1.0 GB")
  })
})

describe("fileName", () => {
  it("takes the last segment of either separator style", () => {
    expect(fileName("/videos/session/rally.mp4")).toBe("rally.mp4")
    expect(fileName("C:\\videos\\rally.mp4")).toBe("rally.mp4")
    expect(fileName("rally.mp4")).toBe("rally.mp4")
  })

  it("falls back to the input for a trailing separator", () => {
    expect(fileName("/videos/")).toBe("/videos/")
  })
})
