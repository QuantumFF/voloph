import * as React from "react"

export type AppDiagnostics = {
  name: string | null
  version: string | null
  identifier: string | null
  tauriVersion: string | null
}

export type WindowDiagnostics = {
  label: string
  title: string | null
  viewportSize: { width: number; height: number } | null
  tauriInnerSize: { width: number; height: number } | null
  outerSize: { width: number; height: number } | null
  outerPosition: { x: number; y: number } | null
  scaleFactor: number | null
  visible: boolean | null
  focused: boolean | null
  maximized: boolean | null
  fullscreen: boolean | null
  decorated: boolean | null
  monitor: {
    name: string | null
    size: { width: number; height: number }
    scaleFactor: number
  } | null
}

export type ThemeDiagnostics = {
  system: "dark" | "light"
  current: "dark" | "light"
  htmlClass: string
}

export type PathDiagnostics = {
  appDataDir: string | null
  appConfigDir: string | null
  resourceDir: string | null
}

export type LocaleDefaultsDiagnostics = {
  localeChain: string[]
  navigatorLanguage: string
  intlLocale: string
  region: string | null
  timeZone: string
  calendar: string
  numberingSystem: string
  hourCycle: string
  hour12: boolean | null
  firstDayOfWeek: string
  documentLang: string
  documentDir: string
}

export type AccessibilityDefaultsDiagnostics = {
  colorScheme: "dark" | "light"
  reducedMotion: "reduce" | "no-preference"
  contrast: "more" | "less" | "custom" | "no-preference"
  forcedColors: "active" | "none"
  invertedColors: "inverted" | "none"
  reducedTransparency: "reduce" | "no-preference"
}

export type InputDefaultsDiagnostics = {
  platform: string
  userAgent: string
  maxTouchPoints: number
  touchCapable: boolean
  pointer: "fine" | "coarse" | "none"
  anyPointer: "fine" | "coarse" | "none"
  hover: boolean
  anyHover: boolean
  hardwareConcurrency: number | null
  deviceMemory: number | null
}

export type DisplayDefaultsDiagnostics = {
  viewport: string
  screen: string
  availableScreen: string
  devicePixelRatio: number
  orientation: string
  colorGamut: "rec2020" | "p3" | "srgb" | "unknown"
  dynamicRange: "high" | "standard"
  monochrome: boolean
}

export type NetworkDefaultsDiagnostics = {
  online: boolean
  effectiveType: string
  saveData: boolean | null
  downlink: number | null
  rtt: number | null
}

export type NetworkInformationLike = EventTarget & {
  effectiveType?: string
  saveData?: boolean
  downlink?: number
  rtt?: number
}

export type ExtendedNavigator = Navigator & {
  connection?: NetworkInformationLike
  mozConnection?: NetworkInformationLike
  webkitConnection?: NetworkInformationLike
  deviceMemory?: number
  userAgentData?: {
    platform?: string
  }
}

export type SourceOrigin = "web" | "tauri"

export type GridEntryMeta = {
  code: string
  origin: SourceOrigin
}

export type GridEntry =
  | [label: string, value: React.ReactNode]
  | [label: string, value: React.ReactNode, meta: GridEntryMeta]

export type ErrorLog = {
  id: string
  timestamp: string
  source: "error" | "unhandledrejection"
  message: string
}

export type LocationState = {
  pathname: string
  href: string
  search: string
  hash: string
}

export type DebugPanelSide = "left" | "right" | "bottom"
export type DebugPanelTab = "overview" | "runtime" | "system" | "errors"
