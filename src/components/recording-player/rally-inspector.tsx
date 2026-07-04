"use client"

import { FlagIcon, TriangleAlertIcon } from "lucide-react"

import { Button } from "@/components/ui/button"
import { formatClock } from "@/lib/format"
import {
  UNCERTAIN_CONFIDENCE,
  type SessionRally,
} from "@/components/recording-player-transport"
import { LONG_RALLY_MS } from "./index"

/** Seeded aspect vocabulary (CONTEXT.md), previewed in the inspector stub. */
const STUB_ASPECTS = [
  "selection",
  "execution",
  "deception",
  "footwork",
  "positioning",
]

const VERDICT_DOT = {
  good: "bg-emerald-500",
  bad: "bg-amber-500",
  mistake: "bg-red-500",
} as const

/**
 * The right inspector of the studio layout (issue #48): everything about the
 * rally under the playhead. Its identity, bounds, length class, and uncertainty
 * are real; the capture surfaces — flag, verdict, aspect, note, annotation
 * list — are visual stubs until annotations and flags are implemented.
 */
export function RallyInspector({
  rally,
  rallyNumber,
}: {
  rally: SessionRally | null
  rallyNumber: number
}) {
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
                variant="outline"
                size="sm"
                disabled
                title="Flags are coming soon — one keystroke to mark a rally for the export reel."
              >
                <FlagIcon className="size-4" />
                Flag
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

          {/* Annotation capture stub: verdict → aspect → note (CONTEXT.md). */}
          <div className="border-b p-4">
            <div className="mb-2 flex items-baseline justify-between">
              <h3 className="text-xs font-medium text-muted-foreground">
                Verdict at playhead
              </h3>
              <span className="text-xs text-muted-foreground/70">
                coming soon
              </span>
            </div>
            <div className="grid grid-cols-3 gap-1.5">
              {(["good", "bad", "mistake"] as const).map((verdict) => (
                <Button
                  key={verdict}
                  variant="outline"
                  size="sm"
                  disabled
                  className="capitalize"
                >
                  <span
                    className={`size-2 rounded-full ${VERDICT_DOT[verdict]}`}
                  />
                  {verdict}
                </Button>
              ))}
            </div>
            <div className="mt-2 flex flex-wrap gap-1">
              {STUB_ASPECTS.map((aspect) => (
                <span
                  key={aspect}
                  className="rounded-full border px-2 py-0.5 text-xs text-muted-foreground/70"
                >
                  {aspect}
                </span>
              ))}
            </div>
            <textarea
              disabled
              placeholder="Note (optional) — shot type goes here"
              rows={2}
              className="mt-2 w-full resize-none rounded-md border bg-transparent px-2 py-1.5 text-sm placeholder:text-muted-foreground/70 disabled:cursor-not-allowed"
            />
          </div>

          <div className="p-4">
            <div className="mb-2 flex items-baseline justify-between">
              <h3 className="text-xs font-medium text-muted-foreground">
                Annotations
              </h3>
              <span className="text-xs text-muted-foreground/70">
                coming soon
              </span>
            </div>
            <p className="text-sm text-muted-foreground">
              Moments you mark during playback will collect here, pinned to
              their timestamps.
            </p>
          </div>
        </>
      )}
    </aside>
  )
}
