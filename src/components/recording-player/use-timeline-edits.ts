"use client"

import { useCallback } from "react"

import { trackedInvoke } from "@/lib/tauri"
import {
  addAtPlayheadEdit,
  adjustRallyEdit,
  mergeRallyEdit,
  splitRallyEdit,
  type EditPlan,
  type SessionModel,
  type SessionRally,
} from "@/components/recording-player-transport"

/** How wide a rally Add-at-playhead creates around the playhead (ms each side). */
const ADD_RALLY_HALF_MS = 2000

/**
 * The five inline corrections (issue #7). The boundary math and integrity
 * guards that decide what reaches SQLite live behind the transport seam (so
 * they're unit-tested without mpv); here each callback resolves a plan and
 * hands it to `runEdit`, which persists its ops in order then re-reads the
 * affected recording's timeline so playback and the strip reflect it at once.
 */
export function useTimelineEdits({
  session,
  path,
  index,
  currentMs,
  segmentOffset,
  refreshTimeline,
}: {
  session: SessionModel
  /** Path of the current recording (Add-at-playhead's target), if any. */
  path: string | null
  /** Index of the current recording in the playlist. */
  index: number
  /** The playhead within the current recording (ms). */
  currentMs: number
  /** Offset of a recording on the session axis (0 if it isn't placed yet). */
  segmentOffset: (recordingIndex: number) => number
  /** Re-fetch one recording's saved timeline after a persisted correction. */
  refreshTimeline: (recordingPath: string) => void
}) {
  // Recording-local duration of a recording, or +Infinity until it's segmented
  // (the edit math leaves an unknown-duration recording uncapped).
  const recordingDuration = useCallback(
    (recordingIndex: number) =>
      session.segments.find((s) => s.index === recordingIndex)?.durationMs ??
      Number.POSITIVE_INFINITY,
    [session]
  )

  const runEdit = useCallback(
    (plan: EditPlan) => {
      if (plan.kind !== "ops" || plan.ops.length === 0) return
      const recordingPath = plan.ops[0].path
      let chain: Promise<unknown> = Promise.resolve()
      for (const op of plan.ops) {
        const { command, ...args } = op
        chain = chain.then(() => trackedInvoke(command, args))
      }
      void chain.then(() => refreshTimeline(recordingPath)).catch(() => {})
    },
    [refreshTimeline]
  )

  const adjustRally = useCallback(
    (rally: SessionRally, globalStart: number, globalEnd: number) => {
      runEdit(
        adjustRallyEdit(
          rally,
          globalStart,
          globalEnd,
          segmentOffset(rally.recordingIndex),
          recordingDuration(rally.recordingIndex)
        )
      )
    },
    [segmentOffset, recordingDuration, runEdit]
  )

  const addAtPlayhead = useCallback(() => {
    if (!path) return
    runEdit(
      addAtPlayheadEdit(
        path,
        currentMs,
        recordingDuration(index),
        ADD_RALLY_HALF_MS
      )
    )
  }, [path, index, currentMs, recordingDuration, runEdit])

  const deleteRally = useCallback(
    (rally: SessionRally) => {
      runEdit({
        kind: "ops",
        ops: [{ command: "delete_rally", path: rally.path, rallyId: rally.id }],
      })
    },
    [runEdit]
  )

  const splitRally = useCallback(
    (rally: SessionRally, atGlobalMs: number) => {
      runEdit(
        splitRallyEdit(rally, atGlobalMs, segmentOffset(rally.recordingIndex))
      )
    },
    [segmentOffset, runEdit]
  )

  const mergeRallies = useCallback(
    (first: SessionRally, second: SessionRally) => {
      runEdit(mergeRallyEdit(first, second))
    },
    [runEdit]
  )

  return { adjustRally, addAtPlayhead, deleteRally, splitRally, mergeRallies }
}
