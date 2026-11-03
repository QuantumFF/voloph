import { useEffect, useState } from "react"

import { trackedInvoke } from "@/lib/tauri"
import {
  DEFAULT_PADDING,
  type Padding,
} from "@/components/recording-player-transport"

/**
 * The stored playback/export padding (issue #104), loaded once. Starts at the
 * default so playback is padded from the first frame; a failed or empty load
 * keeps the default.
 */
export function usePlaybackPadding(): Padding {
  const [padding, setPadding] = useState<Padding>(DEFAULT_PADDING)
  useEffect(() => {
    let live = true
    trackedInvoke<Padding>("playback_padding")
      .then((p) => {
        if (live && p && typeof p.lead_ms === "number" && typeof p.tail_ms === "number") {
          setPadding(p)
        }
      })
      .catch(() => {})
    return () => {
      live = false
    }
  }, [])
  return padding
}
