import { useCallback, useState } from "react"
import { listen } from "@tauri-apps/api/event"
import { save } from "@tauri-apps/plugin-dialog"

import { trackedInvoke } from "@/lib/tauri"

/**
 * The Export engine's frontend seam (issues #12/#13/#14), shared by the player
 * (condensed recording/session, flagged/mistake reels) and the moment browser
 * (a reel of the current cross-session filter). One place owns the save dialog,
 * the live `export:progress` readout, and error surfacing so every export entry
 * point behaves identically. `progress` is `null` when idle, else a fraction in
 * `[0, 1]`; `error` carries a failed or empty-selection message.
 */
export function useExport() {
  const [progress, setProgress] = useState<number | null>(null)
  const [error, setError] = useState<string | null>(null)

  // Run one export: pick a destination, then invoke `command`/`args` with a live
  // progress readout. A no-op while an export is already in flight.
  const runExport = useCallback(
    async (
      title: string,
      defaultName: string,
      command: string,
      args: Record<string, unknown>
    ) => {
      if (progress != null) return
      setError(null)
      const output = await save({
        title,
        defaultPath: `${defaultName}.mp4`,
        filters: [{ name: "Video", extensions: ["mp4"] }],
      })
      if (!output) return
      setProgress(0)
      const unlisten = await listen<number>("export:progress", (e) =>
        setProgress(e.payload)
      )
      try {
        await trackedInvoke(command, { ...args, output })
      } catch (e) {
        setError(String(e))
      } finally {
        unlisten()
        setProgress(null)
      }
    },
    [progress]
  )

  return { progress, error, setError, runExport }
}
