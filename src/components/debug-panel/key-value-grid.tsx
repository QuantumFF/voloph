import * as React from "react"

import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip"
import { cn } from "@/lib/utils"

import { DebugPanelThemeContext } from "./theme-context"
import type { GridEntry, GridEntryMeta, SourceOrigin } from "./types"

export function SourceOriginChip({ origin }: { origin: SourceOrigin }) {
  return (
    <span
      className={cn(
        "inline-flex h-3.5 items-center rounded-sm border px-1 text-[9px] font-semibold tracking-wide uppercase",
        origin === "tauri"
          ? "border-amber-500/30 bg-amber-500/15 text-amber-600 dark:text-amber-400"
          : "border-sky-500/30 bg-sky-500/15 text-sky-600 dark:text-sky-400"
      )}
    >
      {origin} API
    </span>
  )
}

export function GridLabel({
  label,
  meta,
}: {
  label: string
  meta?: GridEntryMeta
}) {
  const themeStyle = React.useContext(DebugPanelThemeContext)
  const [copied, setCopied] = React.useState(false)

  React.useEffect(() => {
    if (!copied) {
      return
    }

    const timeoutId = window.setTimeout(() => {
      setCopied(false)
    }, 1200)

    return () => {
      window.clearTimeout(timeoutId)
    }
  }, [copied])

  if (!meta) {
    return <span>{label}</span>
  }

  const handleCopySnippet = async () => {
    try {
      await navigator.clipboard.writeText(meta.code)
      setCopied(true)
    } catch {
      setCopied(false)
    }
  }

  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <span className="cursor-help underline decoration-muted-foreground/40 decoration-dotted underline-offset-4">
          {label}
        </span>
      </TooltipTrigger>
      <TooltipContent
        side="right"
        align="start"
        style={themeStyle}
        className="max-w-[320px] border border-border/40 bg-popover p-2 text-popover-foreground shadow-md [&>:last-child]:hidden"
      >
        <div className="flex items-center gap-2">
          <SourceOriginChip origin={meta.origin} />
          <button
            type="button"
            onClick={handleCopySnippet}
            title={copied ? "Copied!" : "Click to copy"}
            className="group relative flex-1 overflow-hidden rounded-sm bg-muted px-1.5 py-1 text-left font-mono text-[10px] leading-4 text-foreground transition-colors hover:bg-muted/70 active:bg-muted/50"
          >
            <pre className="break-all whitespace-pre-wrap">{meta.code}</pre>
            <span
              className={cn(
                "pointer-events-none absolute inset-0 flex items-center justify-center rounded-sm bg-popover/95 text-[10px] font-semibold text-foreground transition-opacity",
                copied ? "opacity-100" : "opacity-0"
              )}
            >
              Copied!
            </span>
          </button>
        </div>
      </TooltipContent>
    </Tooltip>
  )
}

export function KeyValueGrid({
  entries,
}: {
  entries: ReadonlyArray<GridEntry>
}) {
  return (
    <dl className="grid text-[11px] leading-5">
      {entries.map((entry, index) => {
        const [label, value] = entry
        const meta = entry.length === 3 ? entry[2] : undefined

        return (
          <div
            key={label}
            className={cn(
              "grid grid-cols-[92px_minmax(0,1fr)] items-start gap-3 py-1",
              index > 0 && "border-t border-border/20"
            )}
          >
            <dt className="text-muted-foreground/90">
              <GridLabel label={label} meta={meta} />
            </dt>
            <dd className="font-mono break-all text-foreground">{value}</dd>
          </div>
        )
      })}
    </dl>
  )
}
