export interface Recording {
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

export interface Session {
  id: number
  capture_day: string
  recordings: Recording[]
}

/** A designated library and how this device reaches it (ADR 0011). */
export interface Library {
  /** "local" | "shared". */
  kind: string
  /** Where the library is mounted on this device (per-device, never shared). */
  path: string
  /** Declared locality of that mount: "local" | "network". */
  mount: string
}

/**
 * A cross-library carry-over offer (ADR 0011): the same content exists in both
 * libraries (a copy) and exactly one side has hand-touched review state. The app
 * offers — never silently — to carry that review to the other, un-touched copy.
 */
export interface CarryOffer {
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
export interface BundleOffer {
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
export interface BundleSummary {
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
export interface ReceiveResult {
  applied: number
  refused: { path: string; reason: string }[]
  conflicts: string[]
}

export interface ScanResult {
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

/**
 * A bundle receive in progress (ADR 0012): its path, the outcome of the
 * receive, which hand-touched recordings still need a keep-mine-or-take-theirs
 * choice, and how many of those the user has taken-theirs on so far. `resolved`
 * matters for the final count: an updated session's recordings arrive as
 * conflicts (they carry the review from the first receive, so they read as
 * hand-touched), so `applied` is 0 and only the resolutions count.
 */
export interface Receiving {
  bundlePath: string
  result: ReceiveResult
  resolved: number
  // Remaining bundle paths of an "Accept all" run to process after this one,
  // and the recordings already tallied earlier in that run. Both empty/zero
  // for a lone receive.
  queue: string[]
  tally: number
}

/** The per-session bundle browser: which day it is open for and its bundles. */
export interface Browsing {
  day: string
  loading: boolean
  bundles: BundleSummary[]
}

/** A share awaiting the name dialog, and whether it is the "save as" fallback. */
export interface ShareTarget {
  session: Session
  saveAs: boolean
}

export interface Toast {
  id: number
  message: string
}
