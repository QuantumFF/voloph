// @vitest-environment jsdom
/**
 * Alt+scroll zoom over the session strip must keep the moment under the cursor
 * under the cursor — including a burst of wheel notches that lands before the
 * strip re-renders at the new width (the "view jumps away when I zoom fast"
 * bug). jsdom has no layout, so the scroll box is given one: a fixed viewport
 * width, a scroll width read from the content's inline width, and a
 * `scrollLeft` the "browser" clamps to that range, as a real one does.
 */
import { StrictMode, useState } from "react"
import { act, render } from "@testing-library/react"
import { beforeAll, describe, expect, it } from "vitest"

import {
  NO_PADDING,
  buildSessionModel,
} from "@/components/recording-player-transport"
import { SessionTimeline } from "./session-timeline"

const VIEWPORT_PX = 500

beforeAll(() => {
  globalThis.ResizeObserver ??= class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver
})

const session = buildSessionModel([{ path: "a.mp4" }], {
  "a.mp4": {
    segment_state: "ready",
    duration_ms: 600_000,
    rallies: [
      {
        id: 1,
        start_ms: 10_000,
        end_ms: 20_000,
        confidence: 1,
        flagged: false,
      },
    ],
    waveform: [],
  },
})

const noop = () => {}

function Harness({
  initialPx,
  playheadMs,
  onState,
}: {
  initialPx: number
  playheadMs: number | null
  onState: (s: { pxPerSec: number; following: boolean }) => void
}) {
  const [pxPerSec, setPxPerSec] = useState(initialPx)
  const [following, setFollowing] = useState(true)
  const [selectedKey, setSelectedKey] = useState<string | null>(null)
  onState({ pxPerSec, following })
  return (
    <SessionTimeline
      session={session}
      annotations={[]}
      globalPlayheadMs={playheadMs}
      padding={NO_PADDING}
      pxPerSec={pxPerSec}
      setPxPerSec={setPxPerSec}
      following={following}
      setFollowing={setFollowing}
      canPrev={false}
      canNext={false}
      editing={false}
      selectedKey={selectedKey}
      setSelectedKey={setSelectedKey}
      onSeekGlobal={noop}
      onPrevRally={noop}
      onNextRally={noop}
      onNextUncertain={noop}
      onToggleEditing={noop}
      onAdjustRally={noop}
      onAddAtPlayhead={noop}
      onDeleteRally={noop}
      onSplitRally={noop}
      onMergeRallies={noop}
    />
  )
}

/** Give the strip's scroll box a layout jsdom lacks. */
function layOut(scrollBox: HTMLElement) {
  const content = scrollBox.firstElementChild as HTMLElement
  let left = 0
  const max = () => Math.max(0, parseFloat(content.style.width) - VIEWPORT_PX)
  Object.defineProperties(scrollBox, {
    clientWidth: { get: () => VIEWPORT_PX },
    scrollWidth: { get: () => parseFloat(content.style.width) },
    scrollLeft: {
      get: () => left,
      set: (v: number) => {
        left = Math.min(Math.max(v, 0), max())
        scrollBox.dispatchEvent(new Event("scroll"))
      },
    },
  })
  scrollBox.getBoundingClientRect = () =>
    ({ left: 0, top: 0, width: VIEWPORT_PX, height: 100 }) as DOMRect
}

function setup(initialPx: number, playheadMs: number | null = null) {
  const state = { pxPerSec: initialPx, following: true }
  const { container } = render(
    <StrictMode>
      <Harness
        initialPx={initialPx}
        playheadMs={playheadMs}
        onState={(s) => Object.assign(state, s)}
      />
    </StrictMode>
  )
  const scrollBox = container.querySelector(".overflow-x-auto") as HTMLElement
  layOut(scrollBox)
  return { state, scrollBox }
}

/** Within a pixel at this zoom — `scrollLeft` lands on whole pixels. */
const expectWithinPx = (ms: number, expected: number, pxPerSec: number) =>
  expect(Math.abs(ms - expected)).toBeLessThanOrEqual(1000 / pxPerSec)

/** The session time (ms) under viewport x. */
const msUnder = (scrollBox: HTMLElement, x: number, pxPerSec: number) =>
  ((scrollBox.scrollLeft + x) / pxPerSec) * 1000

const altWheel = (deltaY: number, clientX: number) =>
  new WheelEvent("wheel", {
    deltaY,
    clientX,
    altKey: true,
    bubbles: true,
    cancelable: true,
  })

describe("Alt+scroll zoom", () => {
  it("keeps the moment under the cursor when zooming in near the strip's end", () => {
    const { state, scrollBox } = setup(3)
    // 600 s at 3 px/s is 1800 px; scroll to the far end, cursor near the right.
    act(() => {
      scrollBox.scrollLeft = 1300
    })
    const cursorX = 450
    const before = msUnder(scrollBox, cursorX, state.pxPerSec)
    act(() => {
      scrollBox.dispatchEvent(altWheel(-1, cursorX))
    })
    expect(state.pxPerSec).toBeGreaterThan(3)
    expectWithinPx(
      msUnder(scrollBox, cursorX, state.pxPerSec),
      before,
      state.pxPerSec
    )
  })

  it("keeps the moment under the cursor through a fast burst of notches", () => {
    const { state, scrollBox } = setup(3)
    act(() => {
      scrollBox.scrollLeft = 600
    })
    const cursorX = 320
    const before = msUnder(scrollBox, cursorX, state.pxPerSec)
    // Several notches land before React re-renders the strip.
    act(() => {
      for (let i = 0; i < 6; i++) scrollBox.dispatchEvent(altWheel(-1, cursorX))
    })
    expect(state.pxPerSec).toBeCloseTo(3 * 1.15 ** 6, 3)
    expectWithinPx(
      msUnder(scrollBox, cursorX, state.pxPerSec),
      before,
      state.pxPerSec
    )
    act(() => {
      for (let i = 0; i < 6; i++) scrollBox.dispatchEvent(altWheel(1, cursorX))
    })
    expect(state.pxPerSec).toBeCloseTo(3, 3)
    expectWithinPx(
      msUnder(scrollBox, cursorX, state.pxPerSec),
      before,
      state.pxPerSec
    )
  })

  it("stops following rather than yanking the view to an off-screen playhead", () => {
    // Playhead at 10 s, in view, following; zoom in around 150 s — the
    // playhead leaves the view.
    const { state, scrollBox } = setup(3, 10_000)
    expect(state.following).toBe(true)
    const cursorX = 450
    const before = msUnder(scrollBox, cursorX, state.pxPerSec)
    act(() => {
      for (let i = 0; i < 4; i++) scrollBox.dispatchEvent(altWheel(-1, cursorX))
    })
    expectWithinPx(
      msUnder(scrollBox, cursorX, state.pxPerSec),
      before,
      state.pxPerSec
    )
    expect(state.following).toBe(false)
  })

  it("keeps following when the playhead stays in view", () => {
    // Zoom in right on the playhead (80 s, mid-view): it stays put, so
    // follow stays armed.
    const { state, scrollBox } = setup(3, 80_000)
    act(() => {
      for (let i = 0; i < 3; i++) scrollBox.dispatchEvent(altWheel(-1, 240))
    })
    expect(state.following).toBe(true)
  })
})
