"use client"

import { useCallback, useMemo } from "react"

import { fileName } from "@/lib/format"
import { useExport } from "@/components/use-export"
import {
  rallyContains,
  type PlaylistRecording,
  type SessionAnnotation,
  type SessionModel,
} from "@/components/recording-player-transport"

/** How many rallies each export selection holds. */
export interface ExportCounts {
  session: number
  recording: number
  flagged: number
  mistakes: number
}

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

  // The rally selection behind each reel, computed up front so the menu can
  // say how many rallies each item holds (and grey out an empty one).
  // Flagged rallies (#14): the rallies the user marked as ones that matter.
  const flaggedIds = useMemo(
    () => session.rallies.filter((r) => r.flagged).map((r) => r.id),
    [session]
  )
  // Rallies with mistakes (#14): a rally owns the annotations in its span
  // (glossary), so a rally is a "mistake" rally if a `mistake` verdict falls
  // inside it. Reuses the session annotations already lifted onto the axis (#11).
  const mistakeIds = useMemo(() => {
    const mistakeMs = sessionAnnotations
      .filter((a) => a.verdict === "mistake")
      .map((a) => a.globalMs)
    return session.rallies
      .filter((r) => mistakeMs.some((ms) => rallyContains(r, ms)))
      .map((r) => r.id)
  }, [session, sessionAnnotations])

  const exportFlagged = useCallback(
    () => exportReel("flagged rallies", "flagged", flaggedIds),
    [exportReel, flaggedIds]
  )
  const exportMistakes = useCallback(
    () => exportReel("rallies with mistakes", "mistakes", mistakeIds),
    [exportReel, mistakeIds]
  )

  // How many rallies each export would hold, for the menu.
  const counts = useMemo<ExportCounts>(
    () => ({
      session: session.rallies.length,
      recording: path
        ? session.rallies.filter((r) => r.path === path).length
        : 0,
      flagged: flaggedIds.length,
      mistakes: mistakeIds.length,
    }),
    [session, path, flaggedIds, mistakeIds]
  )

  return {
    progress,
    error,
    counts,
    exportCondensed,
    exportSession,
    exportFlagged,
    exportMistakes,
  }
}
