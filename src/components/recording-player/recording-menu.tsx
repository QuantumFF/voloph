"use client"

import { useEffect, useState, type ReactNode } from "react"
import { revealItemInDir } from "@tauri-apps/plugin-opener"
import { CheckIcon, CopyIcon, FolderOpenIcon, RotateCwIcon } from "lucide-react"

import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuItemText,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import { fileName } from "@/lib/format"
import { REANALYZE_DESCRIPTION, REANALYZE_LABEL } from "@/lib/reanalyze-copy"

/** How long a menu action's in-place confirmation ("Copied") shows. */
const ACK_MS = 1500

/** The outcome of a recording action, shown in place in the menu. */
type Ack = { item: "copy" | "reveal"; ok: boolean } | null

/**
 * The open recording's actions, headed by its name: the everyday ones — show
 * it in the file manager, copy its path — and, last, below a separator,
 * Re-analyze, in the destructive colour and the session list's exact words
 * (`lib/reanalyze-copy`): it throws away this recording's rallies — hand
 * corrections and flags included — so it is never a stray click away, and
 * always confirms. One menu, offered from two places: the header's file name
 * and the status bar's ⋯ (`trigger`, `side`, `align` say where it opens).
 */
export function RecordingMenu({
  trigger,
  side,
  align,
  recordingPath,
  recordingIndex,
  recordingCount,
  canReanalyze,
  onReanalyze,
  onOpenChange,
}: {
  /** The button that opens it (rendered `asChild`). */
  trigger: ReactNode
  side: "top" | "bottom"
  align: "start" | "end"
  /** The open recording's file, or null before one is loaded. */
  recordingPath: string | null
  recordingIndex: number
  recordingCount: number
  /** There's a recording loaded and nothing is being analyzed right now. */
  canReanalyze: boolean
  onReanalyze: () => void
  /** Told when the menu opens or closes. */
  onOpenChange?: (open: boolean) => void
}) {
  // The menu stays open while an action confirms itself in place.
  const [menuOpen, setMenuOpenState] = useState(false)
  const [ack, setAck] = useState<Ack>(null)
  const setMenuOpen = (open: boolean) => {
    setMenuOpenState(open)
    onOpenChange?.(open)
  }
  useEffect(() => {
    if (!ack) return
    const timer = setTimeout(() => {
      setAck(null)
      if (ack.ok) {
        setMenuOpenState(false)
        onOpenChange?.(false)
      }
    }, ACK_MS)
    return () => clearTimeout(timer)
  }, [ack, onOpenChange])
  const copyPath = (e: Event) => {
    e.preventDefault()
    if (!recordingPath) return
    navigator.clipboard
      .writeText(recordingPath)
      .then(() => setAck({ item: "copy", ok: true }))
      .catch(() => setAck({ item: "copy", ok: false }))
  }
  const revealFile = (e: Event) => {
    e.preventDefault()
    if (!recordingPath) return
    revealItemInDir(recordingPath)
      .then(() => {
        setAck(null)
        setMenuOpen(false)
      })
      .catch(() => setAck({ item: "reveal", ok: false }))
  }
  return (
    <DropdownMenu
      modal={false}
      open={menuOpen}
      onOpenChange={(open) => {
        setMenuOpen(open)
        if (!open) setAck(null)
      }}
    >
      <DropdownMenuTrigger asChild>{trigger}</DropdownMenuTrigger>
      <DropdownMenuContent
        side={side}
        align={align}
        className="w-72"
        onCloseAutoFocus={(e) => e.preventDefault()}
      >
        <DropdownMenuLabel
          className="truncate"
          title={recordingPath ?? undefined}
        >
          {recordingPath ? fileName(recordingPath) : "No recording open"}
          {recordingCount > 1 ? (
            <span className="font-normal text-muted-foreground tabular-nums">
              {" "}
              · {recordingIndex + 1} of {recordingCount}
            </span>
          ) : null}
        </DropdownMenuLabel>
        <DropdownMenuItem disabled={!recordingPath} onSelect={revealFile}>
          <FolderOpenIcon className="size-4" />
          <DropdownMenuItemText
            description={
              ack?.item === "reveal" && !ack.ok
                ? "Couldn't open the file manager."
                : undefined
            }
          >
            Show in file manager
          </DropdownMenuItemText>
        </DropdownMenuItem>
        <DropdownMenuItem disabled={!recordingPath} onSelect={copyPath}>
          {ack?.item === "copy" && ack.ok ? (
            <CheckIcon className="size-4" />
          ) : (
            <CopyIcon className="size-4" />
          )}
          <DropdownMenuItemText
            description={
              ack?.item === "copy" && !ack.ok
                ? "Couldn't reach the clipboard."
                : undefined
            }
          >
            {ack?.item === "copy" && ack.ok ? "Copied" : "Copy file path"}
          </DropdownMenuItemText>
        </DropdownMenuItem>
        <DropdownMenuSeparator />
        <DropdownMenuItem
          variant="destructive"
          disabled={!canReanalyze}
          onSelect={onReanalyze}
        >
          <RotateCwIcon className="size-4" />
          <DropdownMenuItemText
            description={
              !canReanalyze
                ? "Wait for the current analysis to finish"
                : REANALYZE_DESCRIPTION
            }
          >
            {REANALYZE_LABEL}
          </DropdownMenuItemText>
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  )
}
