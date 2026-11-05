import { CheckIcon, MinusIcon, XIcon } from "lucide-react"

import { VERDICTS, type Verdict } from "@/components/recording-player-transport"
import { cn } from "@/lib/utils"
import { VERDICT_DOT } from "./constants"

const GLYPH = { good: CheckIcon, bad: MinusIcon, mistake: XIcon } as const
const LABEL = { good: "Good", bad: "Bad", mistake: "Mistake" } as const

/**
 * A verdict marker that doesn't rely on colour alone: the verdict's colour as a
 * filled disc carrying a glyph (✓ good, – bad, ✕ mistake). Use wherever a
 * verdict is shown as a small mark (rail, inspector, moment browser).
 */
export function VerdictMark({
  verdict,
  className,
}: {
  verdict: Verdict
  className?: string
}) {
  const Glyph = GLYPH[verdict]
  return (
    <span
      role="img"
      aria-label={LABEL[verdict]}
      title={LABEL[verdict]}
      className={cn(
        "inline-flex size-3.5 shrink-0 items-center justify-center rounded-full text-black/80",
        VERDICT_DOT[verdict],
        className
      )}
    >
      <Glyph className="size-2.5" strokeWidth={3.5} />
    </span>
  )
}

/**
 * The compact form of `VerdictMark`: a bare dot in the verdict's colour, for
 * where a glyph won't fit (the narrow rail). Always shown in verdict order next
 * to its siblings, so position backs up colour; the label carries the count.
 */
export function VerdictPip({
  verdict,
  count,
}: {
  verdict: Verdict
  count: number
}) {
  const label = `${count} ${LABEL[verdict].toLowerCase()}`
  return (
    <span
      role="img"
      aria-label={label}
      title={label}
      className={cn("size-2 shrink-0 rounded-full", VERDICT_DOT[verdict])}
    />
  )
}

/**
 * A rally's verdict tally in the one shared format — `✓4 –2 ✕1`, each a
 * `VerdictMark` followed by its count, in verdict order, zero counts omitted —
 * so the rail, the moment browser and anywhere else that summarises a rally's
 * verdicts read the same. The whole tally carries one aria-label
 * ("4 good, 2 bad, 1 mistake").
 */
export function VerdictCounts({
  counts,
  className,
}: {
  counts: Partial<Record<Verdict, number>>
  className?: string
}) {
  const present = VERDICTS.filter((v) => (counts[v] ?? 0) > 0)
  if (present.length === 0) return null
  const label = present
    .map((v) => `${counts[v]} ${LABEL[v].toLowerCase()}`)
    .join(", ")
  return (
    <span
      role="img"
      aria-label={label}
      title={label}
      className={cn(
        "inline-flex shrink-0 items-center gap-1.5 text-xs text-muted-foreground tabular-nums",
        className
      )}
    >
      {present.map((v) => (
        <span key={v} className="inline-flex items-center gap-0.5" aria-hidden>
          <VerdictMark verdict={v} />
          {counts[v]}
        </span>
      ))}
    </span>
  )
}
