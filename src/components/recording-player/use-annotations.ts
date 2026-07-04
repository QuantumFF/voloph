"use client"

import { useCallback, useEffect, useState } from "react"

import { trackedInvoke } from "@/lib/tauri"
import type {
  Annotation,
  PlaylistRecording,
  Verdict,
} from "@/components/recording-player-transport"

/**
 * Every recording's verdict annotations (issue #8), keyed by path so they
 * survive switching recordings and can be stitched onto the one session strip.
 * Fetched once per recording on load (annotations only change from our own
 * drops, so no polling); `add` drops one at the given recording-local time then
 * re-reads that recording so the marker appears at once.
 */
export function useAnnotations(recordings: PlaylistRecording[]) {
  const [annotations, setAnnotations] = useState<Record<string, Annotation[]>>(
    {}
  )

  const load = useCallback((path: string) => {
    trackedInvoke<Annotation[]>("recording_annotations", { path })
      .then((result) =>
        setAnnotations((prev) => ({ ...prev, [path]: result }))
      )
      .catch(() => {})
  }, [])

  useEffect(() => {
    recordings.forEach((rec) => load(rec.path))
  }, [recordings, load])

  const add = useCallback(
    (path: string, timeMs: number, verdict: Verdict) => {
      void trackedInvoke("add_annotation", {
        path,
        timeMs: Math.round(timeMs),
        verdict,
      })
        .then(() => load(path))
        .catch(() => {})
    },
    [load]
  )

  return { annotations, add }
}
