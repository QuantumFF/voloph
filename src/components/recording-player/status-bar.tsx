"use client"

import {
  CrosshairIcon,
  KeyboardIcon,
  Loader2Icon,
  RotateCwIcon,
  ZoomInIcon,
  ZoomOutIcon,
} from "lucide-react"

import { Button } from "@/components/ui/button"
import {
  SESSION_PX_PER_SEC_DEFAULT,
  SESSION_PX_PER_SEC_MAX,
  SESSION_PX_PER_SEC_MIN,
  SESSION_ZOOM_FACTOR,
} from "./constants"

/**
 * The status bar footer: how segmentation stands across the session, the
 * recording counter, and the timeline zoom / follow / re-analyze / cheat-sheet
 * controls that live outside the timeline strip but drive it.
 */
export function StatusBar({
  totalMs,
  segmentingNow,
  failedCount,
  sessionRallyCount,
  uncertainCount,
  unprocessed,
  recordingCount,
  recordingIndex,
  pxPerSec,
  following,
  canJumpToPlayhead,
  reanalyzing,
  onGoToUncertain,
  onZoom,
  onResetZoom,
  onJumpToPlayhead,
  onReanalyze,
  onShowCheatSheet,
}: {
  totalMs: number
  segmentingNow: boolean
  failedCount: number
  sessionRallyCount: number
  uncertainCount: number
  unprocessed: number
  recordingCount: number
  recordingIndex: number
  pxPerSec: number
  following: boolean
  canJumpToPlayhead: boolean
  reanalyzing: boolean
  onGoToUncertain: () => void
  onZoom: (factor: number) => void
  onResetZoom: () => void
  onJumpToPlayhead: () => void
  onReanalyze: () => void
  onShowCheatSheet: () => void
}) {
  return (
    <footer className="flex h-9 shrink-0 items-center gap-3 border-t px-4 text-xs text-muted-foreground">
      {totalMs === 0 && segmentingNow ? (
        <span className="flex items-center gap-1.5">
          <Loader2Icon className="size-3.5 animate-spin" />
          Detecting rallies…
        </span>
      ) : sessionRallyCount === 0 && failedCount > 0 ? (
        <span>Couldn&apos;t detect rallies for this session.</span>
      ) : sessionRallyCount === 0 ? (
        <span>No rallies detected.</span>
      ) : (
        <span className="tabular-nums">
          {sessionRallyCount} {sessionRallyCount === 1 ? "rally" : "rallies"}{" "}
          across the session
          {uncertainCount > 0 ? (
            <>
              {" · "}
              <button
                type="button"
                onClick={onGoToUncertain}
                className="text-amber-600 hover:underline dark:text-amber-500"
                title="Low-confidence spans the segmenter is unsure about — click to jump to the next one (U)."
              >
                {uncertainCount} uncertain
              </button>
            </>
          ) : null}
        </span>
      )}
      {unprocessed > 0 ? (
        <span className="flex items-center gap-1.5">
          <Loader2Icon className="size-3.5 animate-spin" />
          {unprocessed} more {unprocessed === 1 ? "recording" : "recordings"}{" "}
          preparing
        </span>
      ) : null}
      {recordingCount > 1 ? (
        <span className="tabular-nums">
          Recording {recordingIndex + 1} of {recordingCount}
        </span>
      ) : null}
      <div className="ml-auto flex items-center gap-0.5">
        <Button
          variant="ghost"
          size="icon-sm"
          onClick={() => onZoom(1 / SESSION_ZOOM_FACTOR)}
          disabled={pxPerSec <= SESSION_PX_PER_SEC_MIN}
          title="Zoom the timeline out — fit more of the session on screen."
        >
          <ZoomOutIcon className="size-3.5" />
        </Button>
        <Button
          variant="ghost"
          size="icon-sm"
          onClick={() => onZoom(SESSION_ZOOM_FACTOR)}
          disabled={pxPerSec >= SESSION_PX_PER_SEC_MAX}
          title="Zoom the timeline in — see finer detail around the playhead."
        >
          <ZoomInIcon className="size-3.5" />
        </Button>
        <Button
          variant="ghost"
          size="sm"
          className="text-xs"
          onClick={onResetZoom}
          disabled={pxPerSec === SESSION_PX_PER_SEC_DEFAULT}
          title="Reset the timeline zoom."
        >
          Reset
        </Button>
        <Button
          variant={following ? "ghost" : "outline"}
          size="sm"
          className="text-xs"
          onClick={onJumpToPlayhead}
          disabled={!canJumpToPlayhead}
          title="Scroll the timeline back to the playhead and follow it again (F)."
        >
          <CrosshairIcon className="size-3.5" />
          Playhead
        </Button>
        <Button
          variant="ghost"
          size="sm"
          className="text-xs"
          onClick={onReanalyze}
          disabled={segmentingNow}
          title="Re-run rally detection for the current recording in place (for tuning the segmenter)."
        >
          <RotateCwIcon
            className={`size-3.5 ${reanalyzing ? "animate-spin" : ""}`}
          />
          Re-analyze
        </Button>
        <Button
          variant="ghost"
          size="icon-sm"
          onClick={onShowCheatSheet}
          title="Keyboard shortcuts (?)"
        >
          <KeyboardIcon className="size-3.5" />
        </Button>
      </div>
    </footer>
  )
}
