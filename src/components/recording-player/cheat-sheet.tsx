"use client"

import { Fragment, type ReactNode } from "react"
import {
  ArrowDownIcon,
  ArrowLeftIcon,
  ArrowRightIcon,
  ArrowUpIcon,
  XIcon,
} from "lucide-react"
import { Dialog as DialogPrimitive } from "radix-ui"

import { Button } from "@/components/ui/button"
import type { KeyGroup, Keybinding } from "./keymap"

/** Each section's heading and, where it needs one, a note on when it applies. */
const SECTIONS: Record<
  Exclude<KeyGroup, "help">,
  { title: string; note?: string }
> = {
  playback: { title: "Playback" },
  navigate: { title: "Navigate" },
  review: { title: "Review" },
  edit: {
    title: "Edit",
    note: "In edit mode only (E). Acts on the selected rally, else the one under the playhead.",
  },
  timeline: { title: "Timeline" },
}

/** Which sections stack in which column, balanced for the current keymap. */
const COLUMNS: Exclude<KeyGroup, "help">[][] = [
  ["playback", "navigate"],
  ["review", "edit", "timeline"],
]

const ARROWS: Record<string, { icon: ReactNode; name: string }> = {
  "←": { icon: <ArrowLeftIcon />, name: "Left arrow" },
  "→": { icon: <ArrowRightIcon />, name: "Right arrow" },
  "↑": { icon: <ArrowUpIcon />, name: "Up arrow" },
  "↓": { icon: <ArrowDownIcon />, name: "Down arrow" },
}

/**
 * One key cap. Arrows draw as icons so they can't render as a dash. Its edge
 * is drawn from the foreground (heavier at the foot), not `--border`, so the
 * cap stands off the dialog in light theme too, where muted-on-white with the
 * stock border all but vanishes.
 */
function Key({ k }: { k: string }) {
  const arrow = ARROWS[k]
  return (
    <kbd
      className="inline-flex h-6 min-w-6 items-center justify-center rounded-md border border-b-2 border-foreground/20 border-b-foreground/35 bg-muted px-1.5 font-sans text-xs leading-none font-medium text-foreground [&_svg]:size-3.5"
      aria-label={arrow?.name}
      title={arrow?.name}
    >
      {arrow?.icon ?? k}
    </kbd>
  )
}

/** `Ctrl+←` → `["Ctrl", "←"]`. */
const chordParts = (chord: string) => chord.split("+")

/**
 * A binding's alternative chords as key caps. Alternatives that share their
 * modifiers draw them once — `Ctrl` `←` / `→` — so a row stays short.
 */
function Chords({ keys }: { keys: string[] }) {
  const chords = keys.map(chordParts)
  const mods = chords[0].slice(0, -1)
  const sharedMods =
    mods.length > 0 &&
    chords.every(
      (c) =>
        c.length === mods.length + 1 &&
        c.slice(0, -1).join("+") === mods.join("+")
    )
  const sep = <span className="px-0.5 text-xs text-muted-foreground">/</span>
  if (sharedMods) {
    return (
      <>
        {mods.map((m) => (
          <Key key={m} k={m} />
        ))}
        {chords.map((c, i) => (
          <Fragment key={c.join("+")}>
            {i > 0 ? sep : null}
            <Key k={c[c.length - 1]} />
          </Fragment>
        ))}
      </>
    )
  }
  return (
    <>
      {chords.map((c, i) => (
        <Fragment key={c.join("+")}>
          {i > 0 ? sep : null}
          {c.map((part) => (
            <Key key={part} k={part} />
          ))}
        </Fragment>
      ))}
    </>
  )
}

/**
 * The `?` cheat-sheet overlay: a modal listing every keybinding, grouped by what
 * it's for and rendered straight from the single keymap definition so it can
 * never drift from what the keys actually do. A Radix dialog, so Escape and an
 * outside click close it, focus is trapped inside while open and returns to
 * where it was on close.
 */
export function CheatSheet({
  keymap,
  onClose,
}: {
  keymap: Keybinding[]
  onClose: () => void
}) {
  const help = keymap.find((b) => b.group === "help")
  return (
    <DialogPrimitive.Root open onOpenChange={(open) => !open && onClose()}>
      <DialogPrimitive.Portal>
        <DialogPrimitive.Overlay className="fixed inset-0 z-50 bg-black/60 data-open:animate-in data-open:fade-in-0" />
        <DialogPrimitive.Content
          className="fixed top-1/2 left-1/2 z-50 max-h-[calc(100%-2rem)] w-[calc(100%-2rem)] max-w-3xl -translate-x-1/2 -translate-y-1/2 overflow-y-auto rounded-lg border bg-background p-6 shadow-lg outline-none data-open:animate-in data-open:fade-in-0 data-open:zoom-in-95"
          aria-describedby={undefined}
        >
          <div className="mb-4 flex items-center justify-between">
            <DialogPrimitive.Title className="text-base font-semibold">
              Keyboard shortcuts
            </DialogPrimitive.Title>
            <DialogPrimitive.Close asChild>
              <Button
                variant="ghost"
                size="icon"
                title="Close (Esc)"
                aria-label="Close"
              >
                <XIcon className="size-4" />
              </Button>
            </DialogPrimitive.Close>
          </div>
          <div className="grid gap-x-10 gap-y-5 sm:grid-cols-2">
            {COLUMNS.map((column) => (
              <div key={column.join()} className="space-y-5">
                {column.map((group) => {
                  const bindings = keymap.filter((b) => b.group === group)
                  if (bindings.length === 0) return null
                  const { title, note } = SECTIONS[group]
                  return (
                    <section key={group} aria-label={title}>
                      <h3 className="text-xs font-semibold tracking-wide text-muted-foreground uppercase">
                        {title}
                      </h3>
                      {note ? (
                        <p className="mt-0.5 text-xs text-muted-foreground">
                          {note}
                        </p>
                      ) : null}
                      <dl className="mt-2 space-y-1">
                        {bindings.map((b) => (
                          <div
                            key={b.label}
                            className="flex min-h-7 items-center justify-between gap-4"
                          >
                            <dt className="text-sm">{b.label}</dt>
                            <dd className="flex shrink-0 items-center gap-1">
                              <Chords keys={b.keys} />
                            </dd>
                          </div>
                        ))}
                      </dl>
                    </section>
                  )
                })}
              </div>
            ))}
          </div>
          {help ? (
            <p className="mt-5 flex items-center gap-1.5 border-t pt-4 text-xs text-muted-foreground">
              Press <Chords keys={help.keys} /> or <Key k="Esc" /> to close.
            </p>
          ) : null}
        </DialogPrimitive.Content>
      </DialogPrimitive.Portal>
    </DialogPrimitive.Root>
  )
}
