import { describe, expect, it, vi } from "vitest"

import { buildKeymap, type KeymapActions } from "./keymap"

/** Every action a spy, so a test can tell which one a key reached. */
function actions(editing = false): KeymapActions {
  return {
    togglePlay: vi.fn(),
    seekRelative: vi.fn(),
    changeVolume: vi.fn(),
    frameStep: vi.fn(),
    goToRally: vi.fn(),
    goToUncertain: vi.fn(),
    goToNextUnmarked: vi.fn(),
    toggleLoop: vi.fn(),
    toggleGapSkip: vi.fn(),
    jumpToPlayhead: vi.fn(),
    zoomTimeline: vi.fn(),
    fitTimeline: vi.fn(),
    toggleMute: vi.fn(),
    stepSpeed: vi.fn(),
    resetSpeed: vi.fn(),
    annotate: vi.fn(),
    flagCurrentRally: vi.fn(),
    editing,
    toggleEditing: vi.fn(),
    addAtPlayhead: vi.fn(),
    splitEditTarget: vi.fn(),
    mergeEditTarget: vi.fn(),
    deleteEditTarget: vi.fn(),
    toggleCheatSheet: vi.fn(),
  }
}

/** A keydown as the window handler sees it. */
function key(
  k: string,
  mods: Partial<
    Pick<KeyboardEvent, "ctrlKey" | "metaKey" | "altKey" | "shiftKey">
  > = {},
  code = ""
): KeyboardEvent {
  return {
    key: k,
    code,
    ctrlKey: false,
    metaKey: false,
    altKey: false,
    shiftKey: false,
    ...mods,
  } as KeyboardEvent
}

/** The bindings a keydown matches — the handler runs the first. */
function matches(keymap: ReturnType<typeof buildKeymap>, e: KeyboardEvent) {
  return keymap.filter((b) => b.match(e))
}

describe("keymap", () => {
  it("never matches one keystroke to two bindings", () => {
    const presses = [
      key(" ", {}, "Space"),
      ...["k", "u", "n", "l", "g", "f", "e", "a", "s", "c", "m", "x", "z"].map(
        (k) => key(k)
      ),
      ...["1", "2", "3", "0", "-", "=", ",", ".", "[", "]", "?"].map((k) =>
        key(k)
      ),
      key("+", { shiftKey: true }),
      key("_", { shiftKey: true }),
      key("-", { ctrlKey: true }),
      key("=", { ctrlKey: true }),
      key("0", { ctrlKey: true }),
      key("ArrowLeft"),
      key("ArrowLeft", { ctrlKey: true }),
      key("ArrowLeft", { shiftKey: true }),
      key("ArrowUp"),
      key("Delete"),
      key("Backspace"),
    ]
    for (const editing of [false, true]) {
      const keymap = buildKeymap(actions(editing))
      for (const e of presses) {
        expect(
          matches(keymap, e).length,
          `${e.key} (ctrl ${e.ctrlKey}, shift ${e.shiftKey})`
        ).toBeLessThanOrEqual(1)
      }
    }
  })

  it("zooms the timeline on bare - / = and fits on 0 or Z", () => {
    const a = actions()
    const keymap = buildKeymap(a)
    const press = (e: KeyboardEvent) => keymap.find((b) => b.match(e))?.run(e)
    press(key("-"))
    press(key("="))
    press(key("+", { shiftKey: true }))
    expect(a.zoomTimeline).toHaveBeenNthCalledWith(1, -1)
    expect(a.zoomTimeline).toHaveBeenNthCalledWith(2, 1)
    expect(a.zoomTimeline).toHaveBeenNthCalledWith(3, 1)
    press(key("0"))
    press(key("z"))
    expect(a.fitTimeline).toHaveBeenCalledTimes(2)
    expect(a.stepSpeed).not.toHaveBeenCalled()
    expect(a.resetSpeed).not.toHaveBeenCalled()
  })

  it("keeps Ctrl+- / Ctrl+= / Ctrl+0 on playback speed", () => {
    const a = actions()
    const keymap = buildKeymap(a)
    const press = (e: KeyboardEvent) => keymap.find((b) => b.match(e))?.run(e)
    press(key("-", { ctrlKey: true }))
    press(key("=", { ctrlKey: true }))
    press(key("0", { ctrlKey: true }))
    expect(a.stepSpeed).toHaveBeenNthCalledWith(1, -1)
    expect(a.stepSpeed).toHaveBeenNthCalledWith(2, 1)
    expect(a.resetSpeed).toHaveBeenCalledTimes(1)
    expect(a.zoomTimeline).not.toHaveBeenCalled()
    expect(a.fitTimeline).not.toHaveBeenCalled()
  })

  it("goes to the next unmarked rally on N", () => {
    const a = actions()
    const keymap = buildKeymap(a)
    const e = key("n")
    keymap.find((b) => b.match(e))?.run(e)
    expect(a.goToNextUnmarked).toHaveBeenCalledTimes(1)
  })

  it("lists the new keys for the cheat-sheet", () => {
    const keymap = buildKeymap(actions())
    const keysOf = (label: string) =>
      keymap.find((b) => b.label === label)?.keys
    expect(keysOf("Zoom the timeline out / in")).toEqual(["-", "="])
    expect(keysOf("Fit the whole session")).toEqual(["0", "Z"])
    expect(keysOf("Next unmarked rally")).toEqual(["N"])
    expect(
      keymap.filter((b) => b.group === "timeline").map((b) => b.keys[0])
    ).toEqual(expect.arrayContaining(["F", "-", "0"]))
  })
})
