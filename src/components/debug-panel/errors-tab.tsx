import { AlertTriangleIcon } from "lucide-react"

import { DebugSection } from "./debug-section"
import { formatTimestamp } from "./diagnostics"
import type { ErrorLog } from "./types"

export function ErrorsTab({ errors }: { errors: ErrorLog[] }) {
  return (
    <div className="h-full overflow-y-auto pr-1">
      <DebugSection
        title="Recent errors"
        description="Uncaught runtime errors and unhandled promise rejections."
      >
        {errors.length ? (
          <ul className="space-y-1.5 text-[11px]">
            {errors.map((entry) => (
              <li
                key={entry.id}
                className="rounded-md border border-border/20 bg-card p-2.5"
              >
                <div className="mb-1 flex items-center gap-2">
                  <AlertTriangleIcon className="size-3.5 text-destructive" />
                  <span className="font-medium">{entry.source}</span>
                  <span className="text-muted-foreground">
                    {formatTimestamp(entry.timestamp)}
                  </span>
                </div>
                <p className="break-words text-muted-foreground">
                  {entry.message}
                </p>
              </li>
            ))}
          </ul>
        ) : (
          <p className="text-xs text-muted-foreground">
            No captured runtime errors.
          </p>
        )}
      </DebugSection>
    </div>
  )
}
