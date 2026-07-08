"use client"

import { useCallback, useEffect, useRef, useState } from "react"
import { open, save } from "@tauri-apps/plugin-dialog"
import {
  AlertTriangleIcon,
  CheckCircle2Icon,
  ClapperboardIcon,
  DownloadIcon,
  FilterIcon,
  FolderOpenIcon,
  ImportIcon,
  Loader2Icon,
  MoreVerticalIcon,
  PlayIcon,
  RefreshCwIcon,
  RotateCwIcon,
  Share2Icon,
  Trash2Icon,
  UsersIcon,
  VideoIcon,
  XIcon,
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
 * A shared bundle as listed for browsing: every foreign bundle in the shared
 * library, regardless of whether it is still on offer. The per-session bundle
 * browser lists these so a review can be found and re-received after its offer
 * has been received or dismissed.
 */
interface BundleSummary {
  bundle_path: string
  capture_day: string
  sharer_label: string
  recording_count: number
  rally_count: number
  annotation_count: number
  /** Already received or declined — no longer a standing offer. */
  seen: boolean
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
  /**
   * Re-scan the active library folder on mount (as the Refresh button does),
   * not just re-read the DB. Set when returning from the player so recordings
   * added while reviewing appear without a manual Refresh.
   */
  rescanOnMount?: boolean
}

/**
 * The homepage: the library of sessions in the studio shell (issue #48) — a
 * thin top bar carrying the app identity and the library actions, over a
 * centered column of session blocks. Each block is one session: its date and
 * stats, a Review button that opens the whole session in the workstation, and
 * the recordings it holds as dense rows.
 */
export function SessionList({
  onPlay,
  onBrowse,
  rescanOnMount = false,
}: SessionListProps) {
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
  // An unresolved recording the user has chosen to forget, awaiting confirmation.
  // Discarding a retained review is destructive (the file may yet come back), so
  // it goes through the same confirm gate as the bulk re-analyze.
  const [forgetPath, setForgetPath] = useState<string | null>(null)
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
  // Transient top-center toasts for share/receive confirmations (issue): a
  // shared or received bundle used to leave a line of grey text that lingered;
  // a toast surfaces the outcome where the eye is and clears itself. Each has a
  // monotonic id (a ref counter, never reused) so removal targets the right one.
  const [toasts, setToasts] = useState<
    { id: number; message: string }[]
  >([])
  const toastSeq = useRef(0)
  const showToast = useCallback((message: string) => {
    const id = (toastSeq.current += 1)
    setToasts((prev) => [...prev, { id, message }])
    setTimeout(
      () => setToasts((prev) => prev.filter((t) => t.id !== id)),
      3500
    )
  }, [])
  // The bundle file currently being received (ADR 0012): its path, the outcome
  // of the receive, which hand-touched recordings still need a keep-mine-or-
  // take-theirs choice, and how many of those the user has taken-theirs on so
  // far. `resolved` matters for the final count: an updated session's recordings
  // arrive as conflicts (they carry the review from the first receive, so they
  // read as hand-touched), so `applied` is 0 and only the resolutions count.
  // Null until the user opens a bundle.
  const [receiving, setReceiving] = useState<{
    bundlePath: string
    result: ReceiveResult
    resolved: number
    // Remaining bundle paths of an "Accept all" run to process after this one,
    // and the recordings already tallied earlier in that run. Both empty/zero
    // for a lone receive.
    queue: string[]
    tally: number
  } | null>(null)
  // The per-session bundle browser (issue): which session day it is open for and
  // the shared bundles found for that day, once loaded. Lets the user re-open a
  // shared review after its offer was received or dismissed. Null when closed.
  const [browsing, setBrowsing] = useState<{
    day: string
    loading: boolean
    bundles: BundleSummary[]
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

  // Accept an offer: carry the review — timeline, flags, annotations, and the
  // analyzed segments — to the other copy, then refresh (the offer disappears once
  // both sides match). The carried copy is not re-analyzed.
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

  // Dismiss an offer's inline button: persist it (ADR 0011) so the carry stops
  // being offered for this copy, then refresh so the button disappears. Unlike a
  // transient "not now" this holds across restarts.
  async function handleDismiss(offer: CarryOffer) {
    setError(null)
    try {
      await trackedInvoke("dismiss_carry", { toPath: offer.to_path })
      await refresh()
    } catch (e) {
      setError(String(e))
    }
  }

  // Receive a run of bundles in order (ADR 0012, issue #67), tallying the
  // recordings whose review changed. Clean bundles apply straight from disk (no
  // probe/segmentation/staging) and are acknowledged so their offer retires; the
  // first bundle that needs a conflict/refusal choice hands off to the dialog,
  // parking the rest of the run in `receiving.queue` — the dialog resumes them
  // via `finishReceive` once the user is done. `tally` carries the count already
  // accrued earlier in the run. This one function backs a single offer, a
  // manually-opened bundle, and "Accept all" alike.
  async function runReceives(paths: string[], tally: number) {
    for (let i = 0; i < paths.length; i++) {
      const bundlePath = paths[i]
      let result: ReceiveResult
      try {
        result = await trackedInvoke<ReceiveResult>("receive_session_bundle", {
          bundlePath,
        })
      } catch (e) {
        setError(String(e))
        return
      }
      if (result.conflicts.length > 0 || result.refused.length > 0) {
        await refresh()
        setReceiving({
          bundlePath,
          result,
          resolved: 0,
          queue: paths.slice(i + 1),
          tally,
        })
        return
      }
      try {
        await trackedInvoke("acknowledge_bundle", { bundlePath })
      } catch (e) {
        setError(String(e))
      }
      tally += result.applied
    }
    await refresh()
    toastReceived(tally)
  }

  // Accept one discovered offer.
  async function handleReceiveOffer(offer: BundleOffer) {
    setError(null)
    await runReceives([offer.bundle_path], 0)
  }

  // Accept every discovered offer at once: receive them in listed order, each
  // conflict/refusal dialog resolved before the next bundle is touched.
  async function handleReceiveAll() {
    setError(null)
    const paths = bundleOffers.map((o) => o.bundle_path)
    if (paths.length === 0) return
    await runReceives(paths, 0)
  }

  // Conclude the current dialog bundle (ADR 0012): acknowledge it so its offer
  // retires (only a re-share re-offers it, as an update), then either resume the
  // rest of an "Accept all" run or, when the run is done, refresh and toast the
  // batch total. `resolved` is the count of conflicts the user took-theirs on.
  async function finishReceive(r: {
    bundlePath: string
    result: ReceiveResult
    resolved: number
    queue: string[]
    tally: number
  }) {
    try {
      await trackedInvoke("acknowledge_bundle", { bundlePath: r.bundlePath })
    } catch (e) {
      setError(String(e))
    }
    const tally = r.tally + r.result.applied + r.resolved
    if (r.queue.length > 0) {
      await runReceives(r.queue, tally)
    } else {
      await refresh()
      toastReceived(tally)
    }
  }

  // Toast the outcome of a receive run: the true number of recordings whose
  // review changed (taken silently plus every take-theirs), or a no-change note.
  function toastReceived(count: number) {
    showToast(
      count > 0
        ? `Received ${count} recording${count === 1 ? "" : "s"}.`
        : "No changes — kept your existing review."
    )
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

  // Dismiss every discovered offer at once: decline each, then refresh so the
  // combined box disappears.
  async function handleDeclineAll() {
    setError(null)
    try {
      for (const offer of bundleOffers) {
        await trackedInvoke("decline_bundle", { bundlePath: offer.bundle_path })
      }
      await refresh()
    } catch (e) {
      setError(String(e))
    }
  }

  // Open the bundle browser for a session day (issue): list every shared review
  // for that day — including ones already received or dismissed, which no longer
  // appear as offers — so the user can find and re-receive them. Opens the dialog
  // straight away with a loading state, then fills it once the list arrives.
  async function openBundleBrowser(day: string) {
    setError(null)
    setBrowsing({ day, loading: true, bundles: [] })
    try {
      const all = await trackedInvoke<BundleSummary[]>("list_bundles")
      setBrowsing({
        day,
        loading: false,
        bundles: all.filter((b) => b.capture_day === day),
      })
    } catch (e) {
      setError(String(e))
      setBrowsing(null)
    }
  }

  // Receive a bundle chosen from the browser: close the browser first so the
  // receive (and any conflict dialog it opens) is not stacked under it, then run
  // the same receive flow as an offer.
  async function receiveFromBrowser(bundlePath: string) {
    setBrowsing(null)
    await runReceives([bundlePath], 0)
  }

  // The active library's own record (mount path + locality), or undefined until
  // its kind is designated on this device.
  const activeLibrary = libraries.find((l) => l.kind === active)
  const library = activeLibrary?.path ?? null

  useEffect(() => {
    // On mount, load persisted sessions. Returning from the player rescans the
    // active library folder first so recordings added while reviewing appear;
    // otherwise just re-read the DB. `rescanOnMount` is fixed for this mount
    // (the parent clears it only after navigating away, which unmounts us), so
    // reading it once here is correct. The setState lands after an awaited
    // round-trip to Rust, not synchronously within the effect body.
    // eslint-disable-next-line react-hooks/set-state-in-effect
    void (rescanOnMount ? handleRefresh() : refresh())
    // eslint-disable-next-line react-hooks/exhaustive-deps
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

  // Forget an unresolved recording: discard the review state the DB retained for
  // a file that vanished from the library (ADR 0011). Drops it from the amber list
  // and refreshes so any session it emptied disappears too.
  async function handleForget(path: string) {
    setError(null)
    try {
      await trackedInvoke("delete_recording", { path })
      setUnresolved((prev) => prev.filter((p) => p !== path))
      await refresh()
    } catch (e) {
      setError(String(e))
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
        showToast("Bundle saved.")
      } else {
        await trackedInvoke("share_session_bundle", {
          sessionId: session.id,
          sharerLabel: name,
        })
        showToast("Shared to the shared library.")
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
    try {
      const picked = await open({
        multiple: false,
        filters: [{ name: "Voloph bundle", extensions: ["vbundle"] }],
      })
      if (typeof picked !== "string") return
      await runReceives([picked], 0)
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
      }
      const conflicts = receiving.result.conflicts.filter((c) => c !== path)
      const resolved = receiving.resolved + (takeTheirs ? 1 : 0)
      if (conflicts.length === 0 && receiving.result.refused.length === 0) {
        // Last one resolved: close this dialog and conclude — acknowledge, then
        // either resume the rest of an "Accept all" run or toast the total.
        setReceiving(null)
        await finishReceive({ ...receiving, resolved })
      } else {
        setReceiving({
          ...receiving,
          result: { ...receiving.result, conflicts },
          resolved,
        })
      }
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

  // Carry-over offers whose receiving copy lives in the active library (ADR 0011),
  // keyed by that copy's absolute path — the same shape `list_sessions` returns for
  // a recording, so a row can look up its own offer. Offers pointing at the other
  // library's copy surface when that library is active instead.
  const carryByPath = new Map(
    carryOffers.filter((o) => o.to_kind === active).map((o) => [o.to_path, o])
  )

  return (
    <div className="flex h-full flex-col">
      {/* Top-center toast stack: transient share/receive confirmations. Fixed
          and pointer-transparent so it floats over the library without stealing
          clicks; each toast clears itself after a few seconds. */}
      {toasts.length > 0 ? (
        <div className="pointer-events-none fixed inset-x-0 top-4 z-50 flex flex-col items-center gap-2 px-4">
          {toasts.map((t) => (
            <div
              key={t.id}
              className="flex items-center gap-2 rounded-md border bg-popover px-3 py-2 text-sm font-medium text-popover-foreground shadow-md"
            >
              <CheckCircle2Icon className="size-4 text-emerald-600 dark:text-emerald-400" />
              {t.message}
            </div>
          ))}
        </div>
      ) : null}

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

      {/* Forget confirm (ADR 0011): discard the retained review for an unresolved
          recording. Destructive — the file could still come back and re-link — so
          it is confirmed before the delete. */}
      <AlertDialog
        open={forgetPath !== null}
        onOpenChange={(o) => {
          if (!o) setForgetPath(null)
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete this review?</AlertDialogTitle>
            <AlertDialogDescription>
              This permanently discards the saved review for{" "}
              {forgetPath ? fileName(forgetPath) : "this recording"}. If the file
              comes back later it will be treated as new, with no review to
              re-link.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={() => {
                if (forgetPath) void handleForget(forgetPath)
                setForgetPath(null)
              }}
              className={buttonVariants({ variant: "destructive" })}
            >
              Delete review
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
          // Closing with conflicts/refusals still listed keeps mine for the
          // rest — conclude the receive here (toast + acknowledge) so it does
          // not linger. A programmatic close from resolveConflict already
          // finished and does not re-enter this handler.
          if (!o && receiving) {
            const snapshot = receiving
            setReceiving(null)
            void finishReceive(snapshot)
          }
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

      {/* Bundle browser (issue): every shared review for one session day, so a
          review can be found and re-received after its offer was received or
          dismissed. A "New" tag marks ones still on offer; the rest were already
          received or declined and can be pulled again. */}
      <AlertDialog
        open={browsing !== null}
        onOpenChange={(o) => {
          if (!o) setBrowsing(null)
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>
              Shared reviews
              {browsing ? ` — ${formatCaptureDay(browsing.day)}` : ""}
            </AlertDialogTitle>
            <AlertDialogDescription>
              Every review shared for this session, including ones you have
              already received or dismissed. Receiving applies their timeline,
              annotations, and flags — no video is copied.
            </AlertDialogDescription>
          </AlertDialogHeader>
          {browsing?.loading ? (
            <p className="flex items-center gap-2 py-2 text-sm text-muted-foreground">
              <Loader2Icon className="size-4 animate-spin" />
              Looking for shared reviews…
            </p>
          ) : browsing && browsing.bundles.length === 0 ? (
            <p className="py-2 text-sm text-muted-foreground">
              No shared reviews for this session yet.
            </p>
          ) : (
            <ul className="space-y-2">
              {browsing?.bundles.map((bundle) => (
                <li
                  key={bundle.bundle_path}
                  className="flex items-center gap-3 rounded-md border px-3 py-2 text-sm"
                >
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-2 font-medium">
                      <span className="truncate">{bundle.sharer_label}</span>
                      {!bundle.seen ? (
                        <span className="shrink-0 rounded bg-emerald-500/15 px-1.5 py-0.5 text-xs font-medium text-emerald-700 dark:text-emerald-400">
                          New
                        </span>
                      ) : null}
                    </div>
                    <p className="text-muted-foreground tabular-nums">
                      {bundle.recording_count} recording
                      {bundle.recording_count === 1 ? "" : "s"} ·{" "}
                      {bundle.rally_count}{" "}
                      {bundle.rally_count === 1 ? "rally" : "rallies"} ·{" "}
                      {bundle.annotation_count} verdict
                      {bundle.annotation_count === 1 ? "" : "s"}
                    </p>
                  </div>
                  <Button
                    size="sm"
                    variant="outline"
                    className="shrink-0"
                    onClick={() => void receiveFromBrowser(bundle.bundle_path)}
                  >
                    <DownloadIcon className="size-4" />
                    Receive
                  </Button>
                </li>
              ))}
            </ul>
          )}
          <AlertDialogFooter>
            <AlertDialogCancel>Close</AlertDialogCancel>
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
            {/* Reserve the wider label's width so swapping the text on click
                doesn't resize the button and reflow the row. */}
            <span className="grid text-center">
              <span className="invisible col-start-1 row-start-1">
                Refreshing…
              </span>
              <span className="col-start-1 row-start-1">
                {refreshing ? "Refreshing…" : "Refresh"}
              </span>
            </span>
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
                  <li key={path} className="flex items-center gap-2">
                    <span className="min-w-0 flex-1 truncate" title={path}>
                      {fileName(path)}
                    </span>
                    <Button
                      size="sm"
                      variant="ghost"
                      className="h-6 shrink-0 px-2 text-muted-foreground hover:text-destructive"
                      onClick={() => setForgetPath(path)}
                    >
                      <Trash2Icon className="size-3.5" />
                      Delete review
                    </Button>
                  </li>
                ))}
              </ul>
            </div>
          ) : null}
          {/* Carry-over offers (ADR 0011) are no longer a top banner — they surface
              as an inline button on the receiving recording's row below, so the
              offer stays available (with a dismiss) instead of expiring. */}
          {/* Discovered shared reviews (ADR 0012, issue #67): bundles other people
              dropped into the shared library, offered before analysis runs on the
              recordings they cover. One combined box lists every offer with a per-
              row Receive/dismiss and, when more than one is waiting, Accept/Dismiss
              all. Accept receives; decline stops the nagging until a re-share. */}
          {bundleOffers.length > 0 ? (
            <div className="rounded-lg border border-emerald-500/50 bg-emerald-500/5 px-4 py-3 text-sm">
              <div className="flex items-center justify-between gap-3">
                <div className="font-medium text-emerald-700 dark:text-emerald-400">
                  {bundleOffers.length} shared review
                  {bundleOffers.length === 1 ? "" : "s"} available
                </div>
                {bundleOffers.length > 1 ? (
                  <div className="flex shrink-0 gap-2">
                    <Button size="sm" onClick={() => void handleReceiveAll()}>
                      Accept all
                    </Button>
                    <Button
                      size="sm"
                      variant="outline"
                      onClick={() => void handleDeclineAll()}
                    >
                      Dismiss all
                    </Button>
                  </div>
                ) : null}
              </div>
              <p className="mt-1 text-muted-foreground">
                Applies their timeline, annotations, and flags. No video is
                copied, and the covered recordings are not re-analyzed.
              </p>
              <ul className="mt-2 space-y-1.5">
                {bundleOffers.map((offer) => (
                  <li
                    key={offer.bundle_path}
                    className="flex items-center gap-2"
                  >
                    <span className="min-w-0 flex-1 truncate">
                      {offer.is_update ? (
                        <span className="mr-1 rounded bg-emerald-500/15 px-1.5 py-0.5 text-xs font-medium text-emerald-700 dark:text-emerald-400">
                          Updated
                        </span>
                      ) : null}
                      {formatCaptureDay(offer.capture_day)} from{" "}
                      <span className="font-medium">{offer.sharer_label}</span>
                    </span>
                    <Button
                      size="sm"
                      variant="outline"
                      className="h-7 shrink-0"
                      onClick={() => void handleReceiveOffer(offer)}
                    >
                      Receive
                    </Button>
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      className="size-7 shrink-0 text-muted-foreground"
                      onClick={() => void handleDeclineOffer(offer)}
                      title="Not now — stop offering this until it is re-shared"
                    >
                      <XIcon className="size-3.5" />
                      <span className="sr-only">Dismiss this shared review</span>
                    </Button>
                  </li>
                ))}
              </ul>
            </div>
          ) : null}
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
                  {/* Shared reviews for this session (issue): browse every
                      bundle shared for this day, including ones already received
                      or dismissed, and re-receive any. Only the shared library
                      holds bundles, so it is disabled elsewhere. */}
                  <Button
                    variant="outline"
                    size="icon-sm"
                    className="shrink-0"
                    disabled={active !== "shared"}
                    onClick={() => void openBundleBrowser(session.capture_day)}
                    title={
                      active === "shared"
                        ? "Shared reviews for this session"
                        : "Switch to the shared library to see shared reviews."
                    }
                  >
                    <UsersIcon className="size-4" />
                    <span className="sr-only">
                      Shared reviews for this session
                    </span>
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
                      {/* Carry-over (ADR 0011): this copy is byte-identical to one
                          already reviewed in the other library, and it is un-touched
                          here — offer to bring that review (timeline, flags,
                          annotations, segments) over. Dismiss hides it for good. */}
                      {(() => {
                        const offer = carryByPath.get(recording.path)
                        if (!offer) return null
                        return (
                          <div className="flex shrink-0 items-center gap-1">
                            <Button
                              size="sm"
                              variant="outline"
                              className="h-7 border-sky-500/50 text-sky-700 hover:bg-sky-500/10 hover:text-sky-700 dark:text-sky-400 dark:hover:text-sky-400"
                              onClick={() => void handleCarry(offer)}
                              title="Bring the review from your other-library copy — timeline, flags, annotations, and segments — onto this copy."
                            >
                              <ImportIcon className="size-3.5" />
                              Carry review
                            </Button>
                            <Button
                              variant="ghost"
                              size="icon-sm"
                              className="size-7 shrink-0 text-muted-foreground"
                              onClick={() => void handleDismiss(offer)}
                              title="Dismiss this carry-over offer"
                            >
                              <XIcon className="size-3.5" />
                              <span className="sr-only">
                                Dismiss carry-over offer
                              </span>
                            </Button>
                          </div>
                        )
                      })()}
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
