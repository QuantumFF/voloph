/**
 * The library menu's choice tables (ADR 0019, ADR 0020): the segmenters and the
 * clip padding presets, and how a backend value maps onto them.
 */
import { describe, expect, it } from "vitest"

import { DEFAULT_PADDING } from "@/components/recording-player-transport"
import {
  describePadding,
  PADDING_PRESETS,
  paddingForPreset,
  paddingPresetId,
  SEGMENTERS,
  segmenterId,
} from "./library-settings"

describe("segmenters", () => {
  it("lists Standard, Precise, Classic in menu order", () => {
    expect(SEGMENTERS.map((s) => [s.id, s.label])).toEqual([
      ["student", "Standard"],
      ["teacher", "Precise"],
      ["v6", "Classic"],
    ])
  })

  it("knows every listed id, and no other", () => {
    for (const s of SEGMENTERS) expect(segmenterId(s.id)).toBe(s.id)
    expect(segmenterId("student2")).toBeNull()
    expect(segmenterId("")).toBeNull()
  })
})

describe("padding presets", () => {
  it("names the preset a padding matches, else custom", () => {
    expect(paddingPresetId(DEFAULT_PADDING)).toBe("normal")
    expect(paddingPresetId({ lead_ms: 0, tail_ms: 0 })).toBe("none")
    expect(paddingPresetId({ lead_ms: 750, tail_ms: 1500 })).toBe("custom")
  })

  it("maps a preset id to just its padding, and custom to nothing", () => {
    expect(paddingForPreset("long")).toEqual({ lead_ms: 2000, tail_ms: 3000 })
    expect(paddingForPreset("normal")).toEqual(DEFAULT_PADDING)
    expect(paddingForPreset("custom")).toBeNull()
    for (const p of PADDING_PRESETS) {
      expect(paddingPresetId(paddingForPreset(p.id)!)).toBe(p.id)
    }
  })

  it("describes a padding in seconds", () => {
    expect(describePadding({ lead_ms: 1000, tail_ms: 1500 })).toBe(
      "1 s before, 1.5 s after"
    )
    expect(describePadding({ lead_ms: 0, tail_ms: 0 })).toBe(
      "The rally only, serve to landing"
    )
  })
})
