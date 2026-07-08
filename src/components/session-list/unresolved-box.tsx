"use client"

import { AlertTriangleIcon, Trash2Icon } from "lucide-react"

import { Button } from "@/components/ui/button"
import { fileName } from "@/lib/format"

/**
 * Known recordings the last scan could not find under the library (ADR 0011).
 * Their review state is retained; this box lets the user put the files back (they
 * re-link automatically) or forget the retained review.
 */
export function UnresolvedBox({
  unresolved,
  onForget,
}: {
  unresolved: string[]
  onForget: (path: string) => void
}) {
  if (unresolved.length === 0) return null
  return (
    <div className="rounded-lg border border-amber-500/50 bg-amber-500/5 px-4 py-3 text-sm">
      <div className="flex items-center gap-2 font-medium text-amber-700 dark:text-amber-500">
        <AlertTriangleIcon className="size-4" />
        {unresolved.length} recording
        {unresolved.length === 1 ? "" : "s"} not found in your library
      </div>
      <p className="mt-1 text-muted-foreground">
        Their review stays saved. Put the file
        {unresolved.length === 1 ? "" : "s"} back anywhere under your library
        and Refresh — the review re-links automatically.
      </p>
      <ul className="mt-2 space-y-0.5 text-muted-foreground">
        {unresolved.map((path) => (
          <li key={path} className="flex items-center gap-2">
            <span className="min-w-0 flex-1 truncate" title={path}>
              {fileName(path)}
            </span>
            <Button
              size="sm"
              variant="ghost"
              className="h-6 shrink-0 px-2 text-muted-foreground hover:text-destructive"
              onClick={() => onForget(path)}
            >
              <Trash2Icon className="size-3.5" />
              Delete review
            </Button>
          </li>
        ))}
      </ul>
    </div>
  )
}
