import * as React from "react"

import type { DebugPanelSide, DebugPanelTab } from "./types"

export const MAX_LOG_ITEMS = 10
export const COLOR_SCHEME_QUERY = "(prefers-color-scheme: dark)"
export const DEBUG_PANEL_SIDE_KEY = "ctui-debug-panel-side"
export const DEBUG_PANEL_TAB_KEY = "ctui-debug-panel-tab"
export const DEBUG_PANEL_ATTACH_KEY = "ctui-debug-panel-attach"
export const SIDE_PANEL_WIDTH = 480
export const BOTTOM_PANEL_HEIGHT = 360

export const DEBUG_PANEL_THEME_STYLE_LIGHT = {
  colorScheme: "light",
  "--background": "oklch(0.955 0.006 250)",
  "--foreground": "oklch(0.255 0.015 250)",
  "--card": "oklch(0.985 0.004 250)",
  "--card-foreground": "oklch(0.255 0.015 250)",
  "--popover": "oklch(0.955 0.006 250)",
  "--popover-foreground": "oklch(0.255 0.015 250)",
  "--primary": "oklch(0.56 0.1 235)",
  "--primary-foreground": "oklch(0.985 0 0)",
  "--secondary": "oklch(0.925 0.008 250)",
  "--secondary-foreground": "oklch(0.31 0.015 250)",
  "--muted": "oklch(0.935 0.006 250)",
  "--muted-foreground": "oklch(0.52 0.014 250)",
  "--accent": "oklch(0.925 0.01 250)",
  "--accent-foreground": "oklch(0.255 0.015 250)",
  "--destructive": "oklch(0.63 0.20 24)",
  "--destructive-foreground": "oklch(0.985 0 0)",
  "--border": "oklch(0.86 0.008 250)",
  "--input": "oklch(0.86 0.008 250)",
  "--ring": "oklch(0.62 0.08 235)",
  "--radius": "0.5rem",
} as React.CSSProperties

export const DEBUG_PANEL_THEME_STYLE_DARK = {
  colorScheme: "dark",
  "--background": "oklch(0.205 0.012 250)",
  "--foreground": "oklch(0.92 0.008 250)",
  "--card": "oklch(0.235 0.012 250)",
  "--card-foreground": "oklch(0.92 0.008 250)",
  "--popover": "oklch(0.205 0.012 250)",
  "--popover-foreground": "oklch(0.92 0.008 250)",
  "--primary": "oklch(0.74 0.085 230)",
  "--primary-foreground": "oklch(0.19 0.01 250)",
  "--secondary": "oklch(0.28 0.012 250)",
  "--secondary-foreground": "oklch(0.9 0.008 250)",
  "--muted": "oklch(0.255 0.012 250)",
  "--muted-foreground": "oklch(0.7 0.012 250)",
  "--accent": "oklch(0.28 0.012 250)",
  "--accent-foreground": "oklch(0.92 0.008 250)",
  "--destructive": "oklch(0.66 0.19 24)",
  "--destructive-foreground": "oklch(0.98 0 0)",
  "--border": "oklch(0.36 0.01 250)",
  "--input": "oklch(0.36 0.01 250)",
  "--ring": "oklch(0.68 0.07 230)",
  "--radius": "0.5rem",
} as React.CSSProperties

export function isDebugPanelSide(value: string): value is DebugPanelSide {
  return value === "left" || value === "right" || value === "bottom"
}

export function isDebugPanelTab(value: string): value is DebugPanelTab {
  return (
    value === "overview" ||
    value === "runtime" ||
    value === "system" ||
    value === "errors"
  )
}
