"use client"

import * as React from "react"
import {
  CopyIcon,
  EllipsisIcon,
  PinIcon,
  RefreshCwIcon,
  TerminalSquareIcon,
  XIcon,
} from "lucide-react"

import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuShortcut,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs"
import { TooltipProvider } from "@/components/ui/tooltip"
import { cn } from "@/lib/utils"

import {
  BOTTOM_PANEL_HEIGHT,
  DEBUG_PANEL_ATTACH_KEY,
  DEBUG_PANEL_SIDE_KEY,
  DEBUG_PANEL_TAB_KEY,
  DEBUG_PANEL_THEME_STYLE_DARK,
  DEBUG_PANEL_THEME_STYLE_LIGHT,
  SIDE_PANEL_WIDTH,
  isDebugPanelSide,
  isDebugPanelTab,
} from "./constants"
import { isTypingTarget } from "./diagnostics"
import { ErrorsTab } from "./errors-tab"
import { OverviewTab } from "./overview-tab"
import { RuntimeTab } from "./runtime-tab"
import { SystemTab } from "./system-tab"
import { DebugPanelThemeContext } from "./theme-context"
import type { DebugPanelSide, DebugPanelTab } from "./types"
import { useDebugDiagnostics } from "./use-debug-diagnostics"

export function DebugPanel() {
  const [open, setOpen] = React.useState(false)
  // Persisted panel prefs hydrate via lazy initializers rather than a mount
  // effect, so the first render already has them (and no setState-in-effect).
  const [panelSide, setPanelSide] = React.useState<DebugPanelSide>(() => {
    const saved = window.sessionStorage.getItem(DEBUG_PANEL_SIDE_KEY)
    return saved && isDebugPanelSide(saved) ? saved : "right"
  })
  const [activeTab, setActiveTab] = React.useState<DebugPanelTab>(() => {
    const saved = window.sessionStorage.getItem(DEBUG_PANEL_TAB_KEY)
    return saved && isDebugPanelTab(saved) ? saved : "overview"
  })
  const [attached, setAttached] = React.useState(
    () => window.sessionStorage.getItem(DEBUG_PANEL_ATTACH_KEY) === "true"
  )
  const [copied, setCopied] = React.useState(false)
  const copiedTimeoutRef = React.useRef<number | undefined>(undefined)

  const {
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
  } = useDebugDiagnostics()

  React.useEffect(() => {
    return () => {
      window.clearTimeout(copiedTimeoutRef.current)
    }
  }, [])

  React.useEffect(() => {
    if (typeof window === "undefined") {
      return
    }

    window.sessionStorage.setItem(DEBUG_PANEL_SIDE_KEY, panelSide)
  }, [panelSide])

  React.useEffect(() => {
    if (typeof window === "undefined") {
      return
    }

    window.sessionStorage.setItem(DEBUG_PANEL_TAB_KEY, activeTab)
  }, [activeTab])

  React.useEffect(() => {
    if (typeof window === "undefined") {
      return
    }

    window.sessionStorage.setItem(DEBUG_PANEL_ATTACH_KEY, String(attached))
  }, [attached])

  React.useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      if (event.defaultPrevented || event.repeat) {
        return
      }

      if (!(event.metaKey || event.ctrlKey) || event.altKey || event.shiftKey) {
        return
      }

      if (event.key.toLowerCase() !== "d") {
        return
      }

      if (isTypingTarget(event.target)) {
        return
      }

      event.preventDefault()
      setOpen((current) => !current)
    }

    window.addEventListener("keydown", onKeyDown)

    return () => {
      window.removeEventListener("keydown", onKeyDown)
    }
  }, [])

  React.useEffect(() => {
    const mainElement = document.querySelector("[data-ui-scroll-container]")

    if (!(mainElement instanceof HTMLElement)) {
      return
    }

    mainElement.style.paddingLeft = ""
    mainElement.style.paddingRight = ""
    mainElement.style.paddingBottom = ""

    if (open && attached) {
      if (panelSide === "left") {
        mainElement.style.paddingLeft = `${SIDE_PANEL_WIDTH}px`
      }

      if (panelSide === "right") {
        mainElement.style.paddingRight = `${SIDE_PANEL_WIDTH}px`
      }

      if (panelSide === "bottom") {
        mainElement.style.paddingBottom = `${BOTTOM_PANEL_HEIGHT}px`
      }
    }

    return () => {
      mainElement.style.paddingLeft = ""
      mainElement.style.paddingRight = ""
      mainElement.style.paddingBottom = ""
    }
  }, [attached, open, panelSide])

  async function handleCopyText(value: string) {
    await navigator.clipboard.writeText(value)
  }

  async function handleCopySnapshot() {
    const snapshot = {
      tauriReady,
      panelSide,
      attached,
      appInfo,
      locationState,
      windowInfo,
      themeInfo,
      pathInfo,
      externalLinks,
      invokeLogs,
      runtimeEvents,
      logEntries,
      errors,
      capturedAt: new Date().toISOString(),
    }

    await navigator.clipboard.writeText(JSON.stringify(snapshot, null, 2))
    setCopied(true)
    window.clearTimeout(copiedTimeoutRef.current)
    copiedTimeoutRef.current = window.setTimeout(() => {
      setCopied(false)
    }, 1200)
  }

  const isBottomDock = panelSide === "bottom"
  const runtimeCount =
    invokeLogs.length +
    externalLinks.length +
    runtimeEvents.length +
    logEntries.length
  const overviewLayoutClassName = isBottomDock
    ? "grid min-h-full content-start gap-3 pr-1 xl:grid-cols-[minmax(0,1fr)_minmax(0,1.2fr)_minmax(0,0.9fr)]"
    : "h-full space-y-3 overflow-y-auto pr-1"
  const runtimeLayoutClassName = isBottomDock
    ? "grid min-h-full content-start gap-3 pr-1 xl:grid-cols-2"
    : "h-full space-y-3 overflow-y-auto pr-1"
  const systemLayoutClassName = isBottomDock
    ? "grid min-h-full content-start gap-3 pr-1 xl:grid-cols-[minmax(0,1.2fr)_minmax(0,0.8fr)]"
    : "h-full space-y-3 overflow-y-auto pr-1"
  const debugPanelThemeStyle =
    themeInfo.current === "light"
      ? DEBUG_PANEL_THEME_STYLE_LIGHT
      : DEBUG_PANEL_THEME_STYLE_DARK

  // Only build the (large) panel tree while the panel is actually open.
  const panelContent = open ? (
    <>
      <div className="border-b px-3 py-2">
        <div className="flex items-center justify-between gap-3">
          <div className="min-w-0">
            <h2 className="flex items-center gap-2 text-[13px] font-semibold tracking-tight text-foreground">
              <TerminalSquareIcon className="size-3.5" />
              Development Debug Panel
            </h2>
          </div>
          <div className="flex items-center gap-1.5">
            <Button
              variant={attached ? "secondary" : "outline"}
              size="icon-sm"
              className="size-7"
              onClick={() => setAttached((current) => !current)}
              aria-pressed={attached}
              aria-label={
                attached ? "Detach debug panel" : "Attach debug panel"
              }
              title={attached ? "Detach debug panel" : "Attach debug panel"}
            >
              <PinIcon className="size-3.5" />
            </Button>
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button
                  variant="ghost"
                  size="icon-sm"
                  className="size-7"
                  aria-label="Debug panel options"
                >
                  <EllipsisIcon className="size-3.5" />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end" className="w-52">
                <DropdownMenuItem onClick={() => void handleCopySnapshot()}>
                  <CopyIcon className="size-3.5" />
                  {copied ? "Copied snapshot" : "Copy snapshot"}
                </DropdownMenuItem>
                <DropdownMenuItem onClick={() => window.location.reload()}>
                  <RefreshCwIcon className="size-3.5" />
                  Reload app
                </DropdownMenuItem>
                <DropdownMenuSeparator />
                <DropdownMenuLabel>Dock</DropdownMenuLabel>
                <DropdownMenuRadioGroup
                  value={panelSide}
                  onValueChange={(value) => {
                    if (isDebugPanelSide(value)) {
                      setPanelSide(value)
                    }
                  }}
                >
                  <DropdownMenuRadioItem value="left">
                    Left
                  </DropdownMenuRadioItem>
                  <DropdownMenuRadioItem value="right">
                    Right
                  </DropdownMenuRadioItem>
                  <DropdownMenuRadioItem value="bottom">
                    Bottom
                  </DropdownMenuRadioItem>
                </DropdownMenuRadioGroup>
                <DropdownMenuSeparator />
                <DropdownMenuItem disabled>
                  Toggle panel
                  <DropdownMenuShortcut>⌘/Ctrl+D</DropdownMenuShortcut>
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
            <Button
              variant="ghost"
              size="icon-sm"
              className="size-7"
              onClick={() => setOpen(false)}
              aria-label="Close debug panel"
            >
              <XIcon className="size-3.5" />
            </Button>
          </div>
        </div>
      </div>

      <Tabs
        value={activeTab}
        onValueChange={(value) => {
          if (isDebugPanelTab(value)) {
            setActiveTab(value)
          }
        }}
        className="min-h-0 flex-1 gap-0 px-3"
      >
        <TabsList className="-mx-3 grid h-9 w-[calc(100%+1.5rem)] grid-cols-4 justify-start border-y border-border/20 bg-muted p-0">
          <TabsTrigger
            value="overview"
            className="h-9 rounded-none border-r border-border/20 px-2 text-[12px] font-medium"
          >
            Overview
          </TabsTrigger>
          <TabsTrigger
            value="runtime"
            className="h-9 rounded-none border-r border-border/20 px-2 text-[12px] font-medium"
          >
            Runtime
            {runtimeCount ? (
              <Badge
                variant="secondary"
                className="ml-1 h-4 min-w-4 rounded-sm px-1 text-[10px]"
              >
                {runtimeCount}
              </Badge>
            ) : null}
          </TabsTrigger>
          <TabsTrigger
            value="system"
            className="h-9 rounded-none border-r border-border/20 px-2 text-[12px] font-medium"
          >
            System
          </TabsTrigger>
          <TabsTrigger
            value="errors"
            className="h-9 rounded-none px-2 text-[12px] font-medium"
          >
            Errors
            {errors.length ? (
              <Badge
                variant="destructive"
                className="ml-1 h-4 min-w-4 rounded-sm px-1 text-[10px]"
              >
                {errors.length}
              </Badge>
            ) : null}
          </TabsTrigger>
        </TabsList>

        <TabsContent
          value="overview"
          className={cn(
            "min-h-0 flex-1 pt-2",
            isBottomDock ? "overflow-y-auto" : "overflow-hidden"
          )}
        >
          <OverviewTab
            layoutClassName={overviewLayoutClassName}
            tauriReady={tauriReady}
            appInfo={appInfo}
            windowInfo={windowInfo}
            themeInfo={themeInfo}
            accessibilityDefaults={accessibilityDefaults}
            locationState={locationState}
          />
        </TabsContent>

        <TabsContent
          value="runtime"
          className={cn(
            "min-h-0 flex-1 pt-2",
            isBottomDock ? "overflow-y-auto" : "overflow-hidden"
          )}
        >
          <RuntimeTab
            layoutClassName={runtimeLayoutClassName}
            isBottomDock={isBottomDock}
            invokeLogs={invokeLogs}
            runtimeEvents={runtimeEvents}
            logEntries={logEntries}
            externalLinks={externalLinks}
            highlightedRuntimeEventIds={highlightedRuntimeEventIds}
            highlightedLogIds={highlightedLogIds}
          />
        </TabsContent>

        <TabsContent
          value="system"
          className={cn(
            "min-h-0 flex-1 pt-2",
            isBottomDock ? "overflow-y-auto" : "overflow-hidden"
          )}
        >
          <SystemTab
            layoutClassName={systemLayoutClassName}
            pathInfo={pathInfo}
            displayDefaults={displayDefaults}
            localeDefaults={localeDefaults}
            inputDefaults={inputDefaults}
            networkDefaults={networkDefaults}
            onCopyText={(value) => void handleCopyText(value)}
          />
        </TabsContent>

        <TabsContent
          value="errors"
          className="min-h-0 flex-1 overflow-hidden pt-2"
        >
          <ErrorsTab errors={errors} />
        </TabsContent>
      </Tabs>
    </>
  ) : null

  const panelFrameClassName = cn(
    "ui-selectable fixed z-50 flex overflow-hidden border border-border/25 bg-background text-[12px] text-foreground",
    panelSide === "bottom"
      ? "inset-x-0 bottom-0 flex-col border-t"
      : panelSide === "left"
        ? "top-0 left-0 h-full flex-col border-r"
        : "top-0 right-0 h-full flex-col border-l"
  )

  return (
    <TooltipProvider delayDuration={200}>
      <DebugPanelThemeContext.Provider value={debugPanelThemeStyle}>
        {open ? (
          <div
            className={panelFrameClassName}
            style={
              panelSide === "bottom"
                ? {
                    ...debugPanelThemeStyle,
                    height: `${BOTTOM_PANEL_HEIGHT}px`,
                  }
                : { ...debugPanelThemeStyle, width: `${SIDE_PANEL_WIDTH}px` }
            }
          >
            {panelContent}
          </div>
        ) : null}
      </DebugPanelThemeContext.Provider>
    </TooltipProvider>
  )
}
