"use client"

import { BundleOffersBox } from "./bundle-offers-box"
import { LibraryHeader } from "./library-header"
import {
  BundleBrowserDialog,
  ConfirmActionDialog,
  ForgetReviewDialog,
  ReceiveResolutionDialog,
  ShareDialog,
} from "./session-dialogs"
import { SessionBlock } from "./session-block"
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
  } = useSessionList(rescanOnMount)

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
        <div className="mx-auto max-w-4xl space-y-4 px-4 py-6">
          {error ? <p className="text-sm text-destructive">{error}</p> : null}
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
              <SessionBlock
                key={session.id}
                session={session}
                active={active}
                carryByPath={carryByPath}
                remainingByRecording={remainingByRecording}
                onPlay={onPlay}
                onBrowseBundles={(day) => void openBundleBrowser(day)}
                onShare={openShare}
                onCarry={(offer) => void handleCarry(offer)}
                onDismissCarry={(offer) => void handleDismiss(offer)}
                onReanalyze={(path) => void handleReanalyze(path)}
              />
            ))
          )}
        </div>
      </div>
    </div>
  )
}
