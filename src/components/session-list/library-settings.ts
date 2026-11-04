import {
  DEFAULT_PADDING,
  type Padding,
} from "@/components/recording-player-transport"

/**
 * The choices the library menu offers, as tables the header maps over: which
 * segmenter draws the active library's drafts (ADR 0019) and the clip padding
 * presets (ADR 0020). The order here is the order in the menu.
 */

/** The segmenters a library can choose (ADR 0019); ids match `Segmenter::id`. */
export const SEGMENTERS = [
  { id: "student", label: "Standard", hint: "Fast on any computer" },
  {
    id: "teacher",
    label: "Precise",
    hint: "Tracks the shuttle; best with a GPU",
  },
  {
    id: "v6",
    label: "Classic",
    hint: "Catches more very short rallies, but invents more",
  },
] as const
export type SegmenterId = (typeof SEGMENTERS)[number]["id"]

/** The segmenter a library uses until the backend says otherwise. */
export const DEFAULT_SEGMENTER: SegmenterId = "student"

/**
 * A segmenter id from the backend (or a menu value), or `null` when it is none
 * the menu knows — e.g. a newer build's segmenter. The menu then shows no
 * choice checked rather than a wrong one.
 */
export function segmenterId(id: string): SegmenterId | null {
  return SEGMENTERS.find((s) => s.id === id)?.id ?? null
}

/** The clip padding choices the library menu offers. */
export const PADDING_PRESETS = [
  { id: "none", label: "None", lead_ms: 0, tail_ms: 0 },
  { id: "short", label: "Short", lead_ms: 500, tail_ms: 1000 },
  {
    id: "normal",
    label: "Normal",
    lead_ms: DEFAULT_PADDING.lead_ms,
    tail_ms: DEFAULT_PADDING.tail_ms,
  },
  { id: "long", label: "Long", lead_ms: 2000, tail_ms: 3000 },
] as const

/** The preset `padding` matches, or "custom" when it matches none. */
export function paddingPresetId(padding: Padding): string {
  return (
    PADDING_PRESETS.find(
      (p) => p.lead_ms === padding.lead_ms && p.tail_ms === padding.tail_ms
    )?.id ?? "custom"
  )
}

/**
 * The padding a preset id stands for, or `null` for "custom" or any id that is
 * not a preset (nothing to store then).
 */
export function paddingForPreset(id: string): Padding | null {
  const preset = PADDING_PRESETS.find((p) => p.id === id)
  return preset ? { lead_ms: preset.lead_ms, tail_ms: preset.tail_ms } : null
}

/** "1 s before, 1.5 s after" — a padding in seconds, for menu hints. */
export function describePadding(padding: Padding): string {
  if (padding.lead_ms === 0 && padding.tail_ms === 0) {
    return "The rally only, serve to landing"
  }
  const s = (ms: number) => `${ms / 1000} s`
  return `${s(padding.lead_ms)} before, ${s(padding.tail_ms)} after`
}
