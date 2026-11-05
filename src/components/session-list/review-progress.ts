import type { FilteredRally } from "@/components/recording-player-transport"

import type { Session } from "./types"

/**
 * How far a recording's (or a session's) review has got: its rallies, how many
 * carry at least one annotation ("marked"), the annotations on its rallies
 * ("marks"), and how many rallies are flagged. A rally owns the annotations
 * whose timestamp falls inside its span (GLOSSARY.md), so a mark left in a gap
 * belongs to no rally and is not counted — the same rule the moment browser
 * uses.
 */
export interface ReviewTally {
  rallies: number
  marked: number
  marks: number
  flagged: number
}

const EMPTY: ReviewTally = { rallies: 0, marked: 0, marks: 0, flagged: 0 }

/**
 * Tally review state per recording id from read-only `filter_moments` results:
 * `all` is the unfiltered run (every rally, with its flag), and `annotated` is
 * one run per verdict — each lists the rallies holding a moment of that verdict,
 * with those moments attached. Every annotation has exactly one verdict, so the
 * runs never count a moment twice. A constant number of queries for the whole
 * library, however many recordings it holds.
 */
export function tallyReview(
  all: FilteredRally[],
  annotated: FilteredRally[][]
): Map<number, ReviewTally> {
  const tallies = new Map<number, ReviewTally>()
  const at = (recordingId: number) => {
    let t = tallies.get(recordingId)
    if (!t) {
      t = { ...EMPTY }
      tallies.set(recordingId, t)
    }
    return t
  }
  for (const r of all) {
    const t = at(r.recording_id)
    t.rallies += 1
    if (r.flagged) t.flagged += 1
  }
  const marked = new Set<number>()
  for (const run of annotated) {
    for (const r of run) {
      if (r.annotations.length === 0) continue
      const t = at(r.recording_id)
      t.marks += r.annotations.length
      if (!marked.has(r.rally_id)) {
        marked.add(r.rally_id)
        t.marked += 1
      }
    }
  }
  return tallies
}

/** A session's review state: its recordings' tallies summed. */
export function sessionReview(
  session: Session,
  tallies: Map<number, ReviewTally>
): ReviewTally {
  const sum = { ...EMPTY }
  for (const recording of session.recordings) {
    const t = tallies.get(recording.id)
    if (!t) continue
    sum.rallies += t.rallies
    sum.marked += t.marked
    sum.marks += t.marks
    sum.flagged += t.flagged
  }
  return sum
}

/**
 * The one session whose Review button carries the page's primary emphasis: the
 * most recent session (the list is newest first) that still has rallies with no
 * mark — the next review waiting. Null while review state is unknown or when
 * every rally everywhere is marked.
 */
export function nextToReview(
  sessions: Session[],
  tallies: Map<number, ReviewTally> | null
): number | null {
  if (!tallies) return null
  for (const session of sessions) {
    const t = sessionReview(session, tallies)
    if (t.rallies > t.marked) return session.id
  }
  return null
}

/** The share of a tally's rallies that are marked, 0–1 (0 with no rallies). */
export function markedFraction(t: ReviewTally): number {
  return t.rallies > 0 ? Math.min(1, t.marked / t.rallies) : 0
}
