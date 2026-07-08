"use client"

import { useCallback, useMemo } from "react"

import { fileName } from "@/lib/format"
import { useExport } from "@/components/use-export"
import type {
  PlaylistRecording,
  SessionAnnotation,
  SessionModel,
} from "@/components/recording-player-transport"

/**
 * The Export engine driven from the player (issues #12/#13/#14): the four Export
 * items point one engine at a different rally selection. The save dialog, live
 * progress readout, and error surfacing live in the shared `useExport` hook; this
 * only maps each selection onto the right command and payload.
 */
export function useSessionExport({
  session,
  sessionAnnotations,
  recordings,
  path,
  day,
}: {
  session: SessionModel
  sessionAnnotations: SessionAnnotation[]
  recordings: PlaylistRecording[]
  /** Path of the current recording (the condensed-recording target), if any. */
  path: string | null
  /** The session's capture day, used to name the output file. */
  day?: string
}) {
  const { progress, error, setError, runExport } = useExport()

  const paths = useMemo(() => recordings.map((r) => r.path), [recordings])

  // Condensed recording (#12): every rally of the open recording, gaps removed.
  const exportCondensed = useCallback(() => {
    if (!path) return
    return runExport(
      "Export condensed recording",
      `${fileName(path).replace(/\.[^.]+$/, "")}-condensed`,
      "export_rallies",
      { path }
    )
  }, [path, runExport])

  // Condensed session (#13): every rally across all the session's recordings,
  // gaps removed, concatenated across file boundaries into one portable MP4.
  const exportSession = useCallback(
    () =>
      runExport(
        "Export condensed session",
        `${day ?? "session"}-condensed`,
        "export_session",
        { paths, rallyIds: null }
      ),
    [day, paths, runExport]
  )

  // A targeted reel (#14): the same session engine pointed at a rally-id
  // selection. An empty selection never reaches ffmpeg — we surface it instead.
  const exportReel = useCallback(
    (label: string, name: string, rallyIds: number[]) => {
      if (rallyIds.length === 0) {
        setError(`No ${label} in this session to export.`)
        return
      }
      return runExport(
        `Export ${label}`,
        `${day ?? "session"}-${name}`,
        "export_session",
        { paths, rallyIds }
      )
    },
    [day, paths, runExport, setError]
  )

  // Flagged rallies (#14): the rallies the user marked as ones that matter.
  const exportFlagged = useCallback(
    () =>
      exportReel(
        "flagged rallies",
        "flagged",
        session.rallies.filter((r) => r.flagged).map((r) => r.id)
      ),
    [exportReel, session]
  )

  // Rallies with mistakes (#14): a rally owns the annotations in its span
  // (glossary), so a rally is a "mistake" rally if a `mistake` verdict falls
  // inside it. Reuses the session annotations already lifted onto the axis (#11).
  const exportMistakes = useCallback(() => {
    const mistakeMs = sessionAnnotations
      .filter((a) => a.verdict === "mistake")
      .map((a) => a.globalMs)
    const ids = session.rallies
      .filter((r) =>
        mistakeMs.some((ms) => ms >= r.globalStart && ms < r.globalEnd)
      )
      .map((r) => r.id)
    return exportReel("rallies with mistakes", "mistakes", ids)
  }, [exportReel, session, sessionAnnotations])

  return {
    progress,
    error,
    exportCondensed,
    exportSession,
    exportFlagged,
    exportMistakes,
  }
}
