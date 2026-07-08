"use client"

import { useCallback, useEffect, useState } from "react"
import {
  ArrowLeftIcon,
  DownloadIcon,
  FlagIcon,
  Loader2Icon,
  PlayIcon,
  SearchXIcon,
} from "lucide-react"

import { Button } from "@/components/ui/button"
import { fileName, formatClock, formatDuration } from "@/lib/format"
import { trackedInvoke } from "@/lib/tauri"
import { formatCaptureDay } from "@/lib/utils"
import { useExport } from "@/components/use-export"
import {
  ASPECTS,
  VERDICTS,
  type FilteredRally,
  type Verdict,
} from "@/components/recording-player-transport"
import { LONG_RALLY_MS, VERDICT_DOT } from "@/components/recording-player"

/** One recording in a session, enough to build a jump playlist. */
interface Recording {
  path: string
}
/** A session as `list_sessions` returns it, for resolving a jump target. */
interface Session {
  id: number
  recordings: Recording[]
}

/** The active filter selection; a null field means "any". */
interface Filters {
  verdict: Verdict | null
  aspect: string | null
  /** true = long, false = short, null = any. */
  length: boolean | null
  /** true = flagged only, null = any. */
  flagged: boolean | null
}

const EMPTY_FILTERS: Filters = {
  verdict: null,
  aspect: null,
  length: null,
  flagged: null,
}

/** Where a jump lands: the moment's session recordings, which one, and when. */
export interface JumpTarget {
  recordings: Recording[]
  startIndex: number
  startMs: number
  day: string
}

/**
 * The cross-session browse/filter view (issue #11 — the payoff of the structured
 * data). Filter every rally across all sessions by verdict, aspect, rally length
 * (long/short, derived from duration), and flag; the filters combine with AND.
 * Each result lists the matching rally with enough context to identify it — its
 * session day, recording, timecode, length, flag, and (when a verdict/aspect
 * filter is set) its matching moments — and jumps straight to it in the player.
 */
export function MomentBrowser({
  onBack,
  onJump,
}: {
  onBack: () => void
  onJump: (target: JumpTarget) => void
}) {
  const [filters, setFilters] = useState<Filters>(EMPTY_FILTERS)
  const [results, setResults] = useState<FilteredRally[]>([])
  const [sessions, setSessions] = useState<Session[]>([])
  const [error, setError] = useState<string | null>(null)
  // Aspects actually present in the active library's annotations (issue #66) —
  // includes any a received bundle imported that lie outside the seeded list
  // (ADR 0012). Unioned with ASPECTS below so every real aspect is filterable.
  const [importedAspects, setImportedAspects] = useState<string[]>([])

  // Sessions are the source for building a jump playlist (all of a session's
  // recordings in order). Loaded once; the results carry the rally coordinates.
  useEffect(() => {
    trackedInvoke<Session[]>("list_sessions")
      .then(setSessions)
      .catch((e) => setError(String(e)))
    trackedInvoke<string[]>("aspect_vocabulary")
      .then(setImportedAspects)
      .catch((e) => setError(String(e)))
  }, [])

  // Seeded vocabulary plus any imported aspects, de-duplicated, seeds first.
  const aspectOptions = [
    ...ASPECTS,
    ...importedAspects.filter((a) => !ASPECTS.includes(a as never)),
  ]

  const runFilter = useCallback((f: Filters) => {
    trackedInvoke<FilteredRally[]>("filter_moments", {
      verdict: f.verdict,
      aspect: f.aspect,
      length: f.length,
      flagged: f.flagged,
    })
      .then(setResults)
      .catch((e) => setError(String(e)))
  }, [])

  useEffect(() => {
    runFilter(filters)
  }, [filters, runFilter])

  // Open a result in the player at its rally start. Resolve the moment's session
  // and the recording's index within it (path order matches the backend).
  const jump = useCallback(
    (r: FilteredRally) => {
      const session = sessions.find((s) => s.id === r.session_id)
      if (!session) return
      const startIndex = session.recordings.findIndex(
        (rec) => rec.path === r.recording_path
      )
      if (startIndex < 0) return
      onJump({
        recordings: session.recordings,
        startIndex,
        startMs: r.start_ms,
        day: r.capture_day,
      })
    },
    [sessions, onJump]
  )

  // Export a reel of the current filter (issue #14): every rally the filter
  // matched, across every session, stitched into one MP4 in the order shown.
  // The backend has already resolved verdict/aspect/length/flag to rally ids, so
  // the export just hands the session engine those ids plus each result's
  // recording as a source (distinct paths, in first-seen order). The button is
  // disabled on an empty result set, so this never renders nothing.
  const {
    progress: exportProgress,
    error: exportError,
    runExport,
  } = useExport()

  const exportReel = useCallback(() => {
    if (results.length === 0) return
    const paths = [...new Set(results.map((r) => r.recording_path))]
    const rallyIds = results.map((r) => r.rally_id)
    return runExport("Export filtered reel", "reel", "export_session", {
      paths,
      rallyIds,
    })
  }, [results, runExport])

  const active =
    filters.verdict !== null ||
    filters.aspect !== null ||
    filters.length !== null ||
    filters.flagged !== null

  return (
    <div className="flex h-full flex-col">
      <header className="flex h-11 shrink-0 items-center gap-3 border-b px-4">
        <Button variant="ghost" size="sm" onClick={onBack}>
          <ArrowLeftIcon className="size-4" />
          Sessions
        </Button>
        <span className="font-medium">Browse moments</span>
        <span className="text-sm text-muted-foreground">
          Filter across every session
        </span>
        <div className="ml-auto flex items-center">
          {/* Export the current filter as a reel (issue #14): the same session
              engine the player uses, pointed at the filtered rally selection. */}
          {exportError ? (
            <span className="mr-3 text-sm text-destructive" role="alert">
              {exportError}
            </span>
          ) : null}
          <Button
            variant="outline"
            size="sm"
            disabled={exportProgress != null || results.length === 0}
            onClick={() => void exportReel()}
            title="Render one MP4 of every rally matching the current filter, in order."
          >
            {exportProgress != null ? (
              <>
                <Loader2Icon className="size-4 animate-spin" />
                Exporting… {Math.round(exportProgress * 100)}%
              </>
            ) : (
              <>
                <DownloadIcon className="size-4" />
                Export reel
              </>
            )}
          </Button>
        </div>
      </header>

      <div className="min-h-0 flex-1 overflow-y-auto">
        <div className="mx-auto max-w-4xl space-y-5 px-4 py-6">
          {/* Filter controls: each row is one dimension; a set chip toggles off. */}
          <div className="space-y-3 rounded-xl border p-4">
            <FilterRow label="Verdict">
              {VERDICTS.map((v) => (
                <Chip
                  key={v}
                  on={filters.verdict === v}
                  onClick={() =>
                    setFilters((f) => ({
                      ...f,
                      verdict: f.verdict === v ? null : v,
                    }))
                  }
                >
                  <span className={`size-2 rounded-full ${VERDICT_DOT[v]}`} />
                  {v}
                </Chip>
              ))}
            </FilterRow>
            <FilterRow label="Aspect">
              {aspectOptions.map((a) => (
                <Chip
                  key={a}
                  on={filters.aspect === a}
                  onClick={() =>
                    setFilters((f) => ({
                      ...f,
                      aspect: f.aspect === a ? null : a,
                    }))
                  }
                >
                  {a}
                </Chip>
              ))}
            </FilterRow>
            <FilterRow label="Length">
              <Chip
                on={filters.length === true}
                onClick={() =>
                  setFilters((f) => ({
                    ...f,
                    length: f.length === true ? null : true,
                  }))
                }
              >
                long
              </Chip>
              <Chip
                on={filters.length === false}
                onClick={() =>
                  setFilters((f) => ({
                    ...f,
                    length: f.length === false ? null : false,
                  }))
                }
              >
                short
              </Chip>
              <span className="self-center text-xs text-muted-foreground">
                ≥ {formatDuration(LONG_RALLY_MS)} is long
              </span>
            </FilterRow>
            <FilterRow label="Flag">
              <Chip
                on={filters.flagged === true}
                onClick={() =>
                  setFilters((f) => ({
                    ...f,
                    flagged: f.flagged === true ? null : true,
                  }))
                }
              >
                <FlagIcon className="size-3" />
                flagged
              </Chip>
            </FilterRow>
            {active ? (
              <Button
                variant="ghost"
                size="sm"
                className="text-xs"
                onClick={() => setFilters(EMPTY_FILTERS)}
              >
                Clear filters
              </Button>
            ) : null}
          </div>

          {error ? <p className="text-sm text-destructive">{error}</p> : null}

          <p className="text-sm text-muted-foreground tabular-nums">
            {results.length} {results.length === 1 ? "rally" : "rallies"}
          </p>

          {results.length === 0 ? (
            <div className="flex flex-col items-center gap-2 rounded-xl border border-dashed px-6 py-16 text-center text-muted-foreground">
              <SearchXIcon className="size-6" />
              <p className="font-medium text-foreground">Nothing matches</p>
              <p className="text-sm">
                No rallies match these filters. Loosen them, or annotate more
                during review.
              </p>
            </div>
          ) : (
            <ul className="space-y-2">
              {results.map((r) => (
                <li key={r.rally_id}>
                  <button
                    type="button"
                    onClick={() => jump(r)}
                    className="group flex w-full items-start gap-3 rounded-xl border px-4 py-3 text-left hover:bg-accent"
                    title="Open this rally in the player at its start."
                  >
                    <PlayIcon className="mt-0.5 size-4 shrink-0 text-muted-foreground group-hover:text-foreground" />
                    <div className="min-w-0 flex-1">
                      <div className="flex flex-wrap items-center gap-x-2 gap-y-1 text-sm">
                        <span className="font-medium">
                          {formatCaptureDay(r.capture_day)}
                        </span>
                        <span
                          className="truncate text-muted-foreground"
                          title={r.recording_path}
                        >
                          {fileName(r.recording_path)}
                        </span>
                        <span className="text-muted-foreground tabular-nums">
                          {formatClock(r.start_ms)}–{formatClock(r.end_ms)}
                        </span>
                        <span className="text-muted-foreground">
                          {r.long ? "long" : "short"}
                        </span>
                        {r.flagged ? (
                          <FlagIcon className="size-3.5 text-sky-500" />
                        ) : null}
                      </div>
                      {r.annotations.length > 0 ? (
                        <ul className="mt-1.5 space-y-0.5 text-sm">
                          {r.annotations.map((a) => (
                            <li
                              key={a.id}
                              className="flex items-center gap-2 text-muted-foreground"
                            >
                              <span
                                className={`size-2 shrink-0 rounded-full ${VERDICT_DOT[a.verdict]}`}
                              />
                              <span className="text-foreground capitalize">
                                {a.verdict}
                              </span>
                              {a.aspect ? <span>{a.aspect}</span> : null}
                              {a.note ? (
                                <span className="truncate italic">
                                  “{a.note}”
                                </span>
                              ) : null}
                            </li>
                          ))}
                        </ul>
                      ) : null}
                    </div>
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>
      </div>
    </div>
  )
}

function FilterRow({
  label,
  children,
}: {
  label: string
  children: React.ReactNode
}) {
  return (
    <div className="flex items-baseline gap-3">
      <span className="w-16 shrink-0 text-xs font-medium text-muted-foreground">
        {label}
      </span>
      <div className="flex flex-wrap gap-1.5">{children}</div>
    </div>
  )
}

function Chip({
  on,
  onClick,
  children,
}: {
  on: boolean
  onClick: () => void
  children: React.ReactNode
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={`flex items-center gap-1.5 rounded-full border px-2.5 py-0.5 text-xs capitalize ${
        on
          ? "border-foreground bg-foreground text-background"
          : "text-muted-foreground hover:bg-muted"
      }`}
    >
      {children}
    </button>
  )
}
