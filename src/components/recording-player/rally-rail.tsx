"use client"

import { useEffect, useMemo, useRef } from "react"
import {
  CircleHelpIcon,
  FlagIcon,
  Loader2Icon,
  TimerIcon,
  TriangleAlertIcon,
} from "lucide-react"

import { fileName, formatClock, formatLength } from "@/lib/format"
import { cn } from "@/lib/utils"
import {
  UNCERTAIN_CONFIDENCE,
  VERDICTS,
  annotationsByRally,
  countVerdicts,
  rallyKey,
  type PlaylistRecording,
  type SessionAnnotation,
  type SessionModel,
  type SessionRally,
  type Verdict,
} from "@/components/recording-player-transport"
import { LONG_RALLY_MS, UNCERTAIN_TEXT } from "./constants"
import { VerdictCounts, VerdictPip } from "./verdict-mark"

/** The brief flash a row gets when a verdict lands in it. */
const VERDICT_FLASH: Record<Verdict, string> = {
  good: "bg-emerald-500/30",
  bad: "bg-amber-500/30",
  mistake: "bg-red-500/30",
}

/**
 * The left rail of the studio layout (issue #48): the session's table of
 * contents — every rally in play order, grouped by recording, with its start
 * time and length, long-rally marker, and on the right what's been marked on
 * it (verdict counts, flag) and whether the segmenter doubts it. Clicking a
 * rally opens it (and selects it in edit mode).
 *
 * Two things are marked, and never confused (issue 1):
 * - the **target** — the one rally the inspector shows (and the flag key acts
 *   on) — gets the filled row, and stays scrolled into view; in edit mode, when
 *   it's the selected rally, it adds a primary bar at its left edge;
 * - the **playhead** — a thin foreground bar at the left edge of the row it is
 *   in (padding included), the strip's playhead colour; in a gap, the rally up
 *   next gets a hollow "next" marker instead.
 * A row flashes in the verdict's colour when a mark lands in it, so a verdict
 * pressed mid-play visibly arrives.
 *
 * In a narrow player (below 75rem, the `player` container) the rail goes
 * compact rather than bare: number, start time and length, a dot for every
 * verdict present (never just the worst) with the total beside them, then the
 * flag, else the uncertain glyph; the row's title carries the counts.
 */
export function RallyRail({
  session,
  recordings,
  annotations,
  targetIndex,
  playheadIndex,
  nextIndex,
  editingTarget,
  flash,
  onSelectRally,
}: {
  session: SessionModel
  recordings: PlaylistRecording[]
  annotations: SessionAnnotation[]
  /** The rally the inspector shows (filled row), or -1 for none. */
  targetIndex: number
  /** The rally the playhead is in, clip padding included, or -1 in a gap. */
  playheadIndex: number
  /** The rally up next while the playhead waits in a gap, else -1. */
  nextIndex: number
  /** The target is the rally selected for editing (edit mode). */
  editingTarget: boolean
  /** A row to flash in a verdict's colour (a mark just landed in it). */
  flash: { key: string; verdict: Verdict } | null
  onSelectRally: (rally: SessionRally) => void
}) {
  // Keep the target rally visible as playback walks the session.
  const targetRef = useRef<HTMLLIElement>(null)
  useEffect(() => {
    targetRef.current?.scrollIntoView({ block: "nearest" })
  }, [targetIndex])

  // Session-wide rally numbers, matching the strip's and inspector's numbering.
  const numbered = session.rallies.map((rally, number) => ({ rally, number }))

  // Per-rally verdict tallies, keyed like the row itself: an annotation belongs
  // to the rally whose session-global span contains it (glossary: a rally owns
  // the annotations in its span), so the rail summarises what's been marked.
  const verdictsByRally = useMemo(() => {
    const map = new Map<string, Record<Verdict, number>>()
    for (const [key, owned] of annotationsByRally(
      session.rallies,
      annotations
    )) {
      map.set(key, countVerdicts(owned))
    }
    return map
  }, [annotations, session.rallies])

  return (
    <aside className="w-40 shrink-0 overflow-y-auto border-r @min-[75rem]/player:w-64">
      {recordings.map((rec, recordingIndex) => {
        const seg = session.segments.find((s) => s.index === recordingIndex)
        const state = seg?.timeline?.segment_state
        const rows = numbered.filter(
          ({ rally }) => rally.recordingIndex === recordingIndex
        )
        return (
          <div key={rec.path}>
            <div
              className="sticky top-0 z-10 truncate border-b bg-background px-2 py-1.5 text-[13px] font-medium text-muted-foreground @min-[75rem]/player:px-3"
              title={rec.path}
            >
              {fileName(rec.path)}
            </div>
            {rows.length > 0 ? (
              <ul>
                {rows.map(({ rally, number }) => {
                  const key = rallyKey(rally)
                  const target = number === targetIndex
                  const playhead = number === playheadIndex
                  const editingThis = target && editingTarget
                  const upNext = number === nextIndex
                  const durationMs = rally.localEnd - rally.localStart
                  const long = durationMs >= LONG_RALLY_MS
                  const uncertain = rally.confidence < UNCERTAIN_CONFIDENCE
                  const verdicts = verdictsByRally.get(key)
                  const flashing = flash?.key === key
                  const present = verdicts
                    ? VERDICTS.filter((v) => verdicts[v] > 0)
                    : []
                  const total = present.reduce(
                    (n, v) => n + (verdicts?.[v] ?? 0),
                    0
                  )
                  const tally = present
                    .map((v) => `${verdicts?.[v] ?? 0} ${v}`)
                    .join(", ")
                  return (
                    <li key={key} ref={target ? targetRef : undefined}>
                      <button
                        type="button"
                        // Keep focus on the player so the keymap and its
                        // Space don't land on this row.
                        onMouseDown={(e) => e.preventDefault()}
                        onClick={() => onSelectRally(rally)}
                        aria-current={target ? "true" : undefined}
                        className={cn(
                          "flex h-8 w-full items-center gap-2 px-2 text-left text-sm tabular-nums transition-colors duration-700 outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset @min-[75rem]/player:px-3",
                          flashing
                            ? `${VERDICT_FLASH[flash.verdict]} duration-150`
                            : target
                              ? "bg-primary/15"
                              : "hover:bg-accent/50",
                          editingThis && playhead
                            ? "shadow-[inset_3px_0_0_var(--primary),inset_5px_0_0_var(--foreground)]"
                            : editingThis
                              ? "shadow-[inset_3px_0_0_var(--primary)]"
                              : playhead
                                ? "shadow-[inset_2px_0_0_var(--foreground)]"
                                : null
                        )}
                        title={`Rally ${number + 1}: ${formatClock(rally.globalStart)}–${formatClock(rally.globalEnd)} · ${formatLength(durationMs)}${long ? " · long" : ""}${tally ? ` · ${tally}` : ""}${uncertain ? " · uncertain" : ""}${rally.flagged ? " · flagged" : ""}${playhead ? " · playhead here" : ""}${upNext ? " · up next" : ""}${editingThis ? " · selected for editing" : ""}`}
                      >
                        <span
                          className={cn(
                            "w-5 shrink-0 @min-[75rem]/player:w-7",
                            target
                              ? "font-semibold text-foreground"
                              : "text-muted-foreground"
                          )}
                        >
                          {number + 1}
                        </span>
                        <span className="flex min-w-0 items-center gap-1.5">
                          <span className={cn(target && "text-foreground")}>
                            {formatClock(rally.globalStart)}
                          </span>
                          <span className="text-muted-foreground">
                            <span className="hidden @min-[75rem]/player:inline">
                              ·{" "}
                            </span>
                            {formatLength(durationMs)}
                          </span>
                          {long ? (
                            <TimerIcon
                              className="hidden size-3.5 shrink-0 text-muted-foreground @min-[75rem]/player:block"
                              aria-label="Long rally"
                            />
                          ) : null}
                        </span>
                        <span className="ml-auto flex shrink-0 items-center gap-1.5">
                          {upNext ? (
                            <span
                              className="flex items-center gap-1 text-xs text-muted-foreground"
                              aria-label="Up next"
                            >
                              <span className="hidden @min-[75rem]/player:inline">
                                next
                              </span>
                              <span className="size-2.5 rounded-full border-[1.5px] border-primary" />
                            </span>
                          ) : null}
                          {/* Compact: a dot per verdict present and the
                              total, then the flag, else the uncertain glyph. */}
                          <span className="flex items-center gap-1 @min-[75rem]/player:hidden">
                            {present.length > 0 ? (
                              <span className="flex items-center gap-0.5">
                                {present.map((v) => (
                                  <VerdictPip
                                    key={v}
                                    verdict={v}
                                    count={verdicts?.[v] ?? 0}
                                  />
                                ))}
                                <span
                                  className="ml-0.5 text-xs text-muted-foreground"
                                  aria-label={`${total} marked`}
                                >
                                  {total}
                                </span>
                              </span>
                            ) : null}
                            {rally.flagged ? (
                              <FlagIcon
                                className="size-3.5 shrink-0 fill-sky-400 text-sky-500"
                                aria-label="Flagged rally"
                              />
                            ) : uncertain && present.length === 0 ? (
                              <CircleHelpIcon
                                className={`size-4 shrink-0 ${UNCERTAIN_TEXT}`}
                                aria-label="Uncertain boundaries — worth checking"
                              />
                            ) : null}
                          </span>
                          {/* Wide: the verdict counts in the shared format,
                              the flag, and doubt. */}
                          <span className="hidden items-center gap-1.5 @min-[75rem]/player:flex">
                            {verdicts ? (
                              <VerdictCounts counts={verdicts} />
                            ) : null}
                            {rally.flagged ? (
                              <FlagIcon
                                className="size-3.5 shrink-0 fill-sky-400 text-sky-500"
                                aria-label="Flagged rally"
                              />
                            ) : null}
                            {uncertain ? (
                              <CircleHelpIcon
                                className={`size-4 shrink-0 ${UNCERTAIN_TEXT}`}
                                aria-label="Uncertain boundaries — worth checking"
                              />
                            ) : null}
                          </span>
                        </span>
                      </button>
                    </li>
                  )
                })}
              </ul>
            ) : state === "failed" ? (
              <p
                className="flex items-center gap-1.5 px-3 py-2 text-sm text-amber-600 dark:text-amber-500"
                title="Couldn't detect rallies for this recording"
              >
                <TriangleAlertIcon className="size-3.5 shrink-0" />
                No timeline
              </p>
            ) : state === "ready" ? (
              <p className="px-3 py-2 text-sm text-muted-foreground">
                No rallies detected.
              </p>
            ) : (
              <p
                className="flex items-center gap-1.5 px-3 py-2 text-sm text-muted-foreground"
                title="Detecting rallies…"
              >
                <Loader2Icon className="size-3.5 shrink-0 animate-spin" />
                <span className="truncate">Detecting rallies…</span>
              </p>
            )}
          </div>
        )
      })}
    </aside>
  )
}
