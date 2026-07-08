import { DebugSection } from "./debug-section"
import { KeyValueGrid } from "./key-value-grid"
import type {
  AccessibilityDefaultsDiagnostics,
  AppDiagnostics,
  LocationState,
  ThemeDiagnostics,
  WindowDiagnostics,
} from "./types"

export function OverviewTab({
  layoutClassName,
  tauriReady,
  appInfo,
  windowInfo,
  themeInfo,
  accessibilityDefaults,
  locationState,
}: {
  layoutClassName: string
  tauriReady: boolean
  appInfo: AppDiagnostics
  windowInfo: WindowDiagnostics
  themeInfo: ThemeDiagnostics
  accessibilityDefaults: AccessibilityDefaultsDiagnostics
  locationState: LocationState
}) {
  return (
    <div className={layoutClassName}>
      <DebugSection
        title="App"
        description="Static application metadata and current route."
      >
        <KeyValueGrid
          entries={[
            [
              "Route",
              locationState.pathname || "/",
              { code: "window.location.pathname", origin: "web" },
            ],
            [
              "URL",
              locationState.href || "Unavailable",
              { code: "window.location.href", origin: "web" },
            ],
            [
              "Search",
              locationState.search || "(none)",
              { code: "window.location.search", origin: "web" },
            ],
            [
              "Hash",
              locationState.hash || "(none)",
              { code: "window.location.hash", origin: "web" },
            ],
            [
              "Bridge",
              tauriReady ? "Tauri" : "Web",
              { code: "isTauri()", origin: "tauri" },
            ],
            [
              "Name",
              appInfo.name ?? "Unavailable",
              { code: "await getName()", origin: "tauri" },
            ],
            [
              "Version",
              appInfo.version ?? "Unavailable",
              { code: "await getVersion()", origin: "tauri" },
            ],
            [
              "Identifier",
              appInfo.identifier ?? "Unavailable",
              { code: "await getIdentifier()", origin: "tauri" },
            ],
            [
              "Tauri",
              appInfo.tauriVersion ?? "Unavailable",
              { code: "await getTauriVersion()", origin: "tauri" },
            ],
            [
              "Webview",
              windowInfo.label,
              { code: "getCurrentWebviewWindow().label", origin: "tauri" },
            ],
            ["Shortcut", "Cmd/Ctrl + D"],
          ]}
        />
      </DebugSection>

      <DebugSection
        title="Window"
        description="Current Tauri window and monitor state."
      >
        <KeyValueGrid
          entries={[
            [
              "Label",
              windowInfo.label,
              { code: "getCurrentWindow().label", origin: "tauri" },
            ],
            [
              "Title",
              windowInfo.title ?? "Unavailable",
              { code: "await getCurrentWindow().title()", origin: "tauri" },
            ],
            [
              "Viewport",
              windowInfo.viewportSize
                ? `${windowInfo.viewportSize.width} × ${windowInfo.viewportSize.height}`
                : "Unavailable",
              { code: "window.innerWidth × window.innerHeight", origin: "web" },
            ],
            [
              "Tauri inner",
              windowInfo.tauriInnerSize
                ? `${windowInfo.tauriInnerSize.width} × ${windowInfo.tauriInnerSize.height}`
                : "Unavailable",
              { code: "await getCurrentWindow().innerSize()", origin: "tauri" },
            ],
            [
              "Window outer",
              windowInfo.outerSize
                ? `${windowInfo.outerSize.width} × ${windowInfo.outerSize.height}`
                : "Unavailable",
              { code: "await getCurrentWindow().outerSize()", origin: "tauri" },
            ],
            [
              "Position",
              windowInfo.outerPosition
                ? `${windowInfo.outerPosition.x}, ${windowInfo.outerPosition.y}`
                : "Unavailable",
              {
                code: "await getCurrentWindow().outerPosition()",
                origin: "tauri",
              },
            ],
            [
              "Scale",
              windowInfo.scaleFactor ?? "Unavailable",
              {
                code: "await getCurrentWindow().scaleFactor()",
                origin: "tauri",
              },
            ],
            [
              "Focused",
              String(windowInfo.focused),
              { code: "await getCurrentWindow().isFocused()", origin: "tauri" },
            ],
            [
              "Visible",
              String(windowInfo.visible),
              { code: "await getCurrentWindow().isVisible()", origin: "tauri" },
            ],
            [
              "Maximized",
              String(windowInfo.maximized),
              {
                code: "await getCurrentWindow().isMaximized()",
                origin: "tauri",
              },
            ],
            [
              "Fullscreen",
              String(windowInfo.fullscreen),
              {
                code: "await getCurrentWindow().isFullscreen()",
                origin: "tauri",
              },
            ],
            [
              "Decorated",
              String(windowInfo.decorated),
              {
                code: "await getCurrentWindow().isDecorated()",
                origin: "tauri",
              },
            ],
            [
              "Monitor",
              windowInfo.monitor
                ? `${windowInfo.monitor.name ?? "Unknown"} (${windowInfo.monitor.size.width} × ${windowInfo.monitor.size.height})`
                : "Unavailable",
              { code: "await currentMonitor()", origin: "tauri" },
            ],
          ]}
        />
      </DebugSection>

      <DebugSection
        title="Theme & A11y"
        description="Theme state plus accessibility preferences from media queries."
      >
        <KeyValueGrid
          entries={[
            [
              "Current",
              themeInfo.current,
              {
                code: 'document.documentElement.classList.contains("dark")',
                origin: "web",
              },
            ],
            [
              "System",
              themeInfo.system,
              {
                code: 'matchMedia("(prefers-color-scheme: dark)")',
                origin: "web",
              },
            ],
            [
              "HTML class",
              themeInfo.htmlClass || "(none)",
              { code: "document.documentElement.className", origin: "web" },
            ],
            [
              "Color scheme",
              accessibilityDefaults.colorScheme,
              {
                code: 'matchMedia("(prefers-color-scheme: dark)")',
                origin: "web",
              },
            ],
            [
              "Reduced motion",
              accessibilityDefaults.reducedMotion,
              {
                code: 'matchMedia("(prefers-reduced-motion: reduce)")',
                origin: "web",
              },
            ],
            [
              "Contrast",
              accessibilityDefaults.contrast,
              {
                code: 'matchMedia("(prefers-contrast: more | less | custom)")',
                origin: "web",
              },
            ],
            [
              "Forced colors",
              accessibilityDefaults.forcedColors,
              { code: 'matchMedia("(forced-colors: active)")', origin: "web" },
            ],
            [
              "Inverted colors",
              accessibilityDefaults.invertedColors,
              {
                code: 'matchMedia("(inverted-colors: inverted)")',
                origin: "web",
              },
            ],
            [
              "Transparency",
              accessibilityDefaults.reducedTransparency,
              {
                code: 'matchMedia("(prefers-reduced-transparency: reduce)")',
                origin: "web",
              },
            ],
          ]}
        />
      </DebugSection>
    </div>
  )
}
