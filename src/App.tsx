import { useState } from "react"
import {
  RecordingPlayer,
  type PlaylistRecording,
} from "@/components/recording-player"
import { SessionList } from "@/components/session-list"
import { TooltipProvider } from "@/components/ui/tooltip"

/** The session playlist currently open in the player, and where to start. */
interface Playing {
  recordings: PlaylistRecording[]
  startIndex: number
  /** The session's capture day, shown in the review top bar. */
  day: string
}

/**
 * The studio shell (issue #48): each screen owns its full layout — a thin top
 * bar over the content — so the app is either the library (sessions homepage)
 * or the review workstation, edge to edge.
 */
export default function App() {
  const [playing, setPlaying] = useState<Playing | null>(null)

  return (
    <TooltipProvider>
      <div className="h-svh overflow-hidden bg-background text-foreground">
        {playing ? (
          <RecordingPlayer
            recordings={playing.recordings}
            startIndex={playing.startIndex}
            day={playing.day}
            onBack={() => setPlaying(null)}
          />
        ) : (
          <SessionList
            onPlay={(recordings, startIndex, day) =>
              setPlaying({ recordings, startIndex, day })
            }
          />
        )}
      </div>
    </TooltipProvider>
  )
}
