/**
 * Pure transport-decision logic for the recording player, factored out of the
 * component so it can be unit-tested without WebKitGTK or a `<video>` element
 * (issue #27). The component owns the side effects (play/pause/seek); this only
 * decides which one a play/pause press should perform given the current state.
 */

/** What a play/pause press should do, given the player's current state. */
export type TogglePlayAction =
  /** Resume out of frame-step by reopening the stream at the stepped frame. */
  | { kind: "reopen-at-frame"; atMs: number }
  /** Start the live element playing. */
  | { kind: "play" }
  /** Pause the live element. */
  | { kind: "pause" }
  /** Do nothing (no live media, or a load is still settling). */
  | { kind: "ignore" }

/** Snapshot of the player state a play/pause press is decided against. */
export interface TogglePlayState {
  /**
   * The accumulated frame-step target (ms), or null when not frame-stepping.
   * Non-null means a press resumes playback by reopening at that frame.
   */
  frameStepMs: number | null
  /**
   * True while a (re)load is in flight — the double-buffer's incoming stream is
   * still buffering (issue #24). During this window the live element is the
   * stale pre-seek one, paused on its held "freeze frame"; the incoming stream
   * autoplays and promotes itself on its own.
   */
  loadInFlight: boolean
  /** Whether a live `<video>` element is currently mounted. */
  hasLiveMedia: boolean
  /** Whether that live element is paused (irrelevant when `hasLiveMedia` is false). */
  livePaused: boolean
}

/**
 * Decide what a play/pause press should do.
 *
 * The key rule (issue #27): while a load is in flight, a press must NOT touch
 * the live element. The live element is the stale frame held over the seek, so
 * resuming it would play the old footage from the freeze frame while the real
 * (incoming) stream is still buffering. The incoming stream autoplays and
 * promotes on its own, so the correct behavior during the buffer is to ignore
 * the press entirely.
 */
export function decideTogglePlay(state: TogglePlayState): TogglePlayAction {
  // Resuming out of frame-step reopens the stream at the stepped frame
  // (frame-stepping only overlaid stills, never moved the video).
  if (state.frameStepMs != null) {
    return { kind: "reopen-at-frame", atMs: state.frameStepMs }
  }
  // A seek/load is still settling: leave the held freeze frame alone (issue #27).
  if (state.loadInFlight) return { kind: "ignore" }
  if (!state.hasLiveMedia) return { kind: "ignore" }
  return state.livePaused ? { kind: "play" } : { kind: "pause" }
}
