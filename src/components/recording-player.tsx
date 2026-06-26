"use client"

import { useCallback, useEffect, useMemo, useRef, useState } from "react"
import {
  ArrowLeftIcon,
  ChevronLeftIcon,
  ChevronRightIcon,
  KeyboardIcon,
  Loader2Icon,
  PauseIcon,
  PencilIcon,
  PlayIcon,
  PlusIcon,
  RepeatIcon,
  RotateCwIcon,
  ScissorsIcon,
  StepBackIcon,
  StepForwardIcon,
  Trash2Icon,
  TriangleAlertIcon,
  Volume2Icon,
  VolumeXIcon,
  XIcon,
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
  /** Probed frame rate (issue #19), for exact frame-step; null when unknown. */
  fps: number | null
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
 * How far into a rally the playhead must be for Prev to *restart* it rather than
 * step to the previous one — the music-player rule: one press rewinds to the
 * current rally's start, a second press (now within this slack of the start)
 * jumps to the previous rally.
 */
const PREV_RESTART_SLACK_MS = 1000

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

/**
 * Frame rate to assume when a recording's real fps hasn't been probed yet
 * (issue #19): the backend defaults to this too, so frame-step still works.
 */
const DEFAULT_FPS = 30

/**
 * Fixed playback-speed ladder (issue #19). `Ctrl+-`/`Ctrl+=` step along it and
 * `Ctrl+0` resets to `1×`. A ladder (not free scrubbing) keeps speed changes
 * predictable and the indicator readable.
 */
const SPEED_LADDER = [0.25, 0.5, 0.75, 1, 1.25, 1.5, 2] as const
/** Index of `1×` on the ladder — the default and the `Ctrl+0` reset target. */
const DEFAULT_SPEED_INDEX = SPEED_LADDER.indexOf(1)

/** Arrow-seek step sizes in ms (session-global), by modifier (issue #19). */
const SEEK_FINE_MS = 2500 // Ctrl+←/→
const SEEK_DEFAULT_MS = 5000 // ←/→
const SEEK_COARSE_MS = 10000 // Shift+←/→

/** How much each Alt+scroll notch over the timeline zooms (issue #19). */
const ALT_SCROLL_ZOOM_FACTOR = 1.15

function clamp(value: number, lo: number, hi: number): number {
  return Math.min(Math.max(value, lo), hi)
}

function formatClock(ms: number): string {
  const total = Math.round(ms / 1000)
  const m = Math.floor(total / 60)
  const s = total % 60
  return `${m}:${s.toString().padStart(2, "0")}`
}

/**
 * Session-global timecode for the transport bar (issue #19): `mm:ss` under an
 * hour, `h:mm:ss` past it, so the position display reads naturally for both a
 * short clip and a long session.
 */
function formatTimecode(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000))
  const h = Math.floor(total / 3600)
  const m = Math.floor((total % 3600) / 60)
  const s = total % 60
  if (h > 0) {
    return `${h}:${m.toString().padStart(2, "0")}:${s.toString().padStart(2, "0")}`
  }
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
 * One loaded stream in the double-buffer (issue #24): a positioned `&t=` URL (or
 * the whole-file head) with a monotonically increasing id. The id keys the
 * `<video>` element so React keeps the live element mounted across renders and
 * only remounts the slot whose load actually changed — re-using one element
 * across rapid `&t=` swaps renders decode garbage ("TV static").
 */
interface Load {
  id: number
  src: string
}

/**
 * One entry in the single keymap definition (issue #19): its display form
 * (`keys`/`label` for the cheat-sheet), the predicate that decides whether a
 * keydown matches it, and the action to run. One array backs both the live key
 * handler and the `?` cheat-sheet, so they cannot drift.
 */
interface Keybinding {
  keys: string[]
  label: string
  match: (e: KeyboardEvent) => boolean
  run: (e: KeyboardEvent) => void
}

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
  // --- Double-buffered playback (issue #24) ---
  // Seeking reloads the stream at a `&t=` URL rather than writing `currentTime`
  // (this WebKitGTK build silently drops `currentTime` seeks). A reloading
  // `<video>` blanks to black, and this build's GStreamer-backed video composites
  // in a separate layer — so it can't be captured to a canvas either (a canvas
  // draw comes out black). So we run two `<video>` elements: the `live` one stays
  // visible and is paused on its last frame (a paused `<video>` natively holds
  // its frame, composited correctly) while the new stream loads in the
  // `incoming` one (hidden); when `incoming` starts playing it's promoted to
  // `live`. The held frame covers the reload — no black flash, and the incoming
  // element is hidden until it plays so its stretched first frame is never seen.
  //
  // recording-local ms = `seekBaseMs + currentTime*1000`, where `seekBaseMs` is
  // the `t` the live stream opened at; the ref lets `timeupdate` read it
  // synchronously without a re-render.
  const seekBaseMsRef = useRef(0)
  // True from a seek until the incoming element is promoted, so `timeupdate`
  // holds the playhead at the optimistic target instead of reacting to a
  // pre-promotion report (issue #24).
  const seekingRef = useRef(false)
  // The visible (playing/paused) load and the one loading after a seek. `live` is
  // only ever set by promotion — i.e. to a load that has already started playing
  // — so it never shows a stretched first frame; the initial load also flows
  // through `incoming` → promote.
  const [live, setLive] = useState<Load | null>(null)
  const [incoming, setIncoming] = useState<Load | null>(null)
  // Mirrors `live` for synchronous reads in DOM event handlers (timeupdate/promote).
  const liveRef = useRef<Load | null>(null)
  const loadIdRef = useRef(0)
  // The mounted `<video>` elements by load id, so transport actions reach the
  // live one without a ref that churns as the live load changes.
  const videoEls = useRef<Map<number, HTMLVideoElement>>(new Map())
  // True while a (re)load hasn't rendered its first frame yet — drives the spinner.
  const [loading, setLoading] = useState(false)
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
  // Transport state (issue #19), now that the native `<video controls>` is gone:
  // the custom bar and the keymap own play/pause, mute, speed, loop, and the
  // cheat-sheet overlay. `fps` is the probed frame rate (for exact frame-step),
  // defaulting to DEFAULT_FPS until resolve_playback supplies it.
  const [fps, setFps] = useState(DEFAULT_FPS)
  const [paused, setPaused] = useState(true)
  const [muted, setMuted] = useState(false)
  const [speedIndex, setSpeedIndex] = useState(DEFAULT_SPEED_INDEX)
  const [looping, setLooping] = useState(false)
  const [showCheatSheet, setShowCheatSheet] = useState(false)
  // Focus host: the container holds focus so the (untabbable) video never gets
  // keystrokes — the old native-control key behavior can't double-fire (issue #19).
  const containerRef = useRef<HTMLDivElement>(null)

  // Which recording in the playlist is loaded, and where to resume once its
  // timeline arrives after a boundary crossing.
  const [index, setIndex] = useState(() =>
    Math.min(Math.max(startIndex, 0), Math.max(recordings.length - 1, 0))
  )
  const [pendingSeek, setPendingSeek] = useState<Resume | null>(null)
  const path = recordings[index]?.path ?? null
  const timeline = path ? (timelines[path] ?? null) : null

  // Build the positioned stream URL for a recording-local time (ms); `t <= 0` is
  // the plain whole-file path (range-seekable, opens at the head) — issue #24.
  const urlAt = useCallback((base: string, ms: number) => {
    if (ms <= 0) return base
    const url = new URL(base)
    url.searchParams.set("t", (ms / 1000).toString())
    return url.toString()
  }, [])

  // The visible `<video>` element (the live load), for transport actions.
  const liveVideo = useCallback(
    () =>
      liveRef.current
        ? (videoEls.current.get(liveRef.current.id) ?? null)
        : null,
    []
  )

  // Promote the just-rendered incoming element to live and drop the old live
  // (React unmounts it on the next render — no black gap, since the promoted
  // element is already showing live frames). Issue #24.
  const promote = useCallback((load: Load) => {
    liveRef.current = load
    seekingRef.current = false
    setLive(load)
    setIncoming(null)
    setLoading(false)
    setPaused(false)
  }, [])

  // Load a recording's head into the buffer whenever it resolves (or changes):
  // route it through `incoming` like any seek, so `live` is set only once the
  // first frame is actually playing (no initial stretch). Issue #24.
  useEffect(() => {
    if (!src) return
    seekBaseMsRef.current = 0
    seekingRef.current = false
    liveRef.current = null
    loadIdRef.current += 1
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setLive(null)
    setIncoming({ id: loadIdRef.current, src })
    setLoading(true)
  }, [src])

  const atFirstRecording = index <= 0
  const atLastRecording = index >= recordings.length - 1

  // Take focus on mount so the keymap acts immediately, without a click first.
  useEffect(() => {
    containerRef.current?.focus()
  }, [])

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
    // A fresh recording starts at its head; the buffer is (re)loaded from the new
    // `src` by the load effect once it resolves. Clear the previous recording's
    // loads now so the ready branch can't briefly re-show its last frame while
    // the new source resolves (issue #24).
    seekBaseMsRef.current = 0
    seekingRef.current = false
    liveRef.current = null
    setLive(null)
    setIncoming(null)
    setLoading(true)

    const resolve = () => {
      trackedInvoke<PlaybackSource>("resolve_playback", { path })
        .then((source) => {
          if (cancelled) return
          if (source.state === "ready") {
            setSrc(playUrl(endpoint, source.path))
            setFps(source.fps && source.fps > 0 ? source.fps : DEFAULT_FPS)
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

  // Seek to a recording-local position (ms) by loading a `&t=` stream positioned
  // there into the incoming buffer, rather than writing `currentTime` — this
  // WebKitGTK build silently drops `currentTime` seeks (issue #24). The live
  // element is paused so it holds its current frame while the incoming stream
  // loads (hidden); the incoming element `autoPlay`s and is promoted on its first
  // `playing`, so a seek resumes playback and never flashes black. `currentMs` is
  // set optimistically so rally-to-rally math (e.g. Prev twice) chains off the
  // target immediately; `seekingRef` holds the playhead there until promotion.
  const seekTo = useCallback(
    (ms: number) => {
      if (!src) return
      const target = Math.max(0, Math.round(ms))
      // Freeze the visible frame: a paused `<video>` holds its current frame.
      liveVideo()?.pause()
      seekBaseMsRef.current = target
      seekingRef.current = true
      setCurrentMs(target)
      setLoading(true)
      loadIdRef.current += 1
      setIncoming({ id: loadIdRef.current, src: urlAt(src, target) })
    },
    [src, liveVideo, urlAt]
  )

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
    /* eslint-disable react-hooks/set-state-in-effect -- once the crossed-into recording is ready this resumes by seeking (which sets state), the whole point of the effect */
    if (status !== "ready" || pendingSeek == null) return
    if (typeof pendingSeek === "object") {
      seekTo(pendingSeek.atMs)
      setPendingSeek(null)
      return
    }
    if (rallies.length === 0) {
      setPendingSeek(null)
      void liveVideo()?.play()
      return
    }
    const target =
      pendingSeek === "start" ? rallies[0] : rallies[rallies.length - 1]
    seekTo(target.start_ms)
    setPendingSeek(null)
    /* eslint-enable react-hooks/set-state-in-effect */
  }, [status, pendingSeek, rallies, seekTo, liveVideo])

  // Gap-free playback (the North Star): as the playhead crosses out of a rally
  // into a gap, jump straight to the next rally's start so only play is watched
  // (ADR 0001). Reads the current saved timeline, so later corrections take
  // effect. Past the final rally of this recording, advance into the next
  // recording (gaps between files are skipped too); only the session's very last
  // rally ends playback. With no rallies, this is inert and the recording plays
  // normally until its timeline arrives.
  const skipGaps = useCallback(
    (ms: number) => {
      if (rallies.length === 0) return
      // Rally-loop (issue #21): with loop on, reaching the current rally's end
      // seeks back to that rally's start instead of gap-skipping onward, so a
      // single point replays on repeat. The "current rally" is the latest one
      // starting at or before the playhead, so Prev/Next rally moves the loop to
      // the newly-selected rally for free. In a gap before any rally (no current
      // rally) the mode is inert until a rally is re-entered. Loop overrides the
      // gap-skip below but leaves free play (a manual move into a gap) alone.
      if (looping && !freePlayRef.current) {
        const current = [...rallies].reverse().find((r) => r.start_ms <= ms)
        if (current && ms >= current.end_ms) {
          seekTo(current.start_ms)
        }
        return
      }
      // The user moved the playhead manually into a gap → play it through, don't
      // yank ahead to the next rally.
      if (freePlayRef.current) return
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
        liveVideo()?.pause()
      }
    },
    [rallies, looping, seekTo, atLastRecording, goToRecording, index, liveVideo]
  )

  // Manual rally-to-rally navigation, across recording boundaries. Next jumps to
  // the first rally starting after the playhead, or into the next recording's
  // first rally when none is left. Prev follows the music-player rule: from
  // inside a rally the first press rewinds to that rally's start, and a second
  // press (now at the start) steps to the previous rally — crossing back into the
  // previous recording's last rally when at the first rally of this one. Reads
  // `currentMs` (kept current by `seekTo`'s optimistic update), so repeated
  // presses chain reliably instead of recomputing from a stale `currentTime`.
  const goToRally = useCallback(
    (direction: "next" | "prev") => {
      // A rally-targeted jump is an explicit gap-free intent — leave free play.
      freePlayRef.current = false
      const ms = currentMs
      if (direction === "next") {
        const target = rallies.find((r) => r.start_ms > ms + 1)
        if (target) {
          seekTo(target.start_ms)
        } else if (!atLastRecording) {
          goToRecording(index + 1, "start")
        }
      } else {
        // The rally we're in or just past: the latest one starting at or before
        // the playhead.
        const current = [...rallies].reverse().find((r) => r.start_ms <= ms)
        // Played meaningfully into it → restart it (first press).
        if (current && ms > current.start_ms + PREV_RESTART_SLACK_MS) {
          seekTo(current.start_ms)
          return
        }
        // At/near its start (or ahead of every rally) → step to the previous one.
        const boundary = current ? current.start_ms : ms
        const target = [...rallies].reverse().find((r) => r.start_ms < boundary)
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

  // Keep the live video element's playback rate and mute in sync with transport
  // state (issue #19), re-applying whenever the live load changes (a seek
  // promotes a fresh element, and the incoming element loads muted, so the newly
  // promoted one must inherit these).
  useEffect(() => {
    const media = liveVideo()
    if (!media) return
    media.playbackRate = SPEED_LADDER[speedIndex]
    media.muted = muted
  }, [speedIndex, muted, live, status, liveVideo])

  // --- Transport (issue #19): the custom bar and keymap drive these. ---

  const togglePlay = useCallback(() => {
    const media = liveVideo()
    if (!media) return
    if (media.paused) void media.play()
    else media.pause()
  }, [liveVideo])

  const toggleMute = useCallback(() => setMuted((m) => !m), [])

  const toggleLoop = useCallback(() => setLooping((l) => !l), [])

  // Step the playback speed along the fixed ladder, clamped at its ends.
  const stepSpeed = useCallback((dir: -1 | 1) => {
    setSpeedIndex((i) => clamp(i + dir, 0, SPEED_LADDER.length - 1))
  }, [])

  const resetSpeed = useCallback(() => setSpeedIndex(DEFAULT_SPEED_INDEX), [])

  // Seek the session by a signed offset in ms, in session-global time so it
  // crosses recording boundaries (reusing the session-seek path). A relative
  // seek is a manual move, so it opts out of gap-free playback like a scrubber
  // drag would. Inert until the current recording is placed on the axis.
  const seekRelative = useCallback(
    (deltaMs: number) => {
      if (globalPlayheadMs == null) return
      seekSession(globalPlayheadMs + deltaMs)
    },
    [globalPlayheadMs, seekSession]
  )

  // Exact frame-step (issue #19): pause, then nudge by one frame using the probed
  // fps. This stays a *direct* `currentTime` write rather than a `&t=` reload
  // (issue #24) — reloading a whole stream per single frame would be absurd, and
  // a paused one-frame nudge is the case least likely to hit the seek-drop bug.
  // Stream `currentTime` is recording-local minus the seek base, so the displayed
  // playhead adds the base back.
  const frameStep = useCallback(
    (dir: -1 | 1) => {
      const media = liveVideo()
      if (!media) return
      media.pause()
      const frame = 1 / (fps > 0 ? fps : DEFAULT_FPS)
      const next = Math.max(0, media.currentTime + dir * frame)
      media.currentTime = next
      setCurrentMs(seekBaseMsRef.current + next * 1000)
    },
    [fps, liveVideo]
  )

  // When the current recording ends naturally (no later rally triggered a skip,
  // e.g. its trailing gap was short), advance into the next recording so the
  // playlist keeps flowing across the boundary.
  const handleEnded = useCallback(() => {
    // The next recording resumes at its first rally — back to gap-free.
    freePlayRef.current = false
    if (!atLastRecording) goToRecording(index + 1, "start")
  }, [atLastRecording, goToRecording, index])

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

  // The single keymap definition (issue #19): the one source of truth behind both
  // the global key handler and the `?` cheat-sheet, so the two can never drift.
  // Each binding declares how it's displayed (`keys`/`label`), whether an event
  // matches it (`match`), and what it does (`run`). Order is the cheat-sheet's
  // listing order. Ctrl-combos that collide with WebKitGTK page-zoom
  // (`Ctrl+-`/`Ctrl+=`/`Ctrl+0`) are matched here so the handler can
  // `preventDefault` them — page-zoom is out of scope.
  const keymap = useMemo<Keybinding[]>(() => {
    const plain = (e: KeyboardEvent) =>
      !e.ctrlKey && !e.metaKey && !e.altKey && !e.shiftKey
    return [
      {
        keys: ["Space", "K"],
        label: "Play / pause",
        match: (e) =>
          plain(e) && (e.code === "Space" || e.key.toLowerCase() === "k"),
        run: togglePlay,
      },
      {
        keys: ["Ctrl+←", "Ctrl+→"],
        label: "Seek ∓ 2.5s",
        match: (e) =>
          (e.ctrlKey || e.metaKey) &&
          !e.shiftKey &&
          !e.altKey &&
          (e.key === "ArrowLeft" || e.key === "ArrowRight"),
        run: (e) =>
          seekRelative(e.key === "ArrowLeft" ? -SEEK_FINE_MS : SEEK_FINE_MS),
      },
      {
        keys: ["←", "→"],
        label: "Seek ∓ 5s",
        match: (e) =>
          plain(e) && (e.key === "ArrowLeft" || e.key === "ArrowRight"),
        run: (e) =>
          seekRelative(
            e.key === "ArrowLeft" ? -SEEK_DEFAULT_MS : SEEK_DEFAULT_MS
          ),
      },
      {
        keys: ["Shift+←", "Shift+→"],
        label: "Seek ∓ 10s",
        match: (e) =>
          e.shiftKey &&
          !e.ctrlKey &&
          !e.metaKey &&
          !e.altKey &&
          (e.key === "ArrowLeft" || e.key === "ArrowRight"),
        run: (e) =>
          seekRelative(
            e.key === "ArrowLeft" ? -SEEK_COARSE_MS : SEEK_COARSE_MS
          ),
      },
      {
        keys: [",", "."],
        label: "Frame step back / forward",
        match: (e) => plain(e) && (e.key === "," || e.key === "."),
        run: (e) => frameStep(e.key === "," ? -1 : 1),
      },
      {
        keys: ["[", "]"],
        label: "Prev / Next rally",
        match: (e) => plain(e) && (e.key === "[" || e.key === "]"),
        run: (e) => goToRally(e.key === "[" ? "prev" : "next"),
      },
      {
        keys: ["U"],
        label: "Next uncertain region",
        match: (e) => plain(e) && e.key.toLowerCase() === "u",
        run: goToUncertain,
      },
      {
        keys: ["L"],
        label: "Loop current rally",
        match: (e) => plain(e) && e.key.toLowerCase() === "l",
        run: toggleLoop,
      },
      {
        keys: ["M"],
        label: "Mute",
        match: (e) => plain(e) && e.key.toLowerCase() === "m",
        run: toggleMute,
      },
      {
        keys: ["Ctrl+-", "Ctrl+="],
        label: "Playback speed down / up",
        // `=`/`-` carry shift on some layouts; accept regardless of shift.
        match: (e) =>
          (e.ctrlKey || e.metaKey) &&
          !e.altKey &&
          (e.key === "-" || e.key === "=" || e.key === "+" || e.key === "_"),
        run: (e) => stepSpeed(e.key === "-" || e.key === "_" ? -1 : 1),
      },
      {
        keys: ["Ctrl+0"],
        label: "Reset speed to 1×",
        match: (e) => (e.ctrlKey || e.metaKey) && !e.altKey && e.key === "0",
        run: resetSpeed,
      },
      {
        keys: ["?"],
        label: "Toggle this cheat-sheet",
        match: (e) => e.key === "?",
        run: () => setShowCheatSheet((s) => !s),
      },
      {
        keys: ["Alt + scroll over timeline"],
        label: "Zoom timeline at the cursor",
        // Handled on the timeline's wheel listener, not here; listed so the
        // cheat-sheet stays the complete reference.
        match: () => false,
        run: () => {},
      },
    ]
  }, [
    togglePlay,
    seekRelative,
    frameStep,
    goToRally,
    goToUncertain,
    toggleLoop,
    toggleMute,
    stepSpeed,
    resetSpeed,
  ])

  // The one global key handler, at window capture so it runs before any
  // native-control default and can `preventDefault` page-zoom (issue #19).
  // Ignores keystrokes while typing in an input/textarea so future text fields
  // aren't hijacked. The video is not a tab stop and the container holds focus,
  // so the old `<video>` key behavior never double-fires.
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement | null
      if (
        target &&
        (target.isContentEditable ||
          target.tagName === "INPUT" ||
          target.tagName === "TEXTAREA" ||
          target.tagName === "SELECT")
      ) {
        return
      }
      const binding = keymap.find((b) => b.match(e))
      if (!binding) return
      e.preventDefault()
      e.stopPropagation()
      binding.run(e)
    }
    window.addEventListener("keydown", onKeyDown, { capture: true })
    return () =>
      window.removeEventListener("keydown", onKeyDown, { capture: true })
  }, [keymap])

  const handleError = useCallback((media: HTMLVideoElement | null) => {
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
    <div
      ref={containerRef}
      // Hold focus on the container so the keymap's window handler is the only
      // thing acting on keystrokes; `outline-none` since this focus is just for
      // key routing, not a visible affordance (issue #19).
      tabIndex={-1}
      className="flex h-full min-h-0 flex-col gap-4 outline-none"
    >
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
          {/* Relative wrapper holding the two double-buffered video elements
              (issue #24): the live one (visible, paused-frame held during a seek)
              and the incoming one (hidden, loading). The fixed flex sizing lives
              here so neither element reflows on a seek. */}
          <div className="relative min-h-0 w-full flex-1 rounded-lg bg-black">
            {[live, incoming].map((load) => {
              if (!load) return null
              const isLive = live?.id === load.id
              return (
                <video
                  // Keyed on the load id so React keeps the live element mounted
                  // and only remounts the slot whose load changed — a fresh
                  // element is a fresh GStreamer pipeline that decodes from its
                  // own keyframe (re-using one element across rapid `&t=` swaps
                  // renders decode garbage, "TV static"). Issue #24.
                  key={load.id}
                  ref={(el) => {
                    if (el) videoEls.current.set(load.id, el)
                    else videoEls.current.delete(load.id)
                  }}
                  // Both elements fill the box and letterbox; only the live one is
                  // visible. The incoming one stays hidden until it's promoted on
                  // its first `playing`, so its stretched first frame (before
                  // `object-contain` applies) is never seen — the live held frame
                  // covers the reload (issue #24).
                  className={`absolute inset-0 size-full rounded-lg bg-black object-contain ${
                    isLive ? "opacity-100" : "opacity-0"
                  }`}
                  src={load.src}
                  // No native `controls` (issue #19): the custom transport bar
                  // below is the only transport, and the element is not a tab stop
                  // so its built-in key handling can't double-fire with the keymap.
                  tabIndex={-1}
                  autoPlay
                  playsInline
                  // The incoming (hidden, loading) element is muted so it can't
                  // double the live audio; the promoted element inherits the
                  // user's mute via the rate/mute effect.
                  muted={muted || !isLive}
                  onError={(e) => handleError(e.currentTarget)}
                  onEnded={() => {
                    if (liveRef.current?.id === load.id) handleEnded()
                  }}
                  onPlay={() => {
                    if (liveRef.current?.id === load.id) setPaused(false)
                  }}
                  onPause={() => {
                    if (liveRef.current?.id === load.id) setPaused(true)
                  }}
                  // First frame rendered: if this is the incoming load, promote it
                  // (reveals it, drops the spinner, unmounts the old live); if it's
                  // the live load resuming, just clear the loading spinner (#24).
                  onPlaying={() => {
                    if (incoming && load.id === incoming.id) {
                      promote(load)
                      return
                    }
                    setPaused(false)
                    if (liveRef.current?.id === load.id) setLoading(false)
                  }}
                  onClick={togglePlay}
                  onTimeUpdate={(e) => {
                    // Only the live element drives the playhead; ignore the
                    // hidden incoming one's pre-promotion reports.
                    if (liveRef.current?.id !== load.id) return
                    // While a seek is settling, hold the playhead at the optimistic
                    // target until the incoming element is promoted (issue #24).
                    if (seekingRef.current) return
                    // Stream `currentTime` is recording-local minus the seek base.
                    const ms =
                      seekBaseMsRef.current + e.currentTarget.currentTime * 1000
                    setCurrentMs(ms)
                    skipGaps(ms)
                  }}
                />
              )
            })}
            {/* Spinner over the held frame while the (re)load hasn't rendered, so
                a seek reads as "loading the next position", not a frozen app. */}
            {loading ? (
              <div className="pointer-events-none absolute inset-0 flex items-center justify-center">
                <Loader2Icon className="size-8 animate-spin text-white/80" />
              </div>
            ) : null}
          </div>
          <TransportBar
            paused={paused}
            muted={muted}
            looping={looping}
            speed={SPEED_LADDER[speedIndex]}
            positionMs={globalPlayheadMs ?? 0}
            durationMs={session.totalMs}
            onTogglePlay={togglePlay}
            onFrameStep={frameStep}
            onToggleMute={toggleMute}
            onToggleLoop={toggleLoop}
            onStepSpeed={stepSpeed}
            onResetSpeed={resetSpeed}
            onShowKeys={() => setShowCheatSheet(true)}
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
        <div className="flex min-h-0 w-full flex-1 items-center justify-center rounded-lg bg-black">
          <Loader2Icon className="size-8 animate-spin text-white/80" />
        </div>
      )}
      {showCheatSheet ? (
        <CheatSheet keymap={keymap} onClose={() => setShowCheatSheet(false)} />
      ) : null}
    </div>
  )
}

/**
 * The transport-only control bar beneath the player (issue #19): media transport
 * only — play/pause, exact frame-step, a session-global timecode mirroring the
 * session playhead and total duration, a playback-speed indicator, a loop toggle
 * (its behavior wired up by the rally-loop slice), and mute. It deliberately has
 * **no scrubber**: the session timeline strip below remains the single
 * scrub/seek surface, and fullscreen is out of scope. The `?` button opens the
 * cheat-sheet listing every key, which the same keymap drives.
 */
function TransportBar({
  paused,
  muted,
  looping,
  speed,
  positionMs,
  durationMs,
  onTogglePlay,
  onFrameStep,
  onToggleMute,
  onToggleLoop,
  onStepSpeed,
  onResetSpeed,
  onShowKeys,
}: {
  paused: boolean
  muted: boolean
  looping: boolean
  speed: number
  positionMs: number
  durationMs: number
  onTogglePlay: () => void
  onFrameStep: (dir: -1 | 1) => void
  onToggleMute: () => void
  onToggleLoop: () => void
  onStepSpeed: (dir: -1 | 1) => void
  onResetSpeed: () => void
  onShowKeys: () => void
}) {
  return (
    <div className="flex shrink-0 flex-wrap items-center gap-2">
      <Button
        variant="outline"
        size="icon"
        onClick={onTogglePlay}
        title={paused ? "Play (Space)" : "Pause (Space)"}
      >
        {paused ? (
          <PlayIcon className="size-4" />
        ) : (
          <PauseIcon className="size-4" />
        )}
      </Button>
      <Button
        variant="outline"
        size="icon"
        onClick={() => onFrameStep(-1)}
        title="Step back one frame (,)"
      >
        <StepBackIcon className="size-4" />
      </Button>
      <Button
        variant="outline"
        size="icon"
        onClick={() => onFrameStep(1)}
        title="Step forward one frame (.)"
      >
        <StepForwardIcon className="size-4" />
      </Button>
      <span className="ml-1 font-mono text-sm text-muted-foreground tabular-nums">
        {formatTimecode(positionMs)} / {formatTimecode(durationMs)}
      </span>
      <div className="ml-auto flex items-center gap-2">
        <div
          className="flex items-center"
          title="Playback speed (Ctrl+- / Ctrl+= , Ctrl+0 to reset)"
        >
          <Button
            variant="outline"
            size="icon"
            onClick={() => onStepSpeed(-1)}
            title="Slower (Ctrl+-)"
          >
            <span className="text-sm">−</span>
          </Button>
          <button
            type="button"
            onClick={onResetSpeed}
            title="Reset speed to 1× (Ctrl+0)"
            className="min-w-12 px-1 text-center font-mono text-sm text-muted-foreground tabular-nums hover:text-foreground"
          >
            {speed}×
          </button>
          <Button
            variant="outline"
            size="icon"
            onClick={() => onStepSpeed(1)}
            title="Faster (Ctrl+=)"
          >
            <span className="text-sm">+</span>
          </Button>
        </div>
        <Button
          variant={looping ? "default" : "outline"}
          size="icon"
          onClick={onToggleLoop}
          title={looping ? "Stop looping (L)" : "Loop the current rally (L)"}
        >
          <RepeatIcon className="size-4" />
        </Button>
        <Button
          variant="outline"
          size="icon"
          onClick={onToggleMute}
          title={muted ? "Unmute (M)" : "Mute (M)"}
        >
          {muted ? (
            <VolumeXIcon className="size-4" />
          ) : (
            <Volume2Icon className="size-4" />
          )}
        </Button>
        <Button
          variant="outline"
          size="icon"
          onClick={onShowKeys}
          title="Keyboard shortcuts (?)"
        >
          <KeyboardIcon className="size-4" />
        </Button>
      </div>
    </div>
  )
}

/**
 * The `?` cheat-sheet overlay (issue #19): a modal listing every keybinding,
 * rendered straight from the single keymap definition so it can never drift from
 * what the keys actually do. Click the backdrop or the close button to dismiss
 * (the `?` key toggles it too).
 */
function CheatSheet({
  keymap,
  onClose,
}: {
  keymap: Keybinding[]
  onClose: () => void
}) {
  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4"
      onClick={onClose}
    >
      <div
        className="max-h-full w-full max-w-md overflow-y-auto rounded-lg border bg-background p-5 shadow-lg"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="mb-3 flex items-center justify-between">
          <h2 className="font-medium">Keyboard shortcuts</h2>
          <Button
            variant="ghost"
            size="icon"
            onClick={onClose}
            title="Close (?)"
          >
            <XIcon className="size-4" />
          </Button>
        </div>
        <dl className="space-y-1.5 text-sm">
          {keymap.map((b) => (
            <div
              key={b.label}
              className="flex items-center justify-between gap-4"
            >
              <dt className="text-muted-foreground">{b.label}</dt>
              <dd className="flex flex-wrap justify-end gap-1">
                {b.keys.map((k) => (
                  <kbd
                    key={k}
                    className="rounded border bg-muted px-1.5 py-0.5 font-mono text-xs"
                  >
                    {k}
                  </kbd>
                ))}
              </dd>
            </div>
          ))}
        </dl>
      </div>
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

  // Alt+scroll over the strip zooms centered on the cursor (issue #19): the
  // session time under the pointer stays fixed while the scale changes, distinct
  // from the zoom buttons' playhead-centering. A non-passive wheel listener is
  // needed so the zoom can `preventDefault` the would-be scroll. Plain scroll
  // (no Alt) is left to the browser as ordinary horizontal scrolling.
  useEffect(() => {
    const el = scrollRef.current
    if (!el) return
    const onWheel = (e: WheelEvent) => {
      if (!e.altKey) return
      e.preventDefault()
      const factor =
        e.deltaY < 0 ? ALT_SCROLL_ZOOM_FACTOR : 1 / ALT_SCROLL_ZOOM_FACTOR
      const rect = el.getBoundingClientRect()
      // Session time (px) under the cursor before the zoom.
      const cursorContentPx = e.clientX - rect.left + el.scrollLeft
      setPxPerSec((p) => {
        const nextPx = clamp(
          p * factor,
          SESSION_PX_PER_SEC_MIN,
          SESSION_PX_PER_SEC_MAX
        )
        const scale = nextPx / p
        // Keep the time under the cursor pinned: new scrollLeft so the same
        // content point lands back under the pointer at the new scale.
        el.scrollLeft = cursorContentPx * scale - (e.clientX - rect.left)
        return nextPx
      })
    }
    el.addEventListener("wheel", onWheel, { passive: false })
    return () => el.removeEventListener("wheel", onWheel)
  }, [])

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
