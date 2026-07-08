"use client"

import { AlertTriangleIcon, DownloadIcon, Loader2Icon } from "lucide-react"

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
import { fileName } from "@/lib/format"
import { formatCaptureDay } from "@/lib/utils"

import type { Browsing, Receiving, ShareTarget } from "./types"

const confirmCopy = {
  reanalyze: {
    title: "Re-analyze all recordings?",
    description:
      "This re-detects rallies in every recording and discards every draft timeline — including any manual corrections you have made.",
    action: "Re-analyze all",
    destructive: true,
  },
} as const

/** Confirm a bulk action (currently only re-analyze all). */
export function ConfirmActionDialog({
  action,
  onCancel,
  onConfirm,
}: {
  action: "reanalyze" | null
  onCancel: () => void
  onConfirm: () => void
}) {
  const copy = action ? confirmCopy[action] : null
  return (
    <AlertDialog
      open={action !== null}
      onOpenChange={(o) => {
        if (!o) onCancel()
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
            onClick={onConfirm}
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
  )
}

/**
 * Forget confirm (ADR 0011): discard the retained review for an unresolved
 * recording. Destructive — the file could still come back and re-link — so it is
 * confirmed before the delete.
 */
export function ForgetReviewDialog({
  forgetPath,
  onCancel,
  onConfirm,
}: {
  forgetPath: string | null
  onCancel: () => void
  onConfirm: (path: string) => void
}) {
  return (
    <AlertDialog
      open={forgetPath !== null}
      onOpenChange={(o) => {
        if (!o) onCancel()
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
              if (forgetPath) onConfirm(forgetPath)
              onCancel()
            }}
            className={buttonVariants({ variant: "destructive" })}
          >
            Delete review
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  )
}

/**
 * Share dialog (ADR 0012): name yourself once, then write a metadata-only bundle
 * of the session's review. Re-sharing overwrites only your own bundle. Shown for
 * both "share into shared library" and "save as".
 */
export function ShareDialog({
  shareTarget,
  shareName,
  onShareNameChange,
  onCancel,
  onConfirm,
}: {
  shareTarget: ShareTarget | null
  shareName: string
  onShareNameChange: (name: string) => void
  onCancel: () => void
  onConfirm: () => void
}) {
  return (
    <AlertDialog
      open={shareTarget !== null}
      onOpenChange={(o) => {
        if (!o) onCancel()
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
          onChange={(e) => onShareNameChange(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && shareName.trim()) onConfirm()
          }}
          placeholder="Your name"
          className="w-full rounded-md border bg-transparent px-3 py-2 text-sm outline-none focus-visible:ring-1 focus-visible:ring-ring"
        />
        <AlertDialogFooter>
          <AlertDialogCancel>Cancel</AlertDialogCancel>
          <AlertDialogAction onClick={onConfirm} disabled={!shareName.trim()}>
            {shareTarget?.saveAs ? "Save" : "Share"}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  )
}

/**
 * Receive resolution (ADR 0012): after a bundle is received, name any files that
 * failed verification and let the user choose keep-mine or take-theirs per
 * hand-touched recording. Whole-recording granularity — nothing merges. Closing
 * dismisses any unresolved conflicts (keep-mine).
 */
export function ReceiveResolutionDialog({
  receiving,
  onClose,
  onResolve,
}: {
  receiving: Receiving | null
  onClose: (snapshot: Receiving) => void
  onResolve: (path: string, takeTheirs: boolean) => void
}) {
  return (
    <AlertDialog
      open={receiving !== null}
      onOpenChange={(o) => {
        // Closing with conflicts/refusals still listed keeps mine for the
        // rest — conclude the receive here (toast + acknowledge) so it does
        // not linger. A programmatic close from resolveConflict already
        // finished and does not re-enter this handler.
        if (!o && receiving) onClose(receiving)
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
                  onClick={() => onResolve(path, false)}
                >
                  Keep mine
                </Button>
                <Button size="sm" onClick={() => onResolve(path, true)}>
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
  )
}

/**
 * Bundle browser (issue): every shared review for one session day, so a review
 * can be found and re-received after its offer was received or dismissed. A
 * "New" tag marks ones still on offer; the rest were already received or
 * declined and can be pulled again.
 */
export function BundleBrowserDialog({
  browsing,
  onClose,
  onReceive,
}: {
  browsing: Browsing | null
  onClose: () => void
  onReceive: (bundlePath: string) => void
}) {
  return (
    <AlertDialog
      open={browsing !== null}
      onOpenChange={(o) => {
        if (!o) onClose()
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
                  onClick={() => onReceive(bundle.bundle_path)}
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
  )
}
