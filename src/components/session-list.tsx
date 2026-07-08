"use client"

import { useCallback, useEffect, useState } from "react"
import { open, save } from "@tauri-apps/plugin-dialog"
import {
  AlertTriangleIcon,
  ClapperboardIcon,
  DownloadIcon,
  FilterIcon,
  FolderOpenIcon,
  Loader2Icon,
  MoreVerticalIcon,
  PlayIcon,
  RefreshCwIcon,
  RotateCwIcon,
  Share2Icon,
  VideoIcon,
} from "lucide-react"

import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog"
import { Button, buttonVariants } from "@/components/ui/button"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import { fileName, formatDuration, formatSize } from "@/lib/format"
import { trackedInvoke } from "@/lib/tauri"
import { formatCaptureDay } from "@/lib/utils"

interface Recording {
  id: number
  path: string
  file_size: number
  quick_hash: string
  capture_day: string
  /**
   * Playability lifecycle: unknown (not yet probed) | ready | failed. libmpv
   * plays originals directly (ADR 0008), so there is no transcode step.
   */
  probe_state: string
  /** Segmentation lifecycle: unknown | ready | failed (ADR 0002). */
  segment_state: string
  /** Recording duration in ms; null until segmented. */
  duration_ms: number | null
  /** Rallies in the draft timeline (0 until segmented). */
  rally_count: number
}

/** True during the brief window before a recording has been probed for playback. */
function isPreparing(state: string): boolean {
  return state === "unknown"
}

/**
 * True while a recording is playable but its draft timeline is still being
 * produced — audio extraction + segmentation (ADR 0002). Segmentation only
 * starts once the recording is probed (`ready`), so a still-`unknown` segment
 * state on a ready recording means "queued or analyzing".
 */
function isAnalyzing(recording: Recording): boolean {
  return (
    recording.probe_state === "ready" && recording.segment_state === "unknown"
  )
}

/** True while any background media work is still pending for this recording. */
function isProcessing(recording: Recording): boolean {
  return isPreparing(recording.probe_state) || isAnalyzing(recording)
}

interface Session {
  id: number
  capture_day: string
  recordings: Recording[]
}

/** A designated library and how this device reaches it (ADR 0011). */
interface Library {
  /** "local" | "shared". */
  kind: string
  /** Where the library is mounted on this device (per-device, never shared). */
  path: string
  /** Declared locality of that mount: "local" | "network". */
  mount: string
}

/** Human label for a library kind, for the switcher and buttons. */
function kindLabel(kind: string): string {
  return kind === "shared" ? "Shared" : "Local"
}

/**
 * A cross-library carry-over offer (ADR 0011): the same content exists in both
 * libraries (a copy) and exactly one side has hand-touched review state. The app
 * offers — never silently — to carry that review to the other, un-touched copy.
 */
interface CarryOffer {
  /** Absolute path of the copy that has the review. */
  from_path: string
  /** Absolute path of the copy that would receive it. */
  to_path: string
  /** Library kind ("local" | "shared") of the receiving copy. */
  to_kind: string
}

/**
 * A shared review discovered in the shared library (ADR 0012, issue #67): a
 * bundle another person dropped in, offered by session + sharer label before
 * analysis runs on the recordings it covers. Accepting runs the receive flow;
 * declining stops it nagging until the sharer re-shares it (`is_update`).
 */
interface BundleOffer {
  /** Absolute path of the `.vbundle` on this device. */
  bundle_path: string
  capture_day: string
  sharer_label: string
  /** A changed re-share of a bundle the user previously declined. */
  is_update: boolean
}

/**
 * The outcome of receiving a session bundle (ADR 0012, issue #66). `applied` is
 * the count taken silently (registered fresh or replacing machine-only state);
 * `refused` names files that failed verification; `conflicts` are the library-
 * relative paths of hand-touched recordings awaiting a keep-mine-or-take-theirs
 * choice — nothing changed for them until the user decides.
 */
interface ReceiveResult {
  applied: number
  refused: { path: string; reason: string }[]
  conflicts: string[]
}

interface ScanResult {
  registered: number
  skipped: number
  /** Known recordings re-linked after being moved/renamed inside the library. */
  relocated: number
  /**
   * Absolute paths of known recordings no longer found under the library after a
   * scan (ADR 0011). Their review state is retained, not deleted; one that
   * reappears (same hash + size) re-links on a later scan.
   */
  unresolved: string[]
}

/** The stats line under a session's date: recordings, rallies, footage length. */
function sessionSummary(session: Session): string {
  const parts = [
    `${session.recordings.length} recording${session.recordings.length === 1 ? "" : "s"}`,
  ]
  const segmented = session.recordings.filter(
    (r) => r.segment_state === "ready"
  )
  if (segmented.length > 0) {
    const rallies = segmented.reduce((sum, r) => sum + r.rally_count, 0)
    parts.push(`${rallies} ${rallies === 1 ? "rally" : "rallies"}`)
  }
  const durationMs = session.recordings.reduce(
    (sum, r) => sum + (r.duration_ms ?? 0),
    0
  )
  if (durationMs > 0) parts.push(formatDuration(durationMs))
  return parts.join(" · ")
}

interface SessionListProps {
  /**
   * Open a session in the player as one continuous playlist. `recordings` is the
   * session's recordings in capture-time order; `startIndex` is the one to open
   * first (which recording the user clicked); `day` is the session's capture day
   * for the review top bar.
   */
  onPlay: (
    recordings: { path: string }[],
    startIndex: number,
    day: string
  ) => void
  /** Open the cross-session moment browser (issue #11). */
  onBrowse: () => void
}

/**
 * The homepage: the library of sessions in the studio shell (issue #48) — a
 * thin top bar carrying the app identity and the library actions, over a
 * centered column of session blocks. Each block is one session: its date and
 * stats, a Review button that opens the whole session in the workstation, and
 * the recordings it holds as dense rows.
 */
export function SessionList({ onPlay, onBrowse }: SessionListProps) {
  const [sessions, setSessions] = useState<Session[]>([])
  // The designated libraries (ADR 0011) and which kind the switcher has active.
  // At most one of each kind; the session list, filters, and review scope to the
  // active one. The whole app's world of recordings is the active library.
  const [libraries, setLibraries] = useState<Library[]>([])
  const [active, setActive] = useState<string>("local")
  const [scanning, setScanning] = useState(false)
  const [refreshing, setRefreshing] = useState(false)
  const [reanalyzingAll, setReanalyzingAll] = useState(false)
  // Which bulk action is awaiting confirmation in the dialog, if any.
  const [confirmAction, setConfirmAction] = useState<"reanalyze" | null>(null)
  const [error, setError] = useState<string | null>(null)
  // Known recordings the last scan could not find under the library (ADR 0011).
  // Retained in the DB with their review state; listed here so the user can put
  // them back (they re-link automatically) rather than losing the work silently.
  const [unresolved, setUnresolved] = useState<string[]>([])
  // Cross-library carry-over offers (ADR 0011): the same content sits in both
  // libraries and only one side has hand-touched review. The app offers — never
  // silently — to carry that review to the other copy; the user accepts or declines
  // per offer, and declining leaves both sides untouched.
  const [carryOffers, setCarryOffers] = useState<CarryOffer[]>([])
  // Shared reviews discovered in the shared library (ADR 0012, issue #67): bundles
  // other people dropped in, offered by session + sharer before analysis runs on
  // the recordings they cover. Accepting receives; declining stops the nagging
  // until the bundle changes.
  const [bundleOffers, setBundleOffers] = useState<BundleOffer[]>([])
  // The name this device signs its shared bundles with (ADR 0012), persisted in
  // the DB so the user names themselves only once. Empty until they do.
  const [sharerLabel, setSharerLabel] = useState<string>("")
  // Which session's share is awaiting the name dialog, and whether that share is
  // the "save elsewhere" fallback rather than a write into the shared library.
  const [shareTarget, setShareTarget] = useState<{
    session: Session
    saveAs: boolean
  } | null>(null)
  // Draft name in the share dialog, seeded from the persisted label.
  const [shareName, setShareName] = useState<string>("")
  // One-line confirmation after a successful share, cleared on the next action.
  const [shareNote, setShareNote] = useState<string | null>(null)
  // The bundle file currently being received (ADR 0012): its path, the outcome
  // of the receive, and which hand-touched recordings still need a keep-mine-or-
  // take-theirs choice. Null until the user opens a bundle.
  const [receiving, setReceiving] = useState<{
    bundlePath: string
    result: ReceiveResult
  } | null>(null)

  const refresh = useCallback(async () => {
    try {
      const [next, state, offers, label, bundles] = await Promise.all([
        trackedInvoke<Session[]>("list_sessions"),
        trackedInvoke<[Library[], string]>("library_state"),
        trackedInvoke<CarryOffer[]>("carry_offers"),
        trackedInvoke<string | null>("sharer_label"),
        trackedInvoke<BundleOffer[]>("discover_bundles"),
      ])
      setSessions(next)
      setLibraries(state[0])
      setActive(state[1])
      setCarryOffers(offers)
      setSharerLabel(label ?? "")
      setBundleOffers(bundles)
    } catch (e) {
      setError(String(e))
    }
  }, [])

  // Accept an offer: carry the review to the other copy, then refresh (the offer
  // disappears once both sides match). Declining is just dismissing the row.
  async function handleCarry(offer: CarryOffer) {
    setError(null)
    try {
      await trackedInvoke("carry_review", {
        fromPath: offer.from_path,
        toPath: offer.to_path,
      })
      await refresh()
    } catch (e) {
      setError(String(e))
    }
  }

  function handleDecline(offer: CarryOffer) {
    setCarryOffers((prev) => prev.filter((o) => o !== offer))
  }

  // Accept a discovered bundle offer (ADR 0012, issue #67): run the receive flow
  // on it, which registers the covered recordings straight from the bundle (no
  // probe/segmentation/staging). Surfaces the same conflict/refusal resolution
  // dialog as a manually-opened bundle when anything needs the user's choice.
  async function handleReceiveOffer(offer: BundleOffer) {
    setError(null)
    setShareNote(null)
    try {
      const result = await trackedInvoke<ReceiveResult>(
        "receive_session_bundle",
        { bundlePath: offer.bundle_path }
      )
      await refresh()
      if (result.conflicts.length > 0 || result.refused.length > 0) {
        setReceiving({ bundlePath: offer.bundle_path, result })
      } else {
        setShareNote(
          `Received ${result.applied} recording${result.applied === 1 ? "" : "s"}.`
        )
      }
    } catch (e) {
      setError(String(e))
    }
  }

  // Decline a discovered bundle offer (ADR 0012, issue #67): record it so it
  // stops being offered until the sharer re-shares it, and release the recordings
  // it held back to the analysis queue. Refresh so the offer disappears.
  async function handleDeclineOffer(offer: BundleOffer) {
    setError(null)
    try {
      await trackedInvoke("decline_bundle", { bundlePath: offer.bundle_path })
      await refresh()
    } catch (e) {
      setError(String(e))
    }
  }

  // The active library's own record (mount path + locality), or undefined until
  // its kind is designated on this device.
  const activeLibrary = libraries.find((l) => l.kind === active)
  const library = activeLibrary?.path ?? null

  useEffect(() => {
    // Load persisted sessions once on mount. The setState lands after an
    // awaited round-trip to Rust, not synchronously within the effect body.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    void refresh()
  }, [refresh])

  // While any recording is still being prepared or segmented in the
  // background, poll so the row flips from "Preparing…"/"Analyzing…" to its
  // rally count once the draft timeline is ready. Keyed on the derived boolean
  // (not `sessions`, a fresh array each poll) so the interval survives across
  // polls instead of being torn down and re-created every tick.
  const stillWorking = sessions.some((session) =>
    session.recordings.some((recording) => isProcessing(recording))
  )
  useEffect(() => {
    if (!stillWorking) return
    const interval = setInterval(() => void refresh(), 3000)
    return () => clearInterval(interval)
  }, [stillWorking, refresh])

  // Designate (or re-designate) a library of `kind` ("local" | "shared") with the
  // folder where it is mounted here and its declared `mount` locality ("local" |
  // "network"; ADR 0011). Adopts every known recording of this kind under it to
  // library-relative identity with its review state intact, then scans it so new
  // files appear. The designated kind becomes active.
  async function handleDesignateLibrary(kind: string, mount: string) {
    setError(null)
    const folder = await open({ directory: true, multiple: false })
    if (typeof folder !== "string") return

    setScanning(true)
    try {
      const result = await trackedInvoke<ScanResult>("designate_library", {
        kind,
        folder,
        mount,
      })
      setUnresolved(result.unresolved)
      await refresh()
    } catch (e) {
      setError(String(e))
    } finally {
      setScanning(false)
    }
  }

  // Switch the active library (ADR 0011). The session list, filters, and review
  // scope to it; switching back and forth loses nothing. Re-scans the newly active
  // library so files added since last time appear.
  async function handleSwitch(kind: string) {
    if (kind === active) return
    setError(null)
    setUnresolved([])
    try {
      const result = await trackedInvoke<ScanResult>("switch_library", { kind })
      setUnresolved(result.unresolved)
      await refresh()
    } catch (e) {
      setError(String(e))
    }
  }

  // Re-walk the library for recordings added to it since the last scan, without
  // re-picking the folder. New recordings flow through the same import pipeline.
  async function handleRefresh() {
    setError(null)
    setRefreshing(true)
    try {
      const result = await trackedInvoke<ScanResult>("rescan_library")
      setUnresolved(result.unresolved)
      await refresh()
    } catch (e) {
      setError(String(e))
    } finally {
      setRefreshing(false)
    }
  }

  // Re-detect rallies for every recording. Discards every draft timeline,
  // including manual corrections, so it is confirmed through the dialog first.
  async function runReanalyzeAll() {
    setError(null)
    setReanalyzingAll(true)
    try {
      await trackedInvoke("reanalyze_all")
      await refresh()
    } catch (e) {
      setError(String(e))
    } finally {
      setReanalyzingAll(false)
    }
  }

  // Run whichever bulk action the confirmation dialog is open for, then close it.
  function handleConfirm() {
    if (confirmAction === "reanalyze") void runReanalyzeAll()
    setConfirmAction(null)
  }

  // Per-recording re-analyze: re-run rally detection for one recording in place
  // (discards its draft timeline). Mirrors the player's Re-analyze action.
  async function handleReanalyze(path: string) {
    setError(null)
    try {
      await trackedInvoke("reanalyze_recording", { path })
      await refresh()
    } catch (e) {
      setError(String(e))
    }
  }

  // Open the share dialog for a session (ADR 0012). `saveAs` picks the fallback
  // path (write the same artifact anywhere) over writing into the shared library.
  // Seeds the name field with the persisted label so the user confirms it once.
  function openShare(session: Session, saveAs: boolean) {
    setError(null)
    setShareNote(null)
    setShareName(sharerLabel)
    setShareTarget({ session, saveAs })
  }

  // Confirm a share: write the metadata-only bundle either into the shared
  // library (keyed by session + name, overwriting this sharer's own previous
  // bundle) or to a user-chosen path. The name is remembered for next time.
  async function confirmShare() {
    if (!shareTarget) return
    const name = shareName.trim()
    if (!name) return
    const { session, saveAs } = shareTarget
    setShareTarget(null)
    setError(null)
    try {
      if (saveAs) {
        const output = await save({
          title: "Save session bundle",
          defaultPath: `${session.capture_day}__${name}.vbundle`,
          filters: [{ name: "Voloph bundle", extensions: ["vbundle"] }],
        })
        if (!output) return
        await trackedInvoke("save_session_bundle_as", {
          sessionId: session.id,
          sharerLabel: name,
          output,
        })
        setShareNote("Bundle saved.")
      } else {
        await trackedInvoke("share_session_bundle", {
          sessionId: session.id,
          sharerLabel: name,
        })
        setShareNote("Shared to the shared library.")
      }
      setSharerLabel(name)
    } catch (e) {
      setError(String(e))
    }
  }

  // Receive a bundle (ADR 0012): pick a .vbundle, apply its review against the
  // shared library. Unknown recordings register straight from it (self-sufficient);
  // machine-only state is replaced silently; hand-touched recordings surface as
  // keep-mine-or-take-theirs conflicts. Refreshes so applied state appears at once.
  async function handleReceive() {
    setError(null)
    setShareNote(null)
    try {
      const picked = await open({
        multiple: false,
        filters: [{ name: "Voloph bundle", extensions: ["vbundle"] }],
      })
      if (typeof picked !== "string") return
      const result = await trackedInvoke<ReceiveResult>(
        "receive_session_bundle",
        { bundlePath: picked }
      )
      await refresh()
      if (result.conflicts.length > 0 || result.refused.length > 0) {
        setReceiving({ bundlePath: picked, result })
      } else {
        setShareNote(
          `Received ${result.applied} recording${result.applied === 1 ? "" : "s"}.`
        )
      }
    } catch (e) {
      setError(String(e))
    }
  }

  // Resolve one keep-mine-or-take-theirs conflict (ADR 0012), whole-recording:
  // take-theirs replaces the recipient's review with the bundle's; keep-mine
  // dismisses. Drops the row from the pending list either way; closes the dialog
  // once none remain.
  async function resolveConflict(path: string, takeTheirs: boolean) {
    if (!receiving) return
    try {
      if (takeTheirs) {
        await trackedInvoke("resolve_bundle_conflict", {
          bundlePath: receiving.bundlePath,
          path,
          takeTheirs: true,
        })
        await refresh()
      }
      setReceiving((prev) => {
        if (!prev) return prev
        const conflicts = prev.result.conflicts.filter((c) => c !== path)
        if (conflicts.length === 0 && prev.result.refused.length === 0) {
          return null
        }
        return { ...prev, result: { ...prev.result, conflicts } }
      })
    } catch (e) {
      setError(String(e))
    }
  }

  const confirmCopy = {
    reanalyze: {
      title: "Re-analyze all recordings?",
      description:
        "This re-detects rallies in every recording and discards every draft timeline — including any manual corrections you have made.",
      action: "Re-analyze all",
      destructive: true,
    },
  } as const
  const copy = confirmAction ? confirmCopy[confirmAction] : null

  return (
    <div className="flex h-full flex-col">
      <AlertDialog
        open={confirmAction !== null}
        onOpenChange={(o) => {
          if (!o) setConfirmAction(null)
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{copy?.title}</AlertDialogTitle>
            <AlertDialogDescription>{copy?.description}</AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={handleConfirm}
              className={
                copy?.destructive
                  ? buttonVariants({ variant: "destructive" })
                  : undefined
              }
            >
              {copy?.action}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      {/* Share dialog (ADR 0012): name yourself once, then write a metadata-only
          bundle of the session's review. Re-sharing overwrites only your own
          bundle. Shown for both "share into shared library" and "save as". */}
      <AlertDialog
        open={shareTarget !== null}
        onOpenChange={(o) => {
          if (!o) setShareTarget(null)
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>
              {shareTarget?.saveAs ? "Save session bundle" : "Share session"}
            </AlertDialogTitle>
            <AlertDialogDescription>
              Share your review of{" "}
              {shareTarget
                ? formatCaptureDay(shareTarget.session.capture_day)
                : ""}{" "}
              as a metadata-only bundle. No video is copied. Enter the name to
              share under — re-sharing this session later overwrites only your own
              bundle.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <input
            autoFocus
            value={shareName}
            onChange={(e) => setShareName(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && shareName.trim()) void confirmShare()
            }}
            placeholder="Your name"
            className="w-full rounded-md border bg-transparent px-3 py-2 text-sm outline-none focus-visible:ring-1 focus-visible:ring-ring"
          />
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={confirmShare}
              disabled={!shareName.trim()}
            >
              {shareTarget?.saveAs ? "Save" : "Share"}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      {/* Receive resolution (ADR 0012): after a bundle is received, name any
          files that failed verification and let the user choose keep-mine or
          take-theirs per hand-touched recording. Whole-recording granularity —
          nothing merges. Closing dismisses any unresolved conflicts (keep-mine). */}
      <AlertDialog
        open={receiving !== null}
        onOpenChange={(o) => {
          if (!o) setReceiving(null)
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Received bundle</AlertDialogTitle>
            <AlertDialogDescription>
              Applied {receiving?.result.applied ?? 0} recording
              {receiving?.result.applied === 1 ? "" : "s"}.
              {receiving && receiving.result.conflicts.length > 0
                ? " Choose which review to keep for the recordings you have already edited."
                : ""}
            </AlertDialogDescription>
          </AlertDialogHeader>
          {receiving && receiving.result.refused.length > 0 ? (
            <div className="rounded-md border border-amber-500/50 bg-amber-500/5 px-3 py-2 text-sm">
              <div className="flex items-center gap-2 font-medium text-amber-700 dark:text-amber-500">
                <AlertTriangleIcon className="size-4" />
                {receiving.result.refused.length} recording
                {receiving.result.refused.length === 1 ? "" : "s"} refused
              </div>
              <ul className="mt-1 space-y-0.5 text-muted-foreground">
                {receiving.result.refused.map((r) => (
                  <li key={r.path} className="truncate" title={r.reason}>
                    {fileName(r.path)}
                  </li>
                ))}
              </ul>
            </div>
          ) : null}
          {receiving && receiving.result.conflicts.length > 0 ? (
            <ul className="space-y-2">
              {receiving.result.conflicts.map((path) => (
                <li
                  key={path}
                  className="flex items-center gap-2 rounded-md border px-3 py-2 text-sm"
                >
                  <span className="min-w-0 flex-1 truncate" title={path}>
                    {fileName(path)}
                  </span>
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={() => void resolveConflict(path, false)}
                  >
                    Keep mine
                  </Button>
                  <Button
                    size="sm"
                    onClick={() => void resolveConflict(path, true)}
                  >
                    Take theirs
                  </Button>
                </li>
              ))}
            </ul>
          ) : null}
          <AlertDialogFooter>
            <AlertDialogCancel>Done</AlertDialogCancel>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      <header className="flex h-11 shrink-0 items-center gap-2.5 border-b px-4">
        <ClapperboardIcon className="size-5" />
        <span className="text-sm font-semibold">Voloph</span>
        <span className="text-xs text-muted-foreground">
          Every rally, no downtime
        </span>
        <div className="ml-auto flex items-center gap-2">
          {/* Library switcher (ADR 0011): pick the active library when more than
              one is designated. Each button scopes the whole app to its kind. */}
          {libraries.length > 1 ? (
            <div className="flex overflow-hidden rounded-md border">
              {libraries.map((lib) => (
                <button
                  key={lib.kind}
                  type="button"
                  onClick={() => void handleSwitch(lib.kind)}
                  title={`${kindLabel(lib.kind)} library — ${lib.path}`}
                  className={`px-2.5 py-1 text-xs font-medium ${
                    lib.kind === active
                      ? "bg-primary text-primary-foreground"
                      : "text-muted-foreground hover:bg-accent"
                  }`}
                >
                  {kindLabel(lib.kind)}
                </button>
              ))}
            </div>
          ) : null}
          <Button
            variant="outline"
            size="sm"
            onClick={onBrowse}
            disabled={sessions.length === 0}
            title="Filter moments across every session by verdict, aspect, rally length, and flag."
          >
            <FilterIcon className="size-4" />
            Browse moments
          </Button>
          <Button
            variant="outline"
            size="sm"
            onClick={handleRefresh}
            disabled={refreshing || library === null}
            title="Re-scan the active library for newly added recordings."
          >
            <RefreshCwIcon
              className={`size-4 ${refreshing ? "animate-spin" : ""}`}
            />
            {refreshing ? "Refreshing…" : "Refresh"}
          </Button>
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button size="sm" disabled={scanning}>
                <FolderOpenIcon className="size-4" />
                {scanning ? "Scanning…" : "Libraries"}
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end" className="w-64">
              <DropdownMenuLabel>Local library</DropdownMenuLabel>
              <DropdownMenuItem
                onClick={() => void handleDesignateLibrary("local", "local")}
              >
                <FolderOpenIcon className="size-4" />
                {libraries.some((l) => l.kind === "local")
                  ? "Change local library"
                  : "Designate local library"}
              </DropdownMenuItem>
              <DropdownMenuLabel>Shared library</DropdownMenuLabel>
              {/* The user declares whether this device reaches the shared mount as
                  a local disk or over the network — an explicit choice, never
                  filesystem detection (ADR 0011). */}
              <DropdownMenuItem
                onClick={() => void handleDesignateLibrary("shared", "local")}
              >
                <FolderOpenIcon className="size-4" />
                Designate shared (local mount)
              </DropdownMenuItem>
              <DropdownMenuItem
                onClick={() => void handleDesignateLibrary("shared", "network")}
              >
                <FolderOpenIcon className="size-4" />
                Designate shared (network mount)
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button
                variant="outline"
                size="icon-sm"
                title="More library actions"
              >
                <MoreVerticalIcon className="size-4" />
                <span className="sr-only">More library actions</span>
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end" className="w-44">
              <DropdownMenuLabel>Session bundle</DropdownMenuLabel>
              {/* Receive a shared review (ADR 0012): only meaningful against the
                  shared library, where bundles live and resolve. */}
              <DropdownMenuItem
                onClick={() => void handleReceive()}
                disabled={active !== "shared"}
                className="whitespace-nowrap"
              >
                <DownloadIcon className="size-4" />
                Receive bundle…
              </DropdownMenuItem>
              <DropdownMenuLabel>All recordings</DropdownMenuLabel>
              <DropdownMenuItem
                onClick={() => setConfirmAction("reanalyze")}
                disabled={reanalyzingAll}
                className="whitespace-nowrap"
              >
                <RotateCwIcon className="size-4" />
                Re-analyze all
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      </header>

      <div className="min-h-0 flex-1 overflow-y-auto">
        <div className="mx-auto max-w-4xl space-y-4 px-4 py-6">
          {error ? <p className="text-sm text-destructive">{error}</p> : null}
          {shareNote ? (
            <p className="text-sm text-muted-foreground">{shareNote}</p>
          ) : null}
          {unresolved.length > 0 ? (
            <div className="rounded-lg border border-amber-500/50 bg-amber-500/5 px-4 py-3 text-sm">
              <div className="flex items-center gap-2 font-medium text-amber-700 dark:text-amber-500">
                <AlertTriangleIcon className="size-4" />
                {unresolved.length} recording
                {unresolved.length === 1 ? "" : "s"} not found in your library
              </div>
              <p className="mt-1 text-muted-foreground">
                Their review stays saved. Put the file
                {unresolved.length === 1 ? "" : "s"} back anywhere under your
                library and Refresh — the review re-links automatically.
              </p>
              <ul className="mt-2 space-y-0.5 text-muted-foreground">
                {unresolved.map((path) => (
                  <li key={path} className="truncate" title={path}>
                    {fileName(path)}
                  </li>
                ))}
              </ul>
            </div>
          ) : null}
          {carryOffers.map((offer) => (
            <div
              key={`${offer.from_path}→${offer.to_path}`}
              className="rounded-lg border border-sky-500/50 bg-sky-500/5 px-4 py-3 text-sm"
            >
              <div className="font-medium text-sky-700 dark:text-sky-400">
                Carry your review to the {kindLabel(offer.to_kind).toLowerCase()}{" "}
                library?
              </div>
              <p className="mt-1 text-muted-foreground">
                <span className="font-medium">{fileName(offer.to_path)}</span> is
                the same recording as one you have already reviewed. Bring that
                review — timeline, annotations, and flags — over to this copy?
              </p>
              <div className="mt-2 flex gap-2">
                <Button size="sm" onClick={() => void handleCarry(offer)}>
                  Carry review over
                </Button>
                <Button
                  size="sm"
                  variant="outline"
                  onClick={() => handleDecline(offer)}
                >
                  Not now
                </Button>
              </div>
            </div>
          ))}
          {/* Discovered shared reviews (ADR 0012, issue #67): bundles other people
              dropped into the shared library, offered before analysis runs on the
              recordings they cover. Accept receives; decline stops the nagging. */}
          {bundleOffers.map((offer) => (
            <div
              key={offer.bundle_path}
              className="rounded-lg border border-emerald-500/50 bg-emerald-500/5 px-4 py-3 text-sm"
            >
              <div className="font-medium text-emerald-700 dark:text-emerald-400">
                {offer.is_update ? "An updated shared" : "A shared"} review of{" "}
                {formatCaptureDay(offer.capture_day)} from{" "}
                <span className="font-medium">{offer.sharer_label}</span> is
                available — receive it?
              </div>
              <p className="mt-1 text-muted-foreground">
                Applies their timeline, annotations, and flags. No video is
                copied, and the covered recordings are not re-analyzed.
              </p>
              <div className="mt-2 flex gap-2">
                <Button
                  size="sm"
                  onClick={() => void handleReceiveOffer(offer)}
                >
                  Receive review
                </Button>
                <Button
                  size="sm"
                  variant="outline"
                  onClick={() => void handleDeclineOffer(offer)}
                >
                  Not now
                </Button>
              </div>
            </div>
          ))}
          {sessions.length === 0 ? (
            <div className="rounded-xl border border-dashed px-6 py-16 text-center">
              <p className="font-medium">
                {library === null ? "No library yet" : "No recordings yet"}
              </p>
              <p className="mt-1 text-sm text-muted-foreground">
                {library === null
                  ? "Designate the folder that holds your recordings — it becomes your library, the app's whole world of recordings. Originals play in place and are never modified."
                  : "Add recordings to your library folder, then Refresh. Originals play in place and are never modified."}
              </p>
            </div>
          ) : (
            sessions.map((session) => (
              <div key={session.id} className="rounded-xl border">
                <div className="flex items-center gap-4 border-b px-4 py-3">
                  <div className="min-w-0">
                    <h3 className="font-medium" title={session.capture_day}>
                      {formatCaptureDay(session.capture_day)}
                    </h3>
                    <p className="text-sm text-muted-foreground tabular-nums">
                      {sessionSummary(session)}
                    </p>
                  </div>
                  <Button
                    size="sm"
                    className="ml-auto shrink-0"
                    onClick={() =>
                      onPlay(session.recordings, 0, session.capture_day)
                    }
                    title="Review the whole session — every rally, back-to-back."
                  >
                    <PlayIcon className="size-4" />
                    Review session
                  </Button>
                  <DropdownMenu>
                    <DropdownMenuTrigger asChild>
                      <Button
                        variant="outline"
                        size="icon-sm"
                        className="shrink-0"
                        title="Share this session"
                      >
                        <Share2Icon className="size-4" />
                        <span className="sr-only">Share this session</span>
                      </Button>
                    </DropdownMenuTrigger>
                    <DropdownMenuContent align="end" className="w-56">
                      <DropdownMenuLabel>Session bundle</DropdownMenuLabel>
                      {/* Sharing into the shared library is only meaningful there
                          — recipients cannot reach local files (ADR 0012). */}
                      <DropdownMenuItem
                        onClick={() => openShare(session, false)}
                        disabled={active !== "shared"}
                        title={
                          active === "shared"
                            ? undefined
                            : "Switch to the shared library to share here."
                        }
                      >
                        <Share2Icon className="size-4" />
                        Share to shared library
                      </DropdownMenuItem>
                      <DropdownMenuItem onClick={() => openShare(session, true)}>
                        <FolderOpenIcon className="size-4" />
                        Save bundle as…
                      </DropdownMenuItem>
                    </DropdownMenuContent>
                  </DropdownMenu>
                </div>
                <ul className="divide-y">
                  {session.recordings.map((recording, recordingIndex) => (
                    <li
                      key={recording.id}
                      className="flex items-center hover:bg-accent"
                    >
                      <button
                        type="button"
                        onClick={() =>
                          onPlay(
                            session.recordings,
                            recordingIndex,
                            session.capture_day
                          )
                        }
                        className="flex min-w-0 flex-1 items-center gap-3 px-4 py-2 text-left text-sm"
                      >
                        <VideoIcon className="size-4 shrink-0 text-muted-foreground" />
                        <span
                          className="truncate font-medium"
                          title={recording.path}
                        >
                          {fileName(recording.path)}
                        </span>
                        {isPreparing(recording.probe_state) ? (
                          <span
                            className="ml-auto flex shrink-0 items-center gap-1.5 text-muted-foreground"
                            title="Preparing this recording for playback…"
                          >
                            <Loader2Icon className="size-3.5 animate-spin" />
                            Preparing…
                          </span>
                        ) : recording.probe_state === "failed" ? (
                          <span
                            className="ml-auto flex shrink-0 items-center gap-1.5 text-destructive"
                            title="This recording could not be read for playback."
                          >
                            <AlertTriangleIcon className="size-3.5" />
                            Failed
                          </span>
                        ) : isAnalyzing(recording) ? (
                          <span
                            className="ml-auto flex shrink-0 items-center gap-1.5 text-muted-foreground"
                            title="Detecting rallies in this recording…"
                          >
                            <Loader2Icon className="size-3.5 animate-spin" />
                            Analyzing…
                          </span>
                        ) : (
                          <span className="ml-auto flex shrink-0 items-center gap-3 text-muted-foreground tabular-nums">
                            {recording.segment_state === "ready" ? (
                              <span title="Rallies detected in the draft timeline">
                                {recording.rally_count}{" "}
                                {recording.rally_count === 1
                                  ? "rally"
                                  : "rallies"}
                              </span>
                            ) : recording.segment_state === "failed" ? (
                              <span
                                className="flex items-center gap-1.5 text-amber-600 dark:text-amber-500"
                                title="Could not analyze this recording's audio for rallies."
                              >
                                <AlertTriangleIcon className="size-3.5" />
                                No timeline
                              </span>
                            ) : null}
                            {formatSize(recording.file_size)}
                          </span>
                        )}
                      </button>
                      <DropdownMenu>
                        <DropdownMenuTrigger asChild>
                          <Button
                            variant="ghost"
                            size="icon-sm"
                            className="mr-2 shrink-0 text-muted-foreground"
                            title="Recording actions"
                          >
                            <MoreVerticalIcon className="size-4" />
                            <span className="sr-only">Recording actions</span>
                          </Button>
                        </DropdownMenuTrigger>
                        <DropdownMenuContent align="end" className="w-32">
                          <DropdownMenuItem
                            onClick={() => handleReanalyze(recording.path)}
                            disabled={isProcessing(recording)}
                          >
                            <RotateCwIcon className="size-4" />
                            Re-analyze
                          </DropdownMenuItem>
                        </DropdownMenuContent>
                      </DropdownMenu>
                    </li>
                  ))}
                </ul>
              </div>
            ))
          )}
        </div>
      </div>
    </div>
  )
}
