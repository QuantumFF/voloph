// @vitest-environment jsdom
/**
 * Hook-level harness for the session playback orchestration (issue #74 —
 * scrubbing bugs). Renders the *real* `useSessionPlayback` against a scripted
 * mpv boundary: `trackedInvoke` records every command, and the test emits the
 * Tauri events (`mpv:time-pos`, `mpv:file-loaded`, …) in a chosen order. That
 * order is the whole point — in the live app the `time-pos` stream and the
 * seek invokes are both async, so a tick emitted *before* a scrub is routinely
 * delivered *after* it; the harness makes that interleaving deterministic.
 */
import { StrictMode } from "react"
import { act, renderHook } from "@testing-library/react"
import { beforeEach, describe, expect, it, vi } from "vitest"

import {
  buildSessionModel,
  type Timeline,
} from "@/components/recording-player-transport"
import { useSessionPlayback } from "@/components/recording-player/use-session-playback"

const harness = vi.hoisted(() => {
  type Handler = (event: { payload: unknown }) => void
  const handlers = new Map<string, Set<Handler>>()
  const calls: Array<{ command: string; args?: Record<string, unknown> }> = []
  return {
    handlers,
    calls,
    emit(name: string, payload?: unknown) {
      for (const h of handlers.get(name) ?? new Set()) h({ payload })
    },
    reset() {
      handlers.clear()
      calls.length = 0
    },
  }
})

vi.mock("@/lib/tauri", () => ({
  trackedInvoke: (command: string, args?: Record<string, unknown>) => {
    harness.calls.push({ command, args })
    return Promise.resolve()
  },
  isTauri: () => false,
}))

vi.mock("@tauri-apps/api/event", () => ({
  listen: (name: string, handler: (e: { payload: unknown }) => void) => {
    if (!harness.handlers.has(name)) harness.handlers.set(name, new Set())
    harness.handlers.get(name)!.add(handler)
    return Promise.resolve(() => harness.handlers.get(name)?.delete(handler))
  },
}))

/** All `mpv_seek` targets sent so far, in order. */
const seeksSent = () =>
  harness.calls
    .filter((c) => c.command === "mpv_seek")
    .map((c) => c.args!.ms as number)

/** All `mpv_set_pause` payloads sent so far, in order. */
const pausesSent = () =>
  harness.calls
    .filter((c) => c.command === "mpv_set_pause")
    .map((c) => c.args!.paused as boolean)

/** Deliver one mpv `time-pos` tick (recording-local ms) to the frontend. */
const tick = (ms: number) => act(() => harness.emit("mpv:time-pos", ms))

/** mpv confirms the pending load is open (its resume seek, if any, applied). */
const fileLoaded = () => act(() => harness.emit("mpv:file-loaded"))

/**
 * One 15-minute recording. Rallies leave a ~25s gap after R1, a 3-minute gap
 * after R2 (the segmenter found no play there), and a long trailing region
 * after the last rally — the two kinds of "certain regions" the bug report
 * names.
 */
const RALLIES = [
  { id: 1, start_ms: 20_000, end_ms: 35_000, confidence: 0.9, flagged: false },
  { id: 2, start_ms: 60_000, end_ms: 75_000, confidence: 0.9, flagged: false },
  { id: 3, start_ms: 255_000, end_ms: 270_000, confidence: 0.9, flagged: false },
]

const TIMELINE: Timeline = {
  segment_state: "ready",
  duration_ms: 900_000,
  rallies: RALLIES,
  waveform: [],
}

/**
 * Let pending `listen()` registration/teardown promises settle — in the app
 * they resolve long before the next mpv event arrives, so a test emitting an
 * event right after a mount or unmount must give them the same head start.
 */
const flush = () => act(async () => {})

async function renderPlayback() {
  const recordings = [{ path: "a.mp4" }]
  const timelines = { "a.mp4": TIMELINE }
  const session = buildSessionModel(recordings, timelines)
  const view = renderHook(
    () =>
      useSessionPlayback({
        recordings,
        startIndex: 0,
        timelines,
        session,
        segmentOffset: (i) => session.segments[i]?.offsetMs ?? 0,
      }),
    // The app mounts under StrictMode (main.tsx), whose dev-mode synchronous
    // setup→cleanup→setup is exactly what exposed the listener leak below —
    // the harness must run the same way.
    { wrapper: StrictMode }
  )
  await flush()
  return view
}

/** How many mpv-event handlers are currently subscribed, across all events. */
const liveHandlerCount = () =>
  [...harness.handlers.values()].reduce((n, s) => n + s.size, 0)

beforeEach(() => {
  harness.reset()
})

describe("session playback orchestration vs. the async mpv boundary", () => {
  it("control: gap-free playback skips a gap to the next rally", async () => {
    await renderPlayback()
    fileLoaded()
    tick(34_000) // inside R1
    tick(36_000) // crossed into the gap after R1
    expect(seeksSent()).toEqual([60_000]) // gap-skip to R2's start
  })

  it("a scrub is not overridden by a stale pre-seek tick (auto-seek-forward bug)", async () => {
    const { result } = await renderPlayback()
    fileLoaded()
    tick(25_000) // playing inside R1

    // Scrub into the gap between R2 and R3 → free play, watch the gap.
    act(() => result.current.seekSession(100_000))
    tick(100_000)
    tick(101_000)

    // Scrub back to R1. mpv_seek(25_000) goes out; free play switches off.
    act(() => result.current.seekSession(25_000))
    const seeksBefore = seeksSent().length

    // A tick mpv emitted at the *old* position, delivered after the scrub —
    // always possible, since the event stream and the invoke are both async.
    tick(101_500)
    // The scrub's seek lands.
    tick(25_000)

    // Symptom: the stale gap-position tick must not run gap-skip and yank the
    // playhead ~2.5 minutes forward to R3; the playhead stays where the user
    // scrubbed.
    expect(seeksSent().slice(seeksBefore)).toEqual([])
    expect(result.current.currentMs).toBe(25_000)
  })

  it("a scrub is not paused by a stale tick from past the last rally (pause-on-scrub bug)", async () => {
    const { result } = await renderPlayback()
    fileLoaded()

    // Scrub into the long tail after the last rally → free play, watch it.
    act(() => result.current.seekSession(500_000))
    tick(500_000)
    tick(501_000)

    // Scrub back to R1; a stale tail-position tick is delivered after it.
    act(() => result.current.seekSession(25_000))
    tick(501_500)
    tick(25_000)

    // Symptom: the stale tick reads as "past the session's last rally" and
    // pauses playback in the middle of a scrub.
    expect(pausesSent()).toEqual([])
    expect(result.current.currentMs).toBe(25_000)
  })

  it("closing the player tears down every mpv listener (stale-session gap-skip bug)", async () => {
    const view = await renderPlayback()
    fileLoaded()
    tick(25_000)

    // Close the session (player unmounts). Under StrictMode the first mount's
    // listen() promises resolve only after its cleanup already ran, so an
    // array-collected unlisten misses them — the leaked handlers then gap-skip
    // every later session against *this* session's rally table.
    view.unmount()
    await flush()
    const before = harness.calls.length

    // A tick from whatever plays next, at a position inside this (closed)
    // session's gap: no leaked listener may act on it.
    tick(36_000)
    expect(harness.calls.slice(before)).toEqual([])
    expect(liveHandlerCount()).toBe(0)
  })

  it("resuming play after the end-of-session stop is not instantly re-paused (1-frame-advance bug)", async () => {
    await renderPlayback()
    fileLoaded()
    tick(269_000) // inside R3, the session's last rally

    // Playhead crosses the last rally's end → the designed end-of-session stop.
    tick(270_100)
    expect(pausesSent()).toEqual([true])

    // The user presses play: mpv unpauses and the playhead advances one frame.
    act(() => harness.emit("mpv:pause", false))
    tick(270_140)

    // Symptom: every resume is instantly re-paused, so play only ever advances
    // one frame. A deliberate resume past the last rally must stick.
    expect(pausesSent()).toEqual([true])
  })
})
