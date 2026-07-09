"use client"

import { useCallback, useEffect, useMemo, useRef, useState } from "react"
import { listen } from "@tauri-apps/api/event"

import { trackedInvoke } from "@/lib/tauri"
import {
  clamp,
  gapSkipAction,
  nextRallyMs,
  nextUncertainMs,
  prevRallyAction,
  resumeEntersFreePlay,
  resumeStartMs,
  resumeTickLanded,
  seekTarget,
  seekTickSettled,
  type PlaylistRecording,
  type Resume,
  type SessionModel,
  type Timeline,
} from "@/components/recording-player-transport"

/**
 * How close (ms) the playhead must get to a crossing's resume target before its
 * `time-pos` ticks count as "landed" and normal playback resumes. mpv's resume
 * seek is exact but async, so the first settled tick sits at the target; this
 * slack only clears the near-0 pre-seek lead-in the freshly-loaded file emits
 * before the seek takes hold.
 */
const RESUME_TICK_TOL_MS = 250

/**
 * How close (ms) a tick must sit to an in-flight seek's target to count as the
 * seek having settled (the belt beside the `mpv:playback-restart` signal — see
 * `seekTickSettled`). Same order as the resume slack: comfortably wider than
 * one tick interval, far narrower than any gap worth skipping.
 */
const SEEK_SETTLE_TOL_MS = 250

/**
 * The session playback machinery: which recording is loaded, the playhead from
 * mpv's `time-pos` stream, gap-free playback (ADR 0001), boundary crossings
 * (issue #36), and session-global seeking/navigation. The two load gates
 * (`awaitingLoadRef` + `resumeTargetRef`), the load and deferred-resume
 * effects, and the mpv playback listeners form one protocol and must stay
 * together — see the comments on each piece.
 */
export function useSessionPlayback({
  recordings,
  startIndex,
  startMs,
  timelines,
  session,
  segmentOffset,
}: {
  recordings: PlaylistRecording[]
  /** Index of the recording to open first. */
  startIndex: number
  /**
   * Recording-local time (ms) to open the first recording at — a jump to a
   * specific moment (issue #11). Undefined for a normal review (starts at 0).
   */
  startMs?: number
  /** Every recording's draft timeline, keyed by path. */
  timelines: Record<string, Timeline>
  /** The whole session stitched onto one continuous axis. */
  session: SessionModel
  /** Offset of a recording on the session axis (0 if it isn't placed yet). */
  segmentOffset: (recordingIndex: number) => number
}) {
  // Gap-free playback is the default (the North Star), but a manual playhead move
  // opts out: dragging into a gap, or clicking an empty part of the session
  // strip, flips `freePlayRef` true and gap-skipping stands down so footage
  // between rallies can be watched. A rally-targeted action restores gap-free
  // playback. It's a ref, not state, so the `time-pos` listener reads the latest
  // value synchronously without re-subscribing on every change.
  const freePlayRef = useRef(false)
  // True from the moment a load is initiated until mpv confirms the new file is
  // open (the `mpv:file-loaded` event, fired after its baked-in resume seek has
  // landed). While true the `time-pos` listener drops every tick, because a tick
  // carries no file identity: the *outgoing* recording keeps emitting ticks at
  // its own (often large) playhead position after a `loadfile`, and the
  // freshly-loaded file emits near-0 pre-seek ticks before the resume takes hold.
  // Acting on either runs gap-skip against the wrong position — a stale far-past
  // tick reads as "past the last rally" and yanks the playhead into the *next*
  // recording, overriding a click that crossed into this one. Set synchronously
  // at the crossing (not in the load effect) so no tick slips through the
  // render→effect gap; a ref so the listener reads it without re-subscribing.
  // Starts true so the initial mount load is gated the same way.
  const awaitingLoadRef = useRef(true)
  // The second, position gate, working with `awaitingLoadRef`: the recording-local
  // time the pending crossing resumes at, while the (async) resume seek lands. The
  // identity gate above drops every tick until `mpv:file-loaded`, but mpv applies
  // the seek a moment *after* the file opens, so the file's first ticks are still
  // near 0 — this drops them until the playhead reaches the target, so gap-skip
  // can't read a transient ~0 as "before the first rally" and yank there. Because
  // the identity gate has already filtered out the outgoing recording's stale
  // (often far-past) ticks, a plain "have we reached the target?" check is enough
  // here. Null once landed, or when a crossing has no specific target yet.
  const resumeTargetRef = useRef<number | null>(null)
  // The third gate, for *within-recording* seeks (the two above only cover
  // crossings): the target of an `mpv_seek` that hasn't confirmed applied yet.
  // The invoke and the `time-pos` stream are both async, so ticks carrying the
  // *pre-seek* position are routinely delivered after a scrub — acting on one
  // runs gap-skip against the position the user just left (a stale tick in a
  // gap yanks the playhead to the rally after the *old* spot; one past the
  // session's last rally pauses mid-scrub). While set, every tick is dropped;
  // cleared by `mpv:playback-restart` (mpv's "seek finished"), or by a tick
  // settling at the target (`seekTickSettled` — the belt so a missed restart
  // can't freeze the playhead), or by a load/error superseding the seek.
  const seekInFlightRef = useRef<number | null>(null)

  // The playhead within the current recording (ms), from mpv's `time-pos`.
  // Seeded with a jump target (issue #11) so the optimistic playhead is right
  // from the first render, before mpv's first tick arrives.
  const [currentMs, setCurrentMs] = useState(startMs ?? 0)
  const [looping, setLooping] = useState(false)
  // Whether gap-free playback is on (the North Star default). When off, the
  // playhead runs straight through the gaps between rallies — a manual "watch
  // everything" mode toggled from the transport bar or the `G` key.
  const [gapSkipEnabled, setGapSkipEnabled] = useState(true)
  const [error, setError] = useState<string | null>(null)

  // Which recording in the playlist is loaded, and where to resume once its
  // timeline arrives after a boundary crossing.
  const [index, setIndex] = useState(() =>
    Math.min(Math.max(startIndex, 0), Math.max(recordings.length - 1, 0))
  )
  // A jump to a specific moment (issue #11) opens at `startMs`; the mount load
  // effect bakes this `{ atMs }` into `mpv_load` just like a strip-click crossing,
  // so the recording opens already seeked. Null for a normal review.
  const [pendingSeek, setPendingSeek] = useState<Resume | null>(
    startMs != null ? { atMs: startMs } : null
  )
  const path = recordings[index]?.path ?? null
  const timeline = path ? (timelines[path] ?? null) : null

  const atFirstRecording = index <= 0
  const atLastRecording = index >= recordings.length - 1

  // Rallies for the current recording, ascending by start (sorted in segment.rs).
  // The empty list when there's no timeline means the recording plays straight
  // through until its draft arrives.
  const rallies = useMemo(() => timeline?.rallies ?? [], [timeline])

  // Session-global playhead: the current recording's offset plus the local time.
  // Null until the current recording is placed (its predecessors are segmented).
  const globalPlayheadMs =
    index < session.placedCount ? segmentOffset(index) + currentMs : null

  // Seek the current recording to a local position via mpv's native absolute
  // seek (ADR 0008). `currentMs` is set optimistically so rally-to-rally math
  // (e.g. Prev twice) chains off the target immediately rather than waiting on
  // the next `time-pos` tick.
  const seekTo = useCallback((ms: number) => {
    const target = Math.max(0, Math.round(ms))
    // Arm the in-flight gate before the invoke so no stale pre-seek tick can
    // slip through; a rapid follow-up seek just overwrites the target (last
    // write wins, matching mpv coalescing queued seeks).
    seekInFlightRef.current = target
    setCurrentMs(target)
    void trackedInvoke("mpv_seek", { ms: target }).catch(() => {})
  }, [])

  // Move the playlist to another recording, remembering where to resume once it
  // loads (and reset the optimistic playhead so the resume math is clean).
  const goToRecording = useCallback((next: number, resume: Resume) => {
    // Gate the playhead immediately: stale ticks from the recording we're leaving
    // must be dropped until the new file confirms loaded, or gap-skip acts on the
    // old position and crosses on past where we meant to land. Set here (not in
    // the load effect, which is deferred) so no tick slips through in between.
    awaitingLoadRef.current = true
    setCurrentMs(0)
    setIndex(next)
    setPendingSeek(resume)
  }, [])

  // Load the current recording directly from disk and start playing it (ADR
  // 0008). A boundary crossing re-runs this on the new `path`, carrying the
  // resume position *into the load*: mpv applies `startMs` on `FILE_LOADED`,
  // atomic with opening the file, so a click that lands in another recording
  // resumes where it landed instead of racing a separate seek against the
  // still-loading file (which mpv drops, leaving playback at 0). `pendingSeek`
  // and `rallies` are read from the crossing commit, not subscribed — re-running
  // this on either would reload the file mid-playback — hence the deps lint-off.
  useEffect(() => {
    if (!path) return
    const startMs =
      pendingSeek != null ? resumeStartMs(pendingSeek, rallies) : null
    // Re-assert both gates (the mount load reaches here without a crossing, and
    // re-running this effect means a fresh file is opening either way): drop every
    // tick until `mpv:file-loaded`, then drop the new file's pre-seek ticks until
    // the playhead reaches `startMs`. A null target means no specific resume (a
    // deferred `start`/`end` whose rallies haven't arrived) — play from the top.
    awaitingLoadRef.current = true
    resumeTargetRef.current = startMs
    // A fresh file supersedes any seek still in flight in the old one — left
    // armed, its stale target would gate the new file's ticks forever.
    seekInFlightRef.current = null
    void trackedInvoke("mpv_load", { path, startMs })
      .then(() => {
        // `mpv_load` unpauses; `paused` reconciles from the `mpv:pause` event.
        setError(null)
      })
      .catch((e) => setError(String(e)))
    if (startMs != null) {
      // Resolved here → the deferred resume effect below has nothing left to do.
      /* eslint-disable-next-line react-hooks/set-state-in-effect -- carrying the resume into the load is the point of the effect */
      setCurrentMs(startMs)
      setPendingSeek(null)
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps -- resume/rallies are the crossing-commit values, deliberately not deps (they'd reload the file)
  }, [path])

  // The deferred half of a boundary crossing: a `start`/`end` resume whose
  // recording wasn't segmented yet at load time (so `resumeStartMs` returned
  // null and the load couldn't carry it). Once its draft arrives the file is
  // already loaded, so a plain seek lands cleanly — no race. A specific `{ atMs }`
  // and an already-segmented `start`/`end` are resolved by the load effect above,
  // which clears `pendingSeek`, so they never reach here.
  useEffect(() => {
    /* eslint-disable react-hooks/set-state-in-effect -- resuming a crossed-into recording seeks (which sets state), the whole point of the effect */
    if (pendingSeek == null || typeof pendingSeek === "object") return
    if (rallies.length === 0) {
      // Wait for this recording's draft; until then it plays from the top.
      return
    }
    const target =
      pendingSeek === "start" ? rallies[0] : rallies[rallies.length - 1]
    seekTo(target.start_ms)
    setPendingSeek(null)
    /* eslint-enable react-hooks/set-state-in-effect */
  }, [pendingSeek, rallies, seekTo])

  // Gap-free playback (the North Star): as the playhead crosses out of a rally
  // into a gap, jump straight to the next rally's start so only play is watched
  // (ADR 0001). Past the final rally of this recording, advance into the next
  // recording; only the session's very last rally ends playback. The decision is
  // a pure helper (issue #36) so it stays testable without mpv.
  const skipGaps = useCallback(
    (ms: number) => {
      // Gap-skipping off → play straight through the gaps (mpv's `ended` event
      // still crosses recordings at EOF). Looping a rally is independent of the
      // toggle, so it's left to run.
      if (!gapSkipEnabled && !looping) return
      const action = gapSkipAction(
        rallies,
        ms,
        looping,
        freePlayRef.current,
        atLastRecording
      )
      switch (action.kind) {
        case "seek":
          seekTo(action.ms)
          break
        case "next-recording":
          freePlayRef.current = false
          goToRecording(index + 1, "start")
          break
        case "stop":
          // `paused` reconciles from the `mpv:pause` event (issue #42).
          void trackedInvoke("mpv_set_pause", { paused: true }).catch(() => {})
          break
        case "none":
          break
      }
    },
    [
      rallies,
      looping,
      gapSkipEnabled,
      atLastRecording,
      seekTo,
      goToRecording,
      index,
    ]
  )

  // When the current recording ends naturally (its trailing gap was short enough
  // that no later rally triggered a skip), cross into the next so the playlist
  // keeps flowing; the session's last recording just stops.
  const handleEnded = useCallback(() => {
    freePlayRef.current = false
    if (!atLastRecording) goToRecording(index + 1, "start")
  }, [atLastRecording, goToRecording, index])

  // A user-initiated resume (pause → play) past the session's last rally is a
  // free-play intent: the end-of-session stop has paused playback there, and
  // without this every resume is re-paused on its first tick — play only ever
  // advances one frame. Same manual opt-out as dragging into a gap.
  const handleResume = useCallback(() => {
    if (resumeEntersFreePlay(rallies, currentMs, atLastRecording)) {
      freePlayRef.current = true
    }
  }, [rallies, currentMs, atLastRecording])

  // The mpv-event handlers, mirrored into a ref so the listeners below subscribe
  // once yet always run the latest closures (which capture the current rallies,
  // loop/free-play state, and crossing index) — without re-subscribing the
  // `time-pos` stream on every playhead tick. Synced in an effect, not during
  // render (the codebase forbids writing a ref while rendering).
  const handlersRef = useRef({ skipGaps, handleEnded, handleResume })
  useEffect(() => {
    handlersRef.current = { skipGaps, handleEnded, handleResume }
  }, [skipGaps, handleEnded, handleResume])

  // The playhead, end, and error states all come from mpv's event stream (ADR
  // 0008, issue #35). Each `time-pos` tick drives the playhead and runs gap-skip
  // (issue #36), where the old webview `timeUpdate` handler used to. (The
  // pause/speed/volume/mute reconciliation listeners live in `useMpvTransport`.)
  useEffect(() => {
    const subscriptions = [
      listen<number>("mpv:time-pos", (event) => {
        // Identity gate: drop every tick until the new file confirms loaded. A
        // tick carries no file identity, so before `mpv:file-loaded` it's a
        // stale (often far-past) position from the recording we're leaving —
        // acting on it runs gap-skip against the wrong position and crosses on
        // past the resume target.
        if (awaitingLoadRef.current) return
        const ms = event.payload
        // Position gate: the file is open but mpv applies the resume seek a
        // moment later, so its first ticks are still near 0. Drop them until
        // the playhead reaches the target, then resume normally — otherwise
        // gap-skip reads the transient ~0 as "before the first rally" and
        // yanks the playhead there.
        const target = resumeTargetRef.current
        if (target != null) {
          if (!resumeTickLanded(ms, target, RESUME_TICK_TOL_MS)) return
          resumeTargetRef.current = null
        }
        // Seek gate: an `mpv_seek` is in flight, so this tick may still carry
        // the pre-seek position — drop it, or gap-skip runs against the spot
        // the user just scrubbed away from. A tick settling at the target
        // clears the gate itself in case it beats (or substitutes for)
        // `mpv:playback-restart`.
        const inFlight = seekInFlightRef.current
        if (inFlight != null) {
          if (!seekTickSettled(ms, inFlight, SEEK_SETTLE_TOL_MS)) return
          seekInFlightRef.current = null
        }
        setCurrentMs(ms)
        handlersRef.current.skipGaps(ms)
      }),
      // mpv finished applying a seek (its "playback restarted" signal): ticks
      // from here on carry the post-seek position, so reopen the seek gate.
      listen("mpv:playback-restart", () => {
        seekInFlightRef.current = null
      }),
      // Only resumes matter here (the transport state itself is reconciled in
      // `useMpvTransport`): a resume past the session's last rally opts into
      // free play, or the end-of-session stop would re-pause it instantly.
      listen<boolean>("mpv:pause", (event) => {
        if (event.payload === false) handlersRef.current.handleResume()
      }),
      // The new file is open and its baked-in resume seek has landed: the next
      // `time-pos` reflects the resumed position, so reopen the playhead gate.
      listen("mpv:file-loaded", () => {
        awaitingLoadRef.current = false
      }),
      listen("mpv:ended", () => handlersRef.current.handleEnded()),
      listen<string>("mpv:error", (event) => {
        // A load that errors never fires `mpv:file-loaded`; reopen all gates
        // so a failed crossing (or a seek whose restart never comes) can't
        // leave the playhead frozen.
        awaitingLoadRef.current = false
        resumeTargetRef.current = null
        seekInFlightRef.current = null
        setError(event.payload ?? "playback failed")
      }),
    ]
    return () => {
      // Chain each teardown off its registration promise (the use-export.ts
      // idiom) instead of collecting resolved unlistens into an array: under
      // StrictMode's synchronous setup→cleanup→setup this cleanup runs before
      // the promises resolve, so an array is still empty here and the first
      // mount's listeners leak — and a leaked `time-pos` handler keeps running
      // gap-skip (real `mpv_seek` invokes) against a *closed* session's rally
      // table for the rest of the app's life.
      for (const s of subscriptions) void s.then((off) => off())
    }
  }, [])

  // Seek the session to a global position: find the recording that owns it and
  // either seek within the current recording or cross into it. A seek landing in
  // a gap means "let me watch from here" → free play; one landing inside a rally
  // keeps gap-free.
  const seekSession = useCallback(
    (globalMs: number) => {
      const target = clamp(globalMs, 0, session.totalMs)
      const seg =
        session.segments.find(
          (s) =>
            s.durationMs != null &&
            target >= s.offsetMs &&
            target < s.offsetMs + s.durationMs
        ) ?? session.segments[session.placedCount - 1]
      if (!seg) return
      const localMs = clamp(target - seg.offsetMs, 0, seg.durationMs ?? target)
      freePlayRef.current = !session.rallies.some(
        (r) => target >= r.globalStart && target < r.globalEnd
      )
      if (seg.index === index) {
        seekTo(localMs)
      } else {
        goToRecording(seg.index, { atMs: localMs })
      }
    },
    [session, index, seekTo, goToRecording]
  )

  // Manual rally-to-rally navigation, across recording boundaries. A jump is an
  // explicit gap-free intent — it leaves free play (issue #36).
  const goToRally = useCallback(
    (direction: "next" | "prev") => {
      freePlayRef.current = false
      const ms = currentMs
      if (direction === "next") {
        const target = nextRallyMs(rallies, ms)
        if (target != null) {
          seekTo(target)
        } else if (!atLastRecording) {
          goToRecording(index + 1, "start")
        }
      } else {
        const action = prevRallyAction(rallies, ms, atFirstRecording)
        if (action.kind === "seek") {
          seekTo(action.ms)
        } else if (action.kind === "prev-recording") {
          goToRecording(index - 1, "end")
        }
      }
    },
    [
      currentMs,
      rallies,
      seekTo,
      atFirstRecording,
      atLastRecording,
      goToRecording,
      index,
    ]
  )

  // Jump to the next uncertain region across the whole session (ADR 0002).
  const goToUncertain = useCallback(() => {
    const target = nextUncertainMs(session.rallies, globalPlayheadMs ?? 0)
    if (target != null) seekSession(target)
  }, [session, globalPlayheadMs, seekSession])

  const toggleLoop = useCallback(() => setLooping((l) => !l), [])

  const toggleGapSkip = useCallback(() => setGapSkipEnabled((g) => !g), [])

  // Seek the session by a signed offset (session-global, so it crosses recording
  // boundaries). A relative seek is a manual move, so `seekSession` opts it out
  // of gap-free playback like a scrubber drag. Inert until placed on the axis.
  const seekRelative = useCallback(
    (deltaMs: number) => {
      if (globalPlayheadMs == null) return
      seekSession(seekTarget(globalPlayheadMs, deltaMs))
    },
    [globalPlayheadMs, seekSession]
  )

  return {
    index,
    path,
    currentMs,
    globalPlayheadMs,
    error,
    looping,
    gapSkipEnabled,
    atFirstRecording,
    atLastRecording,
    toggleLoop,
    toggleGapSkip,
    seekSession,
    goToRally,
    goToUncertain,
    seekRelative,
  }
}
