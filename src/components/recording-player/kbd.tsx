import { cn } from "@/lib/utils"

/**
 * The one key cap shown wherever a player control names its shortcut (the
 * inspector's verdict and flag buttons, the timeline's nav, view and edit
 * actions, the transport's modes, the status bar): 11px foreground text at
 * 70% on the muted fill, with a faint foreground edge — at least 4.5:1 in
 * both themes, and a chip of its own on an outline button and a filled
 * (pressed/primary) one alike. `className` adds layout — e.g. the container
 * width at which it shows.
 */
export function Kbd({
  children,
  className,
}: {
  children: string
  className?: string
}) {
  return (
    <kbd
      className={cn(
        "inline-block min-w-4 rounded-[3px] border border-foreground/15 bg-muted px-1 text-center font-sans text-[11px] leading-4 font-medium text-foreground/70",
        className
      )}
    >
      {children}
    </kbd>
  )
}
