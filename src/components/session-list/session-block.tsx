"use client"

import {
  AlertTriangleIcon,
  ClockIcon,
  CopyIcon,
  FlagIcon,
  FolderOpenIcon,
  ImportIcon,
  Loader2Icon,
  MoreVerticalIcon,
  PlayIcon,
  RotateCwIcon,
  Share2Icon,
  UsersIcon,
  VideoIcon,
  XIcon,
} from "lucide-react"

import { Button } from "@/components/ui/button"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuItemText,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import { fileName, formatSize, formatTimecode } from "@/lib/format"
import { REANALYZE_DESCRIPTION, REANALYZE_LABEL } from "@/lib/reanalyze-copy"
import { cn } from "@/lib/utils"

import { formatEta } from "./analysis-progress"
import {
  dayTile,
  formatFootage,
  isAnalyzing,
  isPreparing,
  isProcessing,
  plural,
  sessionStats,
  sessionTitle,
  shortPath,
} from "./recording-state"
import {
  markedFraction,
  sessionReview,
  type ReviewTally,
} from "./review-progress"
import type { CarryOffer, Recording, Session } from "./types"

/**
 * The recording row's columns — name, length, rallies, marked, size — fixed so
 * the numbers line up down a session and across every session on the page.
 * Each column always means the same thing; while the machine is still at work
 * its status spans the rallies and marked columns.
 */
const ROW_COLUMNS =
  "grid grid-cols-[minmax(0,1fr)_4.5rem_5.5rem_3.5rem_4.5rem] items-center gap-x-4"

/**
 * The rallies and marked columns: "18 rallies" and "1/18" (rallies carrying a
 * verdict) once drafted, else one status across both for what the machine is
 * doing.
 */
function RecordingStatus({
  recording,
  remainingMs,
  tally,
}: {
  recording: Recording
  remainingMs: number | undefined
  /** This recording's review state, once loaded. */
  tally: ReviewTally | undefined
}) {
  if (isPreparing(recording.probe_state)) {
    return (
      <span
        className="col-span-2 flex items-center justify-end gap-1.5 text-muted-foreground"
        title="Preparing this recording for playback…"
      >
        <Loader2Icon className="size-3.5 animate-spin" />
        Preparing…
      </span>
    )
  }
  if (recording.probe_state === "failed") {
    return (
      <span
        className="col-span-2 flex items-center justify-end gap-1.5 text-destructive"
        title="This recording could not be read for playback."
      >
        <AlertTriangleIcon className="size-3.5" />
        Can't play
      </span>
    )
  }
  if (isAnalyzing(recording)) {
    return (
      <span
        className="col-span-2 flex items-center justify-end gap-1.5 text-muted-foreground"
        title="Detecting rallies in this recording…"
      >
        <Loader2Icon className="size-3.5 animate-spin" />
        {/* A live remaining-time estimate once the pass has measured its pace
            (#81). Absent until the first ticks arrive. */}
        {remainingMs != null ? formatEta(remainingMs) : "Analyzing…"}
      </span>
    )
  }
  if (recording.segment_state === "failed") {
    return (
      <span
        className="col-span-2 flex items-center justify-end gap-1.5 text-amber-600 dark:text-amber-500"
        title="Could not analyze this recording's audio for rallies."
      >
        <AlertTriangleIcon className="size-3.5" />
        No timeline
      </span>
    )
  }
  // The same source for both columns, so "18 rallies" and "/18" never differ.
  const rallies = tally ? tally.rallies : recording.rally_count
  return (
    <>
      <span
        className="text-right text-foreground/90"
        title="Rallies in the timeline"
      >
        {plural(rallies, "rally", "rallies")}
      </span>
      {/* Empty (not a placeholder) until review state loads, so nothing
          shifts when it lands; dim while no rally is marked yet. */}
      <span
        className={cn(
          "text-right",
          tally && tally.marked > 0
            ? "text-foreground/90"
            : "text-muted-foreground/70"
        )}
        title={tally ? describeTally(tally) : undefined}
      >
        {tally ? (
          <>
            {tally.marked}
            <span className="text-muted-foreground">/{tally.rallies}</span>
          </>
        ) : null}
      </span>
    </>
  )
}

/** A tally in words, for tooltips: "5 of 18 rallies marked · 9 verdicts · 1 flagged". */
function describeTally(t: ReviewTally): string {
  const parts = [
    `${t.marked} of ${plural(t.rallies, "rally", "rallies")} marked`,
  ]
  if (t.marks > 0) parts.push(plural(t.marks, "verdict"))
  if (t.flagged > 0) parts.push(`${t.flagged} flagged`)
  return parts.join(" · ")
}

/**
 * How far a session's review has got, at a glance: "5 of 18 rallies marked"
 * (a rally is marked once it carries any verdict) over a thin progress bar.
 * Before review state loads it shows just the rally count, so nothing jumps.
 */
function ReviewMeter({
  rallies,
  tally,
}: {
  rallies: number
  tally: ReviewTally | null
}) {
  const done =
    tally !== null && tally.rallies > 0 && tally.marked >= tally.rallies
  const label =
    tally === null
      ? plural(rallies, "rally", "rallies")
      : done
        ? `All ${plural(tally.rallies, "rally", "rallies")} marked`
        : tally.marked === 0
          ? `${plural(tally.rallies, "rally", "rallies")} · none marked`
          : null
  return (
    <div
      className="hidden w-44 shrink-0 sm:block"
      title={tally ? describeTally(tally) : undefined}
    >
      <p className="text-right text-xs text-muted-foreground tabular-nums">
        {label ?? (
          <>
            <span className="font-medium text-foreground">{tally?.marked}</span>{" "}
            of {tally?.rallies} rallies marked
          </>
        )}
      </p>
      <div
        role="progressbar"
        aria-label="Rallies marked"
        aria-valuemin={0}
        aria-valuemax={tally?.rallies ?? rallies}
        aria-valuenow={tally?.marked ?? 0}
        className="mt-1.5 h-1 overflow-hidden rounded-full bg-muted-foreground/20"
      >
        <div
          className={cn(
            "h-full rounded-full transition-[width] duration-300",
            done ? "bg-emerald-500" : "bg-primary/80"
          )}
          style={{ width: `${tally ? markedFraction(tally) * 100 : 0}%` }}
        />
      </div>
    </div>
  )
}

/** One recording row: name, length, rallies/status, size, carry-over, actions. */
function RecordingRow({
  session,
  recording,
  recordingIndex,
  carryOffer,
  remainingMs,
  tally,
  onPlay,
  onCarry,
  onDismissCarry,
  onReanalyze,
  onReveal,
  onCopyPath,
}: {
  session: Session
  recording: Recording
  recordingIndex: number
  carryOffer: CarryOffer | undefined
  /** Estimated analysis time left for this recording, if being analyzed (#81). */
  remainingMs: number | undefined
  tally: ReviewTally | undefined
  onPlay: (
    recordings: { path: string }[],
    startIndex: number,
    day: string
  ) => void
  onCarry: (offer: CarryOffer) => void
  onDismissCarry: (offer: CarryOffer) => void
  /** Ask to re-analyze (the list confirms first); `flagged` is what it costs. */
  onReanalyze: (path: string, flagged: number) => void
  /** Show the file in the system file manager. */
  onReveal: (path: string) => void
  /** Copy the file's absolute path to the clipboard. */
  onCopyPath: (path: string) => void
}) {
  const processing = isProcessing(recording)
  return (
    <li className="group/row flex items-center gap-2 pr-2 hover:bg-muted/60">
      <button
        type="button"
        onClick={() =>
          onPlay(session.recordings, recordingIndex, session.capture_day)
        }
        title={`Review from ${fileName(recording.path)}`}
        className={`group/open ${ROW_COLUMNS} min-w-0 flex-1 py-2.5 pl-4 text-left text-sm tabular-nums outline-none focus-visible:bg-muted/60`}
      >
        <span className="flex min-w-0 items-center gap-3">
          {/* The camera icon turns into a play glyph on hover or keyboard
              focus: the whole row opens the session at this recording. */}
          <span className="relative size-4 shrink-0 text-muted-foreground">
            <VideoIcon className="absolute inset-0 size-4 group-hover/row:opacity-0 group-focus-visible/open:opacity-0" />
            <PlayIcon className="absolute inset-0 size-4 text-foreground opacity-0 group-hover/row:opacity-100 group-focus-visible/open:opacity-100" />
          </span>
          <span className="truncate font-medium" title={recording.path}>
            {fileName(recording.path)}
          </span>
        </span>
        <span
          className="flex items-center justify-end gap-1.5 text-muted-foreground"
          title="Recording length"
        >
          <ClockIcon className="size-3.5 opacity-60" />
          {recording.duration_ms != null
            ? formatTimecode(recording.duration_ms)
            : "—"}
        </span>
        <RecordingStatus
          recording={recording}
          remainingMs={remainingMs}
          tally={tally}
        />
        <span className="text-right text-muted-foreground" title="File size">
          {formatSize(recording.file_size)}
        </span>
      </button>
      {/* Carry-over (ADR 0011): this copy is byte-identical to one already
          reviewed in the other library, and it is un-touched here — offer to
          bring that review (timeline, flags, annotations, segments) over.
          Dismiss hides it for good. */}
      {carryOffer ? (
        <div className="flex shrink-0 items-center gap-1">
          <Button
            size="sm"
            variant="outline"
            className="border-sky-500/50 text-sky-700 hover:bg-sky-500/10 hover:text-sky-700 dark:text-sky-400 dark:hover:text-sky-400"
            onClick={() => onCarry(carryOffer)}
            title="Bring the review from your other-library copy — timeline, flags, annotations, and segments — onto this copy."
          >
            <ImportIcon className="size-3.5" />
            Carry review
          </Button>
          <Button
            variant="ghost"
            size="icon-sm"
            className="shrink-0 text-muted-foreground"
            onClick={() => onDismissCarry(carryOffer)}
            title="Dismiss this carry-over offer"
          >
            <XIcon className="size-3.5" />
            <span className="sr-only">Dismiss carry-over offer</span>
          </Button>
        </div>
      ) : null}
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button
            variant="ghost"
            size="icon-sm"
            className="shrink-0 text-muted-foreground"
            title="Recording actions"
          >
            <MoreVerticalIcon className="size-4" />
            <span className="sr-only">Recording actions</span>
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" className="w-80">
          {/* Which file this is — its name and where it lives — then the
              everyday, harmless actions; the destructive one sits last, below
              a separator. */}
          <DropdownMenuLabel className="pb-1.5">
            <span className="block truncate text-[13px] font-medium text-foreground">
              {fileName(recording.path)}
            </span>
            <span
              className="mt-0.5 block truncate text-[11px] font-normal text-muted-foreground"
              title={recording.path}
            >
              {shortPath(recording.path)}
            </span>
          </DropdownMenuLabel>
          <DropdownMenuItem onClick={() => onReveal(recording.path)}>
            <FolderOpenIcon className="size-4" />
            Show in file manager
          </DropdownMenuItem>
          <DropdownMenuItem onClick={() => onCopyPath(recording.path)}>
            <CopyIcon className="size-4" />
            Copy file path
          </DropdownMenuItem>
          <DropdownMenuSeparator />
          {/* Destructive like Re-analyze all (it discards the timeline's
              corrections and flags), so it reads as such and is confirmed.
              The player's ⋯ menu imports the same copy (lib/reanalyze-copy). */}
          <DropdownMenuItem
            variant="destructive"
            onClick={() => onReanalyze(recording.path, tally?.flagged ?? 0)}
            disabled={processing}
          >
            <RotateCwIcon className="size-4" />
            <DropdownMenuItemText
              description={
                processing
                  ? "Still being prepared or analyzed — wait for it to finish"
                  : REANALYZE_DESCRIPTION
              }
            >
              {REANALYZE_LABEL}
            </DropdownMenuItemText>
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </li>
  )
}

/**
 * One session block: a calendar tile and title, its stats and review progress,
 * the session actions (Review, shared reviews, and a ⋮ menu for sharing), and
 * the recordings it holds as aligned rows.
 */
export function SessionBlock({
  session,
  active,
  hasShared,
  carryByPath,
  remainingByRecording,
  review,
  primary,
  onPlay,
  onBrowseBundles,
  onShare,
  onCarry,
  onDismissCarry,
  onReanalyze,
  onReveal,
  onCopyPath,
}: {
  session: Session
  active: string
  /** Whether a shared library is designated on this device at all. */
  hasShared: boolean
  carryByPath: Map<string, CarryOffer>
  /** Per-recording analysis-time estimate (ms left), keyed by recording id (#81). */
  remainingByRecording: Map<number, number>
  /** Review state per recording id; null until it loads. */
  review: Map<number, ReviewTally> | null
  /**
   * Whether this session's Review button carries the page's one primary
   * emphasis — the most recent session with rallies still unmarked.
   */
  primary: boolean
  onPlay: (
    recordings: { path: string }[],
    startIndex: number,
    day: string
  ) => void
  onBrowseBundles: (day: string) => void
  onShare: (session: Session, saveAs: boolean) => void
  onCarry: (offer: CarryOffer) => void
  onDismissCarry: (offer: CarryOffer) => void
  onReanalyze: (path: string, flagged: number) => void
  onReveal: (path: string) => void
  onCopyPath: (path: string) => void
}) {
  const stats = sessionStats(session)
  const tile = dayTile(session.capture_day)
  const inShared = active === "shared"
  const tally = review ? sessionReview(session, review) : null
  return (
    <section className="overflow-hidden rounded-lg border bg-card/40">
      {/* Right padding matches the recording rows' so the header's ⋮ sits in
          the same column as theirs. */}
      <div className="flex items-center gap-4 py-3 pr-2 pl-4">
        {tile ? (
          <div
            aria-hidden
            className="flex size-11 shrink-0 flex-col items-center justify-center rounded-md border bg-background leading-none"
          >
            <span className="text-[10px] font-medium tracking-wider text-muted-foreground">
              {tile.month}
            </span>
            <span className="mt-0.5 text-lg font-semibold tabular-nums">
              {tile.day}
            </span>
          </div>
        ) : null}
        <div className="min-w-0 flex-1">
          <h3 className="truncate font-semibold" title={session.capture_day}>
            {sessionTitle(session.capture_day)}
          </h3>
          <p className="mt-0.5 flex flex-wrap items-center gap-x-1.5 text-xs text-muted-foreground tabular-nums">
            <span>{plural(stats.recordings, "recording")}</span>
            {stats.durationMs > 0 ? (
              <>
                <span aria-hidden>·</span>
                <span>{formatFootage(stats.durationMs)} of footage</span>
              </>
            ) : null}
            {stats.processing > 0 ? (
              <>
                <span aria-hidden>·</span>
                <span className="flex items-center gap-1">
                  <Loader2Icon className="size-3 animate-spin" />
                  analyzing {stats.processing}
                </span>
              </>
            ) : null}
            {tally && tally.marks > 0 ? (
              <>
                <span aria-hidden>·</span>
                <span className="text-foreground/90">
                  {plural(tally.marks, "verdict")}
                </span>
              </>
            ) : null}
            {tally && tally.flagged > 0 ? (
              <>
                <span aria-hidden>·</span>
                <span className="flex items-center gap-1 text-foreground/90">
                  <FlagIcon
                    aria-hidden
                    className="size-3 fill-sky-400 text-sky-500"
                  />
                  {tally.flagged} flagged
                </span>
              </>
            ) : null}
          </p>
        </div>
        {stats.rallies > 0 || (tally && tally.rallies > 0) ? (
          <ReviewMeter rallies={stats.rallies} tally={tally} />
        ) : null}
        <div className="flex shrink-0 items-center gap-1.5">
          {/* Shared reviews for this session: browse every bundle shared for
              this day, including ones already received or dismissed, and
              re-receive any. Only the shared library holds bundles, so outside
              it the button is not shown at all rather than sitting inert. */}
          {inShared ? (
            <Button
              variant="ghost"
              size="sm"
              onClick={() => onBrowseBundles(session.capture_day)}
              title="Reviews other people shared for this session"
            >
              <UsersIcon className="size-3.5" />
              Shared reviews
            </Button>
          ) : null}
          {/* One filled Review per page — the next session waiting (see
              nextToReview) — so the eye lands on it; every other session's is
              outline, quieter but the same size and place. The filled one
              says why it is singled out: it starts or continues a review. */}
          {/* A fixed minimum width so the review meter beside it stays in one
              column whichever label the button carries. */}
          <Button
            size="sm"
            variant={primary ? "default" : "outline"}
            className="min-w-34"
            onClick={() => onPlay(session.recordings, 0, session.capture_day)}
            title={
              primary
                ? "Up next: the newest session with rallies still unmarked. Plays every rally, back-to-back."
                : "Review the whole session — every rally, back-to-back."
            }
          >
            <PlayIcon className="size-3.5" />
            {primary
              ? tally && tally.marked > 0
                ? "Continue review"
                : "Start review"
              : "Review session"}
          </Button>
          {/* The session's only actions are sharing, so the trigger says so —
              a ⋮ here read the same as the recording ⋮ right below it. */}
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button
                variant="ghost"
                size="icon-sm"
                className="text-muted-foreground"
                title="Share this session's review"
              >
                <Share2Icon className="size-4" />
                <span className="sr-only">Share this session's review</span>
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end" className="w-72">
              <DropdownMenuLabel>Share this session's review</DropdownMenuLabel>
              {/* Sharing into the shared library is only meaningful there —
                  recipients cannot reach local files (ADR 0012). A disabled
                  item says why, since it can't show a hover tooltip. */}
              <DropdownMenuItem
                onClick={() => onShare(session, false)}
                disabled={!inShared}
              >
                <Share2Icon className="size-4" />
                <DropdownMenuItemText
                  description={
                    inShared
                      ? "Others who reach the shared library can receive it"
                      : hasShared
                        ? "Switch to the shared library to share there"
                        : "Needs a shared library — designate one under Libraries"
                  }
                >
                  Share to shared library…
                </DropdownMenuItemText>
              </DropdownMenuItem>
              <DropdownMenuItem onClick={() => onShare(session, true)}>
                <FolderOpenIcon className="size-4" />
                <DropdownMenuItemText description="Write the review to a bundle file anywhere">
                  Save bundle as…
                </DropdownMenuItemText>
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      </div>
      <ul className="divide-y border-t">
        {session.recordings.map((recording, recordingIndex) => (
          <RecordingRow
            key={recording.id}
            session={session}
            recording={recording}
            recordingIndex={recordingIndex}
            carryOffer={carryByPath.get(recording.path)}
            remainingMs={remainingByRecording.get(recording.id)}
            tally={review?.get(recording.id)}
            onPlay={onPlay}
            onCarry={onCarry}
            onDismissCarry={onDismissCarry}
            onReanalyze={onReanalyze}
            onReveal={onReveal}
            onCopyPath={onCopyPath}
          />
        ))}
      </ul>
    </section>
  )
}
