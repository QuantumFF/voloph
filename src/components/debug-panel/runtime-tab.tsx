import { ExternalLinkIcon } from "lucide-react"

import { Badge } from "@/components/ui/badge"
import type {
  DebugEvent,
  LogDebugEvent,
  RuntimeEventDebugEvent,
} from "@/lib/debug-events"
import { cn } from "@/lib/utils"

import { DebugSection, EntryHighlight } from "./debug-section"
import {
  formatTimestamp,
  getLogLevelClasses,
  newestFirst,
  parsePluginLogMessage,
} from "./diagnostics"

export function RuntimeTab({
  layoutClassName,
  isBottomDock,
  invokeLogs,
  runtimeEvents,
  logEntries,
  externalLinks,
  highlightedRuntimeEventIds,
  highlightedLogIds,
}: {
  layoutClassName: string
  isBottomDock: boolean
  invokeLogs: DebugEvent[]
  runtimeEvents: RuntimeEventDebugEvent[]
  logEntries: LogDebugEvent[]
  externalLinks: string[]
  highlightedRuntimeEventIds: string[]
  highlightedLogIds: string[]
}) {
  return (
    <div className={layoutClassName}>
      <DebugSection
        title="Recent invoke() calls"
        description="Calls captured through the tracked Tauri helper."
        bodyClassName="min-h-0"
      >
        {invokeLogs.length ? (
          <div
            className={cn(
              "space-y-1.5 overflow-y-auto pr-1",
              isBottomDock ? "h-full" : "max-h-72"
            )}
          >
            {invokeLogs.map((entry) => {
              if (entry.kind !== "invoke") {
                return null
              }

              return (
                <div
                  key={entry.id}
                  className="rounded-md border border-border/20 bg-card p-2.5"
                >
                  <div className="mb-1.5 flex items-center justify-between gap-3">
                    <div className="font-mono text-[11px]">{entry.command}</div>
                    <div className="flex items-center gap-2">
                      <Badge
                        variant={
                          entry.status === "success"
                            ? "secondary"
                            : "destructive"
                        }
                        className="h-4 rounded-sm px-1 text-[10px]"
                      >
                        {entry.status}
                      </Badge>
                      <span className="text-[10px] text-muted-foreground">
                        {entry.durationMs} ms
                      </span>
                    </div>
                  </div>
                  <pre className="overflow-x-auto rounded-md bg-muted p-2 text-[10px] leading-4 break-words whitespace-pre-wrap">
                    {JSON.stringify(
                      {
                        args: entry.args,
                        result: entry.result,
                        error: entry.error,
                      },
                      null,
                      2
                    )}
                  </pre>
                </div>
              )
            })}
          </div>
        ) : (
          <p className="text-xs text-muted-foreground">
            No tracked Tauri invocations yet.
          </p>
        )}
      </DebugSection>

      <DebugSection
        title="Runtime events"
        description="Recent Tauri window and emitted event activity."
        bodyClassName="min-h-0"
      >
        {runtimeEvents.length ? (
          <ul
            className={cn(
              "space-y-1.5 overflow-y-auto pr-1 text-[11px]",
              isBottomDock ? "h-full" : "max-h-72"
            )}
          >
            {newestFirst(runtimeEvents).map((entry) => (
              <li
                key={entry.id}
                className="relative rounded-md border border-border/20 bg-card p-2.5"
              >
                {highlightedRuntimeEventIds.includes(entry.id) ? (
                  <EntryHighlight />
                ) : null}
                <div className="mb-1 flex items-center justify-between gap-3">
                  <span className="font-mono">{entry.name}</span>
                  <Badge
                    variant="outline"
                    className="h-4 rounded-sm px-1 text-[10px]"
                  >
                    {entry.source}
                  </Badge>
                </div>
                <div className="mb-1.5 text-[10px] text-muted-foreground">
                  {formatTimestamp(entry.timestamp)}
                </div>
                {entry.payload !== undefined ? (
                  <pre className="overflow-x-auto rounded-md bg-muted p-2 text-[10px] leading-4 break-words whitespace-pre-wrap">
                    {JSON.stringify(entry.payload, null, 2)}
                  </pre>
                ) : null}
              </li>
            ))}
          </ul>
        ) : (
          <p className="text-xs text-muted-foreground">
            No captured runtime events yet.
          </p>
        )}
      </DebugSection>

      <DebugSection
        title="Plugin logs"
        description="Recent logs surfaced through the browser console and Tauri log plugin."
        bodyClassName="min-h-0"
      >
        {logEntries.length ? (
          <ul
            className={cn(
              "space-y-1.5 overflow-y-auto pr-1 text-[11px]",
              isBottomDock ? "h-full" : "max-h-72"
            )}
          >
            {newestFirst(logEntries).map((entry) => {
              const parsed = parsePluginLogMessage(entry.message)

              return (
                <li
                  key={entry.id}
                  className="relative rounded-md border border-border/20 bg-card p-2.5"
                >
                  {highlightedLogIds.includes(entry.id) ? (
                    <EntryHighlight />
                  ) : null}
                  {parsed ? (
                    <div className="space-y-2">
                      <div className="flex items-start justify-between gap-3">
                        <div className="min-w-0 space-y-1">
                          <div className="flex min-w-0 items-center gap-2">
                            <span
                              className={cn(
                                "inline-flex rounded-sm border px-1 py-0.5 text-[10px] font-medium tracking-wide uppercase",
                                getLogLevelClasses(parsed.level)
                              )}
                            >
                              {parsed.level}
                            </span>
                            <span className="truncate font-mono text-[10px] text-muted-foreground">
                              {parsed.target}
                            </span>
                          </div>
                        </div>
                        <div className="shrink-0 text-right text-[10px] text-muted-foreground">
                          <div>{parsed.time}</div>
                          <div>{parsed.date}</div>
                        </div>
                      </div>
                      <p className="font-mono text-[11px] break-words text-foreground">
                        {parsed.text}
                      </p>
                    </div>
                  ) : (
                    <>
                      <div className="mb-1 flex items-center justify-between gap-3">
                        <Badge
                          variant="outline"
                          className="h-4 rounded-sm px-1 text-[10px]"
                        >
                          {entry.level}
                        </Badge>
                        <span className="text-[10px] text-muted-foreground">
                          {formatTimestamp(entry.timestamp)}
                        </span>
                      </div>
                      <p className="font-mono text-[11px] break-words text-foreground">
                        {entry.message}
                      </p>
                    </>
                  )}
                </li>
              )
            })}
          </ul>
        ) : (
          <p className="text-xs text-muted-foreground">
            No captured plugin logs yet.
          </p>
        )}
      </DebugSection>

      <DebugSection
        title="External links"
        description="URLs intercepted by the external-link guard."
        bodyClassName="min-h-0"
      >
        {externalLinks.length ? (
          <ul
            className={cn(
              "space-y-1.5 overflow-y-auto pr-1 text-[11px]",
              isBottomDock ? "h-full" : "max-h-56"
            )}
          >
            {externalLinks.map((href, index) => (
              <li
                key={`${href}-${index}`}
                className="flex gap-2 rounded-md border border-border/20 bg-card p-2"
              >
                <ExternalLinkIcon className="mt-0.5 size-3 shrink-0 text-muted-foreground" />
                <span className="font-mono break-all">{href}</span>
              </li>
            ))}
          </ul>
        ) : (
          <p className="text-xs text-muted-foreground">
            No external links opened yet.
          </p>
        )}
      </DebugSection>
    </div>
  )
}
