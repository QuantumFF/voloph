"use client"

import {
  ChevronDownIcon,
  ClapperboardIcon,
  DownloadIcon,
  FilterIcon,
  FolderOpenIcon,
  MoreVerticalIcon,
  RefreshCwIcon,
  RotateCwIcon,
  SettingsIcon,
} from "lucide-react"

import { useState } from "react"

import { Button } from "@/components/ui/button"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuItemText,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"

import type { Padding } from "@/components/recording-player-transport"

import type { SegmenterId } from "./library-settings"
import { kindLabel, shortPath } from "./recording-state"
import { SettingsDialog } from "./settings-dialog"
import type { Library } from "./types"

/**
 * The thin top bar (issue #48): app identity, the library switcher, and the
 * library actions — browse moments, refresh, designate libraries, receive a
 * bundle, and re-analyze all. The persistent preferences — rally detection
 * (ADR 0019) and clip padding (ADR 0020) — live in the Settings dialog behind
 * the gear, apart from the one-off and destructive actions in the ⋮ menu.
 */
export function LibraryHeader({
  libraries,
  active,
  library,
  sessions,
  scanning,
  refreshing,
  reanalyzingAll,
  segmenter,
  onSegmenter,
  padding,
  onPadding,
  onBrowse,
  onSwitch,
  onRefresh,
  onDesignateLibrary,
  onReceive,
  onReanalyzeAll,
}: {
  libraries: Library[]
  active: string
  library: string | null
  sessions: unknown[]
  scanning: boolean
  refreshing: boolean
  reanalyzingAll: boolean
  /** Null when the backend names a segmenter the menu doesn't list. */
  segmenter: SegmenterId | null
  onSegmenter: (id: SegmenterId) => void
  padding: Padding
  onPadding: (padding: Padding) => void
  onBrowse: () => void
  onSwitch: (kind: string) => void
  onRefresh: () => void
  onDesignateLibrary: (kind: string, mount: string) => void
  onReceive: () => void
  onReanalyzeAll: () => void
}) {
  const localLib = libraries.find((l) => l.kind === "local")
  const sharedLib = libraries.find((l) => l.kind === "shared")
  const [settingsOpen, setSettingsOpen] = useState(false)
  return (
    <header className="flex h-11 shrink-0 items-center gap-2.5 border-b px-4">
      <ClapperboardIcon className="size-5" />
      <span className="text-sm font-semibold">Voloph</span>
      <span className="hidden text-xs text-muted-foreground lg:inline">
        Every rally, no downtime
      </span>
      <div className="ml-auto flex items-center gap-2">
        {/* Library switcher (ADR 0011): pick the active library when more than
            one is designated. Each button scopes the whole app to its kind. */}
        {libraries.length > 1 ? (
          <div className="flex overflow-hidden rounded-md border">
            {libraries.map((lib) => (
              <button
                key={lib.kind}
                type="button"
                onClick={() => onSwitch(lib.kind)}
                title={`${kindLabel(lib.kind)} library — ${lib.path}`}
                className={`px-2.5 py-1 text-xs font-medium ${
                  lib.kind === active
                    ? "bg-primary text-primary-foreground"
                    : "text-muted-foreground hover:bg-accent"
                }`}
              >
                {kindLabel(lib.kind)}
              </button>
            ))}
          </div>
        ) : null}
        <Button
          variant="outline"
          size="sm"
          onClick={onBrowse}
          disabled={sessions.length === 0}
          title="Filter moments across every session by verdict, aspect, rally length, and flag."
        >
          <FilterIcon className="size-4" />
          Browse moments
        </Button>
        <Button
          variant="outline"
          size="sm"
          onClick={onRefresh}
          disabled={refreshing || library === null}
          title={
            refreshing
              ? "Re-scanning the library…"
              : "Re-scan the active library for newly added recordings."
          }
        >
          <RefreshCwIcon
            className={`size-4 ${refreshing ? "animate-spin" : ""}`}
          />
          {/* The spinning icon carries the busy state, so the label (and the
              button's width) never changes mid-click. */}
          <span aria-live="polite">Refresh</span>
        </Button>
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button variant="outline" size="sm" disabled={scanning}>
              <FolderOpenIcon className="size-4" />
              {scanning ? "Scanning…" : "Libraries"}
              <ChevronDownIcon className="size-3.5 text-muted-foreground" />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="w-72">
            <DropdownMenuLabel>Local library</DropdownMenuLabel>
            {/* A designated library's path is one truncated line (home as ~,
                deep paths elided to their last folders); hovering the item
                shows it whole. */}
            <DropdownMenuItem
              onClick={() => onDesignateLibrary("local", "local")}
              title={localLib?.path}
            >
              <FolderOpenIcon className="size-4" />
              <DropdownMenuItemText
                description={
                  localLib
                    ? shortPath(localLib.path)
                    : "Private to this computer. Pick the folder that holds your recordings."
                }
                className={
                  localLib
                    ? "[&_[data-slot=dropdown-menu-item-description]]:truncate"
                    : undefined
                }
              >
                {localLib ? "Change local library…" : "Choose local library…"}
              </DropdownMenuItemText>
            </DropdownMenuItem>
            <DropdownMenuSeparator />
            <DropdownMenuLabel>
              Shared library
              {sharedLib ? (
                <span
                  className="mt-0.5 block truncate text-[11px] text-muted-foreground/80"
                  title={sharedLib.path}
                >
                  {shortPath(sharedLib.path)} ·{" "}
                  {sharedLib.mount === "network"
                    ? "network mount"
                    : "local mount"}
                </span>
              ) : null}
            </DropdownMenuLabel>
            {/* The user declares whether this device reaches the shared mount as
                a local disk or over the network — an explicit choice, never
                filesystem detection (ADR 0011). */}
            <DropdownMenuItem
              onClick={() => onDesignateLibrary("shared", "local")}
            >
              <FolderOpenIcon className="size-4" />
              <DropdownMenuItemText description="A disk or synced folder on this computer">
                {sharedLib ? "Change shared" : "Designate shared"} (local
                mount)…
              </DropdownMenuItemText>
            </DropdownMenuItem>
            <DropdownMenuItem
              onClick={() => onDesignateLibrary("shared", "network")}
            >
              <FolderOpenIcon className="size-4" />
              <DropdownMenuItemText description="A NAS or network share">
                {sharedLib ? "Change shared" : "Designate shared"} (network
                mount)…
              </DropdownMenuItemText>
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
        <Button
          variant="outline"
          size="icon-sm"
          onClick={() => setSettingsOpen(true)}
          title="Settings — rally detection and clip padding"
        >
          <SettingsIcon className="size-4" />
          <span className="sr-only">Settings</span>
        </Button>
        <SettingsDialog
          open={settingsOpen}
          onOpenChange={setSettingsOpen}
          active={active}
          segmenter={segmenter}
          onSegmenter={onSegmenter}
          padding={padding}
          onPadding={onPadding}
        />
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button
              variant="outline"
              size="icon-sm"
              title="More library actions"
            >
              <MoreVerticalIcon className="size-4" />
              <span className="sr-only">More library actions</span>
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="w-72">
            <DropdownMenuLabel>Session bundle</DropdownMenuLabel>
            {/* Receive a shared review (ADR 0012): only meaningful against the
                shared library, where bundles live and resolve. */}
            <DropdownMenuItem
              onClick={onReceive}
              disabled={active !== "shared"}
            >
              <DownloadIcon className="size-4" />
              <DropdownMenuItemText
                description={
                  active === "shared"
                    ? "Apply a review someone shared with you"
                    : sharedLib
                      ? "Switch to the shared library to receive bundles"
                      : "Needs a shared library — designate one under Libraries"
                }
              >
                Receive bundle…
              </DropdownMenuItemText>
            </DropdownMenuItem>
            <DropdownMenuSeparator />
            <DropdownMenuLabel>All recordings</DropdownMenuLabel>
            {/* Destructive (it discards hand corrections and flags), so it
                reads as such here and still goes through the confirm dialog.
                Same wording as the per-recording Re-analyze. */}
            <DropdownMenuItem
              variant="destructive"
              onClick={onReanalyzeAll}
              disabled={reanalyzingAll}
            >
              <RotateCwIcon
                className={`size-4 ${reanalyzingAll ? "animate-spin" : ""}`}
              />
              <DropdownMenuItemText
                description={
                  reanalyzingAll
                    ? "Already re-analyzing — wait for it to finish"
                    : "Re-detects every recording's rallies. Keeps verdicts; discards your timeline corrections and flags."
                }
              >
                {reanalyzingAll ? "Re-analyzing…" : "Re-analyze all…"}
              </DropdownMenuItemText>
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>
    </header>
  )
}
