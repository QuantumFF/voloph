"use client"

import { useEffect } from "react"

import type { Verdict } from "@/components/recording-player-transport"

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
  /**
   * Alternative chords that trigger it, each written `Mod+Key` (`Ctrl+←`); the
   * cheat-sheet renders every part as its own key cap.
   */
  keys: string[]
  label: string
  /** Which cheat-sheet section it's listed under. */
  group: KeyGroup
  match: (e: KeyboardEvent) => boolean
  run: (e: KeyboardEvent) => void
}

/**
 * The cheat-sheet sections, in display order. `edit` bindings only act in
 * timeline edit mode (`E`); `help` is the cheat-sheet's own toggle, shown as a
 * footnote rather than a section.
 */
export const KEY_GROUPS = [
  "playback",
  "navigate",
  "review",
  "edit",
  "timeline",
  "help",
] as const
export type KeyGroup = (typeof KEY_GROUPS)[number]

/** The player actions the keymap drives, provided by `RecordingPlayer`. */
export interface KeymapActions {
  togglePlay: () => void
  seekRelative: (deltaMs: number) => void
  changeVolume: (delta: number) => void
  frameStep: (forward: boolean) => void
  goToRally: (direction: "next" | "prev") => void
  goToUncertain: () => void
  goToNextUnmarked: () => void
  toggleLoop: () => void
  toggleGapSkip: () => void
  jumpToPlayhead: () => void
  zoomTimeline: (dir: 1 | -1) => void
  fitTimeline: () => void
  toggleMute: () => void
  stepSpeed: (dir: 1 | -1) => void
  resetSpeed: () => void
  annotate: (verdict: Verdict) => void
  flagCurrentRally: () => void
  editing: boolean
  toggleEditing: () => void
  addAtPlayhead: () => void
  splitEditTarget: () => void
  mergeEditTarget: () => void
  deleteEditTarget: () => void
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
    goToNextUnmarked,
    toggleLoop,
    toggleGapSkip,
    jumpToPlayhead,
    zoomTimeline,
    fitTimeline,
    toggleMute,
    stepSpeed,
    resetSpeed,
    annotate,
    flagCurrentRally,
    editing,
    toggleEditing,
    addAtPlayhead,
    splitEditTarget,
    mergeEditTarget,
    deleteEditTarget,
    toggleCheatSheet,
  } = actions
  const plain = (e: KeyboardEvent) =>
    !e.ctrlKey && !e.metaKey && !e.altKey && !e.shiftKey
  return [
    {
      keys: ["Space", "K"],
      label: "Play / pause",
      group: "playback",
      match: (e) =>
        plain(e) && (e.code === "Space" || e.key.toLowerCase() === "k"),
      run: togglePlay,
    },
    {
      keys: ["Ctrl+←", "Ctrl+→"],
      label: "Seek back / forward 2.5s",
      group: "navigate",
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
      label: "Seek back / forward 5s",
      group: "navigate",
      match: (e) =>
        plain(e) && (e.key === "ArrowLeft" || e.key === "ArrowRight"),
      run: (e) =>
        seekRelative(
          e.key === "ArrowLeft" ? -SEEK_DEFAULT_MS : SEEK_DEFAULT_MS
        ),
    },
    {
      keys: ["Shift+←", "Shift+→"],
      label: "Seek back / forward 10s",
      group: "navigate",
      match: (e) =>
        e.shiftKey &&
        !e.ctrlKey &&
        !e.metaKey &&
        !e.altKey &&
        (e.key === "ArrowLeft" || e.key === "ArrowRight"),
      run: (e) =>
        seekRelative(e.key === "ArrowLeft" ? -SEEK_COARSE_MS : SEEK_COARSE_MS),
    },
    {
      keys: ["↑", "↓"],
      label: "Volume up / down",
      group: "playback",
      match: (e) => plain(e) && (e.key === "ArrowUp" || e.key === "ArrowDown"),
      run: (e) =>
        changeVolume(e.key === "ArrowUp" ? VOLUME_STEP : -VOLUME_STEP),
    },
    {
      keys: [",", "."],
      label: "Step one frame back / forward",
      group: "navigate",
      match: (e) => plain(e) && (e.key === "," || e.key === "."),
      run: (e) => frameStep(e.key === "."),
    },
    {
      keys: ["[", "]"],
      label: "Previous / next rally",
      group: "navigate",
      match: (e) => plain(e) && (e.key === "[" || e.key === "]"),
      run: (e) => goToRally(e.key === "[" ? "prev" : "next"),
    },
    {
      keys: ["U"],
      label: "Next uncertain region",
      group: "navigate",
      match: (e) => plain(e) && e.key.toLowerCase() === "u",
      run: goToUncertain,
    },
    {
      keys: ["N"],
      label: "Next unmarked rally",
      group: "navigate",
      match: (e) => plain(e) && e.key.toLowerCase() === "n",
      run: goToNextUnmarked,
    },
    {
      keys: ["L"],
      label: "Loop current rally",
      group: "playback",
      match: (e) => plain(e) && e.key.toLowerCase() === "l",
      run: toggleLoop,
    },
    {
      keys: ["G"],
      label: "Skip gaps on / off",
      group: "playback",
      match: (e) => plain(e) && e.key.toLowerCase() === "g",
      run: toggleGapSkip,
    },
    {
      keys: ["F"],
      label: "Scroll the timeline to the playhead",
      group: "timeline",
      match: (e) => plain(e) && e.key.toLowerCase() === "f",
      run: jumpToPlayhead,
    },
    {
      // Unmodified, so they never meet the speed keys (Ctrl+- / Ctrl+= /
      // Ctrl+0). Shift is allowed so `+` and `_` (Shift+= / Shift+-) count.
      keys: ["-", "="],
      label: "Zoom the timeline out / in",
      group: "timeline",
      match: (e) =>
        !e.ctrlKey &&
        !e.metaKey &&
        !e.altKey &&
        (e.key === "-" || e.key === "_" || e.key === "=" || e.key === "+"),
      run: (e) => zoomTimeline(e.key === "-" || e.key === "_" ? -1 : 1),
    },
    {
      keys: ["0", "Z"],
      label: "Fit the whole session",
      group: "timeline",
      match: (e) => plain(e) && (e.key === "0" || e.key.toLowerCase() === "z"),
      run: fitTimeline,
    },
    {
      keys: ["E"],
      label: "Enter / leave edit mode",
      group: "edit",
      match: (e) => plain(e) && e.key.toLowerCase() === "e",
      run: toggleEditing,
    },
    {
      keys: ["A"],
      label: "Add a rally at the playhead",
      group: "edit",
      match: (e) => editing && plain(e) && e.key.toLowerCase() === "a",
      run: addAtPlayhead,
    },
    {
      keys: ["S"],
      label: "Split the selected rally at the playhead",
      group: "edit",
      match: (e) => editing && plain(e) && e.key.toLowerCase() === "s",
      run: splitEditTarget,
    },
    {
      keys: ["C"],
      label: "Merge the selected rally with the next",
      group: "edit",
      match: (e) => editing && plain(e) && e.key.toLowerCase() === "c",
      run: mergeEditTarget,
    },
    {
      // Not X: X flags in both modes, so muscle-memory flagging in edit mode
      // can never delete a rally.
      keys: ["Delete", "Backspace"],
      label: "Delete the selected rally",
      group: "edit",
      match: (e) =>
        editing && plain(e) && (e.key === "Delete" || e.key === "Backspace"),
      run: deleteEditTarget,
    },
    {
      keys: ["M"],
      label: "Mute / unmute",
      group: "playback",
      match: (e) => plain(e) && e.key.toLowerCase() === "m",
      run: toggleMute,
    },
    {
      keys: ["1"],
      label: "Mark good at the playhead",
      group: "review",
      match: (e) => plain(e) && e.key === "1",
      run: () => annotate("good"),
    },
    {
      keys: ["2"],
      label: "Mark bad at the playhead",
      group: "review",
      match: (e) => plain(e) && e.key === "2",
      run: () => annotate("bad"),
    },
    {
      keys: ["3"],
      label: "Mark mistake at the playhead",
      group: "review",
      match: (e) => plain(e) && e.key === "3",
      run: () => annotate("mistake"),
    },
    {
      keys: ["X"],
      label: "Flag / unflag the current rally",
      group: "review",
      match: (e) => plain(e) && e.key.toLowerCase() === "x",
      run: flagCurrentRally,
    },
    {
      keys: ["Ctrl+-", "Ctrl+="],
      label: "Playback speed down / up",
      group: "playback",
      match: (e) =>
        (e.ctrlKey || e.metaKey) &&
        !e.altKey &&
        (e.key === "-" || e.key === "=" || e.key === "+" || e.key === "_"),
      run: (e) => stepSpeed(e.key === "-" || e.key === "_" ? -1 : 1),
    },
    {
      keys: ["Ctrl+0"],
      label: "Reset speed to 1×",
      group: "playback",
      match: (e) => (e.ctrlKey || e.metaKey) && !e.altKey && e.key === "0",
      run: resetSpeed,
    },
    {
      keys: ["?"],
      label: "Show / hide these shortcuts",
      group: "help",
      match: (e) => e.key === "?",
      run: toggleCheatSheet,
    },
    {
      keys: ["Scroll"],
      label: "Pan the timeline (over the strip)",
      group: "timeline",
      match: () => false,
      run: () => {},
    },
    {
      keys: ["Alt+Scroll"],
      label: "Zoom the timeline at the cursor",
      group: "timeline",
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
      // A modal (the cheat-sheet, a confirmation) owns the keyboard: playback
      // keys must not act on the player hidden behind it. `?` still toggles the
      // cheat-sheet so it closes the way it opened.
      if (
        !binding.keys.includes("?") &&
        document.querySelector(
          '[role="dialog"][data-state="open"], [role="alertdialog"][data-state="open"]'
        )
      ) {
        return
      }
      e.preventDefault()
      e.stopPropagation()
      binding.run(e)
    }
    window.addEventListener("keydown", onKeyDown, { capture: true })
    return () =>
      window.removeEventListener("keydown", onKeyDown, { capture: true })
  }, [keymap])
}
