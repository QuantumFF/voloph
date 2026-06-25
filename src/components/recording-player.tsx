"use client"

import { useCallback, useEffect, useRef, useState } from "react"
import { ArrowLeftIcon, Loader2Icon } from "lucide-react"

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

/** How long to wait before re-checking a recording that is still transcoding. */
const TRANSCODE_POLL_MS = 2000

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
        <video
          ref={videoRef}
          // Re-mount when the resolved source changes so the element reloads.
          key={src}
          className="w-full rounded-lg bg-black"
          src={src}
          controls
          autoPlay
          onError={handleError}
        />
      ) : (
        <div className="flex aspect-video w-full items-center justify-center rounded-lg bg-black" />
      )}
    </div>
  )
}
