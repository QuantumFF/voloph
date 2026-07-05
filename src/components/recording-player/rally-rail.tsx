"use client"

import { useEffect, useMemo, useRef } from "react"
import { FlagIcon, Loader2Icon, TimerIcon, TriangleAlertIcon } from "lucide-react"

import { fileName, formatClock } from "@/lib/format"
import {
  UNCERTAIN_CONFIDENCE,
  VERDICTS,
  type PlaylistRecording,
  type SessionAnnotation,
  type SessionModel,
  type SessionRally,
  type Verdict,
} from "@/components/recording-player-transport"
import { LONG_RALLY_MS } from "./index"

/** Marker colour per verdict (issue #8), matching the inspector's verdict dots. */
const VERDICT_DOT: Record<Verdict, string> = {
  good: "bg-emerald-500",
  bad: "bg-amber-500",
  mistake: "bg-red-500",
}

/**
 * The left rail of the studio layout (issue #48): the session's table of
 * contents — every rally in play order, grouped by recording, with its
 * duration, long-rally marker, and uncertain-region marker. Clicking a rally
 * seeks the session to its start; the row under the playhead stays highlighted
 * and scrolled into view.
 */
export function RallyRail({
  session,
  recordings,
  annotations,
  currentRallyIndex,
  onSelectRally,
}: {
  session: SessionModel
  recordings: PlaylistRecording[]
  annotations: SessionAnnotation[]
  currentRallyIndex: number
  onSelectRally: (rally: SessionRally) => void
}) {
  // Keep the playhead's rally visible as playback walks the session.
  const activeRef = useRef<HTMLLIElement>(null)
  useEffect(() => {
    activeRef.current?.scrollIntoView({ block: "nearest" })
  }, [currentRallyIndex])

  // Session-wide rally numbers, matching the strip's and inspector's numbering.
  const numbered = session.rallies.map((rally, number) => ({ rally, number }))

  // Per-rally verdict tallies, keyed like the row itself: an annotation belongs
  // to the rally whose session-global span contains it (glossary: a rally owns
  // the annotations in its span), so the rail summarises what's been marked.
  const verdictsByRally = useMemo(() => {
    const map = new Map<string, Record<Verdict, number>>()
    for (const a of annotations) {
      const rally = session.rallies.find(
        (r) => a.globalMs >= r.globalStart && a.globalMs < r.globalEnd
      )
      if (!rally) continue
      const key = `${rally.path}:${rally.id}`
      const counts = map.get(key) ?? { good: 0, bad: 0, mistake: 0 }
      counts[a.verdict] += 1
      map.set(key, counts)
    }
    return map
  }, [annotations, session.rallies])

  return (
    <aside className="w-60 shrink-0 overflow-y-auto border-r">
      {recordings.map((rec, recordingIndex) => {
        const seg = session.segments.find((s) => s.index === recordingIndex)
        const state = seg?.timeline?.segment_state
        const rows = numbered.filter(
          ({ rally }) => rally.recordingIndex === recordingIndex
        )
        return (
          <div key={rec.path}>
            <div
              className="sticky top-0 z-10 truncate border-b bg-background px-3 py-1.5 text-xs font-medium text-muted-foreground"
              title={rec.path}
            >
              {fileName(rec.path)}
            </div>
            {rows.length > 0 ? (
              <ul>
                {rows.map(({ rally, number }) => {
                  const active = number === currentRallyIndex
                  const durationMs = rally.localEnd - rally.localStart
                  const verdicts = verdictsByRally.get(
                    `${rally.path}:${rally.id}`
                  )
                  return (
                    <li
                      key={`${rally.path}:${rally.id}`}
                      ref={active ? activeRef : undefined}
                    >
                      <button
                        type="button"
                        onClick={() => onSelectRally(rally)}
                        className={`flex w-full items-center gap-2 px-3 py-1.5 text-left text-sm ${
                          active ? "bg-accent" : "hover:bg-accent/50"
                        }`}
                        title={`Rally ${number + 1}: ${formatClock(rally.globalStart)}–${formatClock(rally.globalEnd)}`}
                      >
                        <span className="w-9 shrink-0 text-muted-foreground tabular-nums">
                          {number + 1}
                        </span>
                        <span className="font-mono text-xs text-muted-foreground tabular-nums">
                          {formatClock(durationMs)}
                        </span>
                        {durationMs >= LONG_RALLY_MS ? (
                          <TimerIcon
                            className="size-3.5 shrink-0 text-muted-foreground"
                            aria-label="Long rally"
                          />
                        ) : null}
                        {verdicts
                          ? VERDICTS.filter((v) => verdicts[v] > 0).map((v) => (
                              <span
                                key={v}
                                className="flex shrink-0 items-center gap-0.5 text-xs text-muted-foreground tabular-nums"
                                aria-label={`${verdicts[v]} ${v}`}
                              >
                                <span
                                  className={`size-2 rounded-full ${VERDICT_DOT[v]}`}
                                />
                                {verdicts[v] > 1 ? verdicts[v] : null}
                              </span>
                            ))
                          : null}
                        {rally.flagged ? (
                          <FlagIcon
                            className="ml-auto size-3.5 shrink-0 fill-sky-400 text-sky-500"
                            aria-label="Flagged rally"
                          />
                        ) : null}
                        {rally.confidence < UNCERTAIN_CONFIDENCE ? (
                          <TriangleAlertIcon
                            className={`size-3.5 shrink-0 text-amber-500 ${rally.flagged ? "" : "ml-auto"}`}
                            aria-label="Uncertain region — worth checking"
                          />
                        ) : null}
                      </button>
                    </li>
                  )
                })}
              </ul>
            ) : state === "failed" ? (
              <p className="flex items-center gap-1.5 px-3 py-2 text-sm text-amber-600 dark:text-amber-500">
                <TriangleAlertIcon className="size-3.5 shrink-0" />
                No timeline
              </p>
            ) : state === "ready" ? (
              <p className="px-3 py-2 text-sm text-muted-foreground">
                No rallies detected.
              </p>
            ) : (
              <p className="flex items-center gap-1.5 px-3 py-2 text-sm text-muted-foreground">
                <Loader2Icon className="size-3.5 shrink-0 animate-spin" />
                Detecting rallies…
              </p>
            )}
          </div>
        )
      })}
    </aside>
  )
}
