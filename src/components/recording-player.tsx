"use client"

import { useCallback, useEffect, useRef, useState } from "react"
import { ArrowLeftIcon } from "lucide-react"

import { Button } from "@/components/ui/button"
import { trackedInvoke } from "@/lib/tauri"

interface RecordingPlayerProps {
  /** Absolute on-disk path of the recording to play. */
  path: string
  /** Return to the session list. */
  onBack: () => void
}

/** Loopback origin + token of the playback server (see `src-tauri/src/media.rs`). */
interface PlaybackEndpoint {
  origin: string
  token: string
}

function fileName(path: string): string {
  const parts = path.split(/[\\/]/)
  return parts[parts.length - 1] || path
}

/**
 * Build a playback URL the webview's `<video>` element can load. The recording
 * path, an optional seek offset, and the per-launch token travel as query
 * parameters so absolute paths survive intact. The origin is the loopback
 * playback server (e.g. `http://127.0.0.1:54321`).
 */
function streamUrl(endpoint: PlaybackEndpoint, path: string, startSeconds: number): string {
  const url = new URL(`${endpoint.origin}/play`)
  url.searchParams.set("path", path)
  url.searchParams.set("token", endpoint.token)
  if (startSeconds > 0) {
    url.searchParams.set("t", String(startSeconds))
  }
  return url.toString()
}

/**
 * Plays a single recording in the in-app player.
 *
 * The source is served through the custom `stream://` protocol (see
 * `src-tauri/src/media.rs`) rather than the raw asset protocol. The Rust side
 * probes the codec and either passes a web-friendly file through untouched
 * (native byte-range seeking) or transcodes it to H.264/AAC on the fly via the
 * ffmpeg sidecar — so HEVC/H.265 iPhone recordings, which WebKitGTK cannot
 * decode, still play.
 *
 * Because a transcoded stream has no stable byte layout, seeking on it cannot
 * use byte ranges; instead a seek restarts the ffmpeg pipeline from the target
 * timestamp. We detect a seek the webview could not satisfy natively and reload
 * the source with a `t` offset, resuming playback from there. Passthrough
 * sources seek natively and never hit this path.
 */
export function RecordingPlayer({ path, onBack }: RecordingPlayerProps) {
  const videoRef = useRef<HTMLVideoElement>(null)
  // Seconds the current stream was started at (the `-ss` offset). A restart on
  // a transcoded source bumps this so reported player time maps back to the
  // real recording position.
  const [startOffset, setStartOffset] = useState(0)
  const [error, setError] = useState<string | null>(null)
  // The loopback playback server endpoint, fetched once from the backend.
  const [endpoint, setEndpoint] = useState<PlaybackEndpoint | null>(null)
  // Guard so the seek handler does not re-trigger itself after we reload.
  const restartingRef = useRef(false)

  // Fetch the playback server endpoint once on mount.
  useEffect(() => {
    let cancelled = false
    trackedInvoke<PlaybackEndpoint>("playback_endpoint")
      .then((result) => {
        if (!cancelled) setEndpoint(result)
      })
      .catch(() => {
        if (!cancelled) setError("Playback is unavailable: the media server did not start.")
      })
    return () => {
      cancelled = true
    }
  }, [])

  // Reset offset and error when the recording changes.
  useEffect(() => {
    setStartOffset(0)
    setError(null)
    restartingRef.current = false
  }, [path])

  const handleSeeking = useCallback(() => {
    const video = videoRef.current
    if (!video || restartingRef.current) return

    // Where the user wants to be, in real recording time.
    const target = video.currentTime + startOffset
    const buffered = video.buffered
    let withinBuffer = false
    for (let i = 0; i < buffered.length; i += 1) {
      if (video.currentTime >= buffered.start(i) && video.currentTime <= buffered.end(i)) {
        withinBuffer = true
        break
      }
    }
    // If the position is already buffered (passthrough or a spot we hold),
    // let the native seek stand. Otherwise restart the stream at the target.
    if (withinBuffer) return

    restartingRef.current = true
    setStartOffset(target)
  }, [startOffset])

  // After a restart, begin playback from the new stream's origin.
  const handleLoadedMetadata = useCallback(() => {
    const video = videoRef.current
    if (!video) return
    if (restartingRef.current) {
      restartingRef.current = false
      void video.play().catch(() => {})
    }
  }, [])

  const handleError = useCallback(() => {
    setError(
      "This recording could not be played. The file may be corrupt or in an unsupported format.",
    )
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
      {error ? (
        <div className="flex aspect-video w-full items-center justify-center rounded-lg bg-black p-6 text-center text-sm text-destructive-foreground">
          {error}
        </div>
      ) : endpoint ? (
        <video
          ref={videoRef}
          // Re-mount when the path or seek offset changes so the source reloads
          // and the ffmpeg pipeline restarts at the new timestamp.
          key={`${path}#${startOffset}`}
          className="w-full rounded-lg bg-black"
          src={streamUrl(endpoint, path, startOffset)}
          controls
          autoPlay
          onSeeking={handleSeeking}
          onLoadedMetadata={handleLoadedMetadata}
          onError={handleError}
        />
      ) : (
        <div className="flex aspect-video w-full items-center justify-center rounded-lg bg-black" />
      )}
    </div>
  )
}
