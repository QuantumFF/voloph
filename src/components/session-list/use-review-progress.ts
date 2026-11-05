import { useEffect, useState } from "react"

import { trackedInvoke } from "@/lib/tauri"
import {
  VERDICTS,
  type FilteredRally,
} from "@/components/recording-player-transport"

import { tallyReview, type ReviewTally } from "./review-progress"
import type { Session } from "./types"

/** Read-only `filter_moments`; a null filter field means "any". */
function filterMoments(verdict: string | null) {
  return trackedInvoke<FilteredRally[]>("filter_moments", {
    verdict,
    aspect: null,
    length: null,
    flagged: null,
  })
}

/**
 * Review state per recording id for the session list, or null until the first
 * load lands. Four read-only queries for the whole library (every rally, then
 * one per verdict) — never one per recording. Reloads when the list's shape
 * changes (a recording added, re-analyzed, or finished analyzing) or when
 * `epoch` moves, which the list bumps on every non-poll refresh (mount,
 * returning from the player, receiving or carrying a review). The previous
 * tallies stay on screen while a reload is in flight, so nothing flickers.
 */
export function useReviewProgress(
  sessions: Session[],
  epoch: number
): Map<number, ReviewTally> | null {
  const shape = sessions
    .flatMap((s) =>
      s.recordings.map((r) => `${r.id}:${r.segment_state}:${r.rally_count}`)
    )
    .join(",")
  const [tallies, setTallies] = useState<Map<number, ReviewTally> | null>(null)

  useEffect(() => {
    if (shape === "") return
    let stale = false
    Promise.all([filterMoments(null), ...VERDICTS.map(filterMoments)])
      .then(([all, ...annotated]) => {
        if (!stale) setTallies(tallyReview(all, annotated))
      })
      // Review state is a progress hint, not the list itself: on failure the
      // rows keep showing their plain rally counts.
      .catch(() => {})
    return () => {
      stale = true
    }
  }, [shape, epoch])

  return shape === "" ? null : tallies
}
