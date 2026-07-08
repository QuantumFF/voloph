import { MAX_LOG_ITEMS } from "./constants"
import type { ExtendedNavigator, LocationState } from "./types"

export function isTypingTarget(target: EventTarget | null) {
  if (!(target instanceof HTMLElement)) {
    return false
  }

  return (
    target.isContentEditable ||
    target.tagName === "INPUT" ||
    target.tagName === "TEXTAREA" ||
    target.tagName === "SELECT"
  )
}

export function readLocationState(): LocationState {
  return {
    pathname: window.location.pathname,
    href: window.location.href,
    search: window.location.search,
    hash: window.location.hash,
  }
}

export function formatTimestamp(value: string) {
  try {
    return new Intl.DateTimeFormat(undefined, {
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    }).format(new Date(value))
  } catch {
    return value
  }
}

export function parsePluginLogMessage(message: string) {
  const match = message.match(
    /^\[([^\]]+)\]\[([^\]]+)\]\[([^\]]+)\]\[([A-Z]+)\]\s*(.*)$/
  )

  if (!match) {
    return null
  }

  const [, date, time, target, level, text] = match

  return {
    date,
    time,
    target,
    level: level.toLowerCase(),
    text,
  }
}

export function getLogLevelClasses(level: string) {
  switch (level) {
    case "error":
      return "border-destructive/25 bg-destructive/15 text-destructive"
    case "warn":
      return "border-amber-500/25 bg-amber-500/15 text-amber-600 dark:text-amber-400"
    case "info":
      return "border-emerald-500/25 bg-emerald-500/15 text-emerald-600 dark:text-emerald-400"
    case "debug":
      return "border-sky-500/25 bg-sky-500/15 text-sky-600 dark:text-sky-400"
    case "trace":
      return "border-border/25 bg-muted text-muted-foreground"
    default:
      return "border-border/25 bg-muted text-foreground"
  }
}

export function createMediaQueryList(query: string) {
  try {
    return window.matchMedia(query)
  } catch {
    return null
  }
}

export function matchesMediaQuery(query: string) {
  return createMediaQueryList(query)?.matches ?? false
}

export function getLocaleChain() {
  const candidates = [...navigator.languages]

  if (navigator.language) {
    candidates.push(navigator.language)
  }

  return [...new Set(candidates.filter(Boolean))]
}

export function getFirstDayOfWeek(locale: string) {
  try {
    const intlLocale = new Intl.Locale(locale) as Intl.Locale & {
      weekInfo?: {
        firstDay: number
      }
    }

    return intlLocale.weekInfo?.firstDay?.toString() ?? "Unavailable"
  } catch {
    return "Unavailable"
  }
}

export function getContrastPreference() {
  if (matchesMediaQuery("(prefers-contrast: more)")) return "more"
  if (matchesMediaQuery("(prefers-contrast: less)")) return "less"
  if (matchesMediaQuery("(prefers-contrast: custom)")) return "custom"
  return "no-preference"
}

export function getColorGamut() {
  if (matchesMediaQuery("(color-gamut: rec2020)")) return "rec2020"
  if (matchesMediaQuery("(color-gamut: p3)")) return "p3"
  if (matchesMediaQuery("(color-gamut: srgb)")) return "srgb"
  return "unknown"
}

export function getPointerPrecision(
  query: "(pointer: fine)" | "(pointer: coarse)"
) {
  if (matchesMediaQuery(query)) {
    return query.includes("fine") ? "fine" : "coarse"
  }

  return "none"
}

export function getAnyPointerPrecision() {
  if (matchesMediaQuery("(any-pointer: fine)")) return "fine"
  if (matchesMediaQuery("(any-pointer: coarse)")) return "coarse"
  return "none"
}

export function getConnectionInfo() {
  const extendedNavigator = navigator as ExtendedNavigator

  return (
    extendedNavigator.connection ??
    extendedNavigator.mozConnection ??
    extendedNavigator.webkitConnection ??
    null
  )
}

export function pushRecent<T>(items: T[], nextItem: T) {
  return [nextItem, ...items].slice(0, MAX_LOG_ITEMS)
}

export function newestFirst<T extends { timestamp: string }>(items: T[]) {
  return [...items].sort(
    (left, right) =>
      new Date(right.timestamp).getTime() - new Date(left.timestamp).getTime()
  )
}
