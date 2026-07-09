import { useCallback, useEffect, useRef, useState } from "react"
import { listen } from "@tauri-apps/api/event"
import { open, save } from "@tauri-apps/plugin-dialog"

import { trackedInvoke } from "@/lib/tauri"

import {
  remainingByRecording as deriveRemaining,
  type AnalysisProgress,
} from "./analysis-progress"
import { isProcessing } from "./recording-state"
import type {
  Browsing,
  BundleOffer,
  BundleSummary,
  CarryOffer,
  Library,
  Receiving,
  ReceiveResult,
  ScanResult,
  Session,
  ShareTarget,
  Toast,
} from "./types"

/**
 * All state and actions for the session list. Owns the library, discovery, and
 * share/receive flows; the view is a pure function of what this returns. Kept as
 * one hook because the flows share `refresh` and the receive run's state.
 */
export function useSessionList(rescanOnMount: boolean) {
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
  const [shareTarget, setShareTarget] = useState<ShareTarget | null>(null)
  // Draft name in the share dialog, seeded from the persisted label.
  const [shareName, setShareName] = useState<string>("")
  // Transient top-center toasts for share/receive confirmations (issue): a
  // shared or received bundle used to leave a line of grey text that lingered;
  // a toast surfaces the outcome where the eye is and clears itself. Each has a
  // monotonic id (a ref counter, never reused) so removal targets the right one.
  const [toasts, setToasts] = useState<Toast[]>([])
  const toastSeq = useRef(0)
  const showToast = useCallback((message: string) => {
    const id = (toastSeq.current += 1)
    setToasts((prev) => [...prev, { id, message }])
    setTimeout(() => setToasts((prev) => prev.filter((t) => t.id !== id)), 3500)
  }, [])
  // The bundle file currently being received (ADR 0012). Null until the user
  // opens a bundle.
  const [receiving, setReceiving] = useState<Receiving | null>(null)
  // The per-session bundle browser (issue): which session day it is open for and
  // the shared bundles found for that day, once loaded. Lets the user re-open a
  // shared review after its offer was received or dismissed. Null when closed.
  const [browsing, setBrowsing] = useState<Browsing | null>(null)

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
  async function finishReceive(r: Receiving) {
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

  // Live analysis-time estimate (issue #81, spec #75 user story #13). The media
  // worker emits `analysis:progress` as it decodes each recording; we keep the
  // latest tick per recording and turn it into a remaining-ms estimate the row
  // renders. One listener for the list's lifetime — a recording's analysis
  // outlives any single poll. A ready/failed recording drops out of the map on
  // the next refresh (it is no longer processing), so stale ticks never linger.
  const [progress, setProgress] = useState<Map<number, AnalysisProgress>>(
    new Map()
  )
  useEffect(() => {
    const unlisten = listen<AnalysisProgress>("analysis:progress", (e) =>
      setProgress((prev) => new Map(prev).set(e.payload.recording_id, e.payload))
    )
    return () => {
      void unlisten.then((off) => off())
    }
  }, [])
  // Only recordings still analyzing carry an estimate; keying on ids keeps the
  // map from growing unbounded across a long session of imports.
  const analyzing = new Set(
    sessions.flatMap((s) =>
      s.recordings.filter((r) => isProcessing(r)).map((r) => r.id)
    )
  )
  const remainingByRecording = deriveRemaining(analyzing, progress)

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

  // Carry-over offers whose receiving copy lives in the active library (ADR 0011),
  // keyed by that copy's absolute path — the same shape `list_sessions` returns for
  // a recording, so a row can look up its own offer. Offers pointing at the other
  // library's copy surface when that library is active instead.
  const carryByPath = new Map(
    carryOffers.filter((o) => o.to_kind === active).map((o) => [o.to_path, o])
  )

  return {
    sessions,
    libraries,
    active,
    scanning,
    refreshing,
    reanalyzingAll,
    confirmAction,
    setConfirmAction,
    error,
    unresolved,
    forgetPath,
    setForgetPath,
    bundleOffers,
    shareTarget,
    setShareTarget,
    shareName,
    setShareName,
    toasts,
    receiving,
    setReceiving,
    browsing,
    setBrowsing,
    library,
    carryByPath,
    remainingByRecording,
    handleCarry,
    handleDismiss,
    handleReceiveOffer,
    handleReceiveAll,
    finishReceive,
    handleDeclineOffer,
    handleDeclineAll,
    openBundleBrowser,
    receiveFromBrowser,
    handleDesignateLibrary,
    handleSwitch,
    handleRefresh,
    handleForget,
    handleConfirm,
    handleReanalyze,
    openShare,
    confirmShare,
    handleReceive,
    resolveConflict,
  }
}
