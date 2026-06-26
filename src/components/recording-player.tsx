"use client"

import { useCallback, useEffect, useMemo, useRef, useState } from "react"
import {
  ArrowLeftIcon,
  ChevronLeftIcon,
  ChevronRightIcon,
  Loader2Icon,
  PencilIcon,
  PlusIcon,
  RotateCwIcon,
  ScissorsIcon,
  Trash2Icon,
  TriangleAlertIcon,
  ZoomInIcon,
  ZoomOutIcon,
} from "lucide-react"

import { Button } from "@/components/ui/button"
import { trackedInvoke } from "@/lib/tauri"

/** One recording in the session playlist, in capture-time order. */
export interface PlaylistRecording {
  /** Absolute on-disk path of the recording. */
  path: string
}

interface RecordingPlayerProps {
  /**
   * The session's recordings, ordered by capture time. Their rallies are
   * flattened into one continuous playlist played back-to-back (the North Star).
   */
  recordings: PlaylistRecording[]
  /** Index of the recording to open first (defaults to the session's start). */
  startIndex?: number
  /** Return to the session list. */
  onBack: () => void
}

/** Result of the `resolve_playback` command (see `src-tauri/src/lib.rs`). */
interface PlaybackSource {
  path: string
  state: "ready" | "unknown" | "pending" | "failed"
}

/** Loopback origin + token of the playback server (see `src-tauri/src/media.rs`). */
interface PlaybackEndpoint {
  origin: string
  token: string
}

/** A rally interval over one recording (see `TimelineRally` in `src-tauri/src/db.rs`). */
interface Rally {
  /** Database row id, so inline corrections (issue #7) can target this rally. */
  id: number
  start_ms: number
  end_ms: number
  /** Per-region confidence in [0, 1]; low values are uncertain regions. */
  confidence: number
}

/** Result of the `recording_timeline` command (see `src-tauri/src/db.rs`). */
interface Timeline {
  segment_state: "unknown" | "ready" | "failed"
  duration_ms: number | null
  rallies: Rally[]
  /**
   * Downsampled audio waveform peaks in [0, 1], evenly spaced over the
   * recording's duration. Shuttle hits show as spikes, so rally boundaries can be
   * eyeballed against the rally blocks laid over them. Empty until segmented.
   */
  waveform: number[]
}

/**
 * One recording placed on the session-global time axis. `offsetMs` is the sum of
 * the durations of every recording before it, so a recording-local time `t` maps
 * to the session position `offsetMs + t`. `durationMs` is null until the
 * recording is segmented (the DB only records a duration then), and a recording
 * with an unknown duration can't have anything laid out after it.
 */
interface SessionSegment {
  index: number
  path: string
  timeline: Timeline | null
  offsetMs: number
  durationMs: number | null
}

/** A rally lifted onto the session-global axis, carrying its owning recording so
 * an inline edit can be mapped back to that recording's local time and row id. */
interface SessionRally {
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
interface SessionModel {
  /** Recordings up to and including the first one with an unknown duration. */
  segments: SessionSegment[]
  /** How many recordings could be placed (have a known duration). */
  placedCount: number
  /** Total placed duration in ms — the length of the session strip. */
  totalMs: number
  /** Every placed recording's rallies, in session order. */
  rallies: SessionRally[]
}

/** How long to wait before re-checking a recording that is still transcoding. */
const TRANSCODE_POLL_MS = 2000

/** How long to wait before re-checking a recording whose timeline is still being produced. */
const SEGMENT_POLL_MS = 2000

/**
 * Confidence below which a rally is shown as an "uncertain region" — a span the
 * segmenter doubts, surfaced as "check this" during review (ADR 0002).
 */
const UNCERTAIN_CONFIDENCE = 0.5

/**
 * Horizontal scale of the session timeline strip in pixels-per-second, so the
 * whole session is one long, horizontally-scrollable strip (rather than the
 * entire session squashed to the viewport width). The playhead auto-scrolls into
 * view, and rallies stay wide enough to grab their edges while editing. The zoom
 * buttons step it between `SESSION_PX_PER_SEC_MIN` (a whole long session at a
 * glance) and `SESSION_PX_PER_SEC_MAX` (frame-level detail), each press scaling
 * by `SESSION_ZOOM_FACTOR`.
 */
const SESSION_PX_PER_SEC_DEFAULT = 3
const SESSION_PX_PER_SEC_MIN = 1
const SESSION_PX_PER_SEC_MAX = 240
const SESSION_ZOOM_FACTOR = 1.5

function clamp(value: number, lo: number, hi: number): number {
  return Math.min(Math.max(value, lo), hi)
}

function formatClock(ms: number): string {
  const total = Math.round(ms / 1000)
  const m = Math.floor(total / 60)
  const s = total % 60
  return `${m}:${s.toString().padStart(2, "0")}`
}

function fileName(path: string): string {
  const parts = path.split(/[\\/]/)
  return parts[parts.length - 1] || path
}

/** Build the loopback `/play` URL for a recording path. */
function playUrl(endpoint: PlaybackEndpoint, path: string): string {
  const url = new URL(`${endpoint.origin}/play`)
  url.searchParams.set("path", path)
  url.searchParams.set("token", endpoint.token)
  return url.toString()
}

/**
 * Where to resume once a recording the playlist just crossed into becomes ready:
 * its first rally (advancing forward), its last rally (stepping back via Prev),
 * or a specific recording-local time (a click on the session strip that landed
 * in another recording).
 */
type Resume = "start" | "end" | { atMs: number }

/**
 * Plays a whole **session** as one continuous playlist (the North Star): the
 * rallies of every recording, in capture-time order, played back-to-back with
 * gaps skipped. A single `<video>` element plays one recording at a time; when
 * the playhead runs past the last rally of the current recording the player
 * advances to the next recording and resumes from its first rally, so file
 * boundaries are invisible. Rally-to-rally navigation likewise crosses
 * boundaries: Next from the final rally steps into the next recording, Prev from
 * the first rally steps back into the previous one.
 *
 * Beneath the player a single **session timeline** stitches every recording's
 * draft timeline onto one continuous axis (each recording offset by the summed
 * durations of those before it), so reviewing a session is one long scrollable
 * strip rather than a strip that swaps out at every file boundary. Clicking
 * anywhere on it seeks the session — into another recording if need be — and
 * inline corrections map back to the recording that owns the rally.
 *
 * Each recording is served by a loopback HTTP server (see `src-tauri/src/media.rs`),
 * not the asset protocol or a custom scheme: WebKitGTK plays HTML5 media through
 * GStreamer, which only loads real `http://` sources, so a `<video>` pointed at
 * `asset://`/`stream://` fails with `MediaError` code 4. The server declares
 * `video/mp4` and supports range requests, so native frame-accurate seeking
 * works. Web-incompatible recordings are transcoded to H.264/AAC in place at
 * import (ADR 0005); the bytes streamed here are always already playable.
 *
 * Because that transcode and the segmentation run in the background, a recording
 * the playlist crosses into may still be converting or have no draft timeline
 * yet (a session is partly processed). The player surfaces a "preparing" state
 * and polls until the source is ready, and plays a not-yet-segmented recording
 * straight through until its rallies arrive — the playlist never stalls on a
 * file it cannot yet skip through. A recording whose duration isn't known yet
 * can't be placed on the session axis, so the strip lays out the processed
 * prefix and notes how many recordings are still being prepared.
 */
export function RecordingPlayer({
  recordings,
  startIndex = 0,
  onBack,
}: RecordingPlayerProps) {
  const videoRef = useRef<HTMLVideoElement>(null)
  // Gap-free playback is the default (the North Star), but a manual playhead move
  // opts out: when the user drags the video's own scrubber into a gap, or clicks
  // an empty part of the session strip, `freePlayRef` flips true and gap-skipping
  // stands down so the footage between rallies can be watched at any point. A
  // rally-targeted action (Prev/Next rally, Next uncertain, clicking a rally
  // block, crossing a file boundary) restores gap-free playback. It's a ref, not
  // state, so the `timeupdate` handler reads the latest value synchronously: a
  // manual `seeked` and the `timeupdate` that immediately follows it must not race
  // a queued React state update (which would let one last skip slip through).
  const freePlayRef = useRef(false)
  // Set right before every programmatic `currentTime` write so the `seeked` event
  // it triggers isn't mistaken for the user dragging the scrubber.
  const programmaticSeekRef = useRef(false)
  const [endpoint, setEndpoint] = useState<PlaybackEndpoint | null>(null)
  const [src, setSrc] = useState<string | null>(null)
  // loading: resolving; preparing: still transcoding; error: unplayable.
  const [status, setStatus] = useState<
    "loading" | "preparing" | "ready" | "error"
  >("loading")
  // Exact MediaError detail surfaced on failure, so a playback problem reports
  // its cause (decode vs. fetch vs. unsupported source) instead of a black box.
  const [errorDetail, setErrorDetail] = useState<string | null>(null)
  // Every recording's draft timeline, keyed by path so it survives switching
  // recordings (and so the whole session can be stitched into one strip). Each
  // unsegmented recording is polled until its rallies arrive (ADR 0002).
  const [timelines, setTimelines] = useState<Record<string, Timeline>>({})
  // Current playhead position (ms) within the current recording.
  const [currentMs, setCurrentMs] = useState(0)
  // Bumped by Re-analyze to re-trigger the timeline fetch/poll after the worker
  // re-segments (the tuning loop, ADR 0002); `reanalyzing` guards the button.
  const [reanalyzeNonce, setReanalyzeNonce] = useState(0)
  const [reanalyzing, setReanalyzing] = useState(false)
  // Whether the timeline strip is in correction mode (issue #7): the five inline
  // edits (adjust, split, merge, add, delete) are exposed only while editing, so
  // ordinary review stays a click-to-seek strip.
  const [editing, setEditing] = useState(false)

  // Which recording in the playlist is loaded, and where to resume once its
  // timeline arrives after a boundary crossing.
  const [index, setIndex] = useState(() =>
    Math.min(Math.max(startIndex, 0), Math.max(recordings.length - 1, 0))
  )
  const [pendingSeek, setPendingSeek] = useState<Resume | null>(null)
  const path = recordings[index]?.path ?? null
  const timeline = path ? (timelines[path] ?? null) : null

  const atFirstRecording = index <= 0
  const atLastRecording = index >= recordings.length - 1

  // Fetch the playback server endpoint once on mount.
  useEffect(() => {
    let cancelled = false
    trackedInvoke<PlaybackEndpoint>("playback_endpoint")
      .then((result) => {
        if (!cancelled) setEndpoint(result)
      })
      .catch(() => {
        if (!cancelled) {
          setErrorDetail("the media server did not start")
          setStatus("error")
        }
      })
    return () => {
      cancelled = true
    }
  }, [])

  useEffect(() => {
    if (!endpoint || !path) return
    let cancelled = false
    let timer: ReturnType<typeof setTimeout> | undefined
    // Reset to a clean loading state when the recording changes before it
    // resolves. (Consistent with the codebase's other reset effects.)
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setSrc(null)
    setStatus("loading")
    setErrorDetail(null)

    const resolve = () => {
      trackedInvoke<PlaybackSource>("resolve_playback", { path })
        .then((source) => {
          if (cancelled) return
          if (source.state === "ready") {
            setSrc(playUrl(endpoint, source.path))
            setStatus("ready")
          } else if (source.state === "failed") {
            setStatus("error")
          } else {
            // unknown | pending — still transcoding; re-check shortly.
            setStatus("preparing")
            timer = setTimeout(resolve, TRANSCODE_POLL_MS)
          }
        })
        .catch(() => {
          if (!cancelled) setStatus("error")
        })
    }
    resolve()

    return () => {
      cancelled = true
      if (timer) clearTimeout(timer)
    }
  }, [path, endpoint])

  // Fetch every recording's draft timeline so the whole session can be stitched
  // into one strip, polling the recordings still being segmented so their
  // rallies appear as soon as the worker finishes (ADR 0002). Re-runs on
  // Re-analyze so the re-segmented recording is re-polled to ready.
  useEffect(() => {
    let cancelled = false
    const timers: ReturnType<typeof setTimeout>[] = []

    const loadOne = (recordingPath: string) => {
      trackedInvoke<Timeline>("recording_timeline", { path: recordingPath })
        .then((result) => {
          if (cancelled) return
          setTimelines((prev) => ({ ...prev, [recordingPath]: result }))
          if (result.segment_state === "unknown") {
            timers.push(
              setTimeout(() => loadOne(recordingPath), SEGMENT_POLL_MS)
            )
          }
        })
        .catch(() => {
          // A timeline failure is non-fatal — playback still works without it.
        })
    }
    recordings.forEach((rec) => loadOne(rec.path))

    return () => {
      cancelled = true
      timers.forEach(clearTimeout)
    }
  }, [recordings, reanalyzeNonce])

  // Re-fetch a single recording's saved timeline after an inline correction
  // (issue #7) without disturbing the rest of the session — the recording stays
  // `ready`, only its rallies changed.
  const refreshTimeline = useCallback((recordingPath: string) => {
    trackedInvoke<Timeline>("recording_timeline", { path: recordingPath })
      .then((result) =>
        setTimelines((prev) => ({ ...prev, [recordingPath]: result }))
      )
      .catch(() => {})
  }, [])

  // Seek the player to a recording-local position (ms) and resume playback. Marks
  // the seek as programmatic so the `seeked` it fires isn't taken for a manual
  // scrubber drag (which would toggle free play).
  const seekTo = useCallback((ms: number) => {
    const media = videoRef.current
    if (!media) return
    programmaticSeekRef.current = true
    media.currentTime = ms / 1000
    void media.play()
  }, [])

  // Rallies for the current recording, ascending by start (sorted by
  // construction in segment.rs). The empty list when there's no timeline means
  // playback is plain — the recording plays straight through.
  const rallies = useMemo(() => timeline?.rallies ?? [], [timeline])

  // Stitch every recording's timeline onto one continuous session axis. Offsets
  // accumulate over the recordings whose duration is known; the first recording
  // with an unknown duration is included (so its "preparing" state shows) but
  // nothing after it can be placed, so layout stops there.
  const session = useMemo<SessionModel>(() => {
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
    const sessionRallies: SessionRally[] = []
    for (const seg of segments) {
      if (seg.durationMs == null || !seg.timeline) continue
      for (const r of seg.timeline.rallies) {
        sessionRallies.push({
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
    return { segments, placedCount, totalMs: offset, rallies: sessionRallies }
  }, [recordings, timelines])

  // Offset of a recording on the session axis (0 if it isn't placed yet).
  const segmentOffset = useCallback(
    (recordingIndex: number) =>
      session.segments.find((s) => s.index === recordingIndex)?.offsetMs ?? 0,
    [session]
  )

  // Session-global playhead: the current recording's offset plus the local time.
  // Null until the current recording is placed (its predecessors are segmented).
  const globalPlayheadMs =
    index < session.placedCount ? segmentOffset(index) + currentMs : null

  // Move the playlist to another recording, remembering where to resume once it
  // loads.
  const goToRecording = useCallback((next: number, resume: Resume) => {
    setIndex(next)
    setPendingSeek(resume)
  }, [])

  // Seek the session to a global position: find the recording that owns it and
  // either seek within the current recording or cross into that one, resuming at
  // the matching local time. Past the placed prefix, snaps to the last placed
  // recording's end.
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
      // A seek that lands in a gap means "let me watch from here" → free play; one
      // that lands inside a rally (e.g. clicking a rally block) keeps gap-free.
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

  // After a boundary crossing, once the new recording is ready, seek to the
  // requested resume point and play. A specific time seeks straight there; the
  // first/last rally waits for the timeline (with no rallies yet there is nothing
  // to seek to, so the recording plays from the top until its draft arrives and a
  // later pass re-applies the resume).
  useEffect(() => {
    if (status !== "ready" || pendingSeek == null) return
    if (typeof pendingSeek === "object") {
      seekTo(pendingSeek.atMs)
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setPendingSeek(null)
      return
    }
    if (rallies.length === 0) {
      setPendingSeek(null)
      void videoRef.current?.play()
      return
    }
    const target =
      pendingSeek === "start" ? rallies[0] : rallies[rallies.length - 1]
    seekTo(target.start_ms)
    setPendingSeek(null)
  }, [status, pendingSeek, rallies, seekTo])

  // Gap-free playback (the North Star): as the playhead crosses out of a rally
  // into a gap, jump straight to the next rally's start so only play is watched
  // (ADR 0001). Reads the current saved timeline, so later corrections take
  // effect. Past the final rally of this recording, advance into the next
  // recording (gaps between files are skipped too); only the session's very last
  // rally ends playback. With no rallies, this is inert and the recording plays
  // normally until its timeline arrives.
  const skipGaps = useCallback(
    (ms: number) => {
      // The user moved the playhead manually into a gap → play it through, don't
      // yank ahead to the next rally.
      if (freePlayRef.current) return
      if (rallies.length === 0) return
      // Inside a rally → nothing to skip.
      if (rallies.some((r) => ms >= r.start_ms && ms < r.end_ms)) return
      const next = rallies.find((r) => r.start_ms > ms)
      if (next) {
        // In a gap before a later rally (including the head gap) → jump ahead.
        seekTo(next.start_ms)
      } else if (!atLastRecording) {
        // Past this recording's last rally → cross into the next recording.
        goToRecording(index + 1, "start")
      } else {
        // Past the session's final rally → no more play left; stop.
        videoRef.current?.pause()
      }
    },
    [rallies, seekTo, atLastRecording, goToRecording, index]
  )

  // Manual rally-to-rally navigation, across recording boundaries. Next jumps to
  // the first rally starting after the playhead, or into the next recording's
  // first rally when none is left. Previous jumps to the last rally starting
  // before the playhead (with a small slack so it rewinds past the rally you're
  // in), or back into the previous recording's last rally when at the first.
  const goToRally = useCallback(
    (direction: "next" | "prev") => {
      // A rally-targeted jump is an explicit gap-free intent — leave free play.
      freePlayRef.current = false
      const ms = (videoRef.current?.currentTime ?? 0) * 1000
      if (direction === "next") {
        const target = rallies.find((r) => r.start_ms > ms + 1)
        if (target) {
          seekTo(target.start_ms)
        } else if (!atLastRecording) {
          goToRecording(index + 1, "start")
        }
      } else {
        const target = [...rallies]
          .reverse()
          .find((r) => r.start_ms < ms - 1000)
        if (target) {
          seekTo(target.start_ms)
        } else if (!atFirstRecording) {
          goToRecording(index - 1, "end")
        } else if (rallies.length > 0) {
          // First recording, before its first rally → snap to its start.
          seekTo(rallies[0].start_ms)
        }
      }
    },
    [rallies, seekTo, atFirstRecording, atLastRecording, goToRecording, index]
  )

  // Jump to the next uncertain region across the whole session — the spans the
  // segmenter flagged as low-confidence (ADR 0002), surfaced so correction
  // becomes "visit the few spots the machine doubts." Seeks to the first
  // uncertain rally starting after the global playhead, wrapping to the first
  // when none is left ahead, so repeated presses cycle through every doubt in the
  // session (crossing recording boundaries).
  const goToUncertain = useCallback(() => {
    const uncertain = session.rallies.filter(
      (r) => r.confidence < UNCERTAIN_CONFIDENCE
    )
    if (uncertain.length === 0) return
    const here = globalPlayheadMs ?? 0
    const target =
      uncertain.find((r) => r.globalStart > here + 1) ?? uncertain[0]
    seekSession(target.globalStart)
  }, [session, globalPlayheadMs, seekSession])

  // When the current recording ends naturally (no later rally triggered a skip,
  // e.g. its trailing gap was short), advance into the next recording so the
  // playlist keeps flowing across the boundary.
  const handleEnded = useCallback(() => {
    // The next recording resumes at its first rally — back to gap-free.
    freePlayRef.current = false
    if (!atLastRecording) goToRecording(index + 1, "start")
  }, [atLastRecording, goToRecording, index])

  // The video fired `seeked`. If we triggered it programmatically, consume the
  // flag and leave the mode alone. Otherwise the user dragged the native
  // scrubber: landing in a gap switches to free play so the gap can be watched;
  // landing inside a rally keeps gap-free playback.
  const handleSeeked = useCallback(() => {
    if (programmaticSeekRef.current) {
      programmaticSeekRef.current = false
      return
    }
    const ms = (videoRef.current?.currentTime ?? 0) * 1000
    freePlayRef.current = !rallies.some(
      (r) => ms >= r.start_ms && ms < r.end_ms
    )
  }, [rallies])

  // Re-run segmentation for the current recording, then re-fetch timelines.
  // Lets a human iterate on the segmenter's tuning without re-importing (ADR 0002).
  const handleReanalyze = useCallback(() => {
    if (!path) return
    setReanalyzing(true)
    trackedInvoke("reanalyze_recording", { path })
      .then(() => setReanalyzeNonce((n) => n + 1))
      .catch(() => {})
      .finally(() => setReanalyzing(false))
  }, [path])

  // The five inline corrections (issue #7), now resolved against the recording
  // that owns the rally rather than just the current one. Each persists
  // immediately to SQLite (surviving restart) and re-reads that recording's
  // timeline so playback and the strip reflect it at once. Confidence is set
  // certain server-side (a hand-corrected rally is no longer uncertain).

  // Adjust a rally's bounds (global → recording-local, clamped to the recording).
  const adjustRally = useCallback(
    (rally: SessionRally, globalStart: number, globalEnd: number) => {
      const offset = segmentOffset(rally.recordingIndex)
      const duration =
        session.segments.find((s) => s.index === rally.recordingIndex)
          ?.durationMs ?? Number.POSITIVE_INFINITY
      const startMs = Math.round(
        clamp(Math.min(globalStart, globalEnd) - offset, 0, duration)
      )
      const endMs = Math.round(
        clamp(Math.max(globalStart, globalEnd) - offset, 0, duration)
      )
      if (endMs <= startMs) return
      void trackedInvoke("update_rally", {
        path: rally.path,
        rallyId: rally.id,
        startMs,
        endMs,
      })
        .then(() => refreshTimeline(rally.path))
        .catch(() => {})
    },
    [segmentOffset, session, refreshTimeline]
  )

  // Add a rally over the span the segmenter missed around the playhead, on the
  // current recording (where the playhead lives).
  const addAtPlayhead = useCallback(() => {
    if (!path) return
    const duration =
      session.segments.find((s) => s.index === index)?.durationMs ??
      Number.POSITIVE_INFINITY
    const start = Math.max(0, Math.round(currentMs - 2000))
    const end = Math.round(clamp(currentMs + 2000, start + 1, duration))
    void trackedInvoke("add_rally", { path, startMs: start, endMs: end })
      .then(() => refreshTimeline(path))
      .catch(() => {})
  }, [path, index, currentMs, session, refreshTimeline])

  const deleteRally = useCallback(
    (rally: SessionRally) => {
      void trackedInvoke("delete_rally", {
        path: rally.path,
        rallyId: rally.id,
      })
        .then(() => refreshTimeline(rally.path))
        .catch(() => {})
    },
    [refreshTimeline]
  )

  // Split a rally at the global playhead: shrink it to end at the cut, then add a
  // new rally from the cut to the old end. A no-op unless the cut falls strictly
  // inside the rally (the caller only enables it then).
  const splitRally = useCallback(
    (rally: SessionRally, atGlobalMs: number) => {
      const offset = segmentOffset(rally.recordingIndex)
      const atLocal = Math.round(atGlobalMs - offset)
      if (atLocal <= rally.localStart || atLocal >= rally.localEnd) return
      void trackedInvoke("update_rally", {
        path: rally.path,
        rallyId: rally.id,
        startMs: rally.localStart,
        endMs: atLocal,
      })
        .then(() =>
          trackedInvoke("add_rally", {
            path: rally.path,
            startMs: atLocal,
            endMs: rally.localEnd,
          })
        )
        .then(() => refreshTimeline(rally.path))
        .catch(() => {})
    },
    [segmentOffset, refreshTimeline]
  )

  // Merge a rally with the next one in the same recording: stretch the first to
  // cover both, then delete the second. The caller only enables this when both
  // belong to the same recording (rallies can't span a file boundary).
  const mergeRallies = useCallback(
    (first: SessionRally, second: SessionRally) => {
      if (first.path !== second.path) return
      void trackedInvoke("update_rally", {
        path: first.path,
        rallyId: first.id,
        startMs: Math.min(first.localStart, second.localStart),
        endMs: Math.max(first.localEnd, second.localEnd),
      })
        .then(() =>
          trackedInvoke("delete_rally", {
            path: first.path,
            rallyId: second.id,
          })
        )
        .then(() => refreshTimeline(first.path))
        .catch(() => {})
    },
    [refreshTimeline]
  )

  const handleError = useCallback(() => {
    const media = videoRef.current
    const err = media?.error
    const codes: Record<number, string> = {
      1: "ABORTED",
      2: "NETWORK",
      3: "DECODE",
      4: "SRC_NOT_SUPPORTED",
    }
    const detail = err
      ? `MediaError code ${err.code} (${codes[err.code] ?? "?"})${err.message ? ` — ${err.message}` : ""} · networkState ${media?.networkState} · src ${media?.currentSrc}`
      : `unknown video error · src ${media?.currentSrc}`
    console.error("[recording-player]", detail)
    setErrorDetail(detail)
    setStatus("error")
  }, [])

  return (
    <div className="flex h-full min-h-0 flex-col gap-4">
      <div className="flex shrink-0 items-center gap-3">
        <Button variant="outline" size="sm" onClick={onBack}>
          <ArrowLeftIcon className="size-4" />
          Sessions
        </Button>
        <span className="truncate font-medium" title={path ?? undefined}>
          {path ? fileName(path) : "No recordings"}
        </span>
        {recordings.length > 1 ? (
          <span className="shrink-0 text-sm text-muted-foreground tabular-nums">
            Recording {index + 1} of {recordings.length}
          </span>
        ) : null}
      </div>
      {status === "error" ? (
        <div className="text-destructive-foreground flex min-h-0 w-full flex-1 flex-col items-center justify-center gap-3 rounded-lg bg-black p-6 text-center text-sm">
          <p>This recording could not be played.</p>
          {errorDetail ? (
            <p className="max-w-full font-mono text-xs break-words text-muted-foreground">
              {errorDetail}
            </p>
          ) : null}
        </div>
      ) : status === "preparing" ? (
        <div className="flex min-h-0 w-full flex-1 flex-col items-center justify-center gap-3 rounded-lg bg-black p-6 text-center text-sm text-muted-foreground">
          <Loader2Icon className="size-6 animate-spin" />
          Preparing this recording for playback (transcoding for the first
          time)…
        </div>
      ) : status === "ready" && src ? (
        <>
          <video
            ref={videoRef}
            // Re-mount when the resolved source changes so the element reloads.
            key={src}
            // Flex to fill the height left between the header and the timeline,
            // letterboxing the frame so the timeline below stays in view.
            className="min-h-0 w-full flex-1 rounded-lg bg-black object-contain"
            src={src}
            controls
            autoPlay
            onError={handleError}
            onEnded={handleEnded}
            onSeeked={handleSeeked}
            onTimeUpdate={(e) => {
              const ms = e.currentTarget.currentTime * 1000
              setCurrentMs(ms)
              skipGaps(ms)
            }}
          />
          <SessionTimeline
            session={session}
            recordingCount={recordings.length}
            globalPlayheadMs={globalPlayheadMs}
            reanalyzing={reanalyzing}
            canPrev={!atFirstRecording}
            canNext={!atLastRecording}
            editing={editing}
            onSeekGlobal={seekSession}
            onPrevRally={() => goToRally("prev")}
            onNextRally={() => goToRally("next")}
            onNextUncertain={goToUncertain}
            onReanalyze={handleReanalyze}
            onToggleEditing={() => setEditing((e) => !e)}
            onAdjustRally={adjustRally}
            onAddAtPlayhead={addAtPlayhead}
            onDeleteRally={deleteRally}
            onSplitRally={splitRally}
            onMergeRallies={mergeRallies}
          />
        </>
      ) : (
        <div className="flex min-h-0 w-full flex-1 items-center justify-center rounded-lg bg-black" />
      )}
    </div>
  )
}

/**
 * The session timeline strip beneath the player: every recording's draft
 * timeline stitched onto one continuous, horizontally-scrollable axis at a fixed
 * pixels-per-second scale (issue #6 extended to the whole session). The audio
 * waveform of each recording fills its span — shuttle hits show as spikes — with
 * detected rallies drawn as blocks over it, gaps the empty space between them
 * (ADR 0001), low-confidence rallies styled as uncertain regions to "check this"
 * (ADR 0002), and faint dividers marking recording boundaries. The playhead
 * tracks the session position and auto-scrolls into view. Clicking the strip
 * seeks the session (crossing recordings as needed); a rally block seeks to its
 * start. Prev/Next rally and Next uncertain cross recording boundaries.
 *
 * In correction mode (issue #7) each edit is resolved against the recording that
 * owns the rally: drag an edge to adjust, split at the playhead, merge with the
 * next rally in the same recording, add around the playhead, or delete. The
 * Re-analyze button re-runs segmentation for the current recording in place —
 * the loop for tuning the heuristic (see `docs/tuning-segmentation.md`).
 */
function SessionTimeline({
  session,
  recordingCount,
  globalPlayheadMs,
  reanalyzing,
  canPrev,
  canNext,
  editing,
  onSeekGlobal,
  onPrevRally,
  onNextRally,
  onNextUncertain,
  onReanalyze,
  onToggleEditing,
  onAdjustRally,
  onAddAtPlayhead,
  onDeleteRally,
  onSplitRally,
  onMergeRallies,
}: {
  session: SessionModel
  recordingCount: number
  globalPlayheadMs: number | null
  reanalyzing: boolean
  canPrev: boolean
  canNext: boolean
  editing: boolean
  onSeekGlobal: (globalMs: number) => void
  onPrevRally: () => void
  onNextRally: () => void
  onNextUncertain: () => void
  onReanalyze: () => void
  onToggleEditing: () => void
  onAdjustRally: (
    rally: SessionRally,
    globalStart: number,
    globalEnd: number
  ) => void
  onAddAtPlayhead: () => void
  onDeleteRally: (rally: SessionRally) => void
  onSplitRally: (rally: SessionRally, atGlobalMs: number) => void
  onMergeRallies: (first: SessionRally, second: SessionRally) => void
}) {
  // The rally currently picked for split/merge/delete, by "path:id" so a row id
  // shared across recordings can never be ambiguous.
  const [selectedKey, setSelectedKey] = useState<string | null>(null)
  // While dragging an edge: the rally key/edge, the fixed (anchor) edge's global
  // position, the live global position, and the recording's bounds so the drag
  // can't leave the file it belongs to.
  const [drag, setDrag] = useState<{
    key: string
    edge: "start" | "end"
    anchorGlobalMs: number
    globalMs: number
    minGlobalMs: number
    maxGlobalMs: number
  } | null>(null)
  const scrollRef = useRef<HTMLDivElement>(null)
  const contentRef = useRef<HTMLDivElement>(null)
  // Strip scale (px per second), driven by the zoom buttons below the strip.
  const [pxPerSec, setPxPerSec] = useState(SESSION_PX_PER_SEC_DEFAULT)

  const totalMs = session.totalMs
  const totalPx = (totalMs / 1000) * pxPerSec
  const rallyKey = (r: SessionRally) => `${r.path}:${r.id}`

  const canZoomIn = pxPerSec < SESSION_PX_PER_SEC_MAX
  const canZoomOut = pxPerSec > SESSION_PX_PER_SEC_MIN
  // Step the zoom by a constant factor, clamped. The playhead re-centres on the
  // next render via the auto-scroll effect (pxPerSec is one of its deps).
  const zoomBy = useCallback((factor: number) => {
    setPxPerSec((p) =>
      clamp(p * factor, SESSION_PX_PER_SEC_MIN, SESSION_PX_PER_SEC_MAX)
    )
  }, [])

  // Map a client x over the strip content to a session-global time (ms, clamped).
  const xToMs = useCallback(
    (clientX: number): number => {
      const rect = contentRef.current?.getBoundingClientRect()
      if (!rect || rect.width === 0) return 0
      const frac = (clientX - rect.left) / rect.width
      return Math.round(clamp(frac, 0, 1) * totalMs)
    },
    [totalMs]
  )

  // While dragging a rally edge, follow the pointer anywhere on the page and
  // persist the new boundary on release (clamped to the rally's own recording).
  // Hooks run before the early return below, so this is unconditional.
  useEffect(() => {
    if (!drag) return
    const move = (e: PointerEvent) =>
      setDrag((d) =>
        d
          ? {
              ...d,
              globalMs: clamp(xToMs(e.clientX), d.minGlobalMs, d.maxGlobalMs),
            }
          : d
      )
    const up = () => {
      setDrag((d) => {
        if (d) {
          const rally = session.rallies.find((r) => rallyKey(r) === d.key)
          if (rally) {
            const start = d.edge === "start" ? d.globalMs : d.anchorGlobalMs
            const end = d.edge === "end" ? d.globalMs : d.anchorGlobalMs
            onAdjustRally(rally, start, end)
          }
        }
        return null
      })
    }
    window.addEventListener("pointermove", move)
    window.addEventListener("pointerup", up)
    return () => {
      window.removeEventListener("pointermove", move)
      window.removeEventListener("pointerup", up)
    }
  }, [drag, session, xToMs, onAdjustRally])

  // Keep the playhead in view as playback advances, crosses recordings, or the
  // zoom changes (re-centres after a zoom step, since `pxPerSec` is a dep).
  useEffect(() => {
    const el = scrollRef.current
    if (!el || globalPlayheadMs == null || totalMs === 0) return
    const x = (globalPlayheadMs / 1000) * pxPerSec
    const margin = el.clientWidth * 0.15
    if (
      x < el.scrollLeft + margin ||
      x > el.scrollLeft + el.clientWidth - margin
    ) {
      el.scrollLeft = x - el.clientWidth / 2
    }
  }, [globalPlayheadMs, totalMs, pxPerSec])

  // Summary mirrors the segmentation lifecycle (ADR 0002), aggregated over the
  // whole session.
  const segmentingNow =
    reanalyzing ||
    session.segments.some((s) => s.timeline?.segment_state === "unknown")
  const failedCount = session.segments.filter(
    (s) => s.timeline?.segment_state === "failed"
  ).length
  const rallyCount = session.rallies.length
  const uncertainCount = session.rallies.filter(
    (r) => r.confidence < UNCERTAIN_CONFIDENCE
  ).length
  const unprocessed = recordingCount - session.placedCount
  const hasRallies = totalMs > 0 && rallyCount > 0

  let summary
  if (totalMs === 0 && segmentingNow) {
    summary = (
      <span className="flex items-center gap-2">
        <Loader2Icon className="size-4 animate-spin" />
        Detecting rallies…
      </span>
    )
  } else if (rallyCount === 0 && failedCount > 0) {
    summary = <span>Couldn&apos;t detect rallies for this session.</span>
  } else if (!hasRallies) {
    summary = <span>No rallies detected.</span>
  } else {
    summary = (
      <span>
        {rallyCount} {rallyCount === 1 ? "rally" : "rallies"} across the session
        {uncertainCount > 0 ? (
          <span
            className="text-amber-600 dark:text-amber-500"
            title="Low-confidence spans the segmenter is unsure about — worth checking."
          >
            {" "}
            · {uncertainCount} uncertain
          </span>
        ) : null}
      </span>
    )
  }

  // The selected rally and its neighbour (for merge), recomputed each render so
  // they stay valid as the timeline changes under an edit. Merge needs a
  // following rally in the SAME recording (rallies can't span a file boundary).
  const selectedIndex = session.rallies.findIndex(
    (r) => rallyKey(r) === selectedKey
  )
  const selected = selectedIndex >= 0 ? session.rallies[selectedIndex] : null
  const next =
    selectedIndex >= 0 ? (session.rallies[selectedIndex + 1] ?? null) : null
  const mergeTarget =
    selected && next && next.recordingIndex === selected.recordingIndex
      ? next
      : null
  // Split is only meaningful when the playhead falls strictly inside the selected
  // rally — which can only happen within the recording currently playing.
  const canSplit =
    selected !== null &&
    globalPlayheadMs !== null &&
    globalPlayheadMs > selected.globalStart &&
    globalPlayheadMs < selected.globalEnd
  const canMerge = mergeTarget !== null

  const playheadPx =
    globalPlayheadMs !== null ? (globalPlayheadMs / 1000) * pxPerSec : null

  return (
    <div className="shrink-0 space-y-2">
      <div className="flex flex-wrap items-center justify-between gap-2 text-sm text-muted-foreground">
        {summary}
        <div className="flex items-center gap-2">
          <Button
            variant="outline"
            size="sm"
            onClick={onPrevRally}
            disabled={!hasRallies && !canPrev}
            title="Jump to the previous rally."
          >
            <ChevronLeftIcon className="size-4" />
            Prev rally
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={onNextRally}
            disabled={!hasRallies && !canNext}
            title="Jump to the next rally."
          >
            Next rally
            <ChevronRightIcon className="size-4" />
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={onNextUncertain}
            disabled={uncertainCount === 0}
            title="Jump to the next uncertain region — a span the segmenter doubts, worth checking."
          >
            <TriangleAlertIcon className="size-4" />
            Next uncertain
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={onReanalyze}
            disabled={segmentingNow}
            title="Re-run rally detection for the current recording in place (for tuning the segmenter)."
          >
            <RotateCwIcon
              className={`size-4 ${reanalyzing ? "animate-spin" : ""}`}
            />
            Re-analyze
          </Button>
          <Button
            variant={editing ? "default" : "outline"}
            size="sm"
            onClick={onToggleEditing}
            disabled={!hasRallies}
            title="Correct the draft timeline: drag rally edges, split, merge, add, or delete."
          >
            <PencilIcon className="size-4" />
            {editing ? "Done editing" : "Edit timeline"}
          </Button>
        </div>
      </div>
      {editing ? (
        <div className="flex flex-wrap items-center gap-2 rounded-md border border-dashed bg-muted/40 px-3 py-2 text-sm text-muted-foreground">
          <span>
            {selected
              ? `Rally ${selectedIndex + 1} selected (${formatClock(
                  selected.globalStart
                )}–${formatClock(selected.globalEnd)} · ${fileName(selected.path)})`
              : "Drag a rally's edge to adjust it, or click a rally to select it."}
          </span>
          <div className="ml-auto flex flex-wrap items-center gap-2">
            <Button
              variant="outline"
              size="sm"
              onClick={onAddAtPlayhead}
              title="Add a rally over a span the segmenter missed (around the playhead, in the current recording)."
            >
              <PlusIcon className="size-4" />
              Add at playhead
            </Button>
            <Button
              variant="outline"
              size="sm"
              onClick={() =>
                selected &&
                globalPlayheadMs !== null &&
                onSplitRally(selected, globalPlayheadMs)
              }
              disabled={!canSplit}
              title="Split the selected rally in two at the playhead."
            >
              <ScissorsIcon className="size-4" />
              Split at playhead
            </Button>
            <Button
              variant="outline"
              size="sm"
              onClick={() =>
                selected && mergeTarget && onMergeRallies(selected, mergeTarget)
              }
              disabled={!canMerge}
              title="Merge the selected rally with the next one in the same recording."
            >
              Merge with next
            </Button>
            <Button
              variant="outline"
              size="sm"
              onClick={() => {
                if (selected) {
                  onDeleteRally(selected)
                  setSelectedKey(null)
                }
              }}
              disabled={!selected}
              title="Delete the selected rally (its span becomes a gap)."
            >
              <Trash2Icon className="size-4" />
              Delete
            </Button>
          </div>
        </div>
      ) : null}
      {hasRallies ? (
        <>
          <div
            ref={scrollRef}
            className="w-full overflow-x-auto rounded-md bg-muted"
          >
            <div
              ref={contentRef}
              className="relative h-16 cursor-pointer"
              style={{ width: `${Math.max(totalPx, 1)}px` }}
              onClick={(e) => {
                // Clicking empty strip seeks the session to that point; while
                // editing it clears the selection instead. Rally blocks stop
                // propagation (seek, or select while editing).
                if (editing) {
                  setSelectedKey(null)
                  return
                }
                onSeekGlobal(xToMs(e.clientX))
              }}
            >
              {session.segments.map((seg) => {
                if (seg.durationMs == null || !seg.timeline) return null
                const left = (seg.offsetMs / 1000) * pxPerSec
                const width = (seg.durationMs / 1000) * pxPerSec
                return (
                  <div
                    key={seg.path}
                    className="pointer-events-none absolute inset-y-0"
                    style={{ left: `${left}px`, width: `${width}px` }}
                  >
                    <Waveform peaks={seg.timeline.waveform} />
                    {seg.index > 0 ? (
                      <div className="absolute inset-y-0 left-0 w-px bg-foreground/30" />
                    ) : null}
                    <span className="absolute top-0.5 left-1 max-w-full truncate text-[10px] text-muted-foreground/70">
                      {fileName(seg.path)}
                    </span>
                  </div>
                )
              })}
              {session.rallies.map((rally, i) => {
                const key = rallyKey(rally)
                // While dragging this rally's edge, draw it at the live position
                // so the resize is visible before it persists on release.
                const dragging = drag?.key === key ? drag : null
                const gStart = dragging
                  ? dragging.edge === "start"
                    ? dragging.globalMs
                    : dragging.anchorGlobalMs
                  : rally.globalStart
                const gEnd = dragging
                  ? dragging.edge === "end"
                    ? dragging.globalMs
                    : dragging.anchorGlobalMs
                  : rally.globalEnd
                const lo = Math.min(gStart, gEnd)
                const hi = Math.max(gStart, gEnd)
                const left = (lo / 1000) * pxPerSec
                const width = ((hi - lo) / 1000) * pxPerSec
                const uncertain = rally.confidence < UNCERTAIN_CONFIDENCE
                const isSelected = editing && key === selectedKey
                const seg = session.segments.find(
                  (s) => s.index === rally.recordingIndex
                )
                const minGlobalMs = seg?.offsetMs ?? 0
                const maxGlobalMs = minGlobalMs + (seg?.durationMs ?? 0)
                return (
                  <button
                    key={key}
                    type="button"
                    onClick={(e) => {
                      e.stopPropagation()
                      if (editing) {
                        setSelectedKey(key)
                      } else {
                        onSeekGlobal(rally.globalStart)
                      }
                    }}
                    className={`absolute inset-y-0 rounded-sm transition-opacity hover:opacity-80 ${
                      uncertain
                        ? "border border-amber-500/70 bg-amber-500/40"
                        : "bg-primary/70"
                    } ${isSelected ? "ring-2 ring-foreground ring-offset-1 ring-offset-muted" : ""}`}
                    style={{
                      left: `${left}px`,
                      width: `${Math.max(width, 3)}px`,
                    }}
                    title={`Rally ${i + 1}: ${formatClock(rally.globalStart)}–${formatClock(
                      rally.globalEnd
                    )}${uncertain ? " (uncertain)" : ""} · confidence ${Math.round(
                      rally.confidence * 100
                    )}% · ${fileName(rally.path)}`}
                  >
                    {editing ? (
                      <>
                        {/* Drag handles to adjust each boundary (issue #7). */}
                        <span
                          role="separator"
                          aria-label="Drag rally start"
                          onClick={(e) => e.stopPropagation()}
                          onPointerDown={(e) => {
                            e.stopPropagation()
                            e.preventDefault()
                            setSelectedKey(key)
                            setDrag({
                              key,
                              edge: "start",
                              anchorGlobalMs: rally.globalEnd,
                              globalMs: rally.globalStart,
                              minGlobalMs,
                              maxGlobalMs,
                            })
                          }}
                          className="absolute inset-y-0 left-0 w-1.5 cursor-ew-resize rounded-l-sm bg-foreground/70 hover:bg-foreground"
                        />
                        <span
                          role="separator"
                          aria-label="Drag rally end"
                          onClick={(e) => e.stopPropagation()}
                          onPointerDown={(e) => {
                            e.stopPropagation()
                            e.preventDefault()
                            setSelectedKey(key)
                            setDrag({
                              key,
                              edge: "end",
                              anchorGlobalMs: rally.globalStart,
                              globalMs: rally.globalEnd,
                              minGlobalMs,
                              maxGlobalMs,
                            })
                          }}
                          className="absolute inset-y-0 right-0 w-1.5 cursor-ew-resize rounded-r-sm bg-foreground/70 hover:bg-foreground"
                        />
                      </>
                    ) : null}
                  </button>
                )
              })}
              {playheadPx !== null ? (
                <div
                  className="pointer-events-none absolute inset-y-0 w-0.5 bg-foreground"
                  style={{ left: `${playheadPx}px` }}
                />
              ) : null}
            </div>
          </div>
          <div className="flex items-center gap-2 text-sm text-muted-foreground">
            <span className="mr-1">Zoom</span>
            <Button
              variant="outline"
              size="sm"
              onClick={() => zoomBy(1 / SESSION_ZOOM_FACTOR)}
              disabled={!canZoomOut}
              title="Zoom out — fit more of the session on screen."
            >
              <ZoomOutIcon className="size-4" />
            </Button>
            <Button
              variant="outline"
              size="sm"
              onClick={() => zoomBy(SESSION_ZOOM_FACTOR)}
              disabled={!canZoomIn}
              title="Zoom in — see finer detail around the playhead."
            >
              <ZoomInIcon className="size-4" />
            </Button>
            <Button
              variant="outline"
              size="sm"
              onClick={() => setPxPerSec(SESSION_PX_PER_SEC_DEFAULT)}
              disabled={pxPerSec === SESSION_PX_PER_SEC_DEFAULT}
              title="Reset the timeline zoom."
            >
              Reset
            </Button>
          </div>
          {unprocessed > 0 ? (
            <p className="text-xs text-muted-foreground">
              {unprocessed} more{" "}
              {unprocessed === 1 ? "recording" : "recordings"} still being
              prepared — they’ll join the timeline once segmented.
            </p>
          ) : null}
        </>
      ) : null}
    </div>
  )
}

/**
 * The audio waveform under the rally blocks (issue #6): each downsampled peak
 * is a vertical bar centred on the strip, so shuttle hits read as spikes and
 * rally boundaries can be eyeballed where the blocks overlay them. Drawn behind
 * the blocks at low contrast (pointer-events disabled so strip clicks seek) and
 * stretched to fill its recording's span via a viewBox in normalized peak
 * coordinates.
 */
function Waveform({ peaks }: { peaks: number[] }) {
  if (peaks.length === 0) return null
  return (
    <svg
      className="pointer-events-none absolute inset-0 size-full text-muted-foreground/50"
      viewBox={`0 0 ${peaks.length} 1`}
      preserveAspectRatio="none"
      aria-hidden
    >
      {peaks.map((peak, i) => {
        // A floor keeps near-silent buckets faintly visible rather than blank.
        const h = Math.max(peak, 0.02)
        return (
          <rect
            key={i}
            x={i + 0.1}
            y={(1 - h) / 2}
            width={0.8}
            height={h}
            fill="currentColor"
          />
        )
      })}
    </svg>
  )
}
