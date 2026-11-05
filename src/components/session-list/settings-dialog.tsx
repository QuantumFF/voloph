"use client"

import { useRef } from "react"
import { RadioGroup as RadioGroupPrimitive } from "radix-ui"

import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { Button } from "@/components/ui/button"
import { cn } from "@/lib/utils"

import type { Padding } from "@/components/recording-player-transport"

import {
  describePadding,
  PADDING_PRESETS,
  paddingForPreset,
  paddingPresetId,
  SEGMENTERS,
  segmenterId,
  type SegmenterId,
} from "./library-settings"
import { kindLabel } from "./recording-state"

/**
 * The app's persistent preferences, opened from the library header's Settings
 * button: which segmenter drafts the active library's timelines (ADR 0019) and
 * the clip padding playback and export add around every rally (ADR 0020).
 * Choices apply the moment they are picked — the same calls the library menu
 * used to make — so the dialog has nothing to save, only Done.
 */
export function SettingsDialog({
  open,
  onOpenChange,
  active,
  segmenter,
  onSegmenter,
  padding,
  onPadding,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  /** The active library's kind ("local" / "shared"). */
  active: string
  /** Null when the backend names a segmenter the dialog doesn't list. */
  segmenter: SegmenterId | null
  onSegmenter: (id: SegmenterId) => void
  padding: Padding
  onPadding: (padding: Padding) => void
}) {
  const paddingId = paddingPresetId(padding)
  // Open on Done rather than the first checked option: an arrow key on a
  // focused option switches it at once, and switching rally detection
  // re-drafts recordings — so it should take a deliberate move.
  const doneRef = useRef<HTMLButtonElement>(null)
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        className="max-w-lg gap-5"
        onOpenAutoFocus={(e) => {
          e.preventDefault()
          doneRef.current?.focus()
        }}
      >
        <DialogHeader>
          <DialogTitle>Settings</DialogTitle>
          <DialogDescription>
            Changes apply right away. Nothing here alters a recording.
          </DialogDescription>
        </DialogHeader>

        <SettingSection
          title="Rally detection"
          scope={`${kindLabel(active)} library`}
          hint="How rallies are found. Each library picks its own; switching re-drafts only recordings nobody has touched, so timelines you corrected are kept."
        >
          {/* Per library (ADR 0019). A segmenter the dialog doesn't know
              leaves nothing checked rather than a wrong choice. */}
          <OptionGroup
            label="Rally detection"
            value={segmenter ?? ""}
            onValueChange={(value) => {
              const id = segmenterId(value)
              if (id) onSegmenter(id)
            }}
            options={SEGMENTERS.map((s) => ({
              value: s.id,
              label: s.label,
              description: s.hint,
            }))}
          />
        </SettingSection>

        <SettingSection
          title="Clip padding"
          scope="Every library"
          hint="The lead-in and tail playback and export add around each rally. Rallies themselves stay serve to landing; changing this re-drafts nothing."
        >
          {/* ADR 0020: one per-device preference. A value set outside the
              presets shows as a checked, unselectable "Custom". */}
          <OptionGroup
            label="Clip padding"
            columns={2}
            value={paddingId}
            onValueChange={(id) => {
              const next = paddingForPreset(id)
              if (next) onPadding(next)
            }}
            options={[
              ...PADDING_PRESETS.map((p) => ({
                value: p.id,
                label: p.label,
                description: describePadding(p),
              })),
              ...(paddingId === "custom"
                ? [
                    {
                      value: "custom",
                      label: "Custom",
                      description: `${describePadding(padding)} — set outside these presets`,
                      disabled: true,
                    },
                  ]
                : []),
            ]}
          />
        </SettingSection>

        <DialogFooter>
          <Button ref={doneRef} size="sm" onClick={() => onOpenChange(false)}>
            Done
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

/** A titled settings block: what it is, whom it applies to, what it does. */
function SettingSection({
  title,
  scope,
  hint,
  children,
}: {
  title: string
  scope?: string
  hint: string
  children: React.ReactNode
}) {
  return (
    <section className="space-y-2.5">
      <div>
        <h3 className="flex items-baseline gap-2 text-[13px] font-medium text-foreground">
          {title}
          {scope ? (
            <span className="text-xs font-normal text-muted-foreground">
              {scope}
            </span>
          ) : null}
        </h3>
        <p className="mt-0.5 text-xs/relaxed text-muted-foreground">{hint}</p>
      </div>
      {children}
    </section>
  )
}

interface Option {
  value: string
  label: string
  description: string
  disabled?: boolean
}

/**
 * A single choice as a column (or grid) of option cards — a radio group, so
 * arrow keys move between options and the checked card carries the ring.
 */
function OptionGroup({
  label,
  value,
  onValueChange,
  options,
  columns = 1,
}: {
  label: string
  value: string
  onValueChange: (value: string) => void
  options: Option[]
  columns?: 1 | 2
}) {
  return (
    <RadioGroupPrimitive.Root
      aria-label={label}
      value={value}
      onValueChange={onValueChange}
      className={cn("grid gap-1.5", columns === 2 && "sm:grid-cols-2")}
    >
      {options.map((o) => (
        <RadioGroupPrimitive.Item
          key={o.value}
          value={o.value}
          disabled={o.disabled}
          className="group/option flex items-start gap-2.5 rounded-md border px-3 py-2 text-left transition-colors outline-none hover:bg-accent/60 focus-visible:ring-2 focus-visible:ring-ring/50 disabled:pointer-events-none disabled:opacity-60 data-[state=checked]:border-primary/70 data-[state=checked]:bg-primary/5 data-[state=checked]:disabled:opacity-100"
        >
          {/* The radio dot: hollow, filled when checked. */}
          <span className="mt-0.5 flex size-3.5 shrink-0 items-center justify-center rounded-full border border-muted-foreground/50 group-data-[state=checked]/option:border-primary">
            <RadioGroupPrimitive.Indicator className="size-1.5 rounded-full bg-primary" />
          </span>
          <span className="flex min-w-0 flex-col gap-0.5">
            <span className="text-[13px] font-medium text-foreground">
              {o.label}
            </span>
            <span className="text-xs leading-snug text-muted-foreground">
              {o.description}
            </span>
          </span>
        </RadioGroupPrimitive.Item>
      ))}
    </RadioGroupPrimitive.Root>
  )
}
