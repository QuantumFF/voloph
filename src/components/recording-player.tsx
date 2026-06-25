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

/** A rally interval over the recording (see `TimelineRally` in `src-tauri/src/db.rs`). */
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

/** How long to wait before re-checking a recording that is still transcoding. */
const TRANSCODE_POLL_MS = 2000

/** How long to wait before re-checking a recording whose timeline is still being produced. */
const SEGMENT_POLL_MS = 2000

/**
 * Confidence below which a rally is shown as an "uncertain region" — a span the
 * segmenter doubts, surfaced as "check this" during review (ADR 0002).
 */
const UNCERTAIN_CONFIDENCE = 0.5

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
 * Plays a whole **session** as one continuous playlist (the North Star): the
 * rallies of every recording, in capture-time order, played back-to-back with
 * gaps skipped. A single `<video>` element plays one recording at a time; when
 * the playhead runs past the last rally of the current recording the player
 * advances to the next recording and resumes from its first rally, so file
 * boundaries are invisible. Rally-to-rally navigation likewise crosses
 * boundaries: Next from the final rally steps into the next recording, Prev from
 * the first rally steps back into the previous one.
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
 * file it cannot yet skip through.
 */
export function RecordingPlayer({ recordings, startIndex = 0, onBack }: RecordingPlayerProps) {
  const videoRef = useRef<HTMLVideoElement>(null)
  const [endpoint, setEndpoint] = useState<PlaybackEndpoint | null>(null)
  const [src, setSrc] = useState<string | null>(null)
  // loading: resolving; preparing: still transcoding; error: unplayable.
  const [status, setStatus] = useState<"loading" | "preparing" | "ready" | "error">("loading")
  // Exact MediaError detail surfaced on failure, so a playback problem reports
  // its cause (decode vs. fetch vs. unsupported source) instead of a black box.
  const [errorDetail, setErrorDetail] = useState<string | null>(null)
  // The draft timeline (rallies + per-region confidence) for the current recording.
  const [timeline, setTimeline] = useState<Timeline | null>(null)
  // Current playhead position (ms), tracked so the timeline strip can show it.
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
  // timeline arrives after a boundary crossing: "start" plays from the first
  // rally (advancing forward), "end" from the last (stepping backward via Prev).
  const [index, setIndex] = useState(() =>
    Math.min(Math.max(startIndex, 0), Math.max(recordings.length - 1, 0)),
  )
  const [pendingSeek, setPendingSeek] = useState<"start" | "end" | null>(null)
  const path = recordings[index]?.path ?? null

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

  // Fetch the draft timeline for the current recording, polling while
  // segmentation is still running so the rallies appear as soon as the worker
  // finishes (ADR 0002).
  useEffect(() => {
    if (!path) return
    let cancelled = false
    let timer: ReturnType<typeof setTimeout> | undefined
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setTimeline(null)

    const load = () => {
      trackedInvoke<Timeline>("recording_timeline", { path })
        .then((result) => {
          if (cancelled) return
          setTimeline(result)
          if (result.segment_state === "unknown") {
            timer = setTimeout(load, SEGMENT_POLL_MS)
          }
        })
        .catch(() => {
          // A timeline failure is non-fatal — playback still works without it.
        })
    }
    load()

    return () => {
      cancelled = true
      if (timer) clearTimeout(timer)
    }
  }, [path, reanalyzeNonce])

  // Seek the player to a position (ms) and resume playback.
  const seekTo = useCallback((ms: number) => {
    const media = videoRef.current
    if (!media) return
    media.currentTime = ms / 1000
    void media.play()
  }, [])

  // Rallies for the current recording, ascending by start (sorted by
  // construction in segment.rs). The empty list when there's no timeline means
  // playback is plain — the recording plays straight through.
  const rallies = useMemo(() => timeline?.rallies ?? [], [timeline])

  // Move the playlist to another recording, remembering where to resume once it
  // loads. Forward crossings resume from the first rally, backward from the last.
  const goToRecording = useCallback(
    (next: number, resume: "start" | "end") => {
      setIndex(next)
      setPendingSeek(resume)
    },
    [],
  )

  // After a boundary crossing, once the new recording's timeline is ready, seek
  // to the requested edge (first/last rally) and play. With no rallies yet there
  // is nothing to seek to, so the recording plays from the top until its draft
  // timeline arrives and a later pass re-applies the resume.
  useEffect(() => {
    if (status !== "ready" || !pendingSeek) return
    if (rallies.length === 0) {
      // Forward into a not-yet-segmented recording: play from the start anyway
      // so the playlist never stalls. Backward we already sit at the start.
      // eslint-disable-next-line react-hooks/set-state-in-effect
      setPendingSeek(null)
      void videoRef.current?.play()
      return
    }
    const target = pendingSeek === "start" ? rallies[0] : rallies[rallies.length - 1]
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
    [rallies, seekTo, atLastRecording, goToRecording, index],
  )

  // Manual rally-to-rally navigation, across recording boundaries. Next jumps to
  // the first rally starting after the playhead, or into the next recording's
  // first rally when none is left. Previous jumps to the last rally starting
  // before the playhead (with a small slack so it rewinds past the rally you're
  // in), or back into the previous recording's last rally when at the first.
  const goToRally = useCallback(
    (direction: "next" | "prev") => {
      const ms = (videoRef.current?.currentTime ?? 0) * 1000
      if (direction === "next") {
        const target = rallies.find((r) => r.start_ms > ms + 1)
        if (target) {
          seekTo(target.start_ms)
        } else if (!atLastRecording) {
          goToRecording(index + 1, "start")
        }
      } else {
        const target = [...rallies].reverse().find((r) => r.start_ms < ms - 1000)
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
    [rallies, seekTo, atFirstRecording, atLastRecording, goToRecording, index],
  )

  // Jump to the next uncertain region — the spans the segmenter flagged as
  // low-confidence (ADR 0002), surfaced so correction becomes "visit the few
  // spots the machine doubts." Seeks to the first uncertain rally starting after
  // the playhead, wrapping to the first when none is left ahead, so repeated
  // presses cycle through every doubt in the current recording.
  const goToUncertain = useCallback(() => {
    const uncertain = rallies.filter((r) => r.confidence < UNCERTAIN_CONFIDENCE)
    if (uncertain.length === 0) return
    const ms = (videoRef.current?.currentTime ?? 0) * 1000
    const target = uncertain.find((r) => r.start_ms > ms + 1) ?? uncertain[0]
    seekTo(target.start_ms)
  }, [rallies, seekTo])

  // When the current recording ends naturally (no later rally triggered a skip,
  // e.g. its trailing gap was short), advance into the next recording so the
  // playlist keeps flowing across the boundary.
  const handleEnded = useCallback(() => {
    if (!atLastRecording) goToRecording(index + 1, "start")
  }, [atLastRecording, goToRecording, index])

  // Re-run segmentation for the current recording, then re-fetch its timeline.
  // Lets a human iterate on the segmenter's tuning without re-importing (ADR 0002).
  const handleReanalyze = useCallback(() => {
    if (!path) return
    setReanalyzing(true)
    trackedInvoke("reanalyze_recording", { path })
      .then(() => setReanalyzeNonce((n) => n + 1))
      .catch(() => {})
      .finally(() => setReanalyzing(false))
  }, [path])

  // Re-fetch the saved timeline after an inline correction (issue #7) without
  // blanking the strip or restarting segmentation polling — the recording stays
  // `ready`, only its rallies changed. Gap-free playback reads `rallies` on the
  // next tick, so the correction takes effect immediately with no reload.
  const refreshTimeline = useCallback(() => {
    if (!path) return
    trackedInvoke<Timeline>("recording_timeline", { path })
      .then(setTimeline)
      .catch(() => {})
  }, [path])

  // The five inline corrections (issue #7). Each persists immediately to SQLite
  // (surviving restart) and re-reads the timeline so playback reflects it at
  // once. Split and merge are composed from the same row-level edits as the
  // primitive add/adjust/delete — these are the complete set on a rally-interval
  // timeline. Confidence is set certain server-side (a hand-corrected rally is
  // no longer an uncertain region).
  const adjustRally = useCallback(
    (rallyId: number, startMs: number, endMs: number) => {
      if (!path) return
      void trackedInvoke("update_rally", { path, rallyId, startMs, endMs })
        .then(refreshTimeline)
        .catch(() => {})
    },
    [path, refreshTimeline],
  )

  const addRally = useCallback(
    (startMs: number, endMs: number) => {
      if (!path) return
      void trackedInvoke("add_rally", { path, startMs, endMs })
        .then(refreshTimeline)
        .catch(() => {})
    },
    [path, refreshTimeline],
  )

  const deleteRally = useCallback(
    (rallyId: number) => {
      if (!path) return
      void trackedInvoke("delete_rally", { path, rallyId })
        .then(refreshTimeline)
        .catch(() => {})
    },
    [path, refreshTimeline],
  )

  // Split a rally in two at `atMs`: shrink it to end at the cut, then add a new
  // rally from the cut to the old end. A no-op unless the cut falls strictly
  // inside the rally.
  const splitRally = useCallback(
    (rally: Rally, atMs: number) => {
      if (!path || atMs <= rally.start_ms || atMs >= rally.end_ms) return
      void trackedInvoke("update_rally", {
        path,
        rallyId: rally.id,
        startMs: rally.start_ms,
        endMs: Math.round(atMs),
      })
        .then(() =>
          trackedInvoke("add_rally", {
            path,
            startMs: Math.round(atMs),
            endMs: rally.end_ms,
          }),
        )
        .then(refreshTimeline)
        .catch(() => {})
    },
    [path, refreshTimeline],
  )

  // Merge a rally with the one after it: stretch the first to cover both, then
  // delete the second. The gap between them is absorbed into the joined rally.
  const mergeRallies = useCallback(
    (first: Rally, second: Rally) => {
      if (!path) return
      void trackedInvoke("update_rally", {
        path,
        rallyId: first.id,
        startMs: Math.min(first.start_ms, second.start_ms),
        endMs: Math.max(first.end_ms, second.end_ms),
      })
        .then(() => trackedInvoke("delete_rally", { path, rallyId: second.id }))
        .then(refreshTimeline)
        .catch(() => {})
    },
    [path, refreshTimeline],
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
    <div className="space-y-4">
      <div className="flex items-center gap-3">
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
        <div className="flex aspect-video w-full flex-col items-center justify-center gap-3 rounded-lg bg-black p-6 text-center text-sm text-destructive-foreground">
          <p>This recording could not be played.</p>
          {errorDetail ? (
            <p className="max-w-full break-words font-mono text-xs text-muted-foreground">
              {errorDetail}
            </p>
          ) : null}
        </div>
      ) : status === "preparing" ? (
        <div className="flex aspect-video w-full flex-col items-center justify-center gap-3 rounded-lg bg-black p-6 text-center text-sm text-muted-foreground">
          <Loader2Icon className="size-6 animate-spin" />
          Preparing this recording for playback (transcoding for the first time)…
        </div>
      ) : status === "ready" && src ? (
        <>
          <video
            ref={videoRef}
            // Re-mount when the resolved source changes so the element reloads.
            key={src}
            className="w-full rounded-lg bg-black"
            src={src}
            controls
            autoPlay
            onError={handleError}
            onEnded={handleEnded}
            onTimeUpdate={(e) => {
              const ms = e.currentTarget.currentTime * 1000
              setCurrentMs(ms)
              skipGaps(ms)
            }}
          />
          <RallyTimeline
            timeline={timeline}
            currentMs={currentMs}
            onSeek={seekTo}
            onPrevRally={() => goToRally("prev")}
            onNextRally={() => goToRally("next")}
            onNextUncertain={goToUncertain}
            onReanalyze={handleReanalyze}
            reanalyzing={reanalyzing}
            canPrev={!atFirstRecording}
            canNext={!atLastRecording}
            editing={editing}
            onToggleEditing={() => setEditing((e) => !e)}
            onAdjustRally={adjustRally}
            onAddRally={addRally}
            onDeleteRally={deleteRally}
            onSplitRally={splitRally}
            onMergeRallies={mergeRallies}
          />
        </>
      ) : (
        <div className="flex aspect-video w-full items-center justify-center rounded-lg bg-black" />
      )}
    </div>
  )
}

/**
 * The draft timeline strip beneath the player (issue #6): the recording's audio
 * waveform fills it — shuttle hits show as spikes, so rally boundaries can be
 * eyeballed — with each detected rally drawn as a block over it, gaps the empty
 * space between them (ADR 0001), and low-confidence rallies styled as uncertain
 * regions to "check this" (ADR 0002). Clicking anywhere on the strip seeks the
 * player to that point (a rally block seeks to its start); a playhead marker
 * tracks the current position. Prev/Next rally cross recording boundaries, so
 * they stay enabled at a recording's edges when another recording remains in the
 * session playlist; Next uncertain cycles through the spans the segmenter
 * doubts. The Re-analyze button re-runs segmentation in place — the loop for
 * tuning the heuristic (see `docs/tuning-segmentation.md`).
 */
function RallyTimeline({
  timeline,
  currentMs,
  onSeek,
  onPrevRally,
  onNextRally,
  onNextUncertain,
  onReanalyze,
  reanalyzing,
  canPrev,
  canNext,
  editing,
  onToggleEditing,
  onAdjustRally,
  onAddRally,
  onDeleteRally,
  onSplitRally,
  onMergeRallies,
}: {
  timeline: Timeline | null
  currentMs: number
  onSeek: (ms: number) => void
  onPrevRally: () => void
  onNextRally: () => void
  onNextUncertain: () => void
  onReanalyze: () => void
  reanalyzing: boolean
  canPrev: boolean
  canNext: boolean
  editing: boolean
  onToggleEditing: () => void
  onAdjustRally: (rallyId: number, startMs: number, endMs: number) => void
  onAddRally: (startMs: number, endMs: number) => void
  onDeleteRally: (rallyId: number) => void
  onSplitRally: (rally: Rally, atMs: number) => void
  onMergeRallies: (first: Rally, second: Rally) => void
}) {
  // The rally currently picked for split/merge/delete in correction mode.
  const [selectedId, setSelectedId] = useState<number | null>(null)
  // While dragging an edge: which rally/edge and the in-progress ms, so the
  // block redraws live and we only persist on release.
  const [drag, setDrag] = useState<{
    rallyId: number
    edge: "start" | "end"
    ms: number
  } | null>(null)
  const stripRef = useRef<HTMLDivElement>(null)

  const duration = timeline?.duration_ms ?? 0

  // Map a client x-coordinate over the strip to a time in ms (clamped). Drives
  // both edge-dragging and the live position while dragging.
  const xToMs = useCallback(
    (clientX: number): number => {
      const rect = stripRef.current?.getBoundingClientRect()
      if (!rect || rect.width === 0) return 0
      const frac = (clientX - rect.left) / rect.width
      return Math.round(Math.min(Math.max(frac, 0), 1) * duration)
    },
    [duration],
  )

  // While dragging a rally edge, follow the pointer anywhere on the page and
  // persist the new boundary on release. Bound globally so the drag survives the
  // pointer leaving the thin strip. Hooks run before the early return below, so
  // this is unconditional even when there's no timeline (then `drag` stays null
  // and the effect is inert).
  useEffect(() => {
    if (!drag) return
    const rallies = timeline?.rallies ?? []
    const move = (e: PointerEvent) =>
      setDrag((d) => (d ? { ...d, ms: xToMs(e.clientX) } : d))
    const up = () => {
      setDrag((d) => {
        if (d) {
          const rally = rallies.find((r) => r.id === d.rallyId)
          if (rally) {
            const start = d.edge === "start" ? d.ms : rally.start_ms
            const end = d.edge === "end" ? d.ms : rally.end_ms
            onAdjustRally(rally.id, start, end)
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
  }, [drag, timeline, xToMs, onAdjustRally])

  if (!timeline) return null

  const analyzing = reanalyzing || timeline.segment_state === "unknown"
  const hasRallies = duration > 0 && timeline.rallies.length > 0
  const uncertainCount = timeline.rallies.filter(
    (r) => r.confidence < UNCERTAIN_CONFIDENCE,
  ).length

  // The summary text mirrors the segmentation lifecycle (ADR 0002).
  let summary
  if (analyzing) {
    summary = (
      <span className="flex items-center gap-2">
        <Loader2Icon className="size-4 animate-spin" />
        Detecting rallies…
      </span>
    )
  } else if (timeline.segment_state === "failed") {
    summary = <span>Couldn&apos;t detect rallies for this recording.</span>
  } else if (!hasRallies) {
    summary = <span>No rallies detected.</span>
  } else {
    summary = (
      <span>
        {timeline.rallies.length}{" "}
        {timeline.rallies.length === 1 ? "rally" : "rallies"} detected
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

  // Clamp the playhead into the strip; only shown once we know the duration.
  const playheadPct =
    duration > 0 ? Math.min(Math.max((currentMs / duration) * 100, 0), 100) : null

  // The selected rally and its neighbour (for merge), recomputed each render so
  // they stay valid as the timeline changes under an edit.
  const rallies = timeline.rallies
  const selectedIndex = rallies.findIndex((r) => r.id === selectedId)
  const selected = selectedIndex >= 0 ? rallies[selectedIndex] : null
  const nextRally = selectedIndex >= 0 ? rallies[selectedIndex + 1] ?? null : null
  // A split is only meaningful when the playhead falls strictly inside the
  // selected rally; merge needs a following rally to join.
  const canSplit =
    selected !== null && currentMs > selected.start_ms && currentMs < selected.end_ms
  const canMerge = selected !== null && nextRally !== null

  return (
    <div className="space-y-2">
      <div className="flex items-center justify-between text-sm text-muted-foreground">
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
            disabled={analyzing}
            title="Re-run rally detection in place (for tuning the segmenter)."
          >
            <RotateCwIcon className={`size-4 ${reanalyzing ? "animate-spin" : ""}`} />
            Re-analyze
          </Button>
          <Button
            variant={editing ? "default" : "outline"}
            size="sm"
            onClick={onToggleEditing}
            disabled={analyzing || timeline.segment_state === "failed"}
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
                  selected.start_ms,
                )}–${formatClock(selected.end_ms)})`
              : "Drag a rally's edge to adjust it, or click a rally to select it."}
          </span>
          <div className="ml-auto flex flex-wrap items-center gap-2">
            <Button
              variant="outline"
              size="sm"
              onClick={() => {
                // Add a rally over the missed span around the playhead, then
                // select it for further tweaking.
                const start = Math.max(0, Math.round(currentMs - 2000))
                const end = Math.min(duration, Math.round(currentMs + 2000))
                onAddRally(start, end)
              }}
              title="Add a rally over a span the segmenter missed (around the playhead)."
            >
              <PlusIcon className="size-4" />
              Add at playhead
            </Button>
            <Button
              variant="outline"
              size="sm"
              onClick={() => selected && onSplitRally(selected, currentMs)}
              disabled={!canSplit}
              title="Split the selected rally in two at the playhead."
            >
              <ScissorsIcon className="size-4" />
              Split at playhead
            </Button>
            <Button
              variant="outline"
              size="sm"
              onClick={() => selected && nextRally && onMergeRallies(selected, nextRally)}
              disabled={!canMerge}
              title="Merge the selected rally with the next one."
            >
              Merge with next
            </Button>
            <Button
              variant="outline"
              size="sm"
              onClick={() => {
                if (selected) {
                  onDeleteRally(selected.id)
                  setSelectedId(null)
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
        <div
          ref={stripRef}
          className="relative h-12 w-full cursor-pointer overflow-hidden rounded-md bg-muted"
          onClick={(e) => {
            // Clicking empty strip seeks playback to that point (issue #6): map
            // the click's x within the strip to a fraction of the recording.
            // While editing, clicking empty strip clears the selection instead.
            // Rally blocks stop propagation (seek, or select while editing).
            if (editing) {
              setSelectedId(null)
              return
            }
            const rect = e.currentTarget.getBoundingClientRect()
            const frac = (e.clientX - rect.left) / rect.width
            onSeek(Math.min(Math.max(frac, 0), 1) * duration)
          }}
        >
          <Waveform peaks={timeline.waveform} />
          {rallies.map((rally, i) => {
            // While dragging this rally's edge, draw it at the live position so
            // the resize is visible before it persists on release.
            const dragging = drag?.rallyId === rally.id ? drag : null
            const startMs = dragging?.edge === "start" ? dragging.ms : rally.start_ms
            const endMs = dragging?.edge === "end" ? dragging.ms : rally.end_ms
            const lo = Math.min(startMs, endMs)
            const hi = Math.max(startMs, endMs)
            const left = (lo / duration) * 100
            const width = ((hi - lo) / duration) * 100
            const uncertain = rally.confidence < UNCERTAIN_CONFIDENCE
            const isSelected = editing && rally.id === selectedId
            return (
              <button
                key={rally.id}
                type="button"
                onClick={(e) => {
                  e.stopPropagation()
                  if (editing) {
                    setSelectedId(rally.id)
                  } else {
                    onSeek(rally.start_ms)
                  }
                }}
                className={`absolute inset-y-0 rounded-sm transition-opacity hover:opacity-80 ${
                  uncertain
                    ? "border border-amber-500/70 bg-amber-500/40"
                    : "bg-primary/70"
                } ${isSelected ? "ring-2 ring-foreground ring-offset-1 ring-offset-muted" : ""}`}
                style={{ left: `${left}%`, width: `${Math.max(width, 0.4)}%` }}
                title={`Rally ${i + 1}: ${formatClock(rally.start_ms)}–${formatClock(
                  rally.end_ms,
                )}${uncertain ? " (uncertain)" : ""} · confidence ${Math.round(
                  rally.confidence * 100,
                )}%`}
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
                        setSelectedId(rally.id)
                        setDrag({ rallyId: rally.id, edge: "start", ms: rally.start_ms })
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
                        setSelectedId(rally.id)
                        setDrag({ rallyId: rally.id, edge: "end", ms: rally.end_ms })
                      }}
                      className="absolute inset-y-0 right-0 w-1.5 cursor-ew-resize rounded-r-sm bg-foreground/70 hover:bg-foreground"
                    />
                  </>
                ) : null}
              </button>
            )
          })}
          {playheadPct !== null ? (
            <div
              className="pointer-events-none absolute inset-y-0 w-0.5 bg-foreground"
              style={{ left: `${playheadPct}%` }}
            />
          ) : null}
        </div>
      ) : null}
    </div>
  )
}

/**
 * The audio waveform under the rally blocks (issue #6): each downsampled peak
 * is a vertical bar centred on the strip, so shuttle hits read as spikes and
 * rally boundaries can be eyeballed where the blocks overlay them. Drawn behind
 * the blocks at low contrast (pointer-events disabled so strip clicks seek) and
 * stretched to fill the strip via a viewBox in normalized peak coordinates.
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
