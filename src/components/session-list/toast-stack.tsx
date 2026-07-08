"use client"

import { CheckCircle2Icon } from "lucide-react"

import type { Toast } from "./types"

/**
 * Top-center toast stack: transient share/receive confirmations. Fixed and
 * pointer-transparent so it floats over the library without stealing clicks;
 * each toast clears itself after a few seconds.
 */
export function ToastStack({ toasts }: { toasts: Toast[] }) {
  if (toasts.length === 0) return null
  return (
    <div className="pointer-events-none fixed inset-x-0 top-4 z-50 flex flex-col items-center gap-2 px-4">
      {toasts.map((t) => (
        <div
          key={t.id}
          className="flex items-center gap-2 rounded-md border bg-popover px-3 py-2 text-sm font-medium text-popover-foreground shadow-md"
        >
          <CheckCircle2Icon className="size-4 text-emerald-600 dark:text-emerald-400" />
          {t.message}
        </div>
      ))}
    </div>
  )
}
