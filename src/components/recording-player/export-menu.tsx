"use client"

import { DownloadIcon, Loader2Icon } from "lucide-react"

import { Button } from "@/components/ui/button"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"

/**
 * The player's Export dropdown: the condensed recording (#12), the condensed
 * *session* (#13), and targeted reels (#14 — flagged rallies, rallies with
 * mistakes) all point the one Export engine at a different rally selection.
 */
export function ExportMenu({
  progress,
  error,
  disabled,
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
        <DropdownMenuContent align="end">
          <DropdownMenuLabel>Export</DropdownMenuLabel>
          <DropdownMenuItem onSelect={onExportSession}>
            Condensed session (gaps removed)
          </DropdownMenuItem>
          <DropdownMenuItem onSelect={onExportCondensed}>
            Condensed recording (gaps removed)
          </DropdownMenuItem>
          <DropdownMenuItem onSelect={onExportFlagged}>
            Flagged rallies
          </DropdownMenuItem>
          <DropdownMenuItem onSelect={onExportMistakes}>
            Rallies with mistakes
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </>
  )
}
