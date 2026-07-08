"use client"

import { useState } from "react"
import { FlagIcon, TriangleAlertIcon, Trash2Icon } from "lucide-react"

import { Button } from "@/components/ui/button"
import { formatClock } from "@/lib/format"
import {
  ASPECTS,
  UNCERTAIN_CONFIDENCE,
  VERDICTS,
  type SessionAnnotation,
  type SessionRally,
  type Verdict,
} from "@/components/recording-player-transport"
import { LONG_RALLY_MS, VERDICT_DOT } from "./constants"

/**
 * The right inspector of the studio layout (issue #48): everything about the
 * rally under the playhead. Verdict capture (issue #8) is live — a click drops
 * a `good`/`bad`/`mistake` annotation at the playhead. Selecting one of the
 * listed annotations enriches it (issue #9): re-classify its verdict, pick an
 * aspect from the seeded vocabulary, type a note, or delete it. Flag (issue #10)
 * marks the rally as one that matters — the source material for an export reel.
 */
export function RallyInspector({
  rally,
  rallyNumber,
  annotations,
  onAnnotate,
  onToggleFlag,
  onUpdate,
  onDelete,
}: {
  rally: SessionRally | null
  rallyNumber: number
  /** Annotations whose timestamp falls inside this rally (glossary), ordered. */
  annotations: SessionAnnotation[]
  /** Drop a verdict at the playhead (same path as the 1/2/3 hotkeys). */
  onAnnotate: (verdict: Verdict) => void
  /** Flag / unflag this rally (same path as the X hotkey). */
  onToggleFlag: () => void
  /** Enrich/re-classify an annotation (issue #9). */
  onUpdate: (
    path: string,
    id: number,
    verdict: Verdict,
    aspect: string | null,
    note: string | null
  ) => void
  /** Remove an annotation (issue #9). */
  onDelete: (path: string, id: number) => void
}) {
  const [selectedId, setSelectedId] = useState<number | null>(null)
  const selected = annotations.find((a) => a.id === selectedId) ?? null
  return (
    <aside className="flex w-72 shrink-0 flex-col overflow-y-auto border-l">
      {rally === null ? (
        <div className="p-4 text-sm text-muted-foreground">
          <p className="font-medium text-foreground">
            No rally at the playhead
          </p>
          <p className="mt-1">
            You&apos;re in a gap, or this recording is still being analyzed.
            Play into a rally to inspect it.
          </p>
        </div>
      ) : (
        <>
          <div className="border-b p-4">
            <div className="flex items-center justify-between">
              <h2 className="font-medium">Rally {rallyNumber}</h2>
              <Button
                variant={rally.flagged ? "default" : "outline"}
                size="sm"
                onClick={onToggleFlag}
                title={
                  rally.flagged
                    ? "Flagged — this rally is in the export reel. Click (or X) to unflag."
                    : "Flag this rally as one that matters, for the export reel (X)."
                }
              >
                <FlagIcon className="size-4" />
                {rally.flagged ? "Flagged" : "Flag"}
              </Button>
            </div>
            <p className="mt-1 text-sm text-muted-foreground tabular-nums">
              {formatClock(rally.globalStart)}–{formatClock(rally.globalEnd)}
              {" · "}
              {formatClock(rally.globalEnd - rally.globalStart)}
              {" · "}
              {rally.globalEnd - rally.globalStart >= LONG_RALLY_MS
                ? "long"
                : "short"}
            </p>
            {rally.confidence < UNCERTAIN_CONFIDENCE ? (
              <p className="mt-2 flex items-center gap-1.5 rounded-md bg-amber-500/10 px-2 py-1.5 text-xs text-amber-600 dark:text-amber-400">
                <TriangleAlertIcon className="size-3.5 shrink-0" />
                Uncertain boundaries — worth a check
              </p>
            ) : null}
          </div>

          {/* Verdict capture (issue #8): one keystroke or click drops an
              annotation at the playhead. Enrich it below (issue #9). */}
          <div className="border-b p-4">
            <div className="mb-2 flex items-baseline justify-between">
              <h3 className="text-xs font-medium text-muted-foreground">
                Verdict at playhead
              </h3>
              <span className="text-xs text-muted-foreground/70">
                1 · 2 · 3
              </span>
            </div>
            <div className="grid grid-cols-3 gap-1.5">
              {VERDICTS.map((verdict) => (
                <Button
                  key={verdict}
                  variant="outline"
                  size="sm"
                  className="capitalize"
                  onClick={() => onAnnotate(verdict)}
                  title={`Mark a ${verdict} at the playhead.`}
                >
                  <span
                    className={`size-2 rounded-full ${VERDICT_DOT[verdict]}`}
                  />
                  {verdict}
                </Button>
              ))}
            </div>
          </div>

          <div className="p-4">
            <h3 className="mb-2 text-xs font-medium text-muted-foreground">
              Annotations
            </h3>
            {annotations.length === 0 ? (
              <p className="text-sm text-muted-foreground">
                Moments you mark during playback collect here, pinned to their
                timestamps.
              </p>
            ) : (
              <ul className="space-y-1 text-sm">
                {annotations.map((a) => (
                  <li key={a.id}>
                    <button
                      type="button"
                      onClick={() =>
                        setSelectedId((id) => (id === a.id ? null : a.id))
                      }
                      className={`flex w-full items-center gap-2 rounded-md px-2 py-1 text-left hover:bg-muted ${
                        a.id === selectedId ? "bg-muted" : ""
                      }`}
                    >
                      <span
                        className={`size-2 shrink-0 rounded-full ${VERDICT_DOT[a.verdict]}`}
                      />
                      <span className="capitalize">{a.verdict}</span>
                      {a.aspect ? (
                        <span className="text-xs text-muted-foreground">
                          {a.aspect}
                        </span>
                      ) : null}
                      <span className="ml-auto text-muted-foreground tabular-nums">
                        {formatClock(a.globalMs)}
                      </span>
                    </button>
                    {a.id === selectedId && selected ? (
                      <AnnotationEditor
                        annotation={selected}
                        onUpdate={onUpdate}
                        onDelete={(path, id) => {
                          onDelete(path, id)
                          setSelectedId(null)
                        }}
                      />
                    ) : null}
                  </li>
                ))}
              </ul>
            )}
          </div>
        </>
      )}
    </aside>
  )
}

/**
 * Enrich the selected annotation (issue #9): re-classify its verdict, toggle an
 * aspect from the seeded vocabulary (clicking the set one clears it), and edit a
 * note. Verdict and aspect persist on click; the note persists on blur so a full
 * sentence is one write, not one per keystroke. Delete removes it.
 */
function AnnotationEditor({
  annotation,
  onUpdate,
  onDelete,
}: {
  annotation: SessionAnnotation
  onUpdate: (
    path: string,
    id: number,
    verdict: Verdict,
    aspect: string | null,
    note: string | null
  ) => void
  onDelete: (path: string, id: number) => void
}) {
  const { path, id, verdict, aspect, note } = annotation
  const [draftNote, setDraftNote] = useState(note ?? "")
  return (
    <div className="mt-1 space-y-2 rounded-md border p-2">
      <div className="grid grid-cols-3 gap-1.5">
        {VERDICTS.map((v) => (
          <Button
            key={v}
            variant={v === verdict ? "default" : "outline"}
            size="sm"
            className="capitalize"
            onClick={() => onUpdate(path, id, v, aspect, note)}
          >
            <span className={`size-2 rounded-full ${VERDICT_DOT[v]}`} />
            {v}
          </Button>
        ))}
      </div>
      <div className="flex flex-wrap gap-1">
        {ASPECTS.map((asp) => (
          <button
            key={asp}
            type="button"
            onClick={() =>
              onUpdate(path, id, verdict, asp === aspect ? null : asp, note)
            }
            className={`rounded-full border px-2 py-0.5 text-xs ${
              asp === aspect
                ? "border-foreground bg-foreground text-background"
                : "text-muted-foreground hover:bg-muted"
            }`}
          >
            {asp}
          </button>
        ))}
      </div>
      <textarea
        value={draftNote}
        onChange={(e) => setDraftNote(e.target.value)}
        onBlur={() => {
          const next = draftNote.trim() || null
          if (next !== (note ?? null)) onUpdate(path, id, verdict, aspect, next)
        }}
        placeholder="Note (optional) — shot type goes here"
        rows={2}
        className="w-full resize-none rounded-md border bg-transparent px-2 py-1.5 text-sm placeholder:text-muted-foreground/70"
      />
      <Button
        variant="ghost"
        size="sm"
        className="w-full text-destructive hover:text-destructive"
        onClick={() => onDelete(path, id)}
      >
        <Trash2Icon className="size-4" />
        Delete annotation
      </Button>
    </div>
  )
}
