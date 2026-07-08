"use client"

import { XIcon } from "lucide-react"

import { Button } from "@/components/ui/button"
import { formatCaptureDay } from "@/lib/utils"

import type { BundleOffer } from "./types"

/**
 * Discovered shared reviews (ADR 0012, issue #67): bundles other people dropped
 * into the shared library, offered before analysis runs on the recordings they
 * cover. One combined box lists every offer with a per-row Receive/dismiss and,
 * when more than one is waiting, Accept/Dismiss all. Accept receives; decline
 * stops the nagging until a re-share.
 */
export function BundleOffersBox({
  bundleOffers,
  onReceiveOffer,
  onReceiveAll,
  onDeclineOffer,
  onDeclineAll,
}: {
  bundleOffers: BundleOffer[]
  onReceiveOffer: (offer: BundleOffer) => void
  onReceiveAll: () => void
  onDeclineOffer: (offer: BundleOffer) => void
  onDeclineAll: () => void
}) {
  if (bundleOffers.length === 0) return null
  return (
    <div className="rounded-lg border border-emerald-500/50 bg-emerald-500/5 px-4 py-3 text-sm">
      <div className="flex items-center justify-between gap-3">
        <div className="font-medium text-emerald-700 dark:text-emerald-400">
          {bundleOffers.length} shared review
          {bundleOffers.length === 1 ? "" : "s"} available
        </div>
        {bundleOffers.length > 1 ? (
          <div className="flex shrink-0 gap-2">
            <Button size="sm" onClick={onReceiveAll}>
              Accept all
            </Button>
            <Button size="sm" variant="outline" onClick={onDeclineAll}>
              Dismiss all
            </Button>
          </div>
        ) : null}
      </div>
      <p className="mt-1 text-muted-foreground">
        Applies their timeline, annotations, and flags. No video is copied, and
        the covered recordings are not re-analyzed.
      </p>
      <ul className="mt-2 space-y-1.5">
        {bundleOffers.map((offer) => (
          <li key={offer.bundle_path} className="flex items-center gap-2">
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
              onClick={() => onReceiveOffer(offer)}
            >
              Receive
            </Button>
            <Button
              variant="ghost"
              size="icon-sm"
              className="size-7 shrink-0 text-muted-foreground"
              onClick={() => onDeclineOffer(offer)}
              title="Not now — stop offering this until it is re-shared"
            >
              <XIcon className="size-3.5" />
              <span className="sr-only">Dismiss this shared review</span>
            </Button>
          </li>
        ))}
      </ul>
    </div>
  )
}
