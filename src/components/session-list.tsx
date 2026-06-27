"use client"

import { useCallback, useEffect, useState } from "react"
import { open } from "@tauri-apps/plugin-dialog"
import {
  AlertTriangleIcon,
  FilmIcon,
  FolderOpenIcon,
  Loader2Icon,
  MoreVerticalIcon,
  RefreshCwIcon,
  RotateCwIcon,
  VideoIcon,
} from "lucide-react"

import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog"
import { Button, buttonVariants } from "@/components/ui/button"
import {
  Card,
  CardAction,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import { trackedInvoke } from "@/lib/tauri"

interface Recording {
  id: number
  path: string
  file_size: number
  quick_hash: string
  capture_day: string
  /** Transcode lifecycle: unknown | pending | ready | failed (ADR 0005). */
  transcode_state: string
  /** Segmentation lifecycle: unknown | ready | failed (ADR 0002). */
  segment_state: string
  /** Recording duration in ms; null until segmented. */
  duration_ms: number | null
  /** Rallies in the draft timeline (0 until segmented). */
  rally_count: number
}

/** True while a recording is still being probed or transcoded for playback. */
function isTranscoding(state: string): boolean {
  return state === "unknown" || state === "pending"
}

/**
 * True while a recording is playable but its draft timeline is still being
 * produced — audio extraction + segmentation (ADR 0002). Segmentation only
 * starts once the transcode is `ready`, so a still-`unknown` segment state on a
 * ready recording means "queued or analyzing".
 */
function isAnalyzing(recording: Recording): boolean {
  return (
    recording.transcode_state === "ready" &&
    recording.segment_state === "unknown"
  )
}

/** True while any background media work is still pending for this recording. */
function isProcessing(recording: Recording): boolean {
  return isTranscoding(recording.transcode_state) || isAnalyzing(recording)
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
  const i = Math.min(
    Math.floor(Math.log(bytes) / Math.log(1024)),
    units.length - 1
  )
  return `${(bytes / 1024 ** i).toFixed(i === 0 ? 0 : 1)} ${units[i]}`
}

function fileName(path: string): string {
  const parts = path.split(/[\\/]/)
  return parts[parts.length - 1] || path
}

interface SessionListProps {
  /**
   * Open a session in the player as one continuous playlist. `recordings` is the
   * session's recordings in capture-time order; `startIndex` is the one to open
   * first (which recording the user clicked).
   */
  onPlay: (recordings: { path: string }[], startIndex: number) => void
}

export function SessionList({ onPlay }: SessionListProps) {
  const [sessions, setSessions] = useState<Session[]>([])
  const [scanning, setScanning] = useState(false)
  const [refreshing, setRefreshing] = useState(false)
  const [retranscodingAll, setRetranscodingAll] = useState(false)
  const [reanalyzingAll, setReanalyzingAll] = useState(false)
  // Which bulk action is awaiting confirmation in the dialog, if any.
  const [confirmAction, setConfirmAction] = useState<
    "retranscode" | "reanalyze" | null
  >(null)
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

  // While any recording is still being transcoded or segmented in the
  // background, poll so the row flips from "Converting…"/"Analyzing…" to its
  // rally count once the draft timeline is ready.
  useEffect(() => {
    const stillWorking = sessions.some((session) =>
      session.recordings.some((recording) => isProcessing(recording))
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

  // Re-walk every previously scanned folder for recordings added since, without
  // re-picking the folder. New recordings flow through the same import pipeline.
  async function handleRefresh() {
    setError(null)
    setRefreshing(true)
    try {
      await trackedInvoke<ScanResult>("rescan_folders")
      await refresh()
    } catch (e) {
      setError(String(e))
    } finally {
      setRefreshing(false)
    }
  }

  // Re-run the transcode for every recording (e.g. after a transcode change).
  // The rows flip back to "Converting…" as the worker re-encodes them. Confirmed
  // through the dialog first, since it can take a while across the library.
  async function runRetranscodeAll() {
    setError(null)
    setRetranscodingAll(true)
    try {
      await trackedInvoke("retranscode_all")
      await refresh()
    } catch (e) {
      setError(String(e))
    } finally {
      setRetranscodingAll(false)
    }
  }

  // Re-detect rallies for every recording. Discards every draft timeline,
  // including manual corrections, so it is confirmed through the dialog first.
  async function runReanalyzeAll() {
    setError(null)
    setReanalyzingAll(true)
    try {
      await trackedInvoke("reanalyze_all")
      await refresh()
    } catch (e) {
      setError(String(e))
    } finally {
      setReanalyzingAll(false)
    }
  }

  // Run whichever bulk action the confirmation dialog is open for, then close it.
  function handleConfirm() {
    if (confirmAction === "retranscode") void runRetranscodeAll()
    else if (confirmAction === "reanalyze") void runReanalyzeAll()
    setConfirmAction(null)
  }

  // Per-recording re-transcode: return one recording to the transcode queue.
  async function handleRetranscode(path: string) {
    setError(null)
    try {
      await trackedInvoke("retranscode_recording", { path })
      await refresh()
    } catch (e) {
      setError(String(e))
    }
  }

  // Per-recording re-analyze: re-run rally detection for one recording in place
  // (discards its draft timeline). Mirrors the player's Re-analyze action.
  async function handleReanalyze(path: string) {
    setError(null)
    try {
      await trackedInvoke("reanalyze_recording", { path })
      await refresh()
    } catch (e) {
      setError(String(e))
    }
  }

  const confirmCopy = {
    retranscode: {
      title: "Re-transcode all recordings?",
      description:
        "This re-runs the transcode for every recording across the whole library. It can take a while, but does not change your draft timelines.",
      action: "Re-transcode all",
      destructive: false,
    },
    reanalyze: {
      title: "Re-analyze all recordings?",
      description:
        "This re-detects rallies in every recording and discards every draft timeline — including any manual corrections you have made.",
      action: "Re-analyze all",
      destructive: true,
    },
  } as const
  const copy = confirmAction ? confirmCopy[confirmAction] : null

  return (
    <Card>
      <AlertDialog
        open={confirmAction !== null}
        onOpenChange={(o) => {
          if (!o) setConfirmAction(null)
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{copy?.title}</AlertDialogTitle>
            <AlertDialogDescription>{copy?.description}</AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={handleConfirm}
              className={
                copy?.destructive
                  ? buttonVariants({ variant: "destructive" })
                  : undefined
              }
            >
              {copy?.action}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
      <CardHeader>
        <CardTitle>Sessions</CardTitle>
        <CardDescription>
          Recordings grouped by capture day, referenced in place. Recordings in
          a format the player can&apos;t handle are converted once on import.
        </CardDescription>
        <CardAction className="flex items-center gap-2">
          <Button
            variant="outline"
            onClick={handleRefresh}
            disabled={refreshing}
            title="Re-scan known folders for newly added recordings."
          >
            <RefreshCwIcon
              className={`size-4 ${refreshing ? "animate-spin" : ""}`}
            />
            {refreshing ? "Refreshing…" : "Refresh"}
          </Button>
          <Button onClick={handlePickFolder} disabled={scanning}>
            <FolderOpenIcon className="size-4" />
            {scanning ? "Scanning…" : "Scan folder"}
          </Button>
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button
                variant="outline"
                size="icon"
                disabled={sessions.length === 0}
                title="More library actions"
              >
                <MoreVerticalIcon className="size-4" />
                <span className="sr-only">More library actions</span>
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end" className="w-34">
              <DropdownMenuLabel>All recordings</DropdownMenuLabel>
              <DropdownMenuItem
                onClick={() => setConfirmAction("retranscode")}
                disabled={retranscodingAll}
                className="whitespace-nowrap"
              >
                <FilmIcon className="size-4" />
                Re-transcode all
              </DropdownMenuItem>
              <DropdownMenuItem
                onClick={() => setConfirmAction("reanalyze")}
                disabled={reanalyzingAll}
                className="whitespace-nowrap"
              >
                <RotateCwIcon className="size-4" />
                Re-analyze all
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </CardAction>
      </CardHeader>
      <CardContent className="space-y-4">
        {error ? <p className="text-sm text-destructive">{error}</p> : null}
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
                <div className="flex items-center gap-3">
                  <span className="text-sm text-muted-foreground">
                    {session.recordings.length} recording
                    {session.recordings.length === 1 ? "" : "s"}
                  </span>
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => onPlay(session.recordings, 0)}
                    title="Play the whole session — every rally, back-to-back."
                  >
                    Play session
                  </Button>
                </div>
              </div>
              <ul className="divide-y">
                {session.recordings.map((recording, recordingIndex) => (
                  <li
                    key={recording.id}
                    className="flex items-center hover:bg-accent"
                  >
                    <button
                      type="button"
                      onClick={() => onPlay(session.recordings, recordingIndex)}
                      className="flex min-w-0 flex-1 items-center gap-3 px-4 py-2 text-left text-sm"
                    >
                      <VideoIcon className="size-4 shrink-0 text-muted-foreground" />
                      <span
                        className="truncate font-medium"
                        title={recording.path}
                      >
                        {fileName(recording.path)}
                      </span>
                      {isTranscoding(recording.transcode_state) ? (
                        <span
                          className="ml-auto flex shrink-0 items-center gap-1.5 text-muted-foreground"
                          title="Converting this recording for playback…"
                        >
                          <Loader2Icon className="size-3.5 animate-spin" />
                          Converting…
                        </span>
                      ) : recording.transcode_state === "failed" ? (
                        <span
                          className="ml-auto flex shrink-0 items-center gap-1.5 text-destructive"
                          title="This recording could not be converted for playback."
                        >
                          <AlertTriangleIcon className="size-3.5" />
                          Failed
                        </span>
                      ) : isAnalyzing(recording) ? (
                        <span
                          className="ml-auto flex shrink-0 items-center gap-1.5 text-muted-foreground"
                          title="Detecting rallies in this recording…"
                        >
                          <Loader2Icon className="size-3.5 animate-spin" />
                          Analyzing…
                        </span>
                      ) : (
                        <span className="ml-auto flex shrink-0 items-center gap-3 text-muted-foreground tabular-nums">
                          {recording.segment_state === "ready" ? (
                            <span title="Rallies detected in the draft timeline">
                              {recording.rally_count}{" "}
                              {recording.rally_count === 1
                                ? "rally"
                                : "rallies"}
                            </span>
                          ) : recording.segment_state === "failed" ? (
                            <span
                              className="flex items-center gap-1.5 text-amber-600 dark:text-amber-500"
                              title="Could not analyze this recording's audio for rallies."
                            >
                              <AlertTriangleIcon className="size-3.5" />
                              No timeline
                            </span>
                          ) : null}
                          {formatSize(recording.file_size)}
                        </span>
                      )}
                    </button>
                    <DropdownMenu>
                      <DropdownMenuTrigger asChild>
                        <Button
                          variant="ghost"
                          size="icon-sm"
                          className="mr-2 shrink-0 text-muted-foreground"
                          title="Recording actions"
                        >
                          <MoreVerticalIcon className="size-4" />
                          <span className="sr-only">Recording actions</span>
                        </Button>
                      </DropdownMenuTrigger>
                      <DropdownMenuContent align="end" className="w-32">
                        <DropdownMenuItem
                          onClick={() => handleRetranscode(recording.path)}
                          disabled={isTranscoding(recording.transcode_state)}
                        >
                          <FilmIcon className="size-4" />
                          Re-transcode
                        </DropdownMenuItem>
                        <DropdownMenuItem
                          onClick={() => handleReanalyze(recording.path)}
                          disabled={isProcessing(recording)}
                        >
                          <RotateCwIcon className="size-4" />
                          Re-analyze
                        </DropdownMenuItem>
                      </DropdownMenuContent>
                    </DropdownMenu>
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
