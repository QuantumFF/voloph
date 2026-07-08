import { CopyIcon } from "lucide-react"

import { Button } from "@/components/ui/button"

import { DebugSection } from "./debug-section"
import { GridLabel, KeyValueGrid } from "./key-value-grid"
import type {
  DisplayDefaultsDiagnostics,
  InputDefaultsDiagnostics,
  LocaleDefaultsDiagnostics,
  NetworkDefaultsDiagnostics,
  PathDiagnostics,
} from "./types"

export function SystemTab({
  layoutClassName,
  pathInfo,
  displayDefaults,
  localeDefaults,
  inputDefaults,
  networkDefaults,
  onCopyText,
}: {
  layoutClassName: string
  pathInfo: PathDiagnostics
  displayDefaults: DisplayDefaultsDiagnostics
  localeDefaults: LocaleDefaultsDiagnostics
  inputDefaults: InputDefaultsDiagnostics
  networkDefaults: NetworkDefaultsDiagnostics
  onCopyText: (value: string) => void
}) {
  return (
    <div className={layoutClassName}>
      <DebugSection
        title="Paths"
        description="Resolved Tauri application directories."
      >
        <div className="space-y-1.5">
          {(
            [
              ["App data", pathInfo.appDataDir, "await appDataDir()"],
              ["App config", pathInfo.appConfigDir, "await appConfigDir()"],
              ["Resources", pathInfo.resourceDir, "await resourceDir()"],
            ] as const
          ).map(([label, value, code]) => (
            <div
              key={label}
              className="grid grid-cols-[92px_minmax(0,1fr)_auto] items-start gap-3 border-t border-border/20 py-1 text-[11px] first:border-t-0 first:pt-0"
            >
              <div className="text-muted-foreground">
                <GridLabel label={label} meta={{ code, origin: "tauri" }} />
              </div>
              <div className="font-mono break-all text-foreground">
                {value ?? "Unavailable"}
              </div>
              <Button
                variant="ghost"
                size="sm"
                disabled={!value}
                onClick={() => value && onCopyText(value)}
              >
                <CopyIcon className="size-3.5" />
                Copy
              </Button>
            </div>
          ))}
        </div>
      </DebugSection>

      <DebugSection
        title="Display"
        description="Viewport, screen, and media capability signals."
      >
        <KeyValueGrid
          entries={[
            [
              "Viewport",
              displayDefaults.viewport,
              { code: "window.innerWidth × window.innerHeight", origin: "web" },
            ],
            [
              "Screen",
              displayDefaults.screen,
              {
                code: "window.screen.width × window.screen.height",
                origin: "web",
              },
            ],
            [
              "Available",
              displayDefaults.availableScreen,
              {
                code: "window.screen.availWidth × window.screen.availHeight",
                origin: "web",
              },
            ],
            [
              "DPR",
              displayDefaults.devicePixelRatio,
              { code: "window.devicePixelRatio", origin: "web" },
            ],
            [
              "Orientation",
              displayDefaults.orientation,
              { code: "window.screen.orientation?.type", origin: "web" },
            ],
            [
              "Color gamut",
              displayDefaults.colorGamut,
              {
                code: 'matchMedia("(color-gamut: rec2020 | p3 | srgb)")',
                origin: "web",
              },
            ],
            [
              "Dynamic range",
              displayDefaults.dynamicRange,
              { code: 'matchMedia("(dynamic-range: high)")', origin: "web" },
            ],
            [
              "Monochrome",
              String(displayDefaults.monochrome),
              { code: 'matchMedia("(monochrome)")', origin: "web" },
            ],
          ]}
        />
      </DebugSection>

      <DebugSection
        title="Locale"
        description="Host locale, timezone, calendar, and Intl formatting defaults."
      >
        <KeyValueGrid
          entries={[
            [
              "Locale chain",
              localeDefaults.localeChain.length
                ? localeDefaults.localeChain.join(", ")
                : "Unavailable",
              {
                code: "[...navigator.languages, navigator.language]",
                origin: "web",
              },
            ],
            [
              "Navigator",
              localeDefaults.navigatorLanguage,
              { code: "navigator.language", origin: "web" },
            ],
            [
              "Intl locale",
              localeDefaults.intlLocale,
              {
                code: "new Intl.DateTimeFormat().resolvedOptions().locale",
                origin: "web",
              },
            ],
            [
              "Region",
              localeDefaults.region ?? "Unavailable",
              { code: "new Intl.Locale(locale).region", origin: "web" },
            ],
            [
              "Time zone",
              localeDefaults.timeZone,
              {
                code: "new Intl.DateTimeFormat().resolvedOptions().timeZone",
                origin: "web",
              },
            ],
            [
              "Calendar",
              localeDefaults.calendar,
              {
                code: "new Intl.DateTimeFormat().resolvedOptions().calendar",
                origin: "web",
              },
            ],
            [
              "Numbering",
              localeDefaults.numberingSystem,
              {
                code: "new Intl.DateTimeFormat().resolvedOptions().numberingSystem",
                origin: "web",
              },
            ],
            [
              "Hour cycle",
              localeDefaults.hourCycle,
              {
                code: "new Intl.DateTimeFormat().resolvedOptions().hourCycle",
                origin: "web",
              },
            ],
            [
              "12-hour",
              localeDefaults.hour12 === null
                ? "Unavailable"
                : String(localeDefaults.hour12),
              {
                code: "new Intl.DateTimeFormat().resolvedOptions().hour12",
                origin: "web",
              },
            ],
            [
              "First day",
              localeDefaults.firstDayOfWeek,
              {
                code: "new Intl.Locale(locale).weekInfo?.firstDay",
                origin: "web",
              },
            ],
            [
              "HTML lang",
              localeDefaults.documentLang,
              { code: "document.documentElement.lang", origin: "web" },
            ],
            [
              "HTML dir",
              localeDefaults.documentDir,
              { code: "document.documentElement.dir", origin: "web" },
            ],
          ]}
        />
      </DebugSection>

      <DebugSection
        title="Input"
        description="Pointer, touch, hover, and hardware defaults."
      >
        <KeyValueGrid
          entries={[
            [
              "Platform",
              inputDefaults.platform,
              {
                code: "navigator.userAgentData?.platform ?? navigator.platform",
                origin: "web",
              },
            ],
            [
              "Pointer",
              inputDefaults.pointer,
              { code: 'matchMedia("(pointer: fine | coarse)")', origin: "web" },
            ],
            [
              "Any pointer",
              inputDefaults.anyPointer,
              {
                code: 'matchMedia("(any-pointer: fine | coarse)")',
                origin: "web",
              },
            ],
            [
              "Hover",
              String(inputDefaults.hover),
              { code: 'matchMedia("(hover: hover)")', origin: "web" },
            ],
            [
              "Any hover",
              String(inputDefaults.anyHover),
              { code: 'matchMedia("(any-hover: hover)")', origin: "web" },
            ],
            [
              "Touch points",
              inputDefaults.maxTouchPoints,
              { code: "navigator.maxTouchPoints", origin: "web" },
            ],
            [
              "Touch capable",
              String(inputDefaults.touchCapable),
              {
                code: 'maxTouchPoints > 0 || matchMedia("(pointer: coarse)") || "ontouchstart" in window',
                origin: "web",
              },
            ],
            [
              "CPU threads",
              inputDefaults.hardwareConcurrency ?? "Unavailable",
              { code: "navigator.hardwareConcurrency", origin: "web" },
            ],
            [
              "Device memory",
              inputDefaults.deviceMemory === null
                ? "Unavailable"
                : `${inputDefaults.deviceMemory} GB`,
              { code: "navigator.deviceMemory", origin: "web" },
            ],
            [
              "User agent",
              inputDefaults.userAgent,
              { code: "navigator.userAgent", origin: "web" },
            ],
          ]}
        />
      </DebugSection>

      <DebugSection
        title="Network"
        description="Online state and connection quality hints from the browser."
      >
        <KeyValueGrid
          entries={[
            [
              "Online",
              String(networkDefaults.online),
              { code: "navigator.onLine", origin: "web" },
            ],
            [
              "Effective type",
              networkDefaults.effectiveType,
              { code: "navigator.connection?.effectiveType", origin: "web" },
            ],
            [
              "Save-Data",
              networkDefaults.saveData === null
                ? "Unavailable"
                : String(networkDefaults.saveData),
              { code: "navigator.connection?.saveData", origin: "web" },
            ],
            [
              "Downlink",
              networkDefaults.downlink === null
                ? "Unavailable"
                : `${networkDefaults.downlink} Mbps`,
              { code: "navigator.connection?.downlink", origin: "web" },
            ],
            [
              "RTT",
              networkDefaults.rtt === null
                ? "Unavailable"
                : `${networkDefaults.rtt} ms`,
              { code: "navigator.connection?.rtt", origin: "web" },
            ],
          ]}
        />
      </DebugSection>
    </div>
  )
}
