"use client"

import {
  AlertTriangleIcon,
  FolderOpenIcon,
  ImportIcon,
  Loader2Icon,
  MoreVerticalIcon,
  PlayIcon,
  RotateCwIcon,
  Share2Icon,
  UsersIcon,
  VideoIcon,
  XIcon,
} from "lucide-react"

import { Button } from "@/components/ui/button"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import { fileName, formatSize } from "@/lib/format"
import { formatCaptureDay } from "@/lib/utils"

import {
  isAnalyzing,
  isPreparing,
  isProcessing,
  sessionSummary,
} from "./recording-state"
import type { CarryOffer, Recording, Session } from "./types"

/** One recording row: name, background/status, its carry-over offer, and actions. */
function RecordingRow({
  session,
  recording,
  recordingIndex,
  carryOffer,
  onPlay,
  onCarry,
  onDismissCarry,
  onReanalyze,
}: {
  session: Session
  recording: Recording
  recordingIndex: number
  carryOffer: CarryOffer | undefined
  onPlay: (
    recordings: { path: string }[],
    startIndex: number,
    day: string
  ) => void
  onCarry: (offer: CarryOffer) => void
  onDismissCarry: (offer: CarryOffer) => void
  onReanalyze: (path: string) => void
}) {
  return (
    <li className="flex items-center hover:bg-accent">
      <button
        type="button"
        onClick={() =>
          onPlay(session.recordings, recordingIndex, session.capture_day)
        }
        className="flex min-w-0 flex-1 items-center gap-3 px-4 py-2 text-left text-sm"
      >
        <VideoIcon className="size-4 shrink-0 text-muted-foreground" />
        <span className="truncate font-medium" title={recording.path}>
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
                {recording.rally_count === 1 ? "rally" : "rallies"}
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
      {/* Carry-over (ADR 0011): this copy is byte-identical to one already
          reviewed in the other library, and it is un-touched here — offer to
          bring that review (timeline, flags, annotations, segments) over.
          Dismiss hides it for good. */}
      {carryOffer ? (
        <div className="flex shrink-0 items-center gap-1">
          <Button
            size="sm"
            variant="outline"
            className="h-7 border-sky-500/50 text-sky-700 hover:bg-sky-500/10 hover:text-sky-700 dark:text-sky-400 dark:hover:text-sky-400"
            onClick={() => onCarry(carryOffer)}
            title="Bring the review from your other-library copy — timeline, flags, annotations, and segments — onto this copy."
          >
            <ImportIcon className="size-3.5" />
            Carry review
          </Button>
          <Button
            variant="ghost"
            size="icon-sm"
            className="size-7 shrink-0 text-muted-foreground"
            onClick={() => onDismissCarry(carryOffer)}
            title="Dismiss this carry-over offer"
          >
            <XIcon className="size-3.5" />
            <span className="sr-only">Dismiss carry-over offer</span>
          </Button>
        </div>
      ) : null}
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
            onClick={() => onReanalyze(recording.path)}
            disabled={isProcessing(recording)}
          >
            <RotateCwIcon className="size-4" />
            Re-analyze
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </li>
  )
}

/**
 * One session block: its date and stats, a Review button that opens the whole
 * session in the workstation, the shared-reviews and share menus, and the
 * recordings it holds as dense rows.
 */
export function SessionBlock({
  session,
  active,
  carryByPath,
  onPlay,
  onBrowseBundles,
  onShare,
  onCarry,
  onDismissCarry,
  onReanalyze,
}: {
  session: Session
  active: string
  carryByPath: Map<string, CarryOffer>
  onPlay: (
    recordings: { path: string }[],
    startIndex: number,
    day: string
  ) => void
  onBrowseBundles: (day: string) => void
  onShare: (session: Session, saveAs: boolean) => void
  onCarry: (offer: CarryOffer) => void
  onDismissCarry: (offer: CarryOffer) => void
  onReanalyze: (path: string) => void
}) {
  return (
    <div className="rounded-xl border">
      <div className="flex items-center gap-4 border-b px-4 py-3">
        <div className="min-w-0">
          <h3 className="font-medium" title={session.capture_day}>
            {formatCaptureDay(session.capture_day)}
          </h3>
          <p className="text-sm text-muted-foreground tabular-nums">
            {sessionSummary(session)}
          </p>
        </div>
        <Button
          size="sm"
          className="ml-auto shrink-0"
          onClick={() => onPlay(session.recordings, 0, session.capture_day)}
          title="Review the whole session — every rally, back-to-back."
        >
          <PlayIcon className="size-4" />
          Review session
        </Button>
        {/* Shared reviews for this session (issue): browse every bundle shared
            for this day, including ones already received or dismissed, and
            re-receive any. Only the shared library holds bundles, so it is
            disabled elsewhere. */}
        <Button
          variant="outline"
          size="icon-sm"
          className="shrink-0"
          disabled={active !== "shared"}
          onClick={() => onBrowseBundles(session.capture_day)}
          title={
            active === "shared"
              ? "Shared reviews for this session"
              : "Switch to the shared library to see shared reviews."
          }
        >
          <UsersIcon className="size-4" />
          <span className="sr-only">Shared reviews for this session</span>
        </Button>
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button
              variant="outline"
              size="icon-sm"
              className="shrink-0"
              title="Share this session"
            >
              <Share2Icon className="size-4" />
              <span className="sr-only">Share this session</span>
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="w-56">
            <DropdownMenuLabel>Session bundle</DropdownMenuLabel>
            {/* Sharing into the shared library is only meaningful there
                — recipients cannot reach local files (ADR 0012). */}
            <DropdownMenuItem
              onClick={() => onShare(session, false)}
              disabled={active !== "shared"}
              title={
                active === "shared"
                  ? undefined
                  : "Switch to the shared library to share here."
              }
            >
              <Share2Icon className="size-4" />
              Share to shared library
            </DropdownMenuItem>
            <DropdownMenuItem onClick={() => onShare(session, true)}>
              <FolderOpenIcon className="size-4" />
              Save bundle as…
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>
      <ul className="divide-y">
        {session.recordings.map((recording, recordingIndex) => (
          <RecordingRow
            key={recording.id}
            session={session}
            recording={recording}
            recordingIndex={recordingIndex}
            carryOffer={carryByPath.get(recording.path)}
            onPlay={onPlay}
            onCarry={onCarry}
            onDismissCarry={onDismissCarry}
            onReanalyze={onReanalyze}
          />
        ))}
      </ul>
    </div>
  )
}
