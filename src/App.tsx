import { useState } from "react"
import {
  RecordingPlayer,
  type PlaylistRecording,
} from "@/components/recording-player"
import { MomentBrowser } from "@/components/moment-browser"
import { SessionList } from "@/components/session-list"
import { TooltipProvider } from "@/components/ui/tooltip"

/** The session playlist currently open in the player, and where to start. */
interface Playing {
  recordings: PlaylistRecording[]
  startIndex: number
  /**
   * Recording-local time (ms) to open at — set when jumping to a moment from the
   * cross-session browser (issue #11); undefined for a normal session review.
   */
  startMs?: number
  /** The session's capture day, shown in the review top bar. */
  day: string
}

/**
 * The studio shell (issue #48): each screen owns its full layout — a thin top
 * bar over the content — so the app is the library (sessions homepage), the
 * cross-session moment browser (issue #11), or the review workstation.
 */
export default function App() {
  const [playing, setPlaying] = useState<Playing | null>(null)
  const [browsing, setBrowsing] = useState(false)

  return (
    <TooltipProvider>
      <div className="h-svh overflow-hidden bg-background text-foreground">
        {playing ? (
          <RecordingPlayer
            recordings={playing.recordings}
            startIndex={playing.startIndex}
            startMs={playing.startMs}
            day={playing.day}
            onBack={() => setPlaying(null)}
          />
        ) : browsing ? (
          <MomentBrowser
            onBack={() => setBrowsing(false)}
            onJump={(target) => {
              setBrowsing(false)
              setPlaying(target)
            }}
          />
        ) : (
          <SessionList
            onBrowse={() => setBrowsing(true)}
            onPlay={(recordings, startIndex, day) =>
              setPlaying({ recordings, startIndex, day })
            }
          />
        )}
      </div>
    </TooltipProvider>
  )
}
