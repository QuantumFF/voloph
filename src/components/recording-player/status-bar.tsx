"use client"

import type { MouseEvent } from "react"
import {
  CheckIcon,
  EllipsisIcon,
  KeyboardIcon,
  Loader2Icon,
  PencilIcon,
  RepeatIcon,
  SkipForwardIcon,
  UnfoldHorizontalIcon,
} from "lucide-react"

import { Button } from "@/components/ui/button"
import { cn } from "@/lib/utils"
import { UNCERTAIN_TEXT } from "./constants"
import { Kbd } from "./kbd"
import { RecordingMenu } from "./recording-menu"

/**
 * Keep keyboard focus on the player when a footer button is clicked: a mouse
 * click shouldn't leave the button focused (looking pressed, and catching the
 * next Space) — the keymap acts on the player, not on the last button used.
 * Keyboard focus (Tab) still works, with its focus-visible ring.
 */
const keepFocus = (e: MouseEvent) => e.preventDefault()

/**
 * The status bar footer: how segmentation stands across the session, how far
 * the review has got ("5 of 18 marked", a thin bar, and the jump to the next
 * unmarked rally, N), the recording counter, the modes that are off their
 * defaults (correction mode, playing through gaps, looping), and the
 * cheat-sheet. The timeline's zoom and follow controls live in the strip's own
 * nav row, right above it.
 *
 * The ⋯ menu holds the open recording's actions (`RecordingMenu`) — the same
 * menu the header's file name opens.
 */
export function StatusBar({
  totalMs,
  segmentingNow,
  failedCount,
  sessionRallyCount,
  uncertainCount,
  markedCount,
  unprocessed,
  recordingCount,
  recordingIndex,
  recordingPath,
  editing,
  editingRally,
  reanalyzing,
  canReanalyze,
  gapSkipEnabled,
  loopingRally,
  onGoToUncertain,
  onNextUnmarked,
  onFinishEditing,
  onReanalyze,
  onShowCheatSheet,
}: {
  totalMs: number
  segmentingNow: boolean
  failedCount: number
  sessionRallyCount: number
  uncertainCount: number
  /** Rallies owning at least one verdict (`markedRallyCount`). */
  markedCount: number
  unprocessed: number
  recordingCount: number
  recordingIndex: number
  /** The open recording's file, or null before one is loaded. */
  recordingPath: string | null
  /** The timeline is in correction mode. */
  editing: boolean
  /** The rally selected for editing (1-based), or null. */
  editingRally: number | null
  reanalyzing: boolean
  /** There's a recording loaded and nothing is being analyzed right now. */
  canReanalyze: boolean
  gapSkipEnabled: boolean
  /** The rally looping (1-based) while Loop is on, else null. */
  loopingRally: number | null
  onGoToUncertain: () => void
  /** Jump to the next rally with no verdict yet (same as N). */
  onNextUnmarked: () => void
  /** Leave correction mode (same as E). */
  onFinishEditing: () => void
  onReanalyze: () => void
  onShowCheatSheet: () => void
}) {
  const allMarked = sessionRallyCount > 0 && markedCount >= sessionRallyCount
  return (
    <footer className="flex h-10 shrink-0 items-center gap-3 border-t px-4 text-[13px] text-muted-foreground">
      <div className="flex min-w-0 items-center gap-3 overflow-hidden whitespace-nowrap">
        {/* Correction mode is a mode: say so where modes are reported, with
            the way out. */}
        {editing ? (
          <button
            type="button"
            onMouseDown={keepFocus}
            onClick={onFinishEditing}
            className="flex h-7 shrink-0 items-center gap-1.5 rounded-md bg-primary px-2 font-medium text-primary-foreground outline-none hover:bg-primary/80 focus-visible:ring-2 focus-visible:ring-ring"
            title="Correction mode is on — click (or press E) to finish."
          >
            <PencilIcon className="size-3.5" />
            Editing timeline
            {editingRally !== null ? <span>· Rally {editingRally}</span> : null}
            <span className="font-normal opacity-80">· E to finish</span>
          </button>
        ) : null}
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
          <>
            <span className="flex items-center tabular-nums">
              {sessionRallyCount}{" "}
              {sessionRallyCount === 1 ? "rally" : "rallies"}
              {uncertainCount > 0 ? (
                <>
                  <span className="px-1">·</span>
                  <button
                    type="button"
                    onMouseDown={keepFocus}
                    onClick={onGoToUncertain}
                    className={cn(
                      "-mx-1 h-8 rounded-md px-1 outline-none hover:underline focus-visible:ring-1 focus-visible:ring-ring",
                      UNCERTAIN_TEXT
                    )}
                    title="Low-confidence spans the segmenter is unsure about — click to jump to the next one (U)."
                  >
                    {uncertainCount} uncertain
                  </button>
                </>
              ) : null}
            </span>
            {/* Review progress: the rallies holding a verdict, as the
                session list counts them. */}
            <span
              className="flex shrink-0 items-center gap-2 tabular-nums"
              title={`${markedCount} of ${sessionRallyCount} rallies hold a verdict.`}
            >
              {allMarked ? (
                <CheckIcon className="size-3.5 text-emerald-600 dark:text-emerald-400" />
              ) : null}
              <span className={cn(allMarked && "text-foreground")}>
                {markedCount}/{sessionRallyCount} marked
              </span>
              <span
                role="progressbar"
                aria-label="Rallies marked"
                aria-valuemin={0}
                aria-valuemax={sessionRallyCount}
                aria-valuenow={markedCount}
                className="h-1 w-16 overflow-hidden rounded-full bg-foreground/15"
              >
                <span
                  className="block h-full rounded-full bg-foreground/70"
                  style={{
                    width: `${(100 * markedCount) / sessionRallyCount}%`,
                  }}
                />
              </span>
            </span>
            {!allMarked ? (
              <button
                type="button"
                onMouseDown={keepFocus}
                onClick={onNextUnmarked}
                className="flex h-7 shrink-0 items-center gap-1.5 rounded-md px-1.5 text-foreground/85 outline-none hover:bg-muted hover:text-foreground focus-visible:ring-1 focus-visible:ring-ring"
                title="Jump to the next rally with no verdict yet (N)."
              >
                <SkipForwardIcon className="size-3.5" />
                Next unmarked
                <Kbd>N</Kbd>
              </button>
            ) : null}
          </>
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
        {/* Playback modes, shown only while off their defaults. */}
        {!gapSkipEnabled ? (
          <span
            className="flex items-center gap-1 text-foreground"
            title="Gap-skipping is off: playback runs through the gaps between rallies (G)."
          >
            <UnfoldHorizontalIcon className="size-3.5" />
            Playing through gaps
          </span>
        ) : null}
        {loopingRally !== null ? (
          <span
            className="flex items-center gap-1 text-foreground tabular-nums"
            title={`Rally ${loopingRally} replays until you stop looping (L).`}
          >
            <RepeatIcon className="size-3.5" />
            Looping rally {loopingRally}
          </span>
        ) : null}
      </div>
      <div className="ml-auto flex shrink-0 items-center gap-1">
        <RecordingMenu
          side="top"
          align="end"
          recordingPath={recordingPath}
          recordingIndex={recordingIndex}
          recordingCount={recordingCount}
          canReanalyze={canReanalyze}
          onReanalyze={onReanalyze}
          trigger={
            <Button
              variant="ghost"
              size="icon"
              className="size-8 text-foreground/80"
              onMouseDown={keepFocus}
              aria-label="More recording actions"
              title="More recording actions"
            >
              {reanalyzing ? (
                <Loader2Icon className="size-4 animate-spin" />
              ) : (
                <EllipsisIcon className="size-4" />
              )}
            </Button>
          }
        />
        <Button
          variant="ghost"
          size="icon"
          className="size-8 text-foreground/80"
          onMouseDown={keepFocus}
          onClick={onShowCheatSheet}
          aria-label="Keyboard shortcuts"
          title="Keyboard shortcuts (?)"
        >
          <KeyboardIcon className="size-4" />
        </Button>
      </div>
    </footer>
  )
}
