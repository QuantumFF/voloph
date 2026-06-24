"use client"

import { convertFileSrc } from "@tauri-apps/api/core"
import { ArrowLeftIcon } from "lucide-react"

import { Button } from "@/components/ui/button"

interface RecordingPlayerProps {
  /** Absolute on-disk path of the recording to play. */
  path: string
  /** Return to the session list. */
  onBack: () => void
}

function fileName(path: string): string {
  const parts = path.split(/[\\/]/)
  return parts[parts.length - 1] || path
}

/**
 * Plays a single recording from its local path. The file is served to the
 * webview through Tauri's asset protocol (`convertFileSrc`), which is enabled
 * with an unrestricted scope so arbitrary user-selected files play — not just
 * bundled assets. Raw playback only: no gap-skipping or timeline yet (#2).
 */
export function RecordingPlayer({ path, onBack }: RecordingPlayerProps) {
  return (
    <div className="space-y-4">
      <div className="flex items-center gap-3">
        <Button variant="outline" size="sm" onClick={onBack}>
          <ArrowLeftIcon className="size-4" />
          Sessions
        </Button>
        <span className="truncate font-medium" title={path}>
          {fileName(path)}
        </span>
      </div>
      <video
        // Re-mount on path change so switching recordings reloads the source
        // and resets playback rather than reusing the previous file's state.
        key={path}
        className="w-full rounded-lg bg-black"
        src={convertFileSrc(path)}
        controls
        autoPlay
      />
    </div>
  )
}
