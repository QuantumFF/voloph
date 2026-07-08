import * as React from "react"
import {
  getIdentifier,
  getName,
  getTauriVersion,
  getVersion,
} from "@tauri-apps/api/app"
import { appConfigDir, appDataDir, resourceDir } from "@tauri-apps/api/path"
import { currentMonitor, getCurrentWindow } from "@tauri-apps/api/window"
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow"
import { attachConsole } from "@tauri-apps/plugin-log"

import {
  DEBUG_EVENT_NAME,
  type DebugEvent,
  type LogDebugEvent,
  type RuntimeEventDebugEvent,
  emitDebugEvent,
  serializeError,
} from "@/lib/debug-events"
import { trackedEmit, isTauri } from "@/lib/tauri"

import { COLOR_SCHEME_QUERY } from "./constants"
import {
  createMediaQueryList,
  getAnyPointerPrecision,
  getColorGamut,
  getConnectionInfo,
  getContrastPreference,
  getFirstDayOfWeek,
  getLocaleChain,
  getPointerPrecision,
  matchesMediaQuery,
  pushRecent,
  readLocationState,
} from "./diagnostics"
import type {
  AccessibilityDefaultsDiagnostics,
  AppDiagnostics,
  DisplayDefaultsDiagnostics,
  ErrorLog,
  ExtendedNavigator,
  InputDefaultsDiagnostics,
  LocaleDefaultsDiagnostics,
  LocationState,
  NetworkDefaultsDiagnostics,
  PathDiagnostics,
  ThemeDiagnostics,
  WindowDiagnostics,
} from "./types"

export type DebugDiagnostics = {
  tauriReady: boolean
  appInfo: AppDiagnostics
  windowInfo: WindowDiagnostics
  themeInfo: ThemeDiagnostics
  pathInfo: PathDiagnostics
  localeDefaults: LocaleDefaultsDiagnostics
  accessibilityDefaults: AccessibilityDefaultsDiagnostics
  inputDefaults: InputDefaultsDiagnostics
  displayDefaults: DisplayDefaultsDiagnostics
  networkDefaults: NetworkDefaultsDiagnostics
  locationState: LocationState
  externalLinks: string[]
  invokeLogs: DebugEvent[]
  runtimeEvents: RuntimeEventDebugEvent[]
  logEntries: LogDebugEvent[]
  highlightedRuntimeEventIds: string[]
  highlightedLogIds: string[]
  errors: ErrorLog[]
}

export function useDebugDiagnostics(): DebugDiagnostics {
  const [tauriReady] = React.useState(() => isTauri())
  const [appInfo, setAppInfo] = React.useState<AppDiagnostics>({
    name: null,
    version: null,
    identifier: null,
    tauriVersion: null,
  })
  const [windowInfo, setWindowInfo] = React.useState<WindowDiagnostics>({
    label: "main",
    title: null,
    viewportSize: null,
    tauriInnerSize: null,
    outerSize: null,
    outerPosition: null,
    scaleFactor: null,
    visible: null,
    focused: null,
    maximized: null,
    fullscreen: null,
    decorated: null,
    monitor: null,
  })
  const [themeInfo, setThemeInfo] = React.useState<ThemeDiagnostics>({
    system: "light",
    current: "light",
    htmlClass: "",
  })
  const [pathInfo, setPathInfo] = React.useState<PathDiagnostics>({
    appDataDir: null,
    appConfigDir: null,
    resourceDir: null,
  })
  const [localeDefaults, setLocaleDefaults] =
    React.useState<LocaleDefaultsDiagnostics>({
      localeChain: [],
      navigatorLanguage: "",
      intlLocale: "Unavailable",
      region: null,
      timeZone: "Unavailable",
      calendar: "Unavailable",
      numberingSystem: "Unavailable",
      hourCycle: "Unavailable",
      hour12: null,
      firstDayOfWeek: "Unavailable",
      documentLang: "",
      documentDir: "",
    })
  const [accessibilityDefaults, setAccessibilityDefaults] =
    React.useState<AccessibilityDefaultsDiagnostics>({
      colorScheme: "light",
      reducedMotion: "no-preference",
      contrast: "no-preference",
      forcedColors: "none",
      invertedColors: "none",
      reducedTransparency: "no-preference",
    })
  const [inputDefaults, setInputDefaults] =
    React.useState<InputDefaultsDiagnostics>({
      platform: "Unavailable",
      userAgent: "",
      maxTouchPoints: 0,
      touchCapable: false,
      pointer: "none",
      anyPointer: "none",
      hover: false,
      anyHover: false,
      hardwareConcurrency: null,
      deviceMemory: null,
    })
  const [displayDefaults, setDisplayDefaults] =
    React.useState<DisplayDefaultsDiagnostics>({
      viewport: "Unavailable",
      screen: "Unavailable",
      availableScreen: "Unavailable",
      devicePixelRatio: 1,
      orientation: "Unavailable",
      colorGamut: "unknown",
      dynamicRange: "standard",
      monochrome: false,
    })
  const [networkDefaults, setNetworkDefaults] =
    React.useState<NetworkDefaultsDiagnostics>({
      online: true,
      effectiveType: "Unavailable",
      saveData: null,
      downlink: null,
      rtt: null,
    })
  const [externalLinks, setExternalLinks] = React.useState<string[]>([])
  const [invokeLogs, setInvokeLogs] = React.useState<DebugEvent[]>([])
  const [runtimeEvents, setRuntimeEvents] = React.useState<
    RuntimeEventDebugEvent[]
  >([])
  const [logEntries, setLogEntries] = React.useState<LogDebugEvent[]>([])
  const [highlightedRuntimeEventIds, setHighlightedRuntimeEventIds] =
    React.useState<string[]>([])
  const [highlightedLogIds, setHighlightedLogIds] = React.useState<string[]>([])
  const [errors, setErrors] = React.useState<ErrorLog[]>([])
  const [locationState, setLocationState] = React.useState<LocationState>({
    pathname: "",
    href: "",
    search: "",
    hash: "",
  })
  const highlightTimeoutsRef = React.useRef<number[]>([])

  React.useEffect(() => {
    return () => {
      for (const timeoutId of highlightTimeoutsRef.current) {
        window.clearTimeout(timeoutId)
      }
    }
  }, [])

  React.useEffect(() => {
    function updateThemeInfo() {
      const htmlClass = document.documentElement.className
      const system = window.matchMedia(COLOR_SCHEME_QUERY).matches
        ? "dark"
        : "light"

      setThemeInfo({
        system,
        current: document.documentElement.classList.contains("dark")
          ? "dark"
          : "light",
        htmlClass,
      })
    }

    updateThemeInfo()

    const mediaQuery = window.matchMedia(COLOR_SCHEME_QUERY)
    const observer = new MutationObserver(() => {
      updateThemeInfo()
    })
    const handleMediaChange = () => {
      updateThemeInfo()
    }

    mediaQuery.addEventListener("change", handleMediaChange)
    observer.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["class"],
    })

    return () => {
      mediaQuery.removeEventListener("change", handleMediaChange)
      observer.disconnect()
    }
  }, [])

  React.useEffect(() => {
    async function loadAppInfo() {
      if (!tauriReady) {
        return
      }

      const [name, version, identifier, tauriVersion] = await Promise.all([
        getName(),
        getVersion(),
        getIdentifier(),
        getTauriVersion(),
      ])

      setAppInfo({
        name,
        version,
        identifier,
        tauriVersion,
      })
    }

    void loadAppInfo()
  }, [tauriReady])

  React.useEffect(() => {
    async function loadPaths() {
      if (!tauriReady) {
        return
      }

      const [dataPath, configPath, resourcesPath] = await Promise.all([
        appDataDir(),
        appConfigDir(),
        resourceDir(),
      ])

      setPathInfo({
        appDataDir: dataPath,
        appConfigDir: configPath,
        resourceDir: resourcesPath,
      })
    }

    void loadPaths()
  }, [tauriReady])

  React.useEffect(() => {
    function refreshSystemDefaults() {
      const resolved = new Intl.DateTimeFormat().resolvedOptions()
      const localeChain = getLocaleChain()
      let region: string | null

      try {
        region = new Intl.Locale(resolved.locale).region ?? null
      } catch {
        region = null
      }

      setLocaleDefaults({
        localeChain,
        navigatorLanguage: navigator.language || "Unavailable",
        intlLocale: resolved.locale,
        region,
        timeZone: resolved.timeZone ?? "Unavailable",
        calendar: resolved.calendar ?? "Unavailable",
        numberingSystem: resolved.numberingSystem ?? "Unavailable",
        hourCycle: resolved.hourCycle ?? "Unavailable",
        hour12: resolved.hour12 ?? null,
        firstDayOfWeek: getFirstDayOfWeek(resolved.locale),
        documentLang: document.documentElement.lang || "(unset)",
        documentDir: document.documentElement.dir || "(unset)",
      })

      const extendedNavigator = navigator as ExtendedNavigator

      setAccessibilityDefaults({
        colorScheme: matchesMediaQuery(COLOR_SCHEME_QUERY) ? "dark" : "light",
        reducedMotion: matchesMediaQuery("(prefers-reduced-motion: reduce)")
          ? "reduce"
          : "no-preference",
        contrast: getContrastPreference(),
        forcedColors: matchesMediaQuery("(forced-colors: active)")
          ? "active"
          : "none",
        invertedColors: matchesMediaQuery("(inverted-colors: inverted)")
          ? "inverted"
          : "none",
        reducedTransparency: matchesMediaQuery(
          "(prefers-reduced-transparency: reduce)"
        )
          ? "reduce"
          : "no-preference",
      })

      setInputDefaults({
        platform:
          extendedNavigator.userAgentData?.platform ??
          navigator.platform ??
          "Unavailable",
        userAgent: navigator.userAgent || "Unavailable",
        maxTouchPoints: navigator.maxTouchPoints ?? 0,
        touchCapable:
          navigator.maxTouchPoints > 0 ||
          matchesMediaQuery("(pointer: coarse)") ||
          "ontouchstart" in window,
        pointer:
          getPointerPrecision("(pointer: fine)") === "fine"
            ? "fine"
            : getPointerPrecision("(pointer: coarse)"),
        anyPointer: getAnyPointerPrecision(),
        hover: matchesMediaQuery("(hover: hover)"),
        anyHover: matchesMediaQuery("(any-hover: hover)"),
        hardwareConcurrency: navigator.hardwareConcurrency ?? null,
        deviceMemory: extendedNavigator.deviceMemory ?? null,
      })

      setDisplayDefaults({
        viewport: `${window.innerWidth} × ${window.innerHeight}`,
        screen: `${window.screen.width} × ${window.screen.height}`,
        availableScreen: `${window.screen.availWidth} × ${window.screen.availHeight}`,
        devicePixelRatio: window.devicePixelRatio,
        orientation:
          window.screen.orientation?.type ??
          (window.innerWidth >= window.innerHeight ? "landscape" : "portrait"),
        colorGamut: getColorGamut(),
        dynamicRange: matchesMediaQuery("(dynamic-range: high)")
          ? "high"
          : "standard",
        monochrome: matchesMediaQuery("(monochrome)"),
      })

      const connection = getConnectionInfo()

      setNetworkDefaults({
        online: navigator.onLine,
        effectiveType: connection?.effectiveType ?? "Unavailable",
        saveData: connection?.saveData ?? null,
        downlink: connection?.downlink ?? null,
        rtt: connection?.rtt ?? null,
      })
    }

    refreshSystemDefaults()

    const mediaQueries = [
      COLOR_SCHEME_QUERY,
      "(prefers-reduced-motion: reduce)",
      "(prefers-contrast: more)",
      "(prefers-contrast: less)",
      "(prefers-contrast: custom)",
      "(forced-colors: active)",
      "(inverted-colors: inverted)",
      "(prefers-reduced-transparency: reduce)",
      "(pointer: fine)",
      "(pointer: coarse)",
      "(any-pointer: fine)",
      "(any-pointer: coarse)",
      "(hover: hover)",
      "(any-hover: hover)",
      "(color-gamut: rec2020)",
      "(color-gamut: p3)",
      "(color-gamut: srgb)",
      "(dynamic-range: high)",
      "(monochrome)",
    ]
      .map((query) => createMediaQueryList(query))
      .filter((query): query is MediaQueryList => query !== null)

    const handleChange = () => {
      refreshSystemDefaults()
    }

    for (const mediaQuery of mediaQueries) {
      mediaQuery.addEventListener("change", handleChange)
    }

    window.addEventListener("resize", handleChange)
    window.addEventListener("online", handleChange)
    window.addEventListener("offline", handleChange)
    window.screen.orientation?.addEventListener?.("change", handleChange)
    document.documentElement.addEventListener(
      "languagechange",
      handleChange as EventListener
    )

    const connection = getConnectionInfo()
    connection?.addEventListener?.("change", handleChange)

    return () => {
      for (const mediaQuery of mediaQueries) {
        mediaQuery.removeEventListener("change", handleChange)
      }

      window.removeEventListener("resize", handleChange)
      window.removeEventListener("online", handleChange)
      window.removeEventListener("offline", handleChange)
      window.screen.orientation?.removeEventListener?.("change", handleChange)
      document.documentElement.removeEventListener(
        "languagechange",
        handleChange as EventListener
      )
      connection?.removeEventListener?.("change", handleChange)
    }
  }, [])

  React.useEffect(() => {
    if (!tauriReady || !locationState.pathname) {
      return
    }

    void trackedEmit("ctui://debug-panel-mounted", {
      route: locationState.pathname,
    })
  }, [locationState.pathname, tauriReady])

  React.useEffect(() => {
    function updateLocationState() {
      const nextState = readLocationState()

      setLocationState((current) => {
        if (
          current.pathname === nextState.pathname &&
          current.href === nextState.href &&
          current.search === nextState.search &&
          current.hash === nextState.hash
        ) {
          return current
        }

        return nextState
      })
    }

    updateLocationState()

    // No router in this app, so nothing calls pushState — hashchange/popstate
    // cover every location change without a polling interval.
    window.addEventListener("hashchange", updateLocationState)
    window.addEventListener("popstate", updateLocationState)

    return () => {
      window.removeEventListener("hashchange", updateLocationState)
      window.removeEventListener("popstate", updateLocationState)
    }
  }, [])

  React.useEffect(() => {
    async function refreshWindowInfo() {
      if (!tauriReady) {
        return
      }

      const currentWindow = getCurrentWindow()
      const currentWebviewWindow = getCurrentWebviewWindow()
      const [
        title,
        tauriInnerSize,
        outerSize,
        outerPosition,
        scaleFactor,
        visible,
        focused,
        maximized,
        fullscreen,
        decorated,
        monitor,
      ] = await Promise.all([
        currentWindow.title(),
        currentWindow.innerSize(),
        currentWindow.outerSize(),
        currentWindow.outerPosition(),
        currentWindow.scaleFactor(),
        currentWindow.isVisible(),
        currentWindow.isFocused(),
        currentWindow.isMaximized(),
        currentWindow.isFullscreen(),
        currentWindow.isDecorated(),
        currentMonitor(),
      ])

      setWindowInfo({
        label: currentWebviewWindow.label,
        title,
        viewportSize: {
          width: window.innerWidth,
          height: window.innerHeight,
        },
        tauriInnerSize,
        outerSize,
        outerPosition,
        scaleFactor,
        visible,
        focused,
        maximized,
        fullscreen,
        decorated,
        monitor: monitor
          ? {
              name: monitor.name,
              size: monitor.size,
              scaleFactor: monitor.scaleFactor,
            }
          : null,
      })
    }

    void refreshWindowInfo()

    if (!tauriReady) {
      return
    }

    const currentWindow = getCurrentWindow()
    let unlistenFocus: (() => void) | undefined
    let unlistenResize: (() => void) | undefined
    let unlistenMove: (() => void) | undefined

    void currentWindow
      .onFocusChanged(() => {
        emitDebugEvent({
          id: crypto.randomUUID(),
          kind: "runtime-event",
          name: "window:focus-changed",
          source: "window",
          timestamp: new Date().toISOString(),
        })
        void refreshWindowInfo()
      })
      .then((unlisten) => {
        unlistenFocus = unlisten
      })

    void currentWindow
      .onResized(() => {
        emitDebugEvent({
          id: crypto.randomUUID(),
          kind: "runtime-event",
          name: "window:resized",
          source: "window",
          timestamp: new Date().toISOString(),
        })
        void refreshWindowInfo()
      })
      .then((unlisten) => {
        unlistenResize = unlisten
      })

    void currentWindow
      .onMoved(() => {
        emitDebugEvent({
          id: crypto.randomUUID(),
          kind: "runtime-event",
          name: "window:moved",
          source: "window",
          timestamp: new Date().toISOString(),
        })
        void refreshWindowInfo()
      })
      .then((unlisten) => {
        unlistenMove = unlisten
      })

    return () => {
      unlistenFocus?.()
      unlistenResize?.()
      unlistenMove?.()
    }
  }, [tauriReady])

  React.useEffect(() => {
    function queueHighlight(
      id: string,
      setIds: React.Dispatch<React.SetStateAction<string[]>>
    ) {
      setIds((current) => (current.includes(id) ? current : [id, ...current]))

      const timeoutId = window.setTimeout(() => {
        setIds((current) => current.filter((currentId) => currentId !== id))
        highlightTimeoutsRef.current = highlightTimeoutsRef.current.filter(
          (currentTimeoutId) => currentTimeoutId !== timeoutId
        )
      }, 900)

      highlightTimeoutsRef.current.push(timeoutId)
    }

    function handleDebugEvent(event: Event) {
      const debugEvent = (event as CustomEvent<DebugEvent>).detail

      if (debugEvent.kind === "external-link") {
        setExternalLinks((current) => pushRecent(current, debugEvent.href))
        return
      }

      if (debugEvent.kind === "runtime-event") {
        setRuntimeEvents((current) => pushRecent(current, debugEvent))
        queueHighlight(debugEvent.id, setHighlightedRuntimeEventIds)
        return
      }

      if (debugEvent.kind === "log") {
        setLogEntries((current) => pushRecent(current, debugEvent))
        queueHighlight(debugEvent.id, setHighlightedLogIds)
        return
      }

      setInvokeLogs((current) => pushRecent(current, debugEvent))
    }

    function handleError(event: ErrorEvent) {
      setErrors((current) =>
        pushRecent(current, {
          id: crypto.randomUUID(),
          timestamp: new Date().toISOString(),
          source: "error",
          message: event.message || "Unknown runtime error",
        })
      )
    }

    function handleRejection(event: PromiseRejectionEvent) {
      const reason = serializeError(event.reason)

      setErrors((current) =>
        pushRecent(current, {
          id: crypto.randomUUID(),
          timestamp: new Date().toISOString(),
          source: "unhandledrejection",
          message: reason.message,
        })
      )
    }

    window.addEventListener(DEBUG_EVENT_NAME, handleDebugEvent as EventListener)
    window.addEventListener("error", handleError)
    window.addEventListener("unhandledrejection", handleRejection)

    return () => {
      window.removeEventListener(
        DEBUG_EVENT_NAME,
        handleDebugEvent as EventListener
      )
      window.removeEventListener("error", handleError)
      window.removeEventListener("unhandledrejection", handleRejection)
    }
  }, [])

  React.useEffect(() => {
    const originalConsole = {
      log: window.console.log,
      info: window.console.info,
      warn: window.console.warn,
      error: window.console.error,
      debug: window.console.debug,
    }

    function capture(
      level: LogDebugEvent["level"],
      args: unknown[],
      originalMethod: (...data: unknown[]) => void
    ) {
      const message = args
        .map((value) => {
          if (typeof value === "string") {
            return value
          }

          try {
            return JSON.stringify(value)
          } catch {
            return String(value)
          }
        })
        .join(" ")

      emitDebugEvent({
        id: crypto.randomUUID(),
        kind: "log",
        level,
        message,
        timestamp: new Date().toISOString(),
      })

      originalMethod(...args)
    }

    window.console.log = (...args: unknown[]) => {
      capture("log", args, originalConsole.log)
    }

    window.console.info = (...args: unknown[]) => {
      capture("info", args, originalConsole.info)
    }

    window.console.warn = (...args: unknown[]) => {
      capture("warn", args, originalConsole.warn)
    }

    window.console.error = (...args: unknown[]) => {
      capture("error", args, originalConsole.error)
    }

    window.console.debug = (...args: unknown[]) => {
      capture("debug", args, originalConsole.debug)
    }

    let detachConsole: (() => void) | undefined
    let cancelled = false

    if (tauriReady) {
      void attachConsole().then((detach) => {
        // The effect may have cleaned up while attachConsole was in flight;
        // detach immediately instead of leaking the attachment.
        if (cancelled) {
          detach()
        } else {
          detachConsole = detach
        }
      })
    }

    return () => {
      cancelled = true
      detachConsole?.()
      window.console.log = originalConsole.log
      window.console.info = originalConsole.info
      window.console.warn = originalConsole.warn
      window.console.error = originalConsole.error
      window.console.debug = originalConsole.debug
    }
  }, [tauriReady])

  return {
    tauriReady,
    appInfo,
    windowInfo,
    themeInfo,
    pathInfo,
    localeDefaults,
    accessibilityDefaults,
    inputDefaults,
    displayDefaults,
    networkDefaults,
    locationState,
    externalLinks,
    invokeLogs,
    runtimeEvents,
    logEntries,
    highlightedRuntimeEventIds,
    highlightedLogIds,
    errors,
  }
}
