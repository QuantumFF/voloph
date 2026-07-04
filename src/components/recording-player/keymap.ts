"use client"

import { useEffect } from "react"

/** Arrow-seek step sizes in ms (session-global), by modifier. */
const SEEK_FINE_MS = 2500 // Ctrl+←/→
const SEEK_DEFAULT_MS = 5000 // ←/→
const SEEK_COARSE_MS = 10000 // Shift+←/→

/** Volume step (0–100) for the up/down arrows. */
const VOLUME_STEP = 10

/**
 * One entry in the single keymap definition: its display form (`keys`/`label`
 * for the cheat-sheet), the predicate that decides whether a keydown matches it,
 * and the action to run. One array backs both the live key handler and the `?`
 * cheat-sheet, so they cannot drift.
 */
export interface Keybinding {
  keys: string[]
  label: string
  match: (e: KeyboardEvent) => boolean
  run: (e: KeyboardEvent) => void
}

/** The player actions the keymap drives, provided by `RecordingPlayer`. */
export interface KeymapActions {
  togglePlay: () => void
  seekRelative: (deltaMs: number) => void
  changeVolume: (delta: number) => void
  frameStep: (forward: boolean) => void
  goToRally: (direction: "next" | "prev") => void
  goToUncertain: () => void
  toggleLoop: () => void
  toggleGapSkip: () => void
  jumpToPlayhead: () => void
  toggleMute: () => void
  stepSpeed: (dir: 1 | -1) => void
  resetSpeed: () => void
  toggleCheatSheet: () => void
}

/**
 * The single keymap definition: the one source of truth behind both the global
 * key handler and the `?` cheat-sheet, so the two can never drift.
 */
export function buildKeymap(actions: KeymapActions): Keybinding[] {
  const {
    togglePlay,
    seekRelative,
    changeVolume,
    frameStep,
    goToRally,
    goToUncertain,
    toggleLoop,
    toggleGapSkip,
    jumpToPlayhead,
    toggleMute,
    stepSpeed,
    resetSpeed,
    toggleCheatSheet,
  } = actions
  const plain = (e: KeyboardEvent) =>
    !e.ctrlKey && !e.metaKey && !e.altKey && !e.shiftKey
  return [
    {
      keys: ["Space", "K"],
      label: "Play / pause",
      match: (e) =>
        plain(e) && (e.code === "Space" || e.key.toLowerCase() === "k"),
      run: togglePlay,
    },
    {
      keys: ["Ctrl+←", "Ctrl+→"],
      label: "Seek ∓ 2.5s",
      match: (e) =>
        (e.ctrlKey || e.metaKey) &&
        !e.shiftKey &&
        !e.altKey &&
        (e.key === "ArrowLeft" || e.key === "ArrowRight"),
      run: (e) =>
        seekRelative(e.key === "ArrowLeft" ? -SEEK_FINE_MS : SEEK_FINE_MS),
    },
    {
      keys: ["←", "→"],
      label: "Seek ∓ 5s",
      match: (e) =>
        plain(e) && (e.key === "ArrowLeft" || e.key === "ArrowRight"),
      run: (e) =>
        seekRelative(
          e.key === "ArrowLeft" ? -SEEK_DEFAULT_MS : SEEK_DEFAULT_MS
        ),
    },
    {
      keys: ["Shift+←", "Shift+→"],
      label: "Seek ∓ 10s",
      match: (e) =>
        e.shiftKey &&
        !e.ctrlKey &&
        !e.metaKey &&
        !e.altKey &&
        (e.key === "ArrowLeft" || e.key === "ArrowRight"),
      run: (e) =>
        seekRelative(
          e.key === "ArrowLeft" ? -SEEK_COARSE_MS : SEEK_COARSE_MS
        ),
    },
    {
      keys: ["↑", "↓"],
      label: "Volume up / down",
      match: (e) =>
        plain(e) && (e.key === "ArrowUp" || e.key === "ArrowDown"),
      run: (e) =>
        changeVolume(e.key === "ArrowUp" ? VOLUME_STEP : -VOLUME_STEP),
    },
    {
      keys: [",", "."],
      label: "Frame step back / forward",
      match: (e) => plain(e) && (e.key === "," || e.key === "."),
      run: (e) => frameStep(e.key === "."),
    },
    {
      keys: ["[", "]"],
      label: "Prev / Next rally",
      match: (e) => plain(e) && (e.key === "[" || e.key === "]"),
      run: (e) => goToRally(e.key === "[" ? "prev" : "next"),
    },
    {
      keys: ["U"],
      label: "Next uncertain region",
      match: (e) => plain(e) && e.key.toLowerCase() === "u",
      run: goToUncertain,
    },
    {
      keys: ["L"],
      label: "Loop current rally",
      match: (e) => plain(e) && e.key.toLowerCase() === "l",
      run: toggleLoop,
    },
    {
      keys: ["G"],
      label: "Toggle skipping gaps",
      match: (e) => plain(e) && e.key.toLowerCase() === "g",
      run: toggleGapSkip,
    },
    {
      keys: ["F"],
      label: "Jump to playhead",
      match: (e) => plain(e) && e.key.toLowerCase() === "f",
      run: jumpToPlayhead,
    },
    {
      keys: ["M"],
      label: "Mute",
      match: (e) => plain(e) && e.key.toLowerCase() === "m",
      run: toggleMute,
    },
    {
      keys: ["Ctrl+-", "Ctrl+="],
      label: "Playback speed down / up",
      match: (e) =>
        (e.ctrlKey || e.metaKey) &&
        !e.altKey &&
        (e.key === "-" || e.key === "=" || e.key === "+" || e.key === "_"),
      run: (e) => stepSpeed(e.key === "-" || e.key === "_" ? -1 : 1),
    },
    {
      keys: ["Ctrl+0"],
      label: "Reset speed to 1×",
      match: (e) => (e.ctrlKey || e.metaKey) && !e.altKey && e.key === "0",
      run: resetSpeed,
    },
    {
      keys: ["?"],
      label: "Toggle this cheat-sheet",
      match: (e) => e.key === "?",
      run: toggleCheatSheet,
    },
    {
      keys: ["Scroll over timeline"],
      label: "Scroll the timeline",
      match: () => false,
      run: () => {},
    },
    {
      keys: ["Alt + scroll over timeline"],
      label: "Zoom timeline at the cursor",
      match: () => false,
      run: () => {},
    },
  ]
}

/**
 * The one global key handler, at window capture so it can `preventDefault`
 * page-zoom. Ignores keystrokes while typing in an input/textarea.
 */
export function useGlobalKeymap(keymap: Keybinding[]) {
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement | null
      if (
        target &&
        (target.isContentEditable ||
          target.tagName === "INPUT" ||
          target.tagName === "TEXTAREA" ||
          target.tagName === "SELECT")
      ) {
        return
      }
      const binding = keymap.find((b) => b.match(e))
      if (!binding) return
      e.preventDefault()
      e.stopPropagation()
      binding.run(e)
    }
    window.addEventListener("keydown", onKeyDown, { capture: true })
    return () =>
      window.removeEventListener("keydown", onKeyDown, { capture: true })
  }, [keymap])
}
