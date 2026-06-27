import { describe, expect, it } from "vitest"

import { decideTogglePlay } from "./recording-player-transport"

describe("decideTogglePlay", () => {
  // Issue #27: during a seek the double-buffer holds the live element on its
  // pre-seek "freeze frame" while the incoming stream buffers. A play/pause press
  // in that window must NOT resume the live element — doing so plays the old
  // footage from the freeze frame. The incoming stream autoplays and promotes
  // itself, so the press is ignored.
  it("ignores a press while a load is still buffering (the freeze-frame bug)", () => {
    expect(
      decideTogglePlay({
        frameStepMs: null,
        loadInFlight: true,
        hasLiveMedia: true,
        livePaused: true, // live element paused on its held frame
      })
    ).toEqual({ kind: "ignore" })
  })

  it("ignores a press while buffering even if the held element reports playing", () => {
    expect(
      decideTogglePlay({
        frameStepMs: null,
        loadInFlight: true,
        hasLiveMedia: true,
        livePaused: false,
      })
    ).toEqual({ kind: "ignore" })
  })

  it("plays when paused and nothing is loading", () => {
    expect(
      decideTogglePlay({
        frameStepMs: null,
        loadInFlight: false,
        hasLiveMedia: true,
        livePaused: true,
      })
    ).toEqual({ kind: "play" })
  })

  it("pauses when playing and nothing is loading", () => {
    expect(
      decideTogglePlay({
        frameStepMs: null,
        loadInFlight: false,
        hasLiveMedia: true,
        livePaused: false,
      })
    ).toEqual({ kind: "pause" })
  })

  it("ignores a press when there is no live media (initial load)", () => {
    expect(
      decideTogglePlay({
        frameStepMs: null,
        loadInFlight: false,
        hasLiveMedia: false,
        livePaused: true,
      })
    ).toEqual({ kind: "ignore" })
  })

  it("reopens at the stepped frame when resuming out of frame-step", () => {
    expect(
      decideTogglePlay({
        frameStepMs: 4200,
        loadInFlight: false,
        hasLiveMedia: true,
        livePaused: true,
      })
    ).toEqual({ kind: "reopen-at-frame", atMs: 4200 })
  })

  it("frame-step resume wins even mid-load (a step parks the live stream)", () => {
    // Frame-step accumulates while paused; resuming reopens the stream regardless
    // of any in-flight buffer, so the frame-step branch is checked first.
    expect(
      decideTogglePlay({
        frameStepMs: 0,
        loadInFlight: true,
        hasLiveMedia: true,
        livePaused: true,
      })
    ).toEqual({ kind: "reopen-at-frame", atMs: 0 })
  })
})
