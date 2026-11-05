"use client"

import { useCallback, useEffect, useRef, useState } from "react"
import {
  ArrowLeftIcon,
  DownloadIcon,
  FlagIcon,
  Loader2Icon,
  PlayIcon,
  SearchXIcon,
  XIcon,
} from "lucide-react"

import { Button } from "@/components/ui/button"
import { fileName, formatClock, formatLength } from "@/lib/format"
import { trackedInvoke } from "@/lib/tauri"
import { plural, sessionTitle } from "@/components/session-list/recording-state"
import { useExport } from "@/components/use-export"
import {
  ASPECTS,
  VERDICTS,
  type Annotation,
  type FilteredRally,
  type Verdict,
} from "@/components/recording-player-transport"
import { LONG_RALLY_MS } from "@/components/recording-player/constants"
import {
  VerdictCounts,
  VerdictMark,
} from "@/components/recording-player/verdict-mark"

/** One recording in a session, enough to build a jump playlist. */
interface Recording {
  path: string
}
/**
 * A rally's annotations as compact marks: note-less moments with the same
 * verdict and aspect fold into one mark with a count (like the player's rail);
 * a moment with a note stays on its own so the note can show.
 */
interface MomentMark {
  key: string
  verdict: Verdict
  aspect: string | null
  note: string | null
  count: number
}

function compactMarks(annotations: Annotation[]): MomentMark[] {
  const marks: MomentMark[] = []
  const byKey = new Map<string, MomentMark>()
  for (const a of annotations) {
    if (a.note) {
      marks.push({ key: `n${a.id}`, ...a, count: 1 })
      continue
    }
    const key = `${a.verdict}|${a.aspect ?? ""}`
    const existing = byKey.get(key)
    if (existing) existing.count += 1
    else {
      const mark = {
        key,
        verdict: a.verdict,
        aspect: a.aspect,
        note: null,
        count: 1,
      }
      byKey.set(key, mark)
      marks.push(mark)
    }
  }
  return marks
}

/** How many of a rally's marks carry each verdict, for `VerdictCounts`. */
function countVerdicts(marks: MomentMark[]): Partial<Record<Verdict, number>> {
  const counts: Partial<Record<Verdict, number>> = {}
  for (const m of marks) counts[m.verdict] = (counts[m.verdict] ?? 0) + m.count
  return counts
}

/** One session's run of results — the backend lists them day by day. */
interface ResultGroup {
  sessionId: number
  day: string
  rallies: FilteredRally[]
}

function groupBySession(results: FilteredRally[]): ResultGroup[] {
  const groups: ResultGroup[] = []
  for (const r of results) {
    const last = groups[groups.length - 1]
    if (last && last.sessionId === r.session_id) last.rallies.push(r)
    else
      groups.push({ sessionId: r.session_id, day: r.capture_day, rallies: [r] })
  }
  return groups
}

/**
 * Number every rally within its session, 1-based, from an unfiltered
 * `filter_moments` run (already in the player's order: recording path, then
 * start).
 */
function numberRallies(all: FilteredRally[]): Map<number, number> {
  const numbers = new Map<number, number>()
  const counts = new Map<number, number>()
  for (const r of all) {
    const n = (counts.get(r.session_id) ?? 0) + 1
    counts.set(r.session_id, n)
    numbers.set(r.rally_id, n)
  }
  return numbers
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

/**
 * Where a jump lands: the moment's session recordings, which one, and the
 * rally's strict start within it (the player pads it, #104).
 */
export interface JumpTarget {
  recordings: Recording[]
  startIndex: number
  rallyStartMs: number
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
  // Every annotation of each result's recording, keyed by path (read-only
  // `recording_annotations`). With no verdict/aspect filter the backend sends
  // rallies without their moments, so rows read them from here to still show
  // their verdict marks. A rally owns the moments inside its strict span.
  const [annotationsByPath, setAnnotationsByPath] = useState<
    Record<string, Annotation[]>
  >({})

  // Each rally's number within its session — the "Rally 12" the player shows —
  // keyed by rally id. Taken from one unfiltered run (sessions → recordings in
  // path order → rallies by start, the player's order), so a filtered row still
  // reads as the same rally the player will open on.
  const [rallyNumbers, setRallyNumbers] = useState<Map<number, number>>(
    new Map()
  )

  // Sessions are the source for building a jump playlist (all of a session's
  // recordings in order). Loaded once; the results carry the rally coordinates.
  useEffect(() => {
    trackedInvoke<Session[]>("list_sessions")
      .then(setSessions)
      .catch((e) => setError(String(e)))
    trackedInvoke<FilteredRally[]>("filter_moments", {
      verdict: null,
      aspect: null,
      length: null,
      flagged: null,
    })
      .then((all) => setRallyNumbers(numberRallies(all)))
      .catch(() => {})
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

  // Paths already requested, so a re-run while a fetch is in flight doesn't
  // ask again. Annotations don't change while browsing.
  const requested = useRef(new Set<string>())
  useEffect(() => {
    for (const path of new Set(results.map((r) => r.recording_path))) {
      if (requested.current.has(path)) continue
      requested.current.add(path)
      trackedInvoke<Annotation[]>("recording_annotations", { path })
        .then((list) =>
          setAnnotationsByPath((prev) => ({ ...prev, [path]: list }))
        )
        .catch(() => {})
    }
  }, [results])

  // With a verdict/aspect filter the backend attaches the matching moments —
  // exactly what made the rally match. Otherwise show all of the rally's own.
  const momentFilter = filters.verdict !== null || filters.aspect !== null
  const marksFor = (r: FilteredRally): MomentMark[] =>
    compactMarks(
      momentFilter
        ? r.annotations
        : (annotationsByPath[r.recording_path] ?? []).filter(
            (a) => a.time_ms >= r.start_ms && a.time_ms < r.end_ms
          )
    )

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
        rallyStartMs: r.start_ms,
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

  const groups = groupBySession(results)

  // Where the recording name goes. A session with one recording names it once,
  // in its header — the same file on every row would be noise. When any
  // session holds several, rows get a recording column (in every session, so
  // the columns still line up across sessions); a timecode alone doesn't say
  // which of the session's files a rally is in. Decided from the library, not
  // the results, so the column doesn't come and go as filters narrow them.
  const recordingsOf = (sessionId: number) =>
    sessions.find((s) => s.id === sessionId)?.recordings ?? []
  const recordingColumn = sessions.some((s) => s.recordings.length > 1)

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
        <span className="font-semibold">Browse moments</span>
        <span className="hidden text-sm text-muted-foreground md:inline">
          Filter every rally across every session
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
            title={
              active
                ? `Render one MP4 of the ${plural(results.length, "rally", "rallies")} matching the current filters, in the order shown.`
                : `Render one MP4 of all ${plural(results.length, "rally", "rallies")} in this library, in the order shown.`
            }
          >
            {exportProgress != null ? (
              <>
                <Loader2Icon className="size-4 animate-spin" />
                Exporting… {Math.round(exportProgress * 100)}%
              </>
            ) : (
              <>
                <DownloadIcon className="size-4" />
                {/* Says exactly what it renders: the rallies listed below,
                    following the filters. */}
                Export {plural(results.length, "rally", "rallies")}
              </>
            )}
          </Button>
        </div>
      </header>

      <div className="min-h-0 flex-1 overflow-y-auto">
        <div className="mx-auto max-w-5xl space-y-5 px-6 py-6">
          {/* Filter controls: each row is one dimension; a set chip toggles off. */}
          <div className="space-y-2.5 rounded-lg border bg-card/40 p-4">
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
                  <VerdictMark verdict={v} className="size-3.5" />
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
              <span className="self-center pl-1 text-xs text-muted-foreground">
                Long is {formatLength(LONG_RALLY_MS)} or more
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
          </div>

          {error ? (
            <p
              role="alert"
              className="rounded-md border border-destructive/40 bg-destructive/5 px-3 py-2 text-sm text-destructive"
            >
              {error}
            </p>
          ) : null}

          <div className="flex min-h-7 items-center gap-3">
            <p className="text-sm tabular-nums">
              <span className="font-medium">
                {results.length} {results.length === 1 ? "rally" : "rallies"}
              </span>
              {groups.length > 0 ? (
                <span className="text-muted-foreground">
                  {" "}
                  across {groups.length}{" "}
                  {groups.length === 1 ? "session" : "sessions"}
                </span>
              ) : null}
            </p>
            {active ? (
              <Button
                variant="ghost"
                size="sm"
                className="ml-auto text-muted-foreground"
                onClick={() => setFilters(EMPTY_FILTERS)}
              >
                <XIcon className="size-3.5" />
                Clear filters
              </Button>
            ) : null}
          </div>

          {results.length === 0 ? (
            <div className="flex flex-col items-center gap-2 rounded-lg border border-dashed px-6 py-16 text-center text-muted-foreground">
              <SearchXIcon className="size-6" />
              <p className="font-medium text-foreground">Nothing matches</p>
              <p className="text-sm">
                No rallies match these filters. Loosen them, or annotate more
                during review.
              </p>
            </div>
          ) : (
            <div className="space-y-5">
              {groups.map((g) => {
                const recordings = recordingsOf(g.sessionId)
                return (
                  <section key={g.sessionId} aria-label={g.day}>
                    {/* Sticky per-session header: the date stays in view while
                        scrolling a long run of that session's rallies. */}
                    <h2 className="sticky top-0 z-10 -mx-1 flex items-baseline gap-2 bg-background/95 px-1 py-2 text-sm backdrop-blur">
                      {/* Named as the session list names it, plus the year
                          (the list carries that in its month headings). */}
                      <span className="font-semibold">
                        {sessionTitle(g.day)}, {g.day.slice(0, 4)}
                      </span>
                      <span className="shrink-0 text-xs text-muted-foreground tabular-nums">
                        {g.rallies.length}{" "}
                        {g.rallies.length === 1 ? "rally" : "rallies"}
                      </span>
                      {!recordingColumn && recordings.length === 1 ? (
                        <span
                          className="min-w-0 truncate text-xs text-muted-foreground"
                          title={recordings[0].path}
                        >
                          <span aria-hidden className="pr-2">
                            ·
                          </span>
                          {fileName(recordings[0].path)}
                        </span>
                      ) : null}
                    </h2>
                    <ul className="divide-y overflow-hidden rounded-lg border">
                      {g.rallies.map((r) => (
                        <MomentRow
                          key={r.rally_id}
                          rally={r}
                          marks={marksFor(r)}
                          number={rallyNumbers.get(r.rally_id)}
                          showRecording={recordingColumn}
                          onJump={() => jump(r)}
                        />
                      ))}
                    </ul>
                  </section>
                )
              })}
            </div>
          )}
        </div>
      </div>
    </div>
  )
}

/**
 * The result row's columns — play, rally, timecode, length, recording, flag,
 * verdict tally, aspects and note, long tag — fixed so they line up down the
 * list and across sessions. The recording column is there only when some
 * session holds more than one recording.
 */
const ROW_COLUMNS =
  "grid grid-cols-[1rem_4.5rem_6.5rem_3rem_1rem_6.5rem_minmax(0,1fr)_auto] items-center gap-x-3"
const ROW_COLUMNS_WITH_RECORDING =
  "grid grid-cols-[1rem_4.5rem_6.5rem_3rem_minmax(0,8rem)_1rem_6.5rem_minmax(0,1fr)_auto] items-center gap-x-3"

/** A rally's marks folded by verdict and aspect, for the aspect chips. */
function aspectChips(
  marks: MomentMark[]
): { key: string; verdict: Verdict; aspect: string; count: number }[] {
  const chips = new Map<
    string,
    { key: string; verdict: Verdict; aspect: string; count: number }
  >()
  for (const m of marks) {
    if (m.aspect === null) continue
    const key = `${m.verdict}|${m.aspect}`
    const chip = chips.get(key)
    if (chip) chip.count += m.count
    else
      chips.set(key, {
        key,
        verdict: m.verdict,
        aspect: m.aspect,
        count: m.count,
      })
  }
  return [...chips.values()]
}

/**
 * One matching rally, as one button that opens the player at that rally: where
 * it is (timecode, and the recording when a session has several), how long
 * it ran, whether it is flagged, its verdict tally in the shared
 * `VerdictCounts` format (the player's rail reads the same), what the marks
 * were about — an aspect chip per verdict and aspect, and the first note — and
 * the long tag. ▷ shows on hover and keyboard focus, as on the session list's
 * recording rows.
 */
function MomentRow({
  rally: r,
  marks,
  number,
  showRecording,
  onJump,
}: {
  rally: FilteredRally
  marks: MomentMark[]
  /** The rally's number within its session (as the player shows it). */
  number: number | undefined
  showRecording: boolean
  onJump: () => void
}) {
  const chips = aspectChips(marks)
  const notes = marks.flatMap((m) => (m.note ? [m.note] : []))
  const name = number != null ? `Rally ${number}` : "this rally"
  return (
    <li>
      <button
        type="button"
        onClick={onJump}
        className={`group ${showRecording ? ROW_COLUMNS_WITH_RECORDING : ROW_COLUMNS} min-h-10 w-full px-4 py-1.5 text-left text-sm tabular-nums outline-none hover:bg-muted/60 focus-visible:bg-muted/60 focus-visible:ring-2 focus-visible:ring-ring/50 focus-visible:ring-inset`}
        title={`Open ${name} in the player`}
      >
        <PlayIcon
          aria-hidden
          className="size-3.5 text-foreground opacity-0 transition-opacity group-hover:opacity-100 group-focus-visible:opacity-100"
        />
        {/* The rally is what the row names, so it leads; where it sits in
            the recording is supporting detail. */}
        <span className="font-medium">
          {number != null ? `Rally ${number}` : null}
        </span>
        <span className="text-muted-foreground">
          {formatClock(r.start_ms)}–{formatClock(r.end_ms)}
        </span>
        <span className="text-muted-foreground" title="Rally length">
          {formatLength(r.end_ms - r.start_ms)}
        </span>
        {showRecording ? (
          <span
            className="truncate text-xs text-muted-foreground"
            title={r.recording_path}
          >
            {fileName(r.recording_path)}
          </span>
        ) : null}
        {/* The flag column: always reserved, so a flag sits in the same
            place on every row. */}
        <span
          className="flex justify-center"
          title={r.flagged ? "Flagged" : undefined}
        >
          {r.flagged ? (
            <FlagIcon
              className="size-3.5 fill-sky-400 text-sky-500"
              aria-label="Flagged"
            />
          ) : null}
        </span>
        <VerdictCounts counts={countVerdicts(marks)} />
        {/* What the marks were about: aspect chips (bare verdicts are all in
            the tally), then the first note, cut to the space left. */}
        <span className="flex min-w-0 flex-wrap items-center gap-1.5">
          {chips.map((c) => (
            <span
              key={c.key}
              className="flex shrink-0 items-center gap-1 rounded-full border py-px pr-2 pl-0.5 text-xs text-muted-foreground capitalize"
            >
              <VerdictMark verdict={c.verdict} className="size-3.5" />
              {c.aspect}
              {c.count > 1 ? (
                <span className="text-muted-foreground/80">×{c.count}</span>
              ) : null}
            </span>
          ))}
          {notes.length > 0 ? (
            <span
              className="flex min-w-0 flex-1 basis-32 items-center gap-1 text-xs text-muted-foreground"
              title={notes.join("\n")}
            >
              <span className="truncate italic">“{notes[0]}”</span>
              {notes.length > 1 ? (
                <span className="shrink-0">+{notes.length - 1}</span>
              ) : null}
            </span>
          ) : null}
        </span>
        <span className="flex justify-end">
          {r.long ? (
            <span
              className="rounded-sm bg-muted px-1.5 py-px text-xs font-medium text-muted-foreground"
              title={`Long rally — ${formatLength(LONG_RALLY_MS)} or more`}
            >
              Long
            </span>
          ) : null}
        </span>
      </button>
    </li>
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
    <div className="flex items-start gap-3">
      <span className="w-16 shrink-0 pt-1.5 text-xs font-medium text-muted-foreground">
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
      aria-pressed={on}
      className={`flex h-7 items-center gap-1.5 rounded-full border px-3 text-xs capitalize transition-colors outline-none focus-visible:ring-2 focus-visible:ring-ring/50 ${
        on
          ? "border-foreground bg-foreground text-background"
          : "text-muted-foreground hover:bg-muted hover:text-foreground"
      }`}
    >
      {children}
    </button>
  )
}
