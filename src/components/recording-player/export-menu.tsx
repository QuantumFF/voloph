"use client"

import type { ReactNode } from "react"
import {
  DownloadIcon,
  FileVideoIcon,
  FlagIcon,
  LayersIcon,
  Loader2Icon,
} from "lucide-react"

import { Button } from "@/components/ui/button"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import type { ExportCounts } from "./use-session-export"
import { VerdictMark } from "./verdict-mark"

/** "1 rally", "12 rallies". */
function ralliesText(count: number): string {
  return `${count} ${count === 1 ? "rally" : "rallies"}`
}

/**
 * One export choice: a one-line title with how many rallies it holds, over a
 * muted line saying what's in it. An empty selection is greyed out and says
 * how to fill it, rather than failing after a click.
 */
function ExportItem({
  icon,
  title,
  count,
  description,
  emptyDescription,
  onSelect,
}: {
  icon: ReactNode
  title: string
  /** Rallies the export would hold. */
  count: number
  description: string
  /** What to say instead of `description` when `count` is 0. */
  emptyDescription: string
  onSelect: () => void
}) {
  const empty = count === 0
  return (
    <DropdownMenuItem
      onSelect={onSelect}
      disabled={empty}
      className="items-start gap-2.5"
    >
      <span className="flex size-5 shrink-0 items-center justify-center text-muted-foreground">
        {icon}
      </span>
      <span className="flex min-w-0 flex-1 flex-col gap-0.5">
        <span className="flex items-baseline gap-2 text-sm leading-5">
          <span className="min-w-0 truncate">{title}</span>
          <span className="ml-auto shrink-0 text-xs text-muted-foreground tabular-nums">
            {ralliesText(count)}
          </span>
        </span>
        <span className="text-xs leading-4 text-muted-foreground">
          {empty ? emptyDescription : description}
        </span>
      </span>
    </DropdownMenuItem>
  )
}

/**
 * The player's Export dropdown: the condensed recording (#12), the condensed
 * *session* (#13), and targeted reels (#14 — flagged rallies, rallies with
 * mistakes) all point the one Export engine at a different rally selection.
 */
export function ExportMenu({
  progress,
  error,
  disabled,
  counts,
  onExportSession,
  onExportCondensed,
  onExportFlagged,
  onExportMistakes,
}: {
  /** Live export fraction in [0, 1], or null when idle. */
  progress: number | null
  /** A failed or empty-selection message, surfaced beside the button. */
  error: string | null
  disabled: boolean
  /** How many rallies each item would export (`useSessionExport`). */
  counts: ExportCounts
  onExportSession: () => void
  onExportCondensed: () => void
  onExportFlagged: () => void
  onExportMistakes: () => void
}) {
  return (
    <>
      {error ? (
        <span className="mr-3 text-sm text-destructive" role="alert">
          {error}
        </span>
      ) : null}
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button
            variant="outline"
            size="sm"
            disabled={disabled}
            title="Render one new video from a selection of rallies."
          >
            {progress != null ? (
              <>
                <Loader2Icon className="size-4 animate-spin" />
                Exporting… {Math.round(progress * 100)}%
              </>
            ) : (
              <>
                <DownloadIcon className="size-4" />
                Export
              </>
            )}
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" className="w-72">
          <DropdownMenuLabel>Condensed — gaps removed</DropdownMenuLabel>
          <ExportItem
            icon={<LayersIcon />}
            title="Condensed session"
            count={counts.session}
            description="Every rally across all recordings"
            emptyDescription="No rallies detected in this session yet"
            onSelect={onExportSession}
          />
          <ExportItem
            icon={<FileVideoIcon />}
            title="Condensed recording"
            count={counts.recording}
            description="Every rally in the open recording"
            emptyDescription="No rallies detected in the open recording"
            onSelect={onExportCondensed}
          />
          <DropdownMenuSeparator />
          <DropdownMenuLabel>Highlight reels</DropdownMenuLabel>
          <ExportItem
            icon={<FlagIcon className="text-sky-500" />}
            title="Flagged rallies"
            count={counts.flagged}
            description="Only the rallies you flagged"
            emptyDescription="Flag a rally (X) to build this reel"
            onSelect={onExportFlagged}
          />
          <ExportItem
            icon={<VerdictMark verdict="mistake" />}
            title="Rallies with mistakes"
            count={counts.mistakes}
            description="Rallies holding a Mistake verdict"
            emptyDescription="Mark a Mistake (3) inside a rally to build this reel"
            onSelect={onExportMistakes}
          />
        </DropdownMenuContent>
      </DropdownMenu>
    </>
  )
}
