"use client"

import {
  FastForwardIcon,
  PauseIcon,
  PlayIcon,
  RepeatIcon,
  StepBackIcon,
  StepForwardIcon,
  Volume2Icon,
  VolumeXIcon,
} from "lucide-react"

import { Button } from "@/components/ui/button"
import { formatTimecode } from "@/lib/format"

/**
 * The transport-only control bar beneath the player: play/pause, exact
 * frame-step, a session-global timecode mirroring the session playhead and total
 * duration, a playback-speed indicator, a loop toggle, mute, and a volume
 * readout. It deliberately has **no scrubber**: the session timeline strip below
 * remains the single scrub/seek surface.
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
    <div className="flex shrink-0 flex-wrap items-center gap-2">
      <Button
        variant="outline"
        size="icon"
        onClick={onTogglePlay}
        title={paused ? "Play (Space)" : "Pause (Space)"}
      >
        {paused ? (
          <PlayIcon className="size-4" />
        ) : (
          <PauseIcon className="size-4" />
        )}
      </Button>
      <Button
        variant="outline"
        size="icon"
        onClick={() => onFrameStep(false)}
        title="Step back one frame (,)"
      >
        <StepBackIcon className="size-4" />
      </Button>
      <Button
        variant="outline"
        size="icon"
        onClick={() => onFrameStep(true)}
        title="Step forward one frame (.)"
      >
        <StepForwardIcon className="size-4" />
      </Button>
      <span className="ml-1 font-mono text-sm text-muted-foreground tabular-nums">
        {formatTimecode(positionMs)} / {formatTimecode(durationMs)}
      </span>
      <div className="ml-auto flex items-center gap-2">
        <div
          className="flex items-center"
          title="Playback speed (Ctrl+- / Ctrl+= , Ctrl+0 to reset)"
        >
          <Button
            variant="outline"
            size="icon"
            onClick={() => onStepSpeed(-1)}
            title="Slower (Ctrl+-)"
          >
            <span className="text-sm">−</span>
          </Button>
          <button
            type="button"
            onClick={onResetSpeed}
            title="Reset speed to 1× (Ctrl+0)"
            className="min-w-12 px-1 text-center font-mono text-sm text-muted-foreground tabular-nums hover:text-foreground"
          >
            {speed}×
          </button>
          <Button
            variant="outline"
            size="icon"
            onClick={() => onStepSpeed(1)}
            title="Faster (Ctrl+=)"
          >
            <span className="text-sm">+</span>
          </Button>
        </div>
        <Button
          variant={gapSkipEnabled ? "default" : "outline"}
          size="icon"
          onClick={onToggleGapSkip}
          title={
            gapSkipEnabled
              ? "Skipping gaps between rallies — click to play everything (G)"
              : "Playing through gaps — click to skip to the next rally (G)"
          }
        >
          <FastForwardIcon className="size-4" />
        </Button>
        <Button
          variant={looping ? "default" : "outline"}
          size="icon"
          onClick={onToggleLoop}
          title={looping ? "Stop looping (L)" : "Loop the current rally (L)"}
        >
          <RepeatIcon className="size-4" />
        </Button>
        <Button
          variant="outline"
          size="icon"
          onClick={onToggleMute}
          title={muted ? "Unmute (M)" : "Mute (M)"}
        >
          {muted ? (
            <VolumeXIcon className="size-4" />
          ) : (
            <Volume2Icon className="size-4" />
          )}
        </Button>
        <span className="min-w-10 text-right font-mono text-sm text-muted-foreground tabular-nums">
          {muted ? "—" : `${volume}%`}
        </span>
      </div>
    </div>
  )
}
