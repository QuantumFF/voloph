/**
 * Pure transport + session-orchestration math for the recording player, factored
 * out of the component so it can be unit-tested without WebKitGTK or a native
 * surface (ADR 0008). The component owns the side effects (the `mpv_*` invokes,
 * the `time-pos` event stream, `loadfile` for cross-file crossing); this only
 * computes the next value an action lands on — the gap-skip, rally-nav,
 * uncertain-jump, and session-axis decisions (issue #36).
 */

/** The playback speed ladder; `Ctrl+-`/`Ctrl+=` step down/up it, `Ctrl+0` resets. */
export const SPEED_LADDER = [0.25, 0.5, 0.75, 1, 1.5, 2] as const

/** Clamp a ladder index into range after a step. */
export function stepSpeedIndex(index: number, dir: 1 | -1): number {
  return Math.min(Math.max(index + dir, 0), SPEED_LADDER.length - 1)
}

/**
 * The absolute playhead a relative seek lands on, never before zero. The clamp
 * to the recording's end is left to mpv (it caps a seek past EOF itself).
 */
export function seekTarget(currentMs: number, deltaMs: number): number {
  return Math.max(0, currentMs + deltaMs)
}

/** Clamp a volume (0–100) after a step. */
export function clampVolume(volume: number): number {
  return Math.min(Math.max(volume, 0), 100)
}

export function clamp(value: number, lo: number, hi: number): number {
  return Math.min(Math.max(value, lo), hi)
}

/**
 * Confidence below which a rally is shown as an "uncertain region" — a span the
 * segmenter doubts, surfaced as "check this" during review (ADR 0002).
 */
export const UNCERTAIN_CONFIDENCE = 0.5

/**
 * How far into a rally the playhead must be for Prev to *restart* it rather than
 * step to the previous one — the music-player rule: one press rewinds to the
 * current rally's start, a second press (now within this slack of the start)
 * jumps to the previous rally.
 */
export const PREV_RESTART_SLACK_MS = 1000

/** A rally interval over one recording (see `TimelineRally` in `src-tauri/src/db.rs`). */
export interface Rally {
  id: number
  start_ms: number
  end_ms: number
  /** Per-region confidence in [0, 1]; low values are uncertain regions. */
  confidence: number
}

/** Result of the `recording_timeline` command (see `src-tauri/src/db.rs`). */
export interface Timeline {
  segment_state: "unknown" | "ready" | "failed"
  duration_ms: number | null
  rallies: Rally[]
  /** Downsampled audio waveform peaks in [0, 1] over the recording's duration. */
  waveform: number[]
}

/** One recording in the session playlist, in capture-time order. */
export interface PlaylistRecording {
  path: string
}

/**
 * One recording placed on the session-global time axis. `offsetMs` is the sum of
 * the durations of every recording before it, so a recording-local time `t` maps
 * to the session position `offsetMs + t`. `durationMs` is null until the
 * recording is segmented, and a recording with an unknown duration can't have
 * anything laid out after it.
 */
export interface SessionSegment {
  index: number
  path: string
  timeline: Timeline | null
  offsetMs: number
  durationMs: number | null
}

/** A rally lifted onto the session-global axis, carrying its owning recording so
 * an inline edit can be mapped back to that recording's local time and row id. */
export interface SessionRally {
  recordingIndex: number
  path: string
  id: number
  /** Recording-local bounds (what the edit commands expect). */
  localStart: number
  localEnd: number
  /** Session-global bounds (what the strip draws). */
  globalStart: number
  globalEnd: number
  confidence: number
}

/** The whole session stitched onto one continuous axis. */
export interface SessionModel {
  /** Recordings up to and including the first one with an unknown duration. */
  segments: SessionSegment[]
  /** How many recordings could be placed (have a known duration). */
  placedCount: number
  /** Total placed duration in ms — the length of the session strip. */
  totalMs: number
  /** Every placed recording's rallies, in session order. */
  rallies: SessionRally[]
}

/**
 * Stitch every recording's timeline onto one continuous session axis. Offsets
 * accumulate over the recordings whose duration is known; the first recording
 * with an unknown duration is included (so its "preparing" state shows) but
 * nothing after it can be placed, so layout stops there.
 */
export function buildSessionModel(
  recordings: PlaylistRecording[],
  timelines: Record<string, Timeline>
): SessionModel {
  const segments: SessionSegment[] = []
  let offset = 0
  let placedCount = 0
  for (let i = 0; i < recordings.length; i++) {
    const t = timelines[recordings[i].path] ?? null
    const durationMs = t?.duration_ms ?? null
    segments.push({
      index: i,
      path: recordings[i].path,
      timeline: t,
      offsetMs: offset,
      durationMs,
    })
    if (durationMs == null) break
    offset += durationMs
    placedCount += 1
  }
  const rallies: SessionRally[] = []
  for (const seg of segments) {
    if (seg.durationMs == null || !seg.timeline) continue
    for (const r of seg.timeline.rallies) {
      rallies.push({
        recordingIndex: seg.index,
        path: seg.path,
        id: r.id,
        localStart: r.start_ms,
        localEnd: r.end_ms,
        globalStart: seg.offsetMs + r.start_ms,
        globalEnd: seg.offsetMs + r.end_ms,
        confidence: r.confidence,
      })
    }
  }
  return { segments, placedCount, totalMs: offset, rallies }
}

/**
 * What gap-free playback should do as the playhead reaches `ms` (recording-local)
 * in the current recording (ADR 0001, the North Star). Pure so the orchestration
 * decision is unit-tested without mpv (issue #36):
 *
 * - `none`: inside a rally, or a manual move into a gap (free-play), or no
 *   timeline yet — let playback run.
 * - `{ seekMs }`: seek to that recording-local time — a rally-loop restart, or a
 *   jump to the next rally's start across a gap.
 * - `next-recording`: past this recording's last rally — cross into the next one.
 * - `stop`: past the session's final rally — nothing left to play.
 */
export type GapSkipAction =
  | { kind: "none" }
  | { kind: "seek"; ms: number }
  | { kind: "next-recording" }
  | { kind: "stop" }

export function gapSkipAction(
  rallies: Rally[],
  ms: number,
  looping: boolean,
  freePlay: boolean,
  atLastRecording: boolean
): GapSkipAction {
  if (rallies.length === 0) return { kind: "none" }
  // Rally-loop: reaching the current rally's end seeks back to its start, so a
  // single point replays. Loop overrides gap-skip but leaves free-play alone.
  if (looping && !freePlay) {
    const current = [...rallies].reverse().find((r) => r.start_ms <= ms)
    if (current && ms >= current.end_ms) {
      return { kind: "seek", ms: current.start_ms }
    }
    return { kind: "none" }
  }
  // Manual move into a gap → play it through, don't yank ahead.
  if (freePlay) return { kind: "none" }
  // Inside a rally → nothing to skip.
  if (rallies.some((r) => ms >= r.start_ms && ms < r.end_ms)) {
    return { kind: "none" }
  }
  const next = rallies.find((r) => r.start_ms > ms)
  if (next) return { kind: "seek", ms: next.start_ms }
  if (!atLastRecording) return { kind: "next-recording" }
  return { kind: "stop" }
}

/**
 * Where Next rally lands within the current recording, given the playhead `ms`:
 * the first rally starting after it, or `null` when none is left (the caller
 * crosses into the next recording then).
 */
export function nextRallyMs(rallies: Rally[], ms: number): number | null {
  const target = rallies.find((r) => r.start_ms > ms + 1)
  return target ? target.start_ms : null
}

/**
 * What Prev rally does within the current recording (the music-player rule):
 *
 * - `{ seekMs }`: restart the rally we're well into, or step back to the previous
 *   rally's start (or snap to the first rally when ahead of all of them).
 * - `prev-recording`: at/before the first rally → cross into the previous one.
 * - `none`: no rallies at all.
 */
export type PrevRallyAction =
  | { kind: "seek"; ms: number }
  | { kind: "prev-recording" }
  | { kind: "none" }

export function prevRallyAction(
  rallies: Rally[],
  ms: number,
  atFirstRecording: boolean
): PrevRallyAction {
  // The rally we're in or just past: the latest starting at or before the playhead.
  const current = [...rallies].reverse().find((r) => r.start_ms <= ms)
  // Played meaningfully into it → restart it (first press).
  if (current && ms > current.start_ms + PREV_RESTART_SLACK_MS) {
    return { kind: "seek", ms: current.start_ms }
  }
  // At/near its start (or ahead of every rally) → step to the previous one.
  const boundary = current ? current.start_ms : ms
  const target = [...rallies].reverse().find((r) => r.start_ms < boundary)
  if (target) return { kind: "seek", ms: target.start_ms }
  if (!atFirstRecording) return { kind: "prev-recording" }
  if (rallies.length > 0) return { kind: "seek", ms: rallies[0].start_ms }
  return { kind: "none" }
}

/**
 * The next uncertain region's session-global start after `hereMs`, wrapping to
 * the first when none is left ahead, so repeated presses cycle through every
 * doubt in the session (ADR 0002). `null` when there are no uncertain regions.
 */
export function nextUncertainMs(
  rallies: SessionRally[],
  hereMs: number
): number | null {
  const uncertain = rallies.filter((r) => r.confidence < UNCERTAIN_CONFIDENCE)
  if (uncertain.length === 0) return null
  const target =
    uncertain.find((r) => r.globalStart > hereMs + 1) ?? uncertain[0]
  return target.globalStart
}

/**
 * One persistence step of an inline correction — exactly the args of the
 * `update_rally` / `add_rally` / `delete_rally` commands (issue #7). The
 * component runs a plan's ops in order, so `command` is stripped off and the rest
 * is the invoke payload.
 */
export type RallyOp =
  | {
      command: "update_rally"
      path: string
      rallyId: number
      startMs: number
      endMs: number
    }
  | { command: "add_rally"; path: string; startMs: number; endMs: number }
  | { command: "delete_rally"; path: string; rallyId: number }

/**
 * The result of resolving an inline correction: `reject` when an integrity guard
 * fails (nothing is written), or an ordered list of ops to run. The coordinate
 * mapping and the guards that decide what reaches SQLite live here so they are
 * unit-tested without mpv or Tauri — the component only dispatches the ops.
 */
export type EditPlan = { kind: "reject" } | { kind: "ops"; ops: RallyOp[] }

/**
 * Adjust a rally's boundaries from a drag over the session strip. `globalStart`
 * and `globalEnd` are session-global and may arrive flipped (hence min/max);
 * `offset` maps them to recording-local time and `durationMs` caps them to the
 * recording (pass `Infinity` until the duration is known). Rejects a result of
 * zero or negative length.
 */
export function adjustRallyEdit(
  rally: Pick<SessionRally, "path" | "id">,
  globalStart: number,
  globalEnd: number,
  offset: number,
  durationMs: number
): EditPlan {
  const startMs = Math.round(
    clamp(Math.min(globalStart, globalEnd) - offset, 0, durationMs)
  )
  const endMs = Math.round(
    clamp(Math.max(globalStart, globalEnd) - offset, 0, durationMs)
  )
  if (endMs <= startMs) return { kind: "reject" }
  return {
    kind: "ops",
    ops: [
      { command: "update_rally", path: rally.path, rallyId: rally.id, startMs, endMs },
    ],
  }
}

/**
 * Add a rally centred on the playhead, spanning `halfMs` each side, clamped into
 * the recording (`durationMs` may be `Infinity` before it's known). `currentMs`
 * is recording-local.
 */
export function addAtPlayheadEdit(
  path: string,
  currentMs: number,
  durationMs: number,
  halfMs: number
): EditPlan {
  const startMs = Math.max(0, Math.round(currentMs - halfMs))
  const endMs = Math.round(clamp(currentMs + halfMs, startMs + 1, durationMs))
  return { kind: "ops", ops: [{ command: "add_rally", path, startMs, endMs }] }
}

/**
 * Split a rally at the session-global playhead `atGlobalMs`: shrink it to end at
 * the cut, then add a new rally from the cut to the old end. `offset` maps the cut
 * to recording-local time. Rejects a cut at or outside the rally's bounds.
 */
export function splitRallyEdit(
  rally: Pick<SessionRally, "path" | "id" | "localStart" | "localEnd">,
  atGlobalMs: number,
  offset: number
): EditPlan {
  const atLocal = Math.round(atGlobalMs - offset)
  if (atLocal <= rally.localStart || atLocal >= rally.localEnd) {
    return { kind: "reject" }
  }
  return {
    kind: "ops",
    ops: [
      {
        command: "update_rally",
        path: rally.path,
        rallyId: rally.id,
        startMs: rally.localStart,
        endMs: atLocal,
      },
      {
        command: "add_rally",
        path: rally.path,
        startMs: atLocal,
        endMs: rally.localEnd,
      },
    ],
  }
}

/**
 * Merge a rally with the next one in the same recording: stretch the first to
 * cover both, then delete the second. Rejects a cross-recording merge.
 */
export function mergeRallyEdit(
  first: Pick<SessionRally, "path" | "id" | "localStart" | "localEnd">,
  second: Pick<SessionRally, "path" | "id" | "localStart" | "localEnd">
): EditPlan {
  if (first.path !== second.path) return { kind: "reject" }
  return {
    kind: "ops",
    ops: [
      {
        command: "update_rally",
        path: first.path,
        rallyId: first.id,
        startMs: Math.min(first.localStart, second.localStart),
        endMs: Math.max(first.localEnd, second.localEnd),
      },
      { command: "delete_rally", path: first.path, rallyId: second.id },
    ],
  }
}
