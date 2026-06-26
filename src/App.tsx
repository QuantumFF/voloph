import { useState, type CSSProperties } from "react"
import { AppSidebar } from "@/components/app-sidebar"
import {
  RecordingPlayer,
  type PlaylistRecording,
} from "@/components/recording-player"
import { SessionList } from "@/components/session-list"
import { SiteHeader } from "@/components/site-header"
import { SidebarInset, SidebarProvider } from "@/components/ui/sidebar"
import { TooltipProvider } from "@/components/ui/tooltip"

/** The session playlist currently open in the player, and where to start. */
interface Playing {
  recordings: PlaylistRecording[]
  startIndex: number
}

export default function App() {
  const [playing, setPlaying] = useState<Playing | null>(null)

  return (
    <TooltipProvider>
      <SidebarProvider
        style={
          {
            "--sidebar-width": "calc(var(--spacing) * 72)",
            "--header-height": "calc(var(--spacing) * 12)",
          } as CSSProperties
        }
      >
        <AppSidebar variant="inset" />
        {/* Bound the inset to the viewport so a full-height player fits without
            page scroll; each branch below owns its own overflow (the player
            fills, the list scrolls). */}
        <SidebarInset className="h-svh overflow-hidden md:h-[calc(100svh-1rem)]">
          <SiteHeader />
          <div className="flex min-h-0 flex-1 flex-col">
            <div className="@container/main flex min-h-0 flex-1 flex-col gap-2 pb-10">
              {playing ? (
                // The player fills the available height — header, video and
                // timeline are laid out as one always-visible column.
                <div className="flex min-h-0 flex-1 flex-col px-4 py-4 md:py-6 lg:px-6">
                  <RecordingPlayer
                    recordings={playing.recordings}
                    startIndex={playing.startIndex}
                    onBack={() => setPlaying(null)}
                  />
                </div>
              ) : (
                <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto py-4 md:gap-6 md:py-6">
                  <div className="px-4 lg:px-6">
                    <SessionList
                      onPlay={(recordings, startIndex) =>
                        setPlaying({ recordings, startIndex })
                      }
                    />
                  </div>
                </div>
              )}
            </div>
          </div>
        </SidebarInset>
      </SidebarProvider>
    </TooltipProvider>
  )
}
