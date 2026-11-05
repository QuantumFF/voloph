"use client"

import {
  FolderOpenIcon,
  Loader2Icon,
  RefreshCwIcon,
  VideoOffIcon,
} from "lucide-react"

import { Button } from "@/components/ui/button"
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip"
import { fileName } from "@/lib/format"

import { BundleOffersBox } from "./bundle-offers-box"
import { LibraryHeader } from "./library-header"
import {
  BundleBrowserDialog,
  ConfirmActionDialog,
  ForgetReviewDialog,
  ReanalyzeRecordingDialog,
  ReceiveResolutionDialog,
  ShareDialog,
} from "./session-dialogs"
import {
  formatFootage,
  groupByMonth,
  kindLabel,
  sessionStats,
} from "./recording-state"
import { nextToReview, type ReviewTally } from "./review-progress"
import { SessionBlock } from "./session-block"
import type { Session } from "./types"
import { ToastStack } from "./toast-stack"
import { UnresolvedBox } from "./unresolved-box"
import { useSessionList } from "./use-session-list"

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
  const {
    sessions,
    libraries,
    active,
    scanning,
    refreshing,
    reanalyzingAll,
    segmenter,
    handleSegmenter,
    padding,
    handlePadding,
    confirmAction,
    setConfirmAction,
    error,
    unresolved,
    forgetPath,
    setForgetPath,
    reanalyzeTarget,
    setReanalyzeTarget,
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
    review,
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
    handleReveal,
    handleCopyPath,
    openShare,
    confirmShare,
    handleReceive,
    resolveConflict,
  } = useSessionList(rescanOnMount)

  // The one session whose Review button is filled: the newest with rallies
  // still unmarked. One primary per page points at the next job; every other
  // session's Review is outline so the list isn't a column of loud buttons.
  const primarySessionId = nextToReview(sessions, review)

  return (
    <div className="flex h-full flex-col">
      <ToastStack toasts={toasts} />

      <ConfirmActionDialog
        action={confirmAction}
        onCancel={() => setConfirmAction(null)}
        onConfirm={handleConfirm}
      />

      <ForgetReviewDialog
        forgetPath={forgetPath}
        onCancel={() => setForgetPath(null)}
        onConfirm={(path) => void handleForget(path)}
      />

      <ReanalyzeRecordingDialog
        target={reanalyzeTarget}
        onCancel={() => setReanalyzeTarget(null)}
        onConfirm={(path) => void handleReanalyze(path)}
      />

      <ShareDialog
        shareTarget={shareTarget}
        shareName={shareName}
        onShareNameChange={setShareName}
        onCancel={() => setShareTarget(null)}
        onConfirm={() => void confirmShare()}
      />

      <ReceiveResolutionDialog
        receiving={receiving}
        onClose={(snapshot) => {
          setReceiving(null)
          void finishReceive(snapshot)
        }}
        onResolve={(path, takeTheirs) => void resolveConflict(path, takeTheirs)}
      />

      <BundleBrowserDialog
        browsing={browsing}
        onClose={() => setBrowsing(null)}
        onReceive={(bundlePath) => void receiveFromBrowser(bundlePath)}
      />

      <LibraryHeader
        libraries={libraries}
        active={active}
        library={library}
        sessions={sessions}
        scanning={scanning}
        refreshing={refreshing}
        reanalyzingAll={reanalyzingAll}
        segmenter={segmenter}
        onSegmenter={(id) => void handleSegmenter(id)}
        padding={padding}
        onPadding={(p) => void handlePadding(p)}
        onBrowse={onBrowse}
        onSwitch={(kind) => void handleSwitch(kind)}
        onRefresh={() => void handleRefresh()}
        onDesignateLibrary={(kind, mount) =>
          void handleDesignateLibrary(kind, mount)
        }
        onReceive={() => void handleReceive()}
        onReanalyzeAll={() => setConfirmAction("reanalyze")}
      />

      <div className="min-h-0 flex-1 overflow-y-auto">
        <div className="mx-auto max-w-5xl space-y-6 px-6 py-6">
          {library !== null && sessions.length > 0 ? (
            <LibraryOverview
              kind={active}
              path={library}
              sessions={sessions}
              review={review}
            />
          ) : null}
          {error ? (
            <p
              role="alert"
              className="rounded-md border border-destructive/40 bg-destructive/5 px-3 py-2 text-sm text-destructive"
            >
              {error}
            </p>
          ) : null}
          <UnresolvedBox
            unresolved={unresolved}
            onForget={(path) => setForgetPath(path)}
          />
          {/* Carry-over offers (ADR 0011) are no longer a top banner — they surface
              as an inline button on the receiving recording's row below, so the
              offer stays available (with a dismiss) instead of expiring. */}
          <BundleOffersBox
            bundleOffers={bundleOffers}
            onReceiveOffer={(offer) => void handleReceiveOffer(offer)}
            onReceiveAll={() => void handleReceiveAll()}
            onDeclineOffer={(offer) => void handleDeclineOffer(offer)}
            onDeclineAll={() => void handleDeclineAll()}
          />
          {sessions.length === 0 ? (
            <EmptyLibrary
              library={library}
              scanning={scanning}
              refreshing={refreshing}
              onChooseFolder={() =>
                void handleDesignateLibrary("local", "local")
              }
              onRefresh={() => void handleRefresh()}
            />
          ) : (
            groupByMonth(sessions).map((group) => (
              <section key={group.key} aria-label={group.label}>
                <h2 className="mb-2 flex items-baseline gap-2 text-xs font-medium tracking-wide text-muted-foreground uppercase">
                  {group.label}
                  <span className="font-normal tracking-normal normal-case tabular-nums">
                    {group.sessions.length}{" "}
                    {group.sessions.length === 1 ? "session" : "sessions"}
                  </span>
                </h2>
                <div className="space-y-3">
                  {group.sessions.map((session) => (
                    <SessionBlock
                      key={session.id}
                      session={session}
                      active={active}
                      hasShared={libraries.some((l) => l.kind === "shared")}
                      carryByPath={carryByPath}
                      remainingByRecording={remainingByRecording}
                      review={review}
                      primary={session.id === primarySessionId}
                      onPlay={onPlay}
                      onBrowseBundles={(day) => void openBundleBrowser(day)}
                      onShare={openShare}
                      onCarry={(offer) => void handleCarry(offer)}
                      onDismissCarry={(offer) => void handleDismiss(offer)}
                      onReanalyze={(path, flagged) =>
                        setReanalyzeTarget({ path, flagged })
                      }
                      onReveal={(path) => void handleReveal(path)}
                      onCopyPath={(path) => void handleCopyPath(path)}
                    />
                  ))}
                </div>
              </section>
            ))
          )}
        </div>
      </div>
    </div>
  )
}

/**
 * The page heading: which library is open (and where it lives) and its
 * headline numbers, so the list reads as a library rather than a loose column.
 */
function LibraryOverview({
  kind,
  path,
  sessions,
  review,
}: {
  kind: string
  path: string
  sessions: Session[]
  review: Map<number, ReviewTally> | null
}) {
  const stats = sessionStats(sessions)
  let marked = 0
  let tallied = 0
  for (const t of review?.values() ?? []) {
    marked += t.marked
    tallied += t.rallies
  }
  // "Rallies marked" reads "1/48", the same fraction the recording rows show.
  const figures: [string, React.ReactNode, string?][] = [
    ["Sessions", String(sessions.length)],
    ["Recordings", String(stats.recordings)],
    ["Rallies", String(stats.rallies)],
    ...(review
      ? [
          [
            "Rallies marked",
            <>
              {marked}
              <span className="text-muted-foreground">/{tallied}</span>
            </>,
            `${marked} of ${tallied} rallies carry at least one verdict`,
          ] as [string, React.ReactNode, string],
        ]
      : []),
    ["Footage", formatFootage(stats.durationMs)],
  ]
  return (
    <div className="flex items-end gap-x-8 border-b pb-5">
      <div className="min-w-0 flex-1">
        <h1 className="text-xl font-semibold tracking-tight">Sessions</h1>
        <p className="mt-1 flex min-w-0 items-center gap-1.5 text-sm text-muted-foreground">
          <FolderOpenIcon className="size-3.5 shrink-0" />
          <span className="shrink-0">{kindLabel(kind)} library</span>
          <span aria-hidden>·</span>
          {/* The folder's own name is what identifies it; the full path is a
              hover (or focus) away rather than a truncated monospace run. */}
          <Tooltip delayDuration={300}>
            <TooltipTrigger asChild>
              <span
                tabIndex={0}
                className="truncate rounded-sm text-foreground/90 outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
              >
                {fileName(path)}
              </span>
            </TooltipTrigger>
            <TooltipContent
              side="bottom"
              align="start"
              className="max-w-lg font-mono break-all"
            >
              {path}
            </TooltipContent>
          </Tooltip>
        </p>
      </div>
      <dl className="flex shrink-0 gap-6">
        {figures.map(([label, value, hint]) => (
          <div key={label} title={hint}>
            <dt className="text-[11px] font-medium tracking-wide text-muted-foreground uppercase">
              {label}
            </dt>
            <dd className="text-lg font-semibold tabular-nums">{value}</dd>
          </div>
        ))}
      </dl>
    </div>
  )
}

/**
 * The first-run and empty-library states, each with the one action that gets
 * the user moving: choose a library folder (the same flow as Libraries →
 * Choose local library), or re-scan the folder after adding recordings.
 */
function EmptyLibrary({
  library,
  scanning,
  refreshing,
  onChooseFolder,
  onRefresh,
}: {
  library: string | null
  scanning: boolean
  refreshing: boolean
  onChooseFolder: () => void
  onRefresh: () => void
}) {
  if (library === null) {
    return (
      <div className="mx-auto mt-10 flex max-w-lg flex-col items-center rounded-lg border border-dashed px-8 py-14 text-center">
        <div className="flex size-12 items-center justify-center rounded-full bg-muted">
          <FolderOpenIcon className="size-6 text-muted-foreground" />
        </div>
        <h1 className="mt-4 text-lg font-semibold">
          Choose your library folder
        </h1>
        <p className="mt-1.5 text-sm text-muted-foreground">
          Pick the folder that holds your recordings — it becomes your library.
          Originals play in place and are never modified.
        </p>
        <Button
          size="lg"
          className="mt-6 px-4"
          onClick={onChooseFolder}
          disabled={scanning}
        >
          {scanning ? (
            <Loader2Icon className="size-4 animate-spin" />
          ) : (
            <FolderOpenIcon className="size-4" />
          )}
          {scanning ? "Scanning…" : "Choose folder…"}
        </Button>
        <p className="mt-4 text-xs text-muted-foreground">
          Recordings on a NAS or shared drive? Use Libraries → Shared library.
        </p>
      </div>
    )
  }
  return (
    <div className="mx-auto mt-10 flex max-w-lg flex-col items-center rounded-lg border border-dashed px-8 py-14 text-center">
      <div className="flex size-12 items-center justify-center rounded-full bg-muted">
        <VideoOffIcon className="size-6 text-muted-foreground" />
      </div>
      <h1 className="mt-4 text-lg font-semibold">No recordings yet</h1>
      <p className="mt-1.5 text-sm text-muted-foreground">
        Copy recordings anywhere under{" "}
        <span className="font-mono text-xs break-all text-foreground">
          {library}
        </span>
        , then refresh. Originals play in place and are never modified.
      </p>
      <Button
        size="lg"
        className="mt-6 px-4"
        onClick={onRefresh}
        disabled={refreshing}
      >
        <RefreshCwIcon
          className={`size-4 ${refreshing ? "animate-spin" : ""}`}
        />
        Refresh library
      </Button>
    </div>
  )
}
