/**
 * Remaining-analysis-time estimate for the background-work UI (issue #81, spec
 * #75 user story #13). The backend emits how much of a recording's footage the
 * analysis pass has processed so far (`analysis:progress`); this turns that
 * measured progress into a wall-clock estimate, so the "Analyzing…" row can say
 * roughly when a freshly imported recording will be reviewable.
 *
 * The estimate is derived, not hardcoded: analysis speed is measured live as
 * processed footage ÷ the wall-clock time it took, then the remaining footage is
 * divided by that speed. A CPU-only pass near 1.5× real-time and a GPU pass far
 * faster both land on a truthful number without any per-machine rate baked in.
 */

/**
 * One `analysis:progress` tick from the media worker (mirrors the Rust
 * `AnalysisProgress` payload): how much of recording `recording_id`'s footage the
 * pass has processed, out of its total (null until probed), and how long the pass
 * has run. Everything the UI needs to compute a live remaining-time estimate.
 */
export interface AnalysisProgress {
  recording_id: number
  processed_ms: number
  total_ms: number | null
  elapsed_ms: number
}

/**
 * Wall-clock milliseconds still expected before this recording's analysis is
 * done, or `null` when there is not yet enough to estimate from (no footage
 * processed, no elapsed time, or an unknown total). Floored at zero once
 * processing has reached the total.
 *
 * @param processedMs footage processed so far, in recording-local ms
 * @param totalMs the recording's total duration in ms (null until known)
 * @param elapsedWallMs wall-clock ms since analysis of this recording began
 */
export function estimateRemainingMs(
  processedMs: number,
  totalMs: number | null,
  elapsedWallMs: number
): number | null {
  if (!totalMs || totalMs <= 0) return null
  if (processedMs <= 0 || elapsedWallMs <= 0) return null
  const remainingFootage = Math.max(0, totalMs - processedMs)
  const speed = processedMs / elapsedWallMs // footage ms per wall ms
  return Math.round(remainingFootage / speed)
}

/**
 * Remaining-ms estimate keyed by recording id, for the rows that are still
 * analyzing. Drops any tick whose recording is no longer in `analyzingIds` (it
 * finished or failed) and any tick too early to estimate from, so the map holds
 * only live, showable estimates — no stale numbers linger on a done row.
 */
export function remainingByRecording(
  analyzingIds: Set<number>,
  progress: Map<number, AnalysisProgress>
): Map<number, number> {
  const remaining = new Map<number, number>()
  for (const [id, tick] of progress) {
    if (!analyzingIds.has(id)) continue
    const ms = estimateRemainingMs(tick.processed_ms, tick.total_ms, tick.elapsed_ms)
    if (ms != null) remaining.set(id, ms)
  }
  return remaining
}

/** Compact "time left" label for the Analyzing row: `~Ns left` / `~Nm left`. */
export function formatEta(remainingMs: number): string {
  const seconds = Math.ceil(remainingMs / 1000)
  if (seconds < 60) return `~${seconds}s left`
  const minutes = Math.ceil(seconds / 60)
  return `~${minutes}m left`
}
