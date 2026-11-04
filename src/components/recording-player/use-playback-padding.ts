import { useSyncExternalStore } from "react"

import { trackedInvoke } from "@/lib/tauri"
import {
  DEFAULT_PADDING,
  type Padding,
} from "@/components/recording-player-transport"

/**
 * The playback/export padding (issue #104, ADR 0020) as one app-wide store: the
 * player and the library menu read the same value, so a change in the menu
 * applies to the next play without a reload. Loaded from the backend on first
 * use; starts at the default so playback is padded from the first frame, and a
 * failed or malformed load keeps the default. The presets the menu offers live
 * in `session-list/library-settings`.
 */
let current: Padding = DEFAULT_PADDING
let loading: Promise<void> | null = null
const listeners = new Set<() => void>()

function publish(p: Padding) {
  current = p
  for (const l of listeners) l()
}

function isPadding(p: unknown): p is Padding {
  return (
    typeof p === "object" &&
    p !== null &&
    typeof (p as Padding).lead_ms === "number" &&
    typeof (p as Padding).tail_ms === "number"
  )
}

function load() {
  loading ??= trackedInvoke<Padding>("playback_padding")
    .then((p) => {
      if (isPadding(p)) publish(p)
    })
    .catch(() => {})
}

function subscribe(listener: () => void) {
  listeners.add(listener)
  load()
  return () => {
    listeners.delete(listener)
  }
}

/** The current playback/export padding; re-renders when it changes. */
export function usePlaybackPadding(): Padding {
  return useSyncExternalStore(subscribe, () => current)
}

/**
 * Store a new padding (the backend clamps each side to 0–10 s) and apply it
 * everywhere. Rejects, leaving the old value in place, when the store fails.
 */
export async function setPlaybackPadding(padding: Padding): Promise<void> {
  await trackedInvoke<void>("set_playback_padding", { padding })
  publish(padding)
}

/** Test seam: forget the loaded value so each test starts from the default. */
export function resetPlaybackPaddingForTests() {
  current = DEFAULT_PADDING
  loading = null
  listeners.clear()
}
