"use client"

import { useCallback, useEffect, useState } from "react"
import { listen } from "@tauri-apps/api/event"

import { trackedInvoke } from "@/lib/tauri"
import {
  SPEED_LADDER,
  clampVolume,
  speedIndexForValue,
  stepSpeedIndex,
} from "@/components/recording-player-transport"

/** Index of `1×` on the speed ladder — the default and the `Ctrl+0` reset target. */
const DEFAULT_SPEED_INDEX = SPEED_LADDER.indexOf(1)

/**
 * mpv's transport state and the commands that drive it (issue #35). The
 * commands only *command* mpv; the resulting state lands via the
 * `mpv:pause`/`speed`/`volume`/`mute` events (issue #42), so a command that
 * silently fails leaves no optimistic value the player never reached.
 */
export function useMpvTransport(path: string | null) {
  const [paused, setPaused] = useState(false)
  const [muted, setMuted] = useState(false)
  const [volume, setVolume] = useState(100)
  const [speedIndex, setSpeedIndex] = useState(DEFAULT_SPEED_INDEX)

  // Re-apply the user's speed/volume/mute after a load. mpv resets these to its
  // defaults when a new file opens and *reports those defaults* through the
  // property events (issue #42) — reporting alone can't carry the user's
  // session-wide preference across a recording boundary, so this re-push stays
  // load-bearing. Its job is preference persistence, not UI sync (the events do
  // that); the events then confirm the re-applied values. Reads the current
  // transport state at the crossing commit, hence the path-only deps.
  useEffect(() => {
    if (!path) return
    void trackedInvoke("mpv_set_speed", {
      speed: SPEED_LADDER[speedIndex],
    }).catch(() => {})
    void trackedInvoke("mpv_set_volume", { volume }).catch(() => {})
    void trackedInvoke("mpv_set_mute", { muted }).catch(() => {})
    // Re-applied per recording (the `path` dep); changes within a recording are
    // commanded by the transport actions themselves.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [path])

  // Transport reconciliation (issue #42): mpv reports its own pause/speed/
  // volume/mute, so the controls reflect the player rather than a hopeful
  // optimistic write. mpv reports speed/volume as raw numbers; the UI tracks a
  // speed *ladder index* and a rounded volume.
  useEffect(() => {
    const subscriptions = [
      listen<boolean>("mpv:pause", (event) => setPaused(event.payload)),
      listen<number>("mpv:speed", (event) =>
        setSpeedIndex(speedIndexForValue(event.payload))
      ),
      listen<number>("mpv:volume", (event) =>
        setVolume(Math.round(event.payload))
      ),
      listen<boolean>("mpv:mute", (event) => setMuted(event.payload)),
    ]
    return () => {
      // Chain each teardown off its registration promise (the use-export.ts
      // idiom): under StrictMode's synchronous setup→cleanup→setup an
      // array-collected unlisten is still empty when cleanup runs, leaking the
      // first mount's listeners for the rest of the app's life.
      for (const s of subscriptions) void s.then((off) => off())
    }
  }, [])

  const togglePlay = useCallback(() => {
    void trackedInvoke("mpv_set_pause", { paused: !paused }).catch(() => {})
  }, [paused])

  const toggleMute = useCallback(() => {
    void trackedInvoke("mpv_set_mute", { muted: !muted }).catch(() => {})
  }, [muted])

  const changeVolume = useCallback(
    (delta: number) => {
      void trackedInvoke("mpv_set_volume", {
        volume: clampVolume(volume + delta),
      }).catch(() => {})
    },
    [volume]
  )

  const stepSpeed = useCallback(
    (dir: 1 | -1) => {
      void trackedInvoke("mpv_set_speed", {
        speed: SPEED_LADDER[stepSpeedIndex(speedIndex, dir)],
      }).catch(() => {})
    },
    [speedIndex]
  )

  const resetSpeed = useCallback(() => {
    void trackedInvoke("mpv_set_speed", { speed: 1 }).catch(() => {})
  }, [])

  const frameStep = useCallback((forward: boolean) => {
    void trackedInvoke("mpv_frame_step", { forward }).catch(() => {})
  }, [])

  return {
    paused,
    muted,
    volume,
    speedIndex,
    togglePlay,
    toggleMute,
    changeVolume,
    stepSpeed,
    resetSpeed,
    frameStep,
  }
}
