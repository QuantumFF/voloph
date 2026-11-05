/**
 * Verdict marks on the session timeline strip: which marks are drawn alone (a
 * thin full-height tick) and which pile up too close to tell apart, so they
 * merge into one pill carrying a count. Pure so the grouping is unit-tested
 * without a DOM; the strip re-groups at every zoom, so zooming in splits a
 * pile back into its ticks.
 */

import type { Verdict } from "@/components/recording-player-transport"

/** Marks closer than this (px) can't be told apart, so they merge. */
export const MARK_MERGE_PX = 6

/** The drawn width (px) of a lone mark's head — the tick itself is 2px. */
export const MARK_HEAD_PX = 6

/**
 * The grab width (px) of a lone mark: an invisible hit area centred on the
 * head, 8px wider than the head so a 6px target isn't a pixel hunt. Wider than
 * the drawn spacing, so neighbouring hit areas may overlap — the later mark
 * wins the overlap, which is fine for a seek.
 */
export const MARK_HIT_PX = MARK_HEAD_PX + 8

/**
 * Height (px) of a merged pile's pill: tall enough for an 11px count to read
 * at a glance (the marker band is sized around it).
 */
export const MARK_PILL_PX = 16

/** Clear space (px) kept between neighbouring heads / pills. */
const MARK_GAP_PX = 2

/** The verdicts in the order a pile lists and stripes them. */
export const VERDICT_ORDER: Verdict[] = ["good", "bad", "mistake"]

/** The count a pill shows: the number, capped so the pill stays small. */
export function pillCount(n: number): string {
  return n > 99 ? "99+" : String(n)
}

/** The text a pile's pill shows: "×18" — a multiplier, so it can't read as a rally number. */
export function pillLabel(n: number): string {
  return `×${pillCount(n)}`
}

/**
 * A pill's width (px): wide enough for its 11px "×" and count (~7px a
 * character, ~6px for the ×, 5px padding and border each side), never narrower
 * than round.
 */
export function pillWidth(n: number): number {
  return Math.max(MARK_PILL_PX + 2, 16 + pillCount(n).length * 7)
}

/**
 * Clear space (px) kept between a pile's pill and the playhead line: the line
 * is 2px with a 1px halo each side, plus a 2px gap.
 */
const PLAYHEAD_CLEAR_PX = 4

/**
 * Where a pile's pill is drawn (its centre, content px) so the playhead line
 * never slices through its count: left where it is when the playhead misses
 * it, else moved just aside — to the side of the playhead the pill's centre is
 * on (left when dead on, so a pile just made at the playhead sits over the
 * marks behind it). Its tick stays at the marks' true position.
 */
export function dodgePlayhead(
  centre: number,
  width: number,
  playheadPx: number | null
): number {
  if (playheadPx === null) return centre
  const clear = width / 2 + PLAYHEAD_CLEAR_PX
  if (Math.abs(centre - playheadPx) >= clear) return centre
  return centre > playheadPx ? playheadPx + clear : playheadPx - clear
}

export interface MarkGroup<T> {
  /** The marks in it, in time order. */
  marks: T[]
  /** x of its first and last mark (content px). */
  lo: number
  hi: number
  /** Where its head or pill is centred (content px). */
  centre: number
  /** How many of each verdict it holds. */
  counts: Record<Verdict, number>
}

function makeGroup<T extends { globalMs: number; verdict: Verdict }>(
  marks: T[],
  pxPerSec: number
): MarkGroup<T> {
  const lo = (marks[0].globalMs / 1000) * pxPerSec
  const hi = (marks[marks.length - 1].globalMs / 1000) * pxPerSec
  const counts: Record<Verdict, number> = { good: 0, bad: 0, mistake: 0 }
  for (const m of marks) counts[m.verdict]++
  return { marks, lo, hi, centre: (lo + hi) / 2, counts }
}

/** The width a group draws in the marker band: a head, or a counted pill. */
function drawnWidth(group: MarkGroup<unknown>): number {
  return group.marks.length === 1 ? MARK_HEAD_PX : pillWidth(group.marks.length)
}

/**
 * Group verdict marks for drawing at `pxPerSec`: a chain of marks each closer
 * than `MARK_MERGE_PX` to the last merges into one group, then neighbouring
 * groups whose heads / pills would touch merge too (a pill is wider than the
 * gap that formed it), until every drawn mark stands clear of the next.
 */
export function groupMarks<T extends { globalMs: number; verdict: Verdict }>(
  marks: readonly T[],
  pxPerSec: number
): MarkGroup<T>[] {
  const sorted = [...marks].sort((a, b) => a.globalMs - b.globalMs)
  const x = (m: T) => (m.globalMs / 1000) * pxPerSec
  const chains: T[][] = []
  for (const m of sorted) {
    const chain = chains[chains.length - 1]
    if (chain && x(m) - x(chain[chain.length - 1]) < MARK_MERGE_PX) {
      chain.push(m)
    } else {
      chains.push([m])
    }
  }
  let groups = chains.map((c) => makeGroup(c, pxPerSec))
  for (let merged = true; merged;) {
    merged = false
    const next: MarkGroup<T>[] = []
    for (const g of groups) {
      const prev = next[next.length - 1]
      if (
        prev &&
        g.centre - drawnWidth(g) / 2 <
          prev.centre + drawnWidth(prev) / 2 + MARK_GAP_PX
      ) {
        next[next.length - 1] = makeGroup([...prev.marks, ...g.marks], pxPerSec)
        merged = true
      } else {
        next.push(g)
      }
    }
    groups = next
  }
  return groups
}
