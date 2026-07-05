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

  // Enrich or re-classify an annotation (issue #9): its verdict, aspect (from the
  // seeded vocabulary), and free-text note. Re-reads the recording so the change
  // shows at once.
  const update = useCallback(
    (
      path: string,
      id: number,
      verdict: Verdict,
      aspect: string | null,
      note: string | null
    ) => {
      void trackedInvoke("update_annotation", { path, id, verdict, aspect, note })
        .then(() => load(path))
        .catch(() => {})
    },
    [load]
  )

  const remove = useCallback(
    (path: string, id: number) => {
      void trackedInvoke("delete_annotation", { path, id })
        .then(() => load(path))
        .catch(() => {})
    },
    [load]
  )

  return { annotations, add, update, remove }
}
