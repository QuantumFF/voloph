// @vitest-environment jsdom
import { vi } from "vitest"

// React enables its render measures only where `console.timeStamp` exists,
// checked once when react-dom loads. WebKit has it; jsdom does not, so stub it
// before the imports below evaluate react-dom.
vi.hoisted(() => {
  console.timeStamp ??= () => {}
})

import { render } from "@testing-library/react"
import { afterEach, describe, expect, it } from "vitest"

import { drainPerformanceBuffer } from "./dev-performance-buffer"

// Issue #87: React's development build records every component render as a
// `performance.measure` entry (its DevTools Performance Tracks). The User
// Timing buffer keeps every entry until something clears it, so a dev app
// that re-renders steadily grows the web process without bound.

function Counter({ n }: { n: number }) {
  return <span>{n}</span>
}

function measureCount() {
  return performance.getEntriesByType("measure").length
}

afterEach(() => {
  vi.useRealTimers()
  performance.clearMeasures()
})

describe("drainPerformanceBuffer", () => {
  it("keeps React's dev render measures from accumulating", () => {
    const { rerender } = render(<Counter n={0} />)
    for (let i = 1; i <= 50; i++) rerender(<Counter n={i} />)
    // The leak's precondition: dev React really does fill the buffer.
    expect(measureCount()).toBeGreaterThan(0)

    vi.useFakeTimers({ toFake: ["setInterval", "clearInterval"] })
    const stop = drainPerformanceBuffer(1_000)
    vi.advanceTimersByTime(1_000)
    expect(measureCount()).toBe(0)

    for (let i = 51; i <= 100; i++) rerender(<Counter n={i} />)
    vi.advanceTimersByTime(1_000)
    expect(measureCount()).toBe(0)
    stop()
  })
})
