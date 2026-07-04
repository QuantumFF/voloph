"use client"

import { useCallback, useEffect, useState } from "react"

import { trackedInvoke } from "@/lib/tauri"
import type {
  PlaylistRecording,
  Timeline,
} from "@/components/recording-player-transport"

/** How long to wait before re-checking a recording whose timeline is still being produced. */
const SEGMENT_POLL_MS = 2000

/**
 * Every recording's draft timeline, keyed by path so it survives switching
 * recordings (and so the whole session can be stitched into one strip). Each
 * unsegmented recording is polled until its rallies arrive (ADR 0002), and
 * Re-analyze re-triggers the fetch/poll for the re-segmented recording.
 */
export function useSessionTimelines(recordings: PlaylistRecording[]) {
  const [timelines, setTimelines] = useState<Record<string, Timeline>>({})
  // Bumped by Re-analyze to re-trigger the timeline fetch/poll; `reanalyzing`
  // guards the button (ADR 0002, the tuning loop).
  const [reanalyzeNonce, setReanalyzeNonce] = useState(0)
  const [reanalyzing, setReanalyzing] = useState(false)

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
  // (issue #7) without disturbing the rest of the session.
  const refreshTimeline = useCallback((recordingPath: string) => {
    trackedInvoke<Timeline>("recording_timeline", { path: recordingPath })
      .then((result) =>
        setTimelines((prev) => ({ ...prev, [recordingPath]: result }))
      )
      .catch(() => {})
  }, [])

  // Re-run segmentation for the given recording, then re-fetch timelines.
  const reanalyze = useCallback((path: string) => {
    setReanalyzing(true)
    trackedInvoke("reanalyze_recording", { path })
      .then(() => setReanalyzeNonce((n) => n + 1))
      .catch(() => {})
      .finally(() => setReanalyzing(false))
  }, [])

  return { timelines, refreshTimeline, reanalyzing, reanalyze }
}
