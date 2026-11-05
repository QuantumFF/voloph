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
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react"
import { afterEach, beforeAll, describe, expect, it } from "vitest"

import {
  NO_PADDING,
  buildSessionModel,
  type SessionAnnotation,
  type Verdict,
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
  annotations = [],
  onSeekGlobal = noop,
  onPlayheadVisibleChange,
}: {
  initialPx: number
  playheadMs: number | null
  onState: (s: { pxPerSec: number; following: boolean }) => void
  annotations?: SessionAnnotation[]
  onSeekGlobal?: (globalMs: number) => void
  onPlayheadVisibleChange?: (visible: boolean) => void
}) {
  const [pxPerSec, setPxPerSec] = useState(initialPx)
  const [following, setFollowing] = useState(true)
  const [selectedKey, setSelectedKey] = useState<string | null>(null)
  onState({ pxPerSec, following })
  return (
    <SessionTimeline
      session={session}
      annotations={annotations}
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
      onSeekGlobal={onSeekGlobal}
      onPrevRally={noop}
      onNextRally={noop}
      onNextUncertain={noop}
      onToggleEditing={noop}
      onAdjustRally={noop}
      onAddAtPlayhead={noop}
      onDeleteRally={noop}
      onSplitRally={noop}
      onMergeRallies={noop}
      onPlayheadVisibleChange={onPlayheadVisibleChange}
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
  const scrollBox = container.querySelector(".overflow-x-scroll") as HTMLElement
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

describe("verdict marks", () => {
  afterEach(cleanup)

  const mark = (id: number, globalMs: number, verdict: Verdict) => ({
    id,
    recordingIndex: 0,
    path: "a.mp4",
    verdict,
    aspect: null,
    note: null,
    globalMs,
  })
  const burst = [
    mark(1, 12_000, "good"),
    mark(2, 12_400, "bad"),
    mark(3, 12_800, "mistake"),
  ]

  it("merges marks too close to tell apart into one counted pill that seeks to the first", () => {
    const seeks: number[] = []
    render(
      <Harness
        initialPx={3}
        playheadMs={null}
        onState={noop}
        annotations={burst}
        onSeekGlobal={(ms) => seeks.push(ms)}
      />
    )
    const pill = screen.getByRole("button", { name: /^3 verdicts/ })
    expect(pill.textContent).toBe("×3")
    expect(pill.title).toContain("Good · 0:12")
    expect(pill.title).toContain("Mistake · 0:12")
    fireEvent.click(pill)
    expect(seeks).toEqual([12_000])
  })

  it("moves a pile's pill aside rather than let the playhead line slice it", () => {
    // At 3px/s the burst's pill is centred at 37.2px; a playhead at 12.4s
    // (37.2px) would cut straight through its count.
    const playheadPx = 37.2
    render(
      <Harness
        initialPx={3}
        playheadMs={12_400}
        onState={noop}
        annotations={burst}
      />
    )
    const pill = screen.getByRole("button", { name: /^3 verdicts/ })
    const left = parseFloat(pill.style.left)
    const right = left + parseFloat(pill.style.width)
    expect(right <= playheadPx - 2 || left >= playheadPx + 2).toBe(true)
  })

  it("gives a lone mark a hit area at least 8px wider than its head", () => {
    render(
      <Harness
        initialPx={60}
        playheadMs={null}
        onState={noop}
        annotations={burst}
      />
    )
    const head = screen.getByRole("button", { name: "Good at 0:12" })
    expect(parseFloat(head.style.width)).toBeGreaterThanOrEqual(6 + 8)
  })

  it("draws each mark on its own once zoomed in far enough", () => {
    render(
      <Harness
        initialPx={60}
        playheadMs={null}
        onState={noop}
        annotations={burst}
      />
    )
    expect(screen.queryByRole("button", { name: /verdicts/ })).toBeNull()
    expect(screen.getByRole("button", { name: "Good at 0:12" })).toBeTruthy()
    expect(screen.getByRole("button", { name: "Bad at 0:12" })).toBeTruthy()
    expect(screen.getByRole("button", { name: "Mistake at 0:12" })).toBeTruthy()
  })
})

describe("rally numbers", () => {
  afterEach(cleanup)

  it("labels each block with its rail number, inside it once it's wide enough", () => {
    // 10s rally at 3px/s: 30px, room for "1".
    render(<Harness initialPx={3} playheadMs={null} onState={noop} />)
    const block = screen.getByRole("button", { name: "Rally 1" })
    expect(block.textContent).toBe("1")
    expect(block.querySelector(".opacity-0")).toBeNull()
  })

  it("keeps a too-narrow block's number for hover", () => {
    // 10s rally at 1px/s: 10px.
    render(<Harness initialPx={1} playheadMs={null} onState={noop} />)
    const block = screen.getByRole("button", { name: "Rally 1" })
    expect(block.textContent).toBe("1")
    expect(block.querySelector(".opacity-0")?.textContent).toBe("1")
  })
})

describe("playhead visibility", () => {
  afterEach(cleanup)

  it("reports the playhead leaving and re-entering the visible window, only on change", () => {
    const reports: boolean[] = []
    const { container } = render(
      <Harness
        initialPx={3}
        playheadMs={100_000}
        onState={noop}
        onPlayheadVisibleChange={(v) => reports.push(v)}
      />
    )
    const scrollBox = container.querySelector(
      ".overflow-x-scroll"
    ) as HTMLElement
    layOut(scrollBox)
    // The playhead sits at 300px; a 500px window from 0 shows it.
    act(() => {
      scrollBox.scrollLeft = 0
    })
    expect(reports).toEqual([])
    act(() => {
      scrollBox.scrollLeft = 1_000
    })
    act(() => {
      scrollBox.scrollLeft = 1_200
    })
    expect(reports).toEqual([false])
    act(() => {
      scrollBox.scrollLeft = 100
    })
    expect(reports).toEqual([false, true])
  })

  it("is harmless without a listener", () => {
    const { container } = render(
      <Harness initialPx={3} playheadMs={100_000} onState={noop} />
    )
    const scrollBox = container.querySelector(
      ".overflow-x-scroll"
    ) as HTMLElement
    layOut(scrollBox)
    act(() => {
      scrollBox.scrollLeft = 1_000
    })
    expect(screen.getByTitle(/playhead is off-screen/)).toBeTruthy()
  })
})
