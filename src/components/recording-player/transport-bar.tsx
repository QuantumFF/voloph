"use client"

import type { MouseEvent, ReactNode } from "react"
import {
  ChevronLeftIcon,
  ChevronRightIcon,
  FoldHorizontalIcon,
  MinusIcon,
  PauseIcon,
  PlayIcon,
  PlusIcon,
  RepeatIcon,
  Volume2Icon,
  VolumeXIcon,
} from "lucide-react"

import { Button } from "@/components/ui/button"
import { formatTimecode } from "@/lib/format"
import { cn } from "@/lib/utils"
import { Kbd } from "./kbd"

/**
 * Keep keyboard focus on the player when a transport button is clicked, so the
 * button doesn't stay focused (looking pressed, and catching the next Space).
 * Tab focus still works, with its focus-visible ring.
 */
const keepFocus = (e: MouseEvent) => e.preventDefault()

/**
 * A labelled on/off toggle: the label says what it does; the look says
 * whether it's on, unlike any ordinary button — on, a sky-tinted fill with a
 * sky icon and a lit dot; off, flat and muted with an unlit (hollow) dot —
 * and `aria-pressed` says so to assistive tech. Its key cap sits beside the
 * label, as on the timeline's buttons. The label drops below 36rem and the
 * key below 48rem, so the bar stays on one line; the title carries both.
 */
function ModeToggle({
  on,
  label,
  shortcut,
  icon,
  title,
  onToggle,
}: {
  on: boolean
  label: string
  /** Its key, shown as a cap. */
  shortcut: string
  icon: ReactNode
  title: string
  onToggle: () => void
}) {
  return (
    <Button
      variant="ghost"
      aria-pressed={on}
      aria-label={label}
      onMouseDown={keepFocus}
      onClick={onToggle}
      title={title}
      className={cn(
        "gap-1.5",
        on
          ? "bg-sky-500/15 text-foreground hover:bg-sky-500/25 dark:hover:bg-sky-500/25 [&>svg]:text-sky-600 dark:[&>svg]:text-sky-400"
          : "text-muted-foreground"
      )}
    >
      <span
        aria-hidden
        className={cn(
          "size-1.5 shrink-0 rounded-full",
          on ? "bg-sky-500" : "border border-current/50"
        )}
      />
      {icon}
      <span className="hidden @min-[36rem]/transport:inline">{label}</span>
      <Kbd className="hidden @min-[48rem]/transport:inline-block">
        {shortcut}
      </Kbd>
    </Button>
  )
}

/**
 * The transport-only control bar beneath the player, always one line:
 * play/pause, exact frame-step (a labelled "Frame ‹ ›" pair), a session-global timecode mirroring the session
 * playhead and total duration, the playback speed, the gap-skip and loop modes,
 * mute, and a volume readout (dropped when narrow). It deliberately has **no
 * scrubber**: the session timeline strip below remains the single scrub/seek
 * surface.
 */
export function TransportBar({
  paused,
  muted,
  looping,
  gapSkipEnabled,
  volume,
  speed,
  positionMs,
  durationMs,
  onTogglePlay,
  onFrameStep,
  onToggleMute,
  onToggleLoop,
  onToggleGapSkip,
  onStepSpeed,
  onResetSpeed,
}: {
  paused: boolean
  muted: boolean
  looping: boolean
  gapSkipEnabled: boolean
  volume: number
  speed: number
  positionMs: number
  durationMs: number
  onTogglePlay: () => void
  onFrameStep: (forward: boolean) => void
  onToggleMute: () => void
  onToggleLoop: () => void
  onToggleGapSkip: () => void
  onStepSpeed: (dir: -1 | 1) => void
  onResetSpeed: () => void
}) {
  return (
    <div className="@container/transport">
      <div className="flex h-10 shrink-0 flex-nowrap items-center gap-2 whitespace-nowrap">
        <Button
          variant="outline"
          size="icon"
          onMouseDown={keepFocus}
          onClick={onTogglePlay}
          aria-label={paused ? "Play" : "Pause"}
          title={paused ? "Play (Space)" : "Pause (Space)"}
        >
          {paused ? (
            <PlayIcon className="size-4" />
          ) : (
            <PauseIcon className="size-4" />
          )}
        </Button>
        {/* Frame-step, as a labelled pair like Speed's: "Frame ‹ ›" reads
            as one frame back / forward, not as the skip glyphs that would
            pass for prev/next rally. */}
        <div
          role="group"
          aria-label="Step one frame"
          className="flex items-center gap-1"
        >
          <span className="mr-1 hidden text-xs text-muted-foreground @min-[40rem]/transport:inline">
            Frame
          </span>
          <Button
            variant="outline"
            size="icon"
            onMouseDown={keepFocus}
            onClick={() => onFrameStep(false)}
            aria-label="Previous frame"
            title="Step back one frame (,)"
          >
            <ChevronLeftIcon className="size-4" />
          </Button>
          <Button
            variant="outline"
            size="icon"
            onMouseDown={keepFocus}
            onClick={() => onFrameStep(true)}
            aria-label="Next frame"
            title="Step forward one frame (.)"
          >
            <ChevronRightIcon className="size-4" />
          </Button>
        </div>
        <span className="ml-1 min-w-0 truncate text-sm tabular-nums">
          {formatTimecode(positionMs)}
          <span className="text-muted-foreground">
            {" / "}
            {formatTimecode(durationMs)}
          </span>
        </span>
        <div className="ml-auto flex shrink-0 items-center gap-2">
          <div
            role="group"
            aria-label="Playback speed"
            className="flex items-center gap-1"
            title="Playback speed (Ctrl+- / Ctrl+= , Ctrl+0 to reset)"
          >
            <span className="mr-1 hidden text-xs text-muted-foreground @min-[30rem]/transport:inline">
              Speed
            </span>
            <Button
              variant="outline"
              size="icon"
              onMouseDown={keepFocus}
              onClick={() => onStepSpeed(-1)}
              aria-label="Slower"
              title="Slower (Ctrl+-)"
            >
              <MinusIcon className="size-3.5" />
            </Button>
            <button
              type="button"
              onMouseDown={keepFocus}
              onClick={onResetSpeed}
              aria-label={`Speed ${speed}×, reset to 1×`}
              title="Reset speed to 1× (Ctrl+0)"
              className={cn(
                "h-8 min-w-11 rounded-md px-1 text-center text-sm tabular-nums outline-none hover:bg-muted focus-visible:ring-1 focus-visible:ring-ring",
                speed === 1 ? "text-muted-foreground" : "text-foreground"
              )}
            >
              {speed}×
            </button>
            <Button
              variant="outline"
              size="icon"
              onMouseDown={keepFocus}
              onClick={() => onStepSpeed(1)}
              aria-label="Faster"
              title="Faster (Ctrl+=)"
            >
              <PlusIcon className="size-3.5" />
            </Button>
          </div>
          <ModeToggle
            on={gapSkipEnabled}
            label="Skip gaps"
            shortcut="G"
            icon={<FoldHorizontalIcon className="size-4" />}
            onToggle={onToggleGapSkip}
            title={
              gapSkipEnabled
                ? "Skipping the gaps between rallies — click to play everything (G)"
                : "Playing through the gaps — click to skip straight to each rally (G)"
            }
          />
          <ModeToggle
            on={looping}
            label="Loop"
            shortcut="L"
            icon={<RepeatIcon className="size-4" />}
            onToggle={onToggleLoop}
            title={
              looping
                ? "Looping the rally — click to stop (L)"
                : "Loop the rally the inspector shows — replay it until you stop (L)"
            }
          />
          <div className="flex items-center">
            <Button
              variant="outline"
              size="icon"
              onMouseDown={keepFocus}
              onClick={onToggleMute}
              aria-label={muted ? "Unmute" : "Mute"}
              title={muted ? "Unmute (M)" : `Mute (M) — volume ${volume}%`}
            >
              {muted ? (
                <VolumeXIcon className="size-4" />
              ) : (
                <Volume2Icon className="size-4" />
              )}
            </Button>
            <span className="ml-1.5 hidden min-w-10 text-right text-sm text-muted-foreground tabular-nums @min-[44rem]/transport:inline">
              {muted ? "—" : `${volume}%`}
            </span>
          </div>
        </div>
      </div>
    </div>
  )
}
