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
 * The ladder index closest to a speed value mpv reports back (issue #42): the UI
 * tracks an index into `SPEED_LADDER`, but mpv's `speed` property is a raw
 * multiplier. We only ever set ladder values, so this normally hits one exactly;
 * the nearest-match fallback keeps the indicator sane if mpv ever reports an
 * off-ladder speed (e.g. a future external speed change).
 */
export function speedIndexForValue(speed: number): number {
  let best = 0
  let bestDiff = Infinity
  for (let i = 0; i < SPEED_LADDER.length; i++) {
    const diff = Math.abs(SPEED_LADDER[i] - speed)
    if (diff < bestDiff) {
      bestDiff = diff
      best = i
    }
  }
  return best
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
 * The clamped `scrollLeft` a programmatic strip-scroll should land on, or `null`
 * when it already equals the current offset and so must be skipped.
 *
 * Skipping the no-op is the whole point: a `scrollLeft` assignment that does not
 * change the value fires **no `scroll` event**, so the "this scroll was ours"
 * guard the caller arms before the write is never cleared and stays true — the
 * next *manual* scroll then reads as programmatic and silently fails to disarm
 * follow. This bites hardest near a recording's start, where centring on a
 * playhead at the far left clamps the target to 0 while the strip is already at
 * 0: every tick re-arms the guard and eats the user's attempt to scroll ahead.
 * Clamping here to the same range the browser would lets the caller detect that
 * no-op before arming the guard.
 */
export function stripScrollTarget(
  targetPx: number,
  currentScrollLeft: number,
  clientWidth: number,
  scrollWidth: number
): number | null {
  const max = Math.max(0, scrollWidth - clientWidth)
  const next = clamp(Math.round(targetPx), 0, max)
  return next === Math.round(currentScrollLeft) ? null : next
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
  /** Whether the user flagged this rally as one that matters (issue #10). */
  flagged: boolean
}

/**
 * Presentation padding (issue #104): stored rallies are strict — serve contact to
 * the shuttle going down — and playback widens each by a lead-in and a tail so the
 * serve setup and the landing are watched. Mirrors `db::Padding`; export applies
 * the same setting in Rust.
 */
export interface Padding {
  lead_ms: number
  tail_ms: number
}

/** No widening: the strict rallies exactly as stored. */
export const NO_PADDING: Padding = { lead_ms: 0, tail_ms: 0 }

/** The app's default, until the stored setting loads — `db::DEFAULT_PADDING`. */
export const DEFAULT_PADDING: Padding = { lead_ms: 1000, tail_ms: 1500 }

/**
 * Where a strict rally's padded lead-in opens, recording-local: `pad.lead_ms`
 * before its start, never before the start of its own recording (#104). The one
 * place the lead-in is clamped — play intervals, session lead-ins and the moment
 * browser's jump all read it.
 */
export function leadInMs(startMs: number, pad: Padding): number {
  return Math.max(0, startMs - pad.lead_ms)
}

/**
 * The intervals playback actually plays: each strict rally widened by `pad`,
 * clamped to the recording, and merged where the padding makes neighbours
 * overlap. Input and output are start-ascending. A merged interval keeps the first
 * rally's id/confidence/flag — navigation reads only the edges.
 */
export function playIntervals(
  rallies: Rally[],
  pad: Padding,
  durationMs: number | null = null
): Rally[] {
  const out: Rally[] = []
  for (const r of rallies) {
    const start = leadInMs(r.start_ms, pad)
    const padded = r.end_ms + pad.tail_ms
    const end = durationMs != null ? Math.min(durationMs, padded) : padded
    const last = out[out.length - 1]
    if (last && start <= last.end_ms) {
      out[out.length - 1] = { ...last, end_ms: Math.max(last.end_ms, end) }
    } else {
      out.push({ ...r, start_ms: start, end_ms: end })
    }
  }
  return out
}

/**
 * Where playback opens a session rally (#104): its padded lead-in on the
 * session axis, clamped to the start of the rally's own recording, as
 * `playIntervals` clamps it, so the lead-in never reaches into the previous
 * recording's tail.
 */
export function rallyLeadInMs(r: SessionRally, pad: Padding): number {
  const recordingStart = r.globalStart - r.localStart
  return recordingStart + leadInMs(r.localStart, pad)
}

/**
 * How far past a rally's strict start correction mode opens it. An exact seek
 * lands on the frame shown at the target, whose timestamp sits up to a frame
 * *before* it — on the start itself that's the lead-in, outside the rally, and
 * Split would refuse. 50 ms clears one frame down to 20 fps while still
 * showing the boundary frame.
 */
export const RALLY_START_INSET_MS = 50

/**
 * Where correction mode opens a session rally: its true start (not the clip
 * padding before it), nudged inside by `RALLY_START_INSET_MS` — but never past
 * the rally's midpoint, so a very short rally is still entered.
 */
export function rallyEditSeekMs(
  r: Pick<SessionRally, "globalStart" | "globalEnd">
): number {
  const mid = (r.globalStart + r.globalEnd) / 2
  return Math.min(r.globalStart + RALLY_START_INSET_MS, mid)
}

/**
 * How far a rally's padding reaches on the session axis (#104) — the span the
 * strip draws behind its strict block: `pad.lead_ms` before and `pad.tail_ms`
 * after, clamped into its own recording `[recordingStart, recordingEnd]`, as
 * `playIntervals` clamps them. Takes bare bounds so a live edge-drag can be
 * padded too.
 */
export function paddedSpanMs(
  globalStart: number,
  globalEnd: number,
  pad: Padding,
  recordingStart: number,
  recordingEnd: number
): { start: number; end: number } {
  return {
    start: Math.max(recordingStart, globalStart - pad.lead_ms),
    end: Math.min(recordingEnd, globalEnd + pad.tail_ms),
  }
}

/** A play interval (see `playIntervals`) lifted onto the session-global axis. */
export interface SessionPlayInterval {
  recordingIndex: number
  globalStart: number
  globalEnd: number
}

/** A strict rally span on the session axis, with its recording's bounds. */
export interface RallySpan {
  recordingIndex: number
  globalStart: number
  globalEnd: number
  recordingStart: number
  recordingEnd: number
}

/**
 * The play intervals of arbitrary strict spans — what the strip draws as each
 * segment's box. Each span is padded (`paddedSpanMs`, clamped into its
 * recording) and merged with its neighbour where the padding makes them touch,
 * never across a recording boundary: the same rule as `playIntervals`, so the
 * boxes are exactly what playback and `[`/`]` step through. Takes bare spans so
 * a live edge-drag reshapes its box as it moves. Ascending.
 */
export function paddedPlayIntervals(
  spans: RallySpan[],
  pad: Padding
): SessionPlayInterval[] {
  const sorted = [...spans].sort(
    (a, b) =>
      a.recordingIndex - b.recordingIndex || a.globalStart - b.globalStart
  )
  const out: SessionPlayInterval[] = []
  for (const s of sorted) {
    const { start, end } = paddedSpanMs(
      s.globalStart,
      s.globalEnd,
      pad,
      s.recordingStart,
      s.recordingEnd
    )
    const last = out[out.length - 1]
    if (
      last &&
      last.recordingIndex === s.recordingIndex &&
      start <= last.globalEnd
    ) {
      last.globalEnd = Math.max(last.globalEnd, end)
    } else {
      out.push({
        recordingIndex: s.recordingIndex,
        globalStart: start,
        globalEnd: end,
      })
    }
  }
  return out
}

/**
 * The whole session's play intervals on the session axis (#104): every placed
 * recording's `playIntervals` — its strict rallies padded, clamped to *that*
 * recording and merged where they overlap — offset onto the strip. A rally's
 * tail never spills into the next recording, nor its lead-in into the previous
 * one, so a moment just past a recording's end is the next recording's gap, not
 * the last rally's tail. Ascending; intervals never cross a recording boundary.
 */
export function sessionPlayIntervals(
  session: SessionModel,
  pad: Padding
): SessionPlayInterval[] {
  const out: SessionPlayInterval[] = []
  for (const seg of session.segments) {
    if (seg.durationMs == null || !seg.timeline) continue
    for (const r of playIntervals(seg.timeline.rallies, pad, seg.durationMs)) {
      out.push({
        recordingIndex: seg.index,
        globalStart: seg.offsetMs + r.start_ms,
        globalEnd: seg.offsetMs + r.end_ms,
      })
    }
  }
  return out
}

/**
 * Whether session-global `globalMs` is in a padded play interval — playback
 * would play it rather than skip it. Half-open, like a rally: an interval's end
 * is already outside it.
 */
export function inPlayInterval(
  intervals: SessionPlayInterval[],
  globalMs: number
): boolean {
  return intervals.some(
    (i) => globalMs >= i.globalStart && globalMs < i.globalEnd
  )
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
 * The three verdicts an annotation can carry (GLOSSARY.md): the one-keystroke
 * classification captured in the moment. `mistake` is an outright, point-ending
 * unforced error; `bad` is suboptimal but not point-ending; `good` is well done.
 */
export const VERDICTS = ["good", "bad", "mistake"] as const
export type Verdict = (typeof VERDICTS)[number]

/**
 * The seeded aspect vocabulary (GLOSSARY.md, issue #9): the dimension a verdict
 * judges, a structured filterable field. This slice exposes the seeded list only;
 * adding custom aspects from settings is deferred. Stored as free text in the DB
 * (a user-editable vocabulary, not a fixed enum), so an annotation may carry an
 * aspect outside this list without breaking.
 */
export const ASPECTS = [
  "selection",
  "execution",
  "deception",
  "footwork",
  "positioning",
] as const
export type Aspect = (typeof ASPECTS)[number]

/**
 * A verdict annotation pinned to a recording-local timestamp (issue #8, matching
 * `Annotation` in `src-tauri/src/db.rs`). The rally it belongs to is implied by
 * which rally's range contains `time_ms`, not stored. Enriched (issue #9) with an
 * optional `aspect` and free-text `note`; both null until the user sets them.
 */
export interface Annotation {
  id: number
  time_ms: number
  verdict: Verdict
  aspect: string | null
  note: string | null
}

/** An annotation lifted onto the session-global axis for the timeline strip. */
export interface SessionAnnotation {
  id: number
  recordingIndex: number
  path: string
  verdict: Verdict
  aspect: string | null
  note: string | null
  /** Session-global timestamp (what the strip draws). */
  globalMs: number
}

/**
 * Lift every placed recording's annotations onto the session-global axis (issue
 * #8), so their markers sit at the right spot on the one continuous strip. A
 * recording that isn't placed yet (unknown duration) contributes nothing, since
 * it has no offset. Ordered by global timestamp.
 */
export function buildSessionAnnotations(
  session: SessionModel,
  annotations: Record<string, Annotation[]>
): SessionAnnotation[] {
  const out: SessionAnnotation[] = []
  for (const seg of session.segments) {
    if (seg.durationMs == null) continue
    for (const a of annotations[seg.path] ?? []) {
      out.push({
        id: a.id,
        recordingIndex: seg.index,
        path: seg.path,
        verdict: a.verdict,
        aspect: a.aspect,
        note: a.note,
        globalMs: seg.offsetMs + a.time_ms,
      })
    }
  }
  return out.sort((a, b) => a.globalMs - b.globalMs)
}

/**
 * A rally returned by the cross-session filter (issue #11), matching
 * `FilteredRally` in `src-tauri/src/db.rs`. Carries enough context to identify
 * it and jump to it — the containing recording's `recording_path` opened at
 * `start_ms`, named by `capture_day`. `annotations` holds the matching moments
 * when the filter carried a verdict/aspect, empty for a length/flag-only filter.
 */
export interface FilteredRally {
  session_id: number
  capture_day: string
  recording_id: number
  recording_path: string
  rally_id: number
  start_ms: number
  end_ms: number
  /** Derived from duration (GLOSSARY.md), never from quality. */
  long: boolean
  flagged: boolean
  annotations: Annotation[]
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
  /** Whether the user flagged this rally as one that matters (issue #10). */
  flagged: boolean
}

/**
 * A rally's stable identity across re-renders and timeline refreshes: row ids
 * are only unique within their recording, so the key pairs the id with the path.
 */
export function rallyKey(r: Pick<SessionRally, "path" | "id">): string {
  return `${r.path}:${r.id}`
}

/**
 * Whether session-global `globalMs` falls inside a rally's strict span — the
 * half-open `[globalStart, globalEnd)`, so the instant a rally ends belongs to
 * the gap after it. The one "rally containing this moment" rule (glossary: a
 * rally owns the annotations in its span); padding never widens it, so the
 * playhead in a lead-in or tail is in no rally.
 */
export function rallyContains(r: SessionRally, globalMs: number): boolean {
  return globalMs >= r.globalStart && globalMs < r.globalEnd
}

/**
 * The index of the rally containing session-global `globalMs` (the first, if
 * hand edits made rallies overlap), or -1 in a gap.
 */
export function rallyIndexAt(
  rallies: SessionRally[],
  globalMs: number
): number {
  return rallies.findIndex((r) => rallyContains(r, globalMs))
}

/** The rally containing session-global `globalMs`, or `null` in a gap. */
export function rallyAt(
  rallies: SessionRally[],
  globalMs: number
): SessionRally | null {
  return rallies[rallyIndexAt(rallies, globalMs)] ?? null
}

/**
 * Which rally the review surfaces (rail highlight, inspector, flag and verdict
 * keys) are about with the playhead at session-global `globalMs`:
 *
 * - `live`: inside a rally's strict span, or its clip padding (ADR 0020) — the
 *   lead-in before it or the tail after it, clamped to its recording. Playback
 *   is showing that rally, so it counts as that rally. Where one rally's tail
 *   meets the next one's lead-in, the nearer strict span wins.
 * - `after`: in a gap — the most recent rally to end, so what was just marked
 *   stays in view until the next rally's lead-in opens.
 * - `none`: before the first rally (or there are none); `index` is -1.
 *
 * `nextIndex` is the rally that starts next (-1 if none follows), for the gap's
 * "Rally 13 in 2s" and the rail's up-next marker; only set when not `live`.
 * Ownership of annotations is untouched: that stays the strict span
 * (`rallyContains`).
 */
export interface RallyFocus {
  kind: "live" | "after" | "none"
  index: number
  nextIndex: number
}

export function rallyFocusAt(
  session: SessionModel,
  pad: Padding,
  globalMs: number
): RallyFocus {
  const { rallies } = session
  const strict = rallyIndexAt(rallies, globalMs)
  if (strict >= 0) return { kind: "live", index: strict, nextIndex: -1 }

  let padded = -1
  let paddedDist = Number.POSITIVE_INFINITY
  let after = -1
  let next = -1
  for (let i = 0; i < rallies.length; i++) {
    const r = rallies[i]
    const recordingStart = r.globalStart - r.localStart
    const durationMs =
      session.segments.find((s) => s.index === r.recordingIndex)?.durationMs ??
      Number.POSITIVE_INFINITY
    const { start, end } = paddedSpanMs(
      r.globalStart,
      r.globalEnd,
      pad,
      recordingStart,
      recordingStart + durationMs
    )
    if (globalMs >= start && globalMs < end) {
      const dist =
        globalMs < r.globalStart
          ? r.globalStart - globalMs
          : globalMs - r.globalEnd
      if (dist < paddedDist) {
        padded = i
        paddedDist = dist
      }
    }
    if (
      r.globalEnd <= globalMs &&
      (after < 0 || r.globalEnd > rallies[after].globalEnd)
    ) {
      after = i
    }
    if (
      r.globalStart > globalMs &&
      (next < 0 || r.globalStart < rallies[next].globalStart)
    ) {
      next = i
    }
  }
  if (padded >= 0) return { kind: "live", index: padded, nextIndex: -1 }
  // Just short of the next rally's lead-in, by no more than a seek's frame:
  // that's where a jump to the lead-in lands, so it's that rally's.
  if (next >= 0) {
    const r = rallies[next]
    const recordingStart = r.globalStart - r.localStart
    const leadIn = Math.max(recordingStart, r.globalStart - pad.lead_ms)
    if (leadIn > recordingStart && globalMs >= leadIn - FOCUS_SEEK_SLACK_MS) {
      return { kind: "live", index: next, nextIndex: -1 }
    }
  }
  return after >= 0
    ? { kind: "after", index: after, nextIndex: next }
    : { kind: "none", index: -1, nextIndex: next }
}

/**
 * How far short of a rally's lead-in the playhead still counts as in it
 * (`rallyFocusAt`). A seek is exact but lands on the frame shown at the
 * target, whose timestamp sits up to a frame *before* it — so a jump to a
 * lead-in can report a moment in the gap just ahead of it, and the review
 * surfaces would flip back to the previous rally when that report arrives
 * (often on an unrelated redraw, like toggling Skip gaps). One
 * `RALLY_START_INSET_MS` absorbs that frame. Never applied where the lead-in
 * is clamped to its recording's start: a seek can't land before a file's 0.
 */
export const FOCUS_SEEK_SLACK_MS = RALLY_START_INSET_MS

/**
 * Where a rally-targeted jump (a rail click, `[`/`]`, Next unmarked, Loop)
 * opens a rally: while playing, on its lead-in — that's what clip padding is
 * for; while paused or correcting the timeline, on its true start nudged
 * inside (`rallyEditSeekMs`), so the playhead is in the rally and a verdict
 * pressed right away is that rally's, and Split can act.
 */
export function rallyOpenMs(
  r: SessionRally,
  pad: Padding,
  { atStrictStart }: { atStrictStart: boolean }
): number {
  return atStrictStart ? rallyEditSeekMs(r) : rallyLeadInMs(r, pad)
}

/**
 * Which rally `[` / `]` step to from session-global `globalMs`, rally by rally
 * (the rail's rows) — for when the jump opens the rally on its true start
 * (`rallyOpenMs`) rather than walking the merged play intervals. Next: the
 * rally after the one in focus, or the one up next from a gap. Prev, the
 * music-player rule: well into a rally (or in the gap after it) restarts it,
 * near its start steps to the one before; before the first rally it snaps to
 * the first. -1 when there's nowhere to go.
 */
export function rallyStepIndex(
  session: SessionModel,
  pad: Padding,
  globalMs: number,
  direction: "next" | "prev"
): number {
  const n = session.rallies.length
  if (n === 0) return -1
  const focus = rallyFocusAt(session, pad, globalMs)
  if (direction === "next") {
    if (focus.kind === "live") return focus.index + 1 < n ? focus.index + 1 : -1
    return focus.nextIndex
  }
  if (focus.kind === "none") return 0
  const r = session.rallies[focus.index]
  if (globalMs > r.globalStart + PREV_RESTART_SLACK_MS) return focus.index
  return Math.max(0, focus.index - 1)
}

/**
 * How many rallies own at least one annotation — the review's progress
 * ("5/18 marked"), on the same rule as the session list's: a rally owns only
 * the marks in its strict span, so a mark in a gap counts for none.
 */
export function markedRallyCount(
  rallies: SessionRally[],
  annotations: SessionAnnotation[]
): number {
  return annotationsByRally(rallies, annotations).size
}

/**
 * The next rally after `fromIndex` that owns no annotation yet, wrapping
 * round to the session's start so repeated presses walk every unmarked rally;
 * -1 once every rally is marked. `fromIndex` -1 starts from the first rally.
 */
export function nextUnmarkedIndex(
  rallies: SessionRally[],
  annotations: SessionAnnotation[],
  fromIndex: number
): number {
  const owned = annotationsByRally(rallies, annotations)
  const n = rallies.length
  for (let step = 1; step <= n; step++) {
    const i = (((fromIndex + step) % n) + n) % n
    if (!owned.has(rallyKey(rallies[i]))) return i
  }
  return -1
}

/** "2 corrected rallies and 1 flag" — `handCorrections` in words, or null if none. */
export function describeHandCorrections({
  corrected,
  flagged,
}: HandCorrections): string | null {
  const parts = [
    corrected > 0
      ? `${corrected} corrected ${corrected === 1 ? "rally" : "rallies"}`
      : null,
    flagged > 0 ? `${flagged} ${flagged === 1 ? "flag" : "flags"}` : null,
  ].filter((p) => p !== null)
  return parts.length > 0 ? parts.join(" and ") : null
}

/**
 * Where a moment sits relative to the rallies, for saying where a verdict lands:
 * `in` a rally's strict span (that rally owns it), or in a gap — `before` the
 * rally whose lead-in it is in (or the next one), `after` the rally whose tail
 * or following gap it is in — or `none` with no rallies around. `padded` says
 * the moment is in that rally's clip padding (its lead-in or tail) rather than
 * the open gap. Read off `rallyFocusAt`, so it names the rally the inspector is
 * showing. A moment outside every strict span belongs to no rally (glossary:
 * a rally owns only the annotations in its strict span); this only says which
 * rally it's next to.
 */
export type MomentPlace =
  | { kind: "in"; index: number }
  | { kind: "before" | "after"; index: number; padded: boolean }
  | { kind: "none" }

export function momentPlace(
  session: SessionModel,
  pad: Padding,
  globalMs: number
): MomentPlace {
  const strict = rallyIndexAt(session.rallies, globalMs)
  if (strict >= 0) return { kind: "in", index: strict }
  const focus = rallyFocusAt(session, pad, globalMs)
  if (focus.kind === "live") {
    const r = session.rallies[focus.index]
    return {
      kind: globalMs < r.globalStart ? "before" : "after",
      index: focus.index,
      padded: true,
    }
  }
  if (focus.kind === "after") {
    return { kind: "after", index: focus.index, padded: false }
  }
  return focus.nextIndex >= 0
    ? { kind: "before", index: focus.nextIndex, padded: false }
    : { kind: "none" }
}

/**
 * Where a mark at `place` lands, receiver first: the rally that will own it
 * ("Rally 3"), else "Gap" — a moment outside every strict span belongs to no
 * rally, so a gap never claims one — with the rally it's next to as context
 * ("Rally 3 lead-in", "Rally 2 tail", "before Rally 1", "after Rally 2").
 */
export interface MomentLanding {
  /** "Rally 3" when a rally owns the moment, else "Gap". */
  receiver: string
  /** The rally it sits beside, when in a gap; null inside a rally. */
  context: string | null
  /** Whether a rally owns the moment. */
  owned: boolean
}

export function momentLanding(place: MomentPlace): MomentLanding {
  if (place.kind === "none") {
    return { receiver: "Gap", context: null, owned: false }
  }
  const name = `Rally ${place.index + 1}`
  if (place.kind === "in") return { receiver: name, context: null, owned: true }
  const context = place.padded
    ? `${name} ${place.kind === "before" ? "lead-in" : "tail"}`
    : `${place.kind} ${name}`
  return { receiver: "Gap", context, owned: false }
}

/**
 * `momentPlace` in words, receiver first and without a time — what a burst of
 * marks is confirmed against: "Rally 3", "Gap (Rally 3 lead-in)",
 * "Gap (after Rally 2)".
 */
export function describeMomentPlace(place: MomentPlace): string {
  const { receiver, context } = momentLanding(place)
  return context ? `${receiver} (${context})` : receiver
}

/**
 * Where a verdict pressed now would land, in the one wording the inspector's
 * slot, its verdict buttons and the held-rally chip share — receiver first,
 * then the exact time it's pinned to (`clock`), then the context in a gap:
 * "→ Rally 13 · 3:36", "→ Gap · 3:34 (Rally 13 lead-in)".
 */
export function describeLanding(place: MomentPlace, clock: string): string {
  const { receiver, context } = momentLanding(place)
  return `→ ${receiver} · ${clock}${context ? ` (${context})` : ""}`
}

/** One verdict in a `MarkBurst`: the annotation's own id, so Undo deletes it. */
export interface BurstMark {
  path: string
  id: number
  verdict: Verdict
  /** Session-global timestamp it was pinned to (never moved). */
  globalMs: number
}

/**
 * The verdicts just dropped in quick succession on one place — confirmed
 * together ("Good · Bad · Mistake → Rally 1") rather than only the last, so
 * pressing 1, 2, 3 mid-play acknowledges all three.
 */
export interface MarkBurst {
  /** Where they landed, in words (`describeMomentPlace`). */
  place: string
  /** In press order; never empty. */
  marks: BurstMark[]
}

/**
 * Add a mark to the confirmation burst: it joins the shown burst when it landed
 * on the same place, else starts a new one. The caller drops the burst once its
 * confirmation times out, so a stale burst is never extended.
 */
export function extendMarkBurst(
  burst: MarkBurst | null,
  place: string,
  mark: BurstMark
): MarkBurst {
  return burst && burst.place === place
    ? { place, marks: [...burst.marks, mark] }
    : { place, marks: [mark] }
}

/**
 * Keep only the burst's marks that still exist (Undo, or a delete from the
 * annotation list, may have removed some); null once none are left.
 */
export function pruneMarkBurst(
  burst: MarkBurst | null,
  exists: (mark: BurstMark) => boolean
): MarkBurst | null {
  if (!burst) return null
  const marks = burst.marks.filter(exists)
  if (marks.length === burst.marks.length) return burst
  return marks.length > 0 ? { ...burst, marks } : null
}

const VERDICT_WORD: Record<Verdict, string> = {
  good: "Good",
  bad: "Bad",
  mistake: "Mistake",
}

/**
 * A burst's verdicts in words, in verdict order with repeats counted:
 * "Good · Bad · Mistake", "Good ×2 · Mistake".
 */
export function describeMarkBurst(burst: MarkBurst): string {
  return VERDICTS.map((v) => {
    const n = burst.marks.filter((m) => m.verdict === v).length
    if (n === 0) return null
    return n > 1 ? `${VERDICT_WORD[v]} ×${n}` : VERDICT_WORD[v]
  })
    .filter((s) => s !== null)
    .join(" · ")
}

/**
 * Whole seconds until session-global `targetMs`, rounded **up**, so a countdown
 * never reads "0s" while there is still time to wait (it reaches 0 only at the
 * moment itself). Never negative.
 */
export function secondsUntil(fromMs: number, targetMs: number): number {
  return Math.max(0, Math.ceil((targetMs - fromMs) / 1000))
}

/**
 * The confidence an inline correction stamps on a rally (`db::CORRECTED_CONFIDENCE`):
 * every machine draft stays strictly below it, so a rally at it was put there by
 * hand (adjusted, split, merged or added).
 */
export const CORRECTED_CONFIDENCE = 1

/**
 * What a re-analysis of this timeline would throw away (ADR 0002): re-analyze
 * discards every rally, so hand-corrected boundaries and flags go with them. The
 * same "hand-touched" reading as `db::is_hand_touched`, minus annotations —
 * those are pinned to time, not to a rally, so a re-analysis keeps them. Errs
 * toward caution: v6 can clamp a fully-active span to the sentinel too, so one of
 * its drafts may count a rally as corrected that nobody touched.
 */
export interface HandCorrections {
  corrected: number
  flagged: number
}

export function handCorrections(
  timeline: Timeline | null | undefined
): HandCorrections {
  let corrected = 0
  let flagged = 0
  for (const r of timeline?.rallies ?? []) {
    if (r.confidence >= CORRECTED_CONFIDENCE) corrected += 1
    if (r.flagged) flagged += 1
  }
  return { corrected, flagged }
}

/**
 * Each annotation grouped under the rally that owns it (`rallyAt` its moment),
 * keyed by `rallyKey`. Annotations in a gap belong to no rally and are dropped;
 * each group keeps the input order.
 */
export function annotationsByRally(
  rallies: SessionRally[],
  annotations: SessionAnnotation[]
): Map<string, SessionAnnotation[]> {
  const out = new Map<string, SessionAnnotation[]>()
  for (const a of annotations) {
    const rally = rallyAt(rallies, a.globalMs)
    if (!rally) continue
    const key = rallyKey(rally)
    const group = out.get(key)
    if (group) group.push(a)
    else out.set(key, [a])
  }
  return out
}

/**
 * Annotations folded into one row per verdict per displayed second — the
 * inspector's list, where five Goods pressed at 0:20 read as "Good ×5 · 0:20"
 * rather than five identical rows. Keyed by verdict and whole session second
 * (what `formatClock` shows), in order of first appearance, so a time-ordered
 * input gives time-ordered groups; each group keeps its marks in input order.
 */
export interface AnnotationGroup {
  /** Stable across renders: verdict and whole session second. */
  key: string
  verdict: Verdict
  /** The first mark's session-global time. */
  globalMs: number
  /** Never empty. */
  marks: SessionAnnotation[]
}

export function groupAnnotations(
  annotations: SessionAnnotation[]
): AnnotationGroup[] {
  const groups = new Map<string, AnnotationGroup>()
  for (const a of annotations) {
    const key = `${a.verdict}@${Math.floor(a.globalMs / 1000)}`
    const group = groups.get(key)
    if (group) group.marks.push(a)
    else
      groups.set(key, {
        key,
        verdict: a.verdict,
        globalMs: a.globalMs,
        marks: [a],
      })
  }
  return [...groups.values()]
}

/** Per-verdict tallies of a list of annotations (zero for a verdict absent). */
export function countVerdicts(
  annotations: Pick<SessionAnnotation, "verdict">[]
): Record<Verdict, number> {
  const counts: Record<Verdict, number> = { good: 0, bad: 0, mistake: 0 }
  for (const a of annotations) counts[a.verdict] += 1
  return counts
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
        flagged: r.flagged,
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
  { kind: "seek"; ms: number } | { kind: "prev-recording" } | { kind: "none" }

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
 * Where to resume once the playlist crosses into a recording: its first rally
 * (advancing forward), its last rally (stepping back via Prev), or a specific
 * recording-local time (a click on the session strip that landed in another
 * recording).
 */
export type Resume = "start" | "end" | { atMs: number }

/**
 * The recording-local time a crossing should open the new recording at, resolved
 * at load time so it can be baked into the load itself (mpv applies it on
 * `FILE_LOADED`, atomic with opening the file — no load/seek race). A specific
 * `{ atMs }` always resolves; `start`/`end` need the recording's rallies, so they
 * return `null` until its timeline arrives (the recording then plays from the top
 * until the deferred resume seeks). `end` resumes at the *last* rally's start.
 */
export function resumeStartMs(resume: Resume, rallies: Rally[]): number | null {
  if (typeof resume === "object") return resume.atMs
  if (rallies.length === 0) return null
  const target = resume === "start" ? rallies[0] : rallies[rallies.length - 1]
  return target.start_ms
}

/**
 * Whether a `time-pos` tick at recording-local `ms` shows the playhead has
 * reached a boundary-crossing resume `target` (within `tolMs`) — the *second*
 * gate after a crossing. The first gate (`mpv:file-loaded`) drops every tick from
 * the recording being left, so by the time this runs the tick is guaranteed to be
 * from the freshly-loaded file; this only has to drop its near-0 pre-seek ticks
 * (mpv's resume seek is async, so it lands a moment after the file opens). The
 * seek is exact, so the first settled tick sits at the target; the slack clears
 * the near-0 lead-in. One-sided (anything at/past the target counts as landed) so
 * an overshooting first tick can't strand the gate.
 */
export function resumeTickLanded(
  ms: number,
  target: number,
  tolMs: number
): boolean {
  return ms + tolMs >= target
}

/**
 * Whether a `time-pos` tick at recording-local `ms` shows an in-flight
 * within-recording seek has settled at its `target`. Ticks between issuing
 * `mpv_seek` and the seek applying still carry the *pre-seek* position (the
 * event stream and the invoke are both async), and acting on one runs gap-skip
 * against a position the user just left — a stale tick in a gap yanks the
 * playhead to the rally after the *old* position, and one past the session's
 * last rally pauses mid-scrub. Unlike a crossing's resume (one-sided, near-0
 * lead-in only), a seek's stale ticks can lie on either side of the target, so
 * this is a two-sided window. The primary "seek done" signal is
 * `mpv:playback-restart`; this is the belt that also clears the gate if a
 * settled tick arrives first (or a restart is ever missed), so the gate cannot
 * stick and freeze the playhead.
 */
export function seekTickSettled(
  ms: number,
  target: number,
  tolMs: number
): boolean {
  return Math.abs(ms - target) <= tolMs
}

/**
 * Whether a user-initiated resume (pause → play) at recording-local `ms` is a
 * free-play intent. Past the session's final rally, gap-skip's end-of-session
 * stop pauses playback on *every* tick — so without this, pressing play there
 * advances one frame and is instantly re-paused, forever. Re-pressing play
 * after that stop means "let me watch the tail anyway": the same manual
 * opt-out as dragging into a gap.
 */
export function resumeEntersFreePlay(
  rallies: Rally[],
  ms: number,
  atLastRecording: boolean
): boolean {
  if (!atLastRecording || rallies.length === 0) return false
  return ms >= rallies[rallies.length - 1].end_ms
}

/**
 * The next uncertain region's session-global start after `hereMs`, wrapping to
 * the first when none is left ahead, so repeated presses cycle through every
 * doubt in the session (ADR 0002). `null` when there are no uncertain regions.
 */
export function nextUncertainMs(
  rallies: SessionRally[],
  hereMs: number,
  pad: Padding = NO_PADDING
): number | null {
  const uncertain = rallies.filter((r) => r.confidence < UNCERTAIN_CONFIDENCE)
  if (uncertain.length === 0) return null
  // Compare lead-ins, not strict starts: after a jump the playhead sits on the
  // rally's lead-in, before its start, and must not find the same rally again.
  const target =
    uncertain.find((r) => rallyLeadInMs(r, pad) > hereMs + 1) ?? uncertain[0]
  return rallyLeadInMs(target, pad)
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
      {
        command: "update_rally",
        path: rally.path,
        rallyId: rally.id,
        startMs,
        endMs,
      },
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

/** What the split/merge controls may do for an edit target (see `editAffordances`). */
export interface EditAffordances {
  /** The rally an edit acts on, or `null` when there is none. */
  target: SessionRally | null
  /** The rally a merge folds into `target`: the next one, same recording only. */
  mergeTarget: SessionRally | null
  /** Whether splitting `target` at the playhead would be accepted. */
  canSplit: boolean
  /** Whether merging `target` with `mergeTarget` would be accepted. */
  canMerge: boolean
}

/**
 * Which inline edits the rally at `targetIndex` affords right now (issue #7):
 * split at the session-global playhead, and merge with the next rally. Read off
 * `splitRallyEdit`/`mergeRallyEdit` themselves, so a control is enabled exactly
 * when its edit would pass the integrity guards — the hotkeys and the toolbar
 * both ask here. `targetIndex` -1 (no target) affords nothing.
 */
export function editAffordances(
  rallies: SessionRally[],
  targetIndex: number,
  playheadMs: number | null
): EditAffordances {
  const target = rallies[targetIndex] ?? null
  if (!target) {
    return { target, mergeTarget: null, canSplit: false, canMerge: false }
  }
  const next = rallies[targetIndex + 1] ?? null
  const mergeTarget =
    next && mergeRallyEdit(target, next).kind === "ops" ? next : null
  // The rally's own recording offset: its global start minus its local one.
  const offset = target.globalStart - target.localStart
  const canSplit =
    playheadMs !== null &&
    splitRallyEdit(target, playheadMs, offset).kind === "ops"
  return { target, mergeTarget, canSplit, canMerge: mergeTarget !== null }
}

/** Why each edit-toolbar action is unavailable, or null when it's available. */
export interface EditBlockedReasons {
  split: string | null
  merge: string | null
  delete: string | null
}

/**
 * The reason a disabled edit control gives (issue 9: a greyed button must say
 * why, and how to enable it). `rallyNumber` is the selected rally's 1-based
 * session-wide number, or null when nothing is selected; `canSplit`/`canMerge`
 * come from `editAffordances`.
 */
export function editBlockedReasons(
  rallyNumber: number | null,
  canSplit: boolean,
  canMerge: boolean
): EditBlockedReasons {
  if (rallyNumber === null) {
    const pick = "Select a rally first — click it on the strip or in the rail"
    return {
      split: `${pick} to split it.`,
      merge: `${pick} to merge it.`,
      delete: `${pick} to delete it.`,
    }
  }
  return {
    split: canSplit
      ? null
      : `Move the playhead inside Rally ${rallyNumber} to split it there.`,
    merge: canMerge
      ? null
      : `Rally ${rallyNumber} has no next rally in the same recording to merge with.`,
    delete: null,
  }
}
