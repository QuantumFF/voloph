"use client"

import { XIcon } from "lucide-react"

import { Button } from "@/components/ui/button"
import type { Keybinding } from "./keymap"

/**
 * The `?` cheat-sheet overlay: a modal listing every keybinding, rendered
 * straight from the single keymap definition so it can never drift from what the
 * keys actually do.
 */
export function CheatSheet({
  keymap,
  onClose,
}: {
  keymap: Keybinding[]
  onClose: () => void
}) {
  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4"
      onClick={onClose}
    >
      <div
        className="max-h-full w-full max-w-md overflow-y-auto rounded-lg border bg-background p-5 shadow-lg"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="mb-3 flex items-center justify-between">
          <h2 className="font-medium">Keyboard shortcuts</h2>
          <Button
            variant="ghost"
            size="icon"
            onClick={onClose}
            title="Close (?)"
          >
            <XIcon className="size-4" />
          </Button>
        </div>
        <dl className="space-y-1.5 text-sm">
          {keymap.map((b) => (
            <div
              key={b.label}
              className="flex items-center justify-between gap-4"
            >
              <dt className="text-muted-foreground">{b.label}</dt>
              <dd className="flex flex-wrap justify-end gap-1">
                {b.keys.map((k) => (
                  <kbd
                    key={k}
                    className="rounded border bg-muted px-1.5 py-0.5 font-mono text-xs"
                  >
                    {k}
                  </kbd>
                ))}
              </dd>
            </div>
          ))}
        </dl>
      </div>
    </div>
  )
}
