"use client"

import { useCallback, useEffect, useRef, useState } from "react"
import { ArrowLeftIcon, Loader2Icon, RotateCwIcon } from "lucide-react"

import { Button } from "@/components/ui/button"
import { trackedInvoke } from "@/lib/tauri"

interface RecordingPlayerProps {
  /** Absolute on-disk path of the recording to play. */
  path: string
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

/** A detected rally interval over the recording (see `src-tauri/src/segment.rs`). */
interface Rally {
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
 * Plays a single recording in the in-app player.
 *
 * The source is served by a loopback HTTP server (see `src-tauri/src/media.rs`),
 * not the asset protocol or a custom scheme: WebKitGTK plays HTML5 media through
 * GStreamer, which only loads real `http://` sources, so a `<video>` pointed at
 * `asset://`/`stream://` fails with `MediaError` code 4. The server declares
 * `video/mp4` and supports range requests, so native frame-accurate seeking
 * works. Web-incompatible recordings are transcoded to H.264/AAC in place at
 * import (ADR 0005); the bytes streamed here are always already playable.
 *
 * Because that transcode runs in the background, a freshly imported recording
 * may still be converting when first opened. Rather than point the player at a
 * still-undecodable file, we surface a "preparing" state and poll until ready.
 */
export function RecordingPlayer({ path, onBack }: RecordingPlayerProps) {
  const videoRef = useRef<HTMLVideoElement>(null)
  const [endpoint, setEndpoint] = useState<PlaybackEndpoint | null>(null)
  const [src, setSrc] = useState<string | null>(null)
  // loading: resolving; preparing: still transcoding; error: unplayable.
  const [status, setStatus] = useState<"loading" | "preparing" | "ready" | "error">("loading")
  // Exact MediaError detail surfaced on failure, so a playback problem reports
  // its cause (decode vs. fetch vs. unsupported source) instead of a black box.
  const [errorDetail, setErrorDetail] = useState<string | null>(null)
  // The draft timeline (rallies + per-region confidence) for this recording.
  const [timeline, setTimeline] = useState<Timeline | null>(null)
  // Current playhead position (ms), tracked so the timeline strip can show it.
  const [currentMs, setCurrentMs] = useState(0)
  // Bumped by Re-analyze to re-trigger the timeline fetch/poll after the worker
  // re-segments (the tuning loop, ADR 0002); `reanalyzing` guards the button.
  const [reanalyzeNonce, setReanalyzeNonce] = useState(0)
  const [reanalyzing, setReanalyzing] = useState(false)

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
    if (!endpoint) return
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

  // Fetch the draft timeline for this recording, polling while segmentation is
  // still running so the rallies appear as soon as the worker finishes (ADR 0002).
  useEffect(() => {
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

  // Seek the player to a rally's start and resume playback.
  const seekTo = useCallback((ms: number) => {
    const media = videoRef.current
    if (!media) return
    media.currentTime = ms / 1000
    void media.play()
  }, [])

  // Re-run segmentation for this recording, then re-fetch its timeline. Lets a
  // human iterate on the segmenter's tuning without re-importing (ADR 0002).
  const handleReanalyze = useCallback(() => {
    setReanalyzing(true)
    trackedInvoke("reanalyze_recording", { path })
      .then(() => setReanalyzeNonce((n) => n + 1))
      .catch(() => {})
      .finally(() => setReanalyzing(false))
  }, [path])

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
        <span className="truncate font-medium" title={path}>
          {fileName(path)}
        </span>
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
            onTimeUpdate={(e) => setCurrentMs(e.currentTarget.currentTime * 1000)}
          />
          <RallyTimeline
            timeline={timeline}
            currentMs={currentMs}
            onSeek={seekTo}
            onReanalyze={handleReanalyze}
            reanalyzing={reanalyzing}
          />
        </>
      ) : (
        <div className="flex aspect-video w-full items-center justify-center rounded-lg bg-black" />
      )}
    </div>
  )
}

/**
 * The draft timeline strip beneath the player: each detected rally is a block
 * laid out over the recording's full duration, gaps are the empty space between
 * them (ADR 0001), and low-confidence rallies are styled as uncertain regions to
 * "check this" (ADR 0002). Clicking a rally seeks the player to its start, and a
 * playhead marker tracks the current position. The Re-analyze button re-runs
 * segmentation in place — the loop for tuning the heuristic (see
 * `docs/tuning-segmentation.md`).
 */
function RallyTimeline({
  timeline,
  currentMs,
  onSeek,
  onReanalyze,
  reanalyzing,
}: {
  timeline: Timeline | null
  currentMs: number
  onSeek: (ms: number) => void
  onReanalyze: () => void
  reanalyzing: boolean
}) {
  if (!timeline) return null

  const analyzing = reanalyzing || timeline.segment_state === "unknown"
  const duration = timeline.duration_ms ?? 0
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

  return (
    <div className="space-y-2">
      <div className="flex items-center justify-between text-sm text-muted-foreground">
        {summary}
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
      </div>
      {hasRallies ? (
        <div className="relative h-8 w-full overflow-hidden rounded-md bg-muted">
          {timeline.rallies.map((rally, i) => {
            const left = (rally.start_ms / duration) * 100
            const width = ((rally.end_ms - rally.start_ms) / duration) * 100
            const uncertain = rally.confidence < UNCERTAIN_CONFIDENCE
            return (
              <button
                key={i}
                type="button"
                onClick={() => onSeek(rally.start_ms)}
                className={
                  uncertain
                    ? "absolute inset-y-0 rounded-sm border border-amber-500/70 bg-amber-500/40 transition-opacity hover:opacity-80"
                    : "absolute inset-y-0 rounded-sm bg-primary/70 transition-opacity hover:opacity-80"
                }
                style={{ left: `${left}%`, width: `${Math.max(width, 0.4)}%` }}
                title={`Rally ${i + 1}: ${formatClock(rally.start_ms)}–${formatClock(
                  rally.end_ms,
                )}${uncertain ? " (uncertain)" : ""} · confidence ${Math.round(
                  rally.confidence * 100,
                )}%`}
              />
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
