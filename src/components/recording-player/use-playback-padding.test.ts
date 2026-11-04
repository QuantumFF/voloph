// @vitest-environment jsdom
/**
 * The playback/export padding store (ADR 0020): one app-wide value that the
 * library menu changes and the player and moment browser follow without a reload.
 */
import { act, renderHook, waitFor } from "@testing-library/react"
import { beforeEach, describe, expect, it, vi } from "vitest"

import { DEFAULT_PADDING } from "@/components/recording-player-transport"
import {
  describePadding,
  paddingPresetId,
  resetPlaybackPaddingForTests,
  setPlaybackPadding,
  usePlaybackPadding,
} from "@/components/recording-player/use-playback-padding"

const invoke = vi.hoisted(() => vi.fn())
vi.mock("@/lib/tauri", () => ({ trackedInvoke: invoke }))

beforeEach(() => {
  invoke.mockReset()
  resetPlaybackPaddingForTests()
})

describe("usePlaybackPadding", () => {
  it("starts at the default and then shows the stored padding", async () => {
    invoke.mockResolvedValue({ lead_ms: 500, tail_ms: 1000 })
    const { result } = renderHook(() => usePlaybackPadding())
    expect(result.current).toEqual(DEFAULT_PADDING)
    await waitFor(() =>
      expect(result.current).toEqual({ lead_ms: 500, tail_ms: 1000 })
    )
    expect(invoke).toHaveBeenCalledWith("playback_padding")
  })

  it("keeps the default when the load fails or returns junk", async () => {
    invoke.mockRejectedValueOnce(new Error("no db"))
    const failed = renderHook(() => usePlaybackPadding())
    await act(async () => {})
    expect(failed.result.current).toEqual(DEFAULT_PADDING)

    resetPlaybackPaddingForTests()
    invoke.mockResolvedValueOnce({ lead_ms: "1" })
    const junk = renderHook(() => usePlaybackPadding())
    await act(async () => {})
    expect(junk.result.current).toEqual(DEFAULT_PADDING)
  })

  it("loads once for every reader", async () => {
    invoke.mockResolvedValue(DEFAULT_PADDING)
    renderHook(() => usePlaybackPadding())
    renderHook(() => usePlaybackPadding())
    await act(async () => {})
    expect(invoke).toHaveBeenCalledTimes(1)
  })

  it("a change reaches every reader at once", async () => {
    invoke.mockResolvedValue(DEFAULT_PADDING)
    const player = renderHook(() => usePlaybackPadding())
    const browser = renderHook(() => usePlaybackPadding())
    await act(async () => {})

    invoke.mockResolvedValueOnce(undefined)
    await act(() => setPlaybackPadding({ lead_ms: 0, tail_ms: 0 }))

    expect(invoke).toHaveBeenLastCalledWith("set_playback_padding", {
      padding: { lead_ms: 0, tail_ms: 0 },
    })
    expect(player.result.current).toEqual({ lead_ms: 0, tail_ms: 0 })
    expect(browser.result.current).toEqual({ lead_ms: 0, tail_ms: 0 })
  })

  it("a failed store keeps the old padding", async () => {
    invoke.mockResolvedValue(DEFAULT_PADDING)
    const { result } = renderHook(() => usePlaybackPadding())
    await act(async () => {})

    invoke.mockRejectedValueOnce(new Error("locked"))
    await expect(
      setPlaybackPadding({ lead_ms: 2000, tail_ms: 3000 })
    ).rejects.toThrow("locked")
    expect(result.current).toEqual(DEFAULT_PADDING)
  })
})

describe("padding presets", () => {
  it("names the preset a padding matches, else custom", () => {
    expect(paddingPresetId(DEFAULT_PADDING)).toBe("standard")
    expect(paddingPresetId({ lead_ms: 0, tail_ms: 0 })).toBe("none")
    expect(paddingPresetId({ lead_ms: 750, tail_ms: 1500 })).toBe("custom")
  })

  it("describes a padding in seconds", () => {
    expect(describePadding({ lead_ms: 1000, tail_ms: 1500 })).toBe(
      "1 s before, 1.5 s after"
    )
    expect(describePadding({ lead_ms: 0, tail_ms: 0 })).toBe(
      "The rally only, serve to landing"
    )
  })
})
