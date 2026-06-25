"use client"

import { useCallback, useEffect, useState } from "react"
import { open } from "@tauri-apps/plugin-dialog"
import { AlertTriangleIcon, FolderOpenIcon, Loader2Icon, VideoIcon } from "lucide-react"

import { Button } from "@/components/ui/button"
import {
  Card,
  CardAction,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import { trackedInvoke } from "@/lib/tauri"

interface Recording {
  id: number
  path: string
  file_size: number
  quick_hash: string
  capture_day: string
  /** Transcode lifecycle: unknown | pending | ready | failed (ADR 0005). */
  transcode_state: string
}

/** True while a recording is still being probed or transcoded for playback. */
function isProcessing(state: string): boolean {
  return state === "unknown" || state === "pending"
}

interface Session {
  id: number
  capture_day: string
  recordings: Recording[]
}

interface ScanResult {
  registered: number
  skipped: number
}

function formatSize(bytes: number): string {
  if (bytes <= 0) return "0 B"
  const units = ["B", "KB", "MB", "GB", "TB"]
  const i = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1)
  return `${(bytes / 1024 ** i).toFixed(i === 0 ? 0 : 1)} ${units[i]}`
}

function fileName(path: string): string {
  const parts = path.split(/[\\/]/)
  return parts[parts.length - 1] || path
}

interface SessionListProps {
  /** Open a recording in the player by its on-disk path. */
  onPlay: (path: string) => void
}

export function SessionList({ onPlay }: SessionListProps) {
  const [sessions, setSessions] = useState<Session[]>([])
  const [scanning, setScanning] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const refresh = useCallback(async () => {
    try {
      const next = await trackedInvoke<Session[]>("list_sessions")
      setSessions(next)
    } catch (e) {
      setError(String(e))
    }
  }, [])

  useEffect(() => {
    // Load persisted sessions once on mount. The setState lands after an
    // awaited round-trip to Rust, not synchronously within the effect body.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    void refresh()
  }, [refresh])

  // While any recording is still being transcoded in the background, poll so
  // the row flips from "Processing…" to its size once it becomes playable.
  useEffect(() => {
    const stillWorking = sessions.some((session) =>
      session.recordings.some((recording) => isProcessing(recording.transcode_state)),
    )
    if (!stillWorking) return
    const interval = setInterval(() => void refresh(), 3000)
    return () => clearInterval(interval)
  }, [sessions, refresh])

  async function handlePickFolder() {
    setError(null)
    const folder = await open({ directory: true, multiple: false })
    if (typeof folder !== "string") return

    setScanning(true)
    try {
      await trackedInvoke<ScanResult>("scan_folder", { folder })
      await refresh()
    } catch (e) {
      setError(String(e))
    } finally {
      setScanning(false)
    }
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>Sessions</CardTitle>
        <CardDescription>
          Recordings grouped by capture day, referenced in place. Recordings in a
          format the player can&apos;t handle are converted once on import.
        </CardDescription>
        <CardAction>
          <Button onClick={handlePickFolder} disabled={scanning}>
            <FolderOpenIcon className="size-4" />
            {scanning ? "Scanning…" : "Scan folder"}
          </Button>
        </CardAction>
      </CardHeader>
      <CardContent className="space-y-4">
        {error ? (
          <p className="text-sm text-destructive">{error}</p>
        ) : null}
        {sessions.length === 0 ? (
          <p className="text-sm text-muted-foreground">
            No sessions yet. Scan a folder of recordings to get started.
          </p>
        ) : (
          sessions.map((session) => (
            <div key={session.id} className="rounded-lg border">
              <div className="flex items-center justify-between border-b px-4 py-2">
                <h3 className="font-medium tabular-nums">
                  {session.capture_day}
                </h3>
                <span className="text-sm text-muted-foreground">
                  {session.recordings.length} recording
                  {session.recordings.length === 1 ? "" : "s"}
                </span>
              </div>
              <ul className="divide-y">
                {session.recordings.map((recording) => (
                  <li key={recording.id}>
                    <button
                      type="button"
                      onClick={() => onPlay(recording.path)}
                      className="flex w-full items-center gap-3 px-4 py-2 text-left text-sm hover:bg-accent"
                    >
                      <VideoIcon className="size-4 shrink-0 text-muted-foreground" />
                      <span className="truncate font-medium" title={recording.path}>
                        {fileName(recording.path)}
                      </span>
                      {isProcessing(recording.transcode_state) ? (
                        <span
                          className="ml-auto flex shrink-0 items-center gap-1.5 text-muted-foreground"
                          title="Converting this recording for playback…"
                        >
                          <Loader2Icon className="size-3.5 animate-spin" />
                          Processing…
                        </span>
                      ) : recording.transcode_state === "failed" ? (
                        <span
                          className="ml-auto flex shrink-0 items-center gap-1.5 text-destructive"
                          title="This recording could not be converted for playback."
                        >
                          <AlertTriangleIcon className="size-3.5" />
                          Failed
                        </span>
                      ) : (
                        <span className="ml-auto shrink-0 tabular-nums text-muted-foreground">
                          {formatSize(recording.file_size)}
                        </span>
                      )}
                    </button>
                  </li>
                ))}
              </ul>
            </div>
          ))
        )}
      </CardContent>
    </Card>
  )
}
