"use client"

import {
  ClapperboardIcon,
  DownloadIcon,
  FilterIcon,
  FolderOpenIcon,
  MoreVerticalIcon,
  RefreshCwIcon,
  RotateCwIcon,
} from "lucide-react"

import { Button } from "@/components/ui/button"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"

import { kindLabel } from "./recording-state"
import type { Library } from "./types"

/**
 * The thin top bar (issue #48): app identity, the library switcher, and the
 * library actions — browse moments, refresh, designate libraries, receive a
 * bundle, and re-analyze all.
 */
export function LibraryHeader({
  libraries,
  active,
  library,
  sessions,
  scanning,
  refreshing,
  reanalyzingAll,
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
  onBrowse: () => void
  onSwitch: (kind: string) => void
  onRefresh: () => void
  onDesignateLibrary: (kind: string, mount: string) => void
  onReceive: () => void
  onReanalyzeAll: () => void
}) {
  return (
    <header className="flex h-11 shrink-0 items-center gap-2.5 border-b px-4">
      <ClapperboardIcon className="size-5" />
      <span className="text-sm font-semibold">Voloph</span>
      <span className="text-xs text-muted-foreground">
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
          title="Re-scan the active library for newly added recordings."
        >
          <RefreshCwIcon
            className={`size-4 ${refreshing ? "animate-spin" : ""}`}
          />
          {/* Reserve the wider label's width so swapping the text on click
              doesn't resize the button and reflow the row. */}
          <span className="grid text-center">
            <span className="invisible col-start-1 row-start-1">
              Refreshing…
            </span>
            <span className="col-start-1 row-start-1">
              {refreshing ? "Refreshing…" : "Refresh"}
            </span>
          </span>
        </Button>
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button size="sm" disabled={scanning}>
              <FolderOpenIcon className="size-4" />
              {scanning ? "Scanning…" : "Libraries"}
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="w-64">
            <DropdownMenuLabel>Local library</DropdownMenuLabel>
            <DropdownMenuItem
              onClick={() => onDesignateLibrary("local", "local")}
            >
              <FolderOpenIcon className="size-4" />
              {libraries.some((l) => l.kind === "local")
                ? "Change local library"
                : "Designate local library"}
            </DropdownMenuItem>
            <DropdownMenuLabel>Shared library</DropdownMenuLabel>
            {/* The user declares whether this device reaches the shared mount as
                a local disk or over the network — an explicit choice, never
                filesystem detection (ADR 0011). */}
            <DropdownMenuItem
              onClick={() => onDesignateLibrary("shared", "local")}
            >
              <FolderOpenIcon className="size-4" />
              Designate shared (local mount)
            </DropdownMenuItem>
            <DropdownMenuItem
              onClick={() => onDesignateLibrary("shared", "network")}
            >
              <FolderOpenIcon className="size-4" />
              Designate shared (network mount)
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
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
          <DropdownMenuContent align="end" className="w-44">
            <DropdownMenuLabel>Session bundle</DropdownMenuLabel>
            {/* Receive a shared review (ADR 0012): only meaningful against the
                shared library, where bundles live and resolve. */}
            <DropdownMenuItem
              onClick={onReceive}
              disabled={active !== "shared"}
              className="whitespace-nowrap"
            >
              <DownloadIcon className="size-4" />
              Receive bundle…
            </DropdownMenuItem>
            <DropdownMenuLabel>All recordings</DropdownMenuLabel>
            <DropdownMenuItem
              onClick={onReanalyzeAll}
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
  )
}
