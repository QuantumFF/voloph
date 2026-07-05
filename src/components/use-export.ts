import {
  createContext,
  createElement,
  useCallback,
  useContext,
  useEffect,
  useState,
  type ReactNode,
} from "react"
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
 *
 * The state lives in a root-level provider rather than each entry point's own
 * component, because an export outlives the view that started it: the backend
 * ffmpeg run keeps emitting `export:progress` while the user navigates back to
 * the session list or the moment browser, and returning to the player must find
 * the readout still live. A component-local hook lost the state (and the
 * listener) on unmount, so the progress silently vanished mid-export.
 */
interface ExportState {
  progress: number | null
  error: string | null
  setError: (error: string | null) => void
  runExport: (
    title: string,
    defaultName: string,
    command: string,
    args: Record<string, unknown>
  ) => Promise<void>
}

const ExportContext = createContext<ExportState | null>(null)

export function ExportProvider({ children }: { children: ReactNode }) {
  const [progress, setProgress] = useState<number | null>(null)
  const [error, setError] = useState<string | null>(null)

  // One listener for the app's lifetime, so progress keeps updating even while
  // the view that launched the export is unmounted (navigation mid-export).
  useEffect(() => {
    const unlisten = listen<number>("export:progress", (e) =>
      setProgress(e.payload)
    )
    return () => {
      void unlisten.then((off) => off())
    }
  }, [])

  // Run one export: pick a destination, then invoke `command`/`args` with a live
  // progress readout. A no-op while an export is already in flight (only one
  // export runs at a time across the whole app).
  const runExport = useCallback<ExportState["runExport"]>(
    async (title, defaultName, command, args) => {
      if (progress != null) return
      setError(null)
      const output = await save({
        title,
        defaultPath: `${defaultName}.mp4`,
        filters: [{ name: "Video", extensions: ["mp4"] }],
      })
      if (!output) return
      setProgress(0)
      try {
        await trackedInvoke(command, { ...args, output })
      } catch (e) {
        setError(String(e))
      } finally {
        setProgress(null)
      }
    },
    [progress]
  )

  return createElement(
    ExportContext.Provider,
    { value: { progress, error, setError, runExport } },
    children
  )
}

export function useExport() {
  const ctx = useContext(ExportContext)
  if (!ctx) throw new Error("useExport must be used within an ExportProvider")
  return ctx
}
