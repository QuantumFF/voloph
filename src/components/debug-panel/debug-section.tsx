import * as React from "react"

import { cn } from "@/lib/utils"

export function DebugSection({
  title,
  description,
  children,
  className,
  bodyClassName,
}: {
  title: string
  description?: string
  children: React.ReactNode
  className?: string
  bodyClassName?: string
}) {
  return (
    <section
      className={cn(
        "min-h-0 overflow-hidden rounded-lg border border-border/25 bg-card",
        className
      )}
    >
      <div className="border-b border-border/20 px-3 py-2">
        <h3 className="text-[12px] font-semibold tracking-tight text-foreground">
          {title}
        </h3>
        {description ? (
          <p className="mt-0.5 text-[11px] leading-4 text-muted-foreground">
            {description}
          </p>
        ) : null}
      </div>
      <div className={cn("min-h-0 p-3", bodyClassName)}>{children}</div>
    </section>
  )
}

export function EntryHighlight() {
  const [visible, setVisible] = React.useState(true)

  React.useEffect(() => {
    const fadeTimer = window.setTimeout(() => {
      setVisible(false)
    }, 180)

    return () => {
      window.clearTimeout(fadeTimer)
    }
  }, [])

  return (
    <span
      aria-hidden="true"
      className={cn(
        "pointer-events-none absolute inset-0 rounded-lg ring-1 ring-primary/50 transition-opacity duration-700",
        visible ? "opacity-100" : "opacity-0"
      )}
    />
  )
}
