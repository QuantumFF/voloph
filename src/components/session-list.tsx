"use client"

import { useCallback, useEffect, useState } from "react"
import { open } from "@tauri-apps/plugin-dialog"
import {
  AlertTriangleIcon,
  ClapperboardIcon,
  FolderOpenIcon,
  Loader2Icon,
  MoreVerticalIcon,
  PlayIcon,
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
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import { fileName, formatDuration, formatSize } from "@/lib/format"
import { trackedInvoke } from "@/lib/tauri"
import { formatCaptureDay } from "@/lib/utils"

interface Recording {
  id: number
  path: string
  file_size: number
  quick_hash: string
  capture_day: string
  /**
   * Playability lifecycle: unknown (not yet probed) | ready | failed. libmpv
   * plays originals directly (ADR 0008), so there is no transcode step.
   */
  probe_state: string
  /** Segmentation lifecycle: unknown | ready | failed (ADR 0002). */
  segment_state: string
  /** Recording duration in ms; null until segmented. */
  duration_ms: number | null
  /** Rallies in the draft timeline (0 until segmented). */
  rally_count: number
}

/** True during the brief window before a recording has been probed for playback. */
function isPreparing(state: string): boolean {
  return state === "unknown"
}

/**
 * True while a recording is playable but its draft timeline is still being
 * produced — audio extraction + segmentation (ADR 0002). Segmentation only
 * starts once the recording is probed (`ready`), so a still-`unknown` segment
 * state on a ready recording means "queued or analyzing".
 */
function isAnalyzing(recording: Recording): boolean {
  return (
    recording.probe_state === "ready" &&
    recording.segment_state === "unknown"
  )
}

/** True while any background media work is still pending for this recording. */
function isProcessing(recording: Recording): boolean {
  return isPreparing(recording.probe_state) || isAnalyzing(recording)
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

/** The stats line under a session's date: recordings, rallies, footage length. */
function sessionSummary(session: Session): string {
  const parts = [
    `${session.recordings.length} recording${session.recordings.length === 1 ? "" : "s"}`,
  ]
  const segmented = session.recordings.filter(
    (r) => r.segment_state === "ready"
  )
  if (segmented.length > 0) {
    const rallies = segmented.reduce((sum, r) => sum + r.rally_count, 0)
    parts.push(`${rallies} ${rallies === 1 ? "rally" : "rallies"}`)
  }
  const durationMs = session.recordings.reduce(
    (sum, r) => sum + (r.duration_ms ?? 0),
    0
  )
  if (durationMs > 0) parts.push(formatDuration(durationMs))
  return parts.join(" · ")
}

interface SessionListProps {
  /**
   * Open a session in the player as one continuous playlist. `recordings` is the
   * session's recordings in capture-time order; `startIndex` is the one to open
   * first (which recording the user clicked); `day` is the session's capture day
   * for the review top bar.
   */
  onPlay: (
    recordings: { path: string }[],
    startIndex: number,
    day: string
  ) => void
}

/**
 * The homepage: the library of sessions in the studio shell (issue #48) — a
 * thin top bar carrying the app identity and the library actions, over a
 * centered column of session blocks. Each block is one session: its date and
 * stats, a Review button that opens the whole session in the workstation, and
 * the recordings it holds as dense rows.
 */
export function SessionList({ onPlay }: SessionListProps) {
  const [sessions, setSessions] = useState<Session[]>([])
  const [scanning, setScanning] = useState(false)
  const [refreshing, setRefreshing] = useState(false)
  const [reanalyzingAll, setReanalyzingAll] = useState(false)
  // Which bulk action is awaiting confirmation in the dialog, if any.
  const [confirmAction, setConfirmAction] = useState<"reanalyze" | null>(null)
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

  // While any recording is still being prepared or segmented in the
  // background, poll so the row flips from "Preparing…"/"Analyzing…" to its
  // rally count once the draft timeline is ready. Keyed on the derived boolean
  // (not `sessions`, a fresh array each poll) so the interval survives across
  // polls instead of being torn down and re-created every tick.
  const stillWorking = sessions.some((session) =>
    session.recordings.some((recording) => isProcessing(recording))
  )
  useEffect(() => {
    if (!stillWorking) return
    const interval = setInterval(() => void refresh(), 3000)
    return () => clearInterval(interval)
  }, [stillWorking, refresh])

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
    if (confirmAction === "reanalyze") void runReanalyzeAll()
    setConfirmAction(null)
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
    <div className="flex h-full flex-col">
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

      <header className="flex h-11 shrink-0 items-center gap-2.5 border-b px-4">
        <ClapperboardIcon className="size-5" />
        <span className="text-sm font-semibold">Voloph</span>
        <span className="text-xs text-muted-foreground">
          Every rally, no downtime
        </span>
        <div className="ml-auto flex items-center gap-2">
          <Button
            variant="outline"
            size="sm"
            onClick={handleRefresh}
            disabled={refreshing}
            title="Re-scan known folders for newly added recordings."
          >
            <RefreshCwIcon
              className={`size-4 ${refreshing ? "animate-spin" : ""}`}
            />
            {refreshing ? "Refreshing…" : "Refresh"}
          </Button>
          <Button size="sm" onClick={handlePickFolder} disabled={scanning}>
            <FolderOpenIcon className="size-4" />
            {scanning ? "Scanning…" : "Scan folder"}
          </Button>
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button
                variant="outline"
                size="icon-sm"
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
                onClick={() => setConfirmAction("reanalyze")}
                disabled={reanalyzingAll}
                className="whitespace-nowrap"
              >
                <RotateCwIcon className="size-4" />
                Re-analyze all
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      </header>

      <div className="min-h-0 flex-1 overflow-y-auto">
        <div className="mx-auto max-w-4xl space-y-4 px-4 py-6">
          {error ? <p className="text-sm text-destructive">{error}</p> : null}
          {sessions.length === 0 ? (
            <div className="rounded-xl border border-dashed px-6 py-16 text-center">
              <p className="font-medium">No sessions yet</p>
              <p className="mt-1 text-sm text-muted-foreground">
                Scan a folder of recordings to get started. Originals play in
                place and are never modified.
              </p>
            </div>
          ) : (
            sessions.map((session) => (
              <div key={session.id} className="rounded-xl border">
                <div className="flex items-center gap-4 border-b px-4 py-3">
                  <div className="min-w-0">
                    <h3
                      className="font-medium"
                      title={session.capture_day}
                    >
                      {formatCaptureDay(session.capture_day)}
                    </h3>
                    <p className="text-sm text-muted-foreground tabular-nums">
                      {sessionSummary(session)}
                    </p>
                  </div>
                  <Button
                    size="sm"
                    className="ml-auto shrink-0"
                    onClick={() =>
                      onPlay(session.recordings, 0, session.capture_day)
                    }
                    title="Review the whole session — every rally, back-to-back."
                  >
                    <PlayIcon className="size-4" />
                    Review session
                  </Button>
                </div>
                <ul className="divide-y">
                  {session.recordings.map((recording, recordingIndex) => (
                    <li
                      key={recording.id}
                      className="flex items-center hover:bg-accent"
                    >
                      <button
                        type="button"
                        onClick={() =>
                          onPlay(
                            session.recordings,
                            recordingIndex,
                            session.capture_day
                          )
                        }
                        className="flex min-w-0 flex-1 items-center gap-3 px-4 py-2 text-left text-sm"
                      >
                        <VideoIcon className="size-4 shrink-0 text-muted-foreground" />
                        <span
                          className="truncate font-medium"
                          title={recording.path}
                        >
                          {fileName(recording.path)}
                        </span>
                        {isPreparing(recording.probe_state) ? (
                          <span
                            className="ml-auto flex shrink-0 items-center gap-1.5 text-muted-foreground"
                            title="Preparing this recording for playback…"
                          >
                            <Loader2Icon className="size-3.5 animate-spin" />
                            Preparing…
                          </span>
                        ) : recording.probe_state === "failed" ? (
                          <span
                            className="ml-auto flex shrink-0 items-center gap-1.5 text-destructive"
                            title="This recording could not be read for playback."
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
        </div>
      </div>
    </div>
  )
}
