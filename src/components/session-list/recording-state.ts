import { formatDuration } from "@/lib/format"

import type { Recording, Session } from "./types"

/** True during the brief window before a recording has been probed for playback. */
export function isPreparing(state: string): boolean {
  return state === "unknown"
}

/**
 * True while a recording is playable but its draft timeline is still being
 * produced — audio extraction + segmentation (ADR 0002). Segmentation only
 * starts once the recording is probed (`ready`), so a still-`unknown` segment
 * state on a ready recording means "queued or analyzing".
 */
export function isAnalyzing(recording: Recording): boolean {
  return (
    recording.probe_state === "ready" && recording.segment_state === "unknown"
  )
}

/** True while any background media work is still pending for this recording. */
export function isProcessing(recording: Recording): boolean {
  return isPreparing(recording.probe_state) || isAnalyzing(recording)
}

/** Human label for a library kind, for the switcher and buttons. */
export function kindLabel(kind: string): string {
  return kind === "shared" ? "Shared" : "Local"
}

/** A session's (or a whole library's) headline numbers. */
export interface SessionStats {
  recordings: number
  /** Rallies across the recordings whose draft timeline is ready. */
  rallies: number
  /** Footage length in ms (recordings not yet measured count as 0). */
  durationMs: number
  /** Recordings still being prepared or analyzed in the background. */
  processing: number
}

/** The numbers a session header (or the library overview) shows. */
export function sessionStats(sessions: Session | Session[]): SessionStats {
  const recordings = (Array.isArray(sessions) ? sessions : [sessions]).flatMap(
    (s) => s.recordings
  )
  return {
    recordings: recordings.length,
    rallies: recordings
      .filter((r) => r.segment_state === "ready")
      .reduce((sum, r) => sum + r.rally_count, 0),
    durationMs: recordings.reduce((sum, r) => sum + (r.duration_ms ?? 0), 0),
    processing: recordings.filter(isProcessing).length,
  }
}

/** `n thing` / `n things`. */
export function plural(n: number, one: string, many = `${one}s`): string {
  return `${n} ${n === 1 ? one : many}`
}

/** Coarse footage length: `45s` under a minute, else formatDuration's `Nm`. */
export function formatFootage(ms: number): string {
  return ms > 0 && ms < 60_000
    ? `${Math.round(ms / 1000)}s`
    : formatDuration(ms)
}

function dayDate(day: string): Date | null {
  const date = new Date(`${day}T00:00:00`)
  return Number.isNaN(date.getTime()) ? null : date
}

/** A capture day's calendar-tile parts: `{ month: "JUN", day: "21" }`. */
export function dayTile(day: string): { month: string; day: string } | null {
  const date = dayDate(day)
  if (!date) return null
  return {
    month: date.toLocaleDateString(undefined, { month: "short" }).toUpperCase(),
    day: String(date.getDate()),
  }
}

/** A capture day as a session title without the year: "Sunday, June 21". */
export function sessionTitle(day: string): string {
  const date = dayDate(day)
  if (!date) return day
  return date.toLocaleDateString(undefined, {
    weekday: "long",
    month: "long",
    day: "numeric",
  })
}

/**
 * Sessions grouped by capture month, keeping their order (the backend lists
 * newest first). The heading carries the year the session titles leave out.
 */
export function groupByMonth<T extends { capture_day: string }>(
  sessions: T[]
): { key: string; label: string; sessions: T[] }[] {
  const groups: { key: string; label: string; sessions: T[] }[] = []
  for (const session of sessions) {
    const key = session.capture_day.slice(0, 7)
    let group = groups[groups.length - 1]
    if (!group || group.key !== key) {
      const date = dayDate(`${key}-01`)
      group = {
        key,
        label: date
          ? date.toLocaleDateString(undefined, {
              month: "long",
              year: "numeric",
            })
          : key,
        sessions: [],
      }
      groups.push(group)
    }
    group.sessions.push(session)
  }
  return groups
}

/**
 * A library path short enough for one menu line: the home directory as `~`,
 * and anything deeper than the last two folders elided — `…/scratchpad/lib`.
 * The folder's own name (the useful end) always survives; the full path goes
 * in a tooltip wherever this is shown.
 */
export function shortPath(path: string): string {
  const home = path.match(/^\/(?:home|Users)\/[^/]+(?=\/|$)/)
  const rest = home ? `~${path.slice(home[0].length)}` : path
  const parts = rest.split(/[\\/]/).filter((p, i) => p !== "" || i === 0)
  if (parts.length <= 4) return rest
  return `${parts[0] === "~" ? "~/" : ""}…/${parts.slice(-2).join("/")}`
}
