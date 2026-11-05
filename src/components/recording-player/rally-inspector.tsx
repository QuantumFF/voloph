"use client"

import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type MouseEvent,
} from "react"
import {
  ChevronRightIcon,
  CircleHelpIcon,
  FlagIcon,
  Loader2Icon,
  LocateFixedIcon,
  PanelRightCloseIcon,
  PanelRightOpenIcon,
  SkipForwardIcon,
  StickyNoteIcon,
  Trash2Icon,
  TriangleAlertIcon,
  Undo2Icon,
} from "lucide-react"

import { Button } from "@/components/ui/button"
import { formatClock, formatLength } from "@/lib/format"
import {
  ASPECTS,
  UNCERTAIN_CONFIDENCE,
  VERDICTS,
  countVerdicts,
  describeMarkBurst,
  groupAnnotations,
  type AnnotationGroup,
  type MarkBurst,
  type MomentLanding,
  type SessionAnnotation,
  type SessionRally,
  type Verdict,
} from "@/components/recording-player-transport"
import { cn } from "@/lib/utils"
import { LONG_RALLY_MS, UNCERTAIN_SOFT_BG, UNCERTAIN_TEXT } from "./constants"
import { Kbd } from "./kbd"
import { VerdictCounts, VerdictMark } from "./verdict-mark"

/** Why there's no rally to inspect (see `RallyInspector`'s empty state). */
export type InspectorEmptyReason =
  "analyzing" | "failed" | "no-rallies" | "between"

/** The rally that starts next, while the playhead waits in a gap. */
export interface UpNext {
  /** Its session-wide number (1-based, as the rail and strip show it). */
  number: number
  /** Whole seconds until its strict start, rounded up (`secondsUntil`). */
  inSeconds: number
}

/**
 * Where a verdict pressed now lands (`momentLanding` of the playhead), with the
 * exact time it's pinned to and the shared wordings.
 */
export interface Landing extends MomentLanding {
  /** The playhead, as the clock reads it ("3:34"). */
  clock: string
  /** "→ Rally 13 · 3:36", "→ Gap · 3:34 (Rally 13 lead-in)". */
  text: string
  /** The place without a time: "Rally 13", "Gap (Rally 13 lead-in)". */
  place: string
}

/**
 * A verdict button's tooltip: what it drops and where, receiver first — and,
 * in a gap, that no rally will own it (glossary: only a rally's strict span
 * owns a moment), though it stays pinned to that exact time.
 */
function verdictTitle(verdict: Verdict, landing: Landing | null): string {
  const key = `(${VERDICT_KEY[verdict]})`
  if (!landing) return `Mark ${VERDICT_LABEL[verdict]} at the playhead ${key}.`
  const where = landing.owned
    ? ""
    : ` — in a gap, so no rally owns it; it stays pinned to ${landing.clock}`
  return `Mark ${VERDICT_LABEL[verdict]} ${landing.text}${where} ${key}.`
}

/** "1 mark", "15 marks". */
function marksText(count: number): string {
  return `${count} ${count === 1 ? "mark" : "marks"}`
}

/** "Rally 13 in 2s" — or "starting" once the countdown has run out. */
function upNextText({ number, inSeconds }: UpNext): string {
  return inSeconds > 0
    ? `Rally ${number} in ${inSeconds}s`
    : `Rally ${number} starting`
}

/** The keys that drop each verdict (keymap: 1 / 2 / 3). */
const VERDICT_KEY: Record<Verdict, string> = {
  good: "1",
  bad: "2",
  mistake: "3",
}

const VERDICT_LABEL: Record<Verdict, string> = {
  good: "Good",
  bad: "Bad",
  mistake: "Mistake",
}

/**
 * Keep keyboard focus on the player when an inspector control is clicked, so a
 * mouse click never leaves a button holding focus (and catching the next Space)
 * — capturing a verdict must not interrupt the review.
 */
const keepFocus = (e: MouseEvent) => e.preventDefault()

/** The burst in one line: "Good · Bad · Mistake → Rally 1". */
function burstText(burst: MarkBurst): string {
  return `${describeMarkBurst(burst)} → ${burst.place}`
}

/** Each mark of a burst with its time, for the tooltip. */
function burstDetail(burst: MarkBurst): string {
  return burst.marks
    .map((m) => `${VERDICT_LABEL[m.verdict]} at ${formatClock(m.globalMs)}`)
    .join(", ")
}

/** What Undo removes: the latest mark of the burst. */
function undoTitle(burst: MarkBurst): string {
  const last = burst.marks[burst.marks.length - 1]
  return `Delete the last mark — ${VERDICT_LABEL[last.verdict]} at ${formatClock(last.globalMs)} (marked: ${burstDetail(burst)})`
}

/**
 * The right inspector of the studio layout (issue #48): everything about the
 * focused rally — the one playing (its padding included), or in a gap the one
 * that just ended, dimmed, with the rally up next and a jump to it. Verdict
 * capture (issue #8) is live — a click drops a `good`/`bad`/`mistake` annotation
 * at the exact playhead, and the marks made in quick succession are confirmed
 * together with an Undo. Right after a mark the inspector holds the rally it was
 * made on for a few seconds even if playback runs into the next one, so what you
 * just marked stays in view (`held`, with a way to follow the playhead now).
 * Selecting one of the listed annotations enriches it (issue #9): re-classify
 * its verdict, pick an aspect, type a note, or delete it. Flag (issue #10) marks
 * the rally as one that matters — the source material for an export reel.
 *
 * The header has a fixed shape — title, one meta line, and one status slot
 * (held / gap and up next / last rally / uncertain) that's always there — so
 * the verdict buttons never move as the playhead goes rally → gap → rally.
 *
 * In a narrow player (below 68.75rem, the `player` container) it collapses to a
 * slim column of labelled controls — flag, the three verdicts with their keys,
 * Undo, the jump to the next rally — so the video and the rail keep their room;
 * the expand button opens the full inspector beside them (pushing, never
 * overlaying: nothing may draw over the native video surface, ADR 0008).
 */
export function RallyInspector({
  rally,
  rallyNumber,
  ended,
  held,
  upNext,
  emptyReason,
  annotations,
  landing,
  burst,
  onAnnotate,
  onUndoMark,
  onJumpNext,
  onFollowPlayhead,
  onToggleFlag,
  onUpdate,
  onDelete,
}: {
  rally: SessionRally | null
  rallyNumber: number
  /** The playhead has left `rally` for the gap after it. */
  ended: boolean
  /**
   * `rally` is held on screen after a mark while the playhead has moved on
   * (into the next rally, say); `playheadPlace` says where it is now.
   */
  held: boolean
  /** The rally that starts next while the playhead is in a gap, else null. */
  upNext: UpNext | null
  /** Why there's no rally, when `rally` is null. */
  emptyReason: InspectorEmptyReason
  /** Annotations whose timestamp falls inside this rally (glossary), ordered. */
  annotations: SessionAnnotation[]
  /** Where a verdict pressed now lands (the playhead), or null without one. */
  landing: Landing | null
  /** The verdicts just dropped (shown with Undo for a few seconds), or null. */
  burst: MarkBurst | null
  /** Drop a verdict at the playhead (same path as the 1/2/3 hotkeys). */
  onAnnotate: (verdict: Verdict) => void
  /** Delete the latest verdict of the burst. */
  onUndoMark: () => void
  /** Jump to the rally up next (opens on its lead-in). */
  onJumpNext: () => void
  /** Stop holding `rally` and show the one under the playhead. */
  onFollowPlayhead: () => void
  /** Flag / unflag this rally (same path as the X hotkey). */
  onToggleFlag: () => void
  /** Enrich/re-classify an annotation (issue #9). */
  onUpdate: (
    path: string,
    id: number,
    verdict: Verdict,
    aspect: string | null,
    note: string | null
  ) => void
  /** Remove an annotation (issue #9). */
  onDelete: (path: string, id: number) => void
}) {
  // Only matters below 68.75rem, where the inspector starts collapsed.
  const [expanded, setExpanded] = useState(false)
  return (
    <aside
      className={cn(
        "flex shrink-0 border-l",
        expanded
          ? "w-60 @min-[75rem]/player:w-72"
          : "w-32 @min-[68.75rem]/player:w-60 @min-[75rem]/player:w-72"
      )}
    >
      {!expanded ? (
        <CollapsedInspector
          rally={rally}
          rallyNumber={rallyNumber}
          ended={ended}
          held={held}
          upNext={upNext}
          annotations={annotations}
          landing={landing}
          burst={burst}
          onExpand={() => setExpanded(true)}
          onAnnotate={onAnnotate}
          onUndoMark={onUndoMark}
          onJumpNext={onJumpNext}
          onFollowPlayhead={onFollowPlayhead}
          onToggleFlag={onToggleFlag}
        />
      ) : null}
      {/* The column scrolls as a whole only when it's too short for the
          annotation list to get any room; otherwise the list scrolls alone,
          under a fixed header and verdict block. */}
      <div
        className={cn(
          "min-w-0 flex-1 flex-col overflow-y-auto",
          expanded ? "flex" : "hidden @min-[68.75rem]/player:flex"
        )}
      >
        {expanded ? (
          <div className="flex shrink-0 justify-end border-b px-2 py-1 @min-[68.75rem]/player:hidden">
            <Button
              variant="ghost"
              size="sm"
              onMouseDown={keepFocus}
              onClick={() => setExpanded(false)}
              title="Collapse the inspector to give the video more room"
            >
              <PanelRightCloseIcon className="size-4" />
              Collapse
            </Button>
          </div>
        ) : null}
        {rally === null ? (
          <EmptyState
            reason={emptyReason}
            upNext={upNext}
            onJumpNext={onJumpNext}
          />
        ) : (
          <>
            <div className="shrink-0 border-b p-4">
              <div className="flex h-8 items-center justify-between gap-2">
                <h2
                  className={cn(
                    "flex min-w-0 items-baseline gap-2 font-medium whitespace-nowrap",
                    ended && "text-muted-foreground"
                  )}
                >
                  Rally {rallyNumber}
                  {ended ? (
                    <span className="truncate text-xs font-normal tabular-nums">
                      ended {formatClock(rally.globalEnd)}
                    </span>
                  ) : null}
                </h2>
                <Button
                  variant={rally.flagged ? "default" : "outline"}
                  size="sm"
                  className="h-8"
                  onMouseDown={keepFocus}
                  onClick={onToggleFlag}
                  aria-pressed={rally.flagged}
                  title={
                    rally.flagged
                      ? `Flagged — rally ${rallyNumber} is in the export reel. Click (or X) to unflag.`
                      : `Flag rally ${rallyNumber} as one that matters, for the export reel (X).`
                  }
                >
                  <FlagIcon className="size-4" />
                  {rally.flagged ? "Flagged" : "Flag"}
                  <Kbd>X</Kbd>
                </Button>
              </div>
              <p className="mt-1 truncate text-sm whitespace-nowrap text-muted-foreground tabular-nums">
                {formatClock(rally.globalStart)}–{formatClock(rally.globalEnd)}
                {" · "}
                {formatLength(rally.globalEnd - rally.globalStart)}
                {" · "}
                {rally.globalEnd - rally.globalStart >= LONG_RALLY_MS
                  ? "long"
                  : "short"}
              </p>
              {/* The status slot: always rendered at one height, whatever it
                  holds (or nothing), so the verdict block below never moves. */}
              <div className="mt-3 flex h-8 items-center">
                <StatusSlot
                  rally={rally}
                  rallyNumber={rallyNumber}
                  ended={ended}
                  held={held}
                  upNext={upNext}
                  // Where the playhead is, not who'd own a mark there:
                  // "Rally 3", "Rally 3 lead-in", "after Rally 2".
                  playheadPlace={
                    landing ? (landing.context ?? landing.receiver) : ""
                  }
                  onJumpNext={onJumpNext}
                  onFollowPlayhead={onFollowPlayhead}
                />
              </div>
            </div>

            {/* Verdict capture (issue #8): one keystroke or click drops an
                annotation at the exact playhead timestamp. One button per row,
                its key beside it, at every width. */}
            <div className="shrink-0 border-b p-4">
              <h3 className="mb-2 text-xs font-medium text-muted-foreground">
                Verdict at playhead
              </h3>
              <div className="grid gap-1.5">
                {VERDICTS.map((verdict) => (
                  <Button
                    key={verdict}
                    variant="outline"
                    className="h-9 justify-start gap-2 px-2.5"
                    onMouseDown={keepFocus}
                    onClick={() => onAnnotate(verdict)}
                    title={verdictTitle(verdict, landing)}
                  >
                    <VerdictMark verdict={verdict} />
                    <span className="min-w-0 truncate">
                      {VERDICT_LABEL[verdict]}
                    </span>
                    <Kbd className="ml-auto">{VERDICT_KEY[verdict]}</Kbd>
                  </Button>
                ))}
              </div>
              {/* Two fixed-height lines, so the list below never jumps:
                  where the next press lands — always, receiver first, and
                  flagged when no rally will own it — and apart from it, for
                  a few seconds, the marks just made, with Undo. */}
              <div
                className="mt-2 text-[13px] leading-5 text-muted-foreground tabular-nums"
                aria-live="polite"
              >
                <div className="flex h-10 items-start">
                  {landing ? <LandingText landing={landing} /> : null}
                </div>
                <div className="flex h-7 items-center gap-1.5">
                  {burst ? (
                    <>
                      <span
                        className="min-w-0 truncate"
                        title={burstDetail(burst)}
                      >
                        Marked{" "}
                        <span className="text-foreground">
                          {describeMarkBurst(burst)}
                        </span>{" "}
                        → {burst.place}
                      </span>
                      <Button
                        variant="ghost"
                        size="xs"
                        className="ml-auto"
                        onMouseDown={keepFocus}
                        onClick={onUndoMark}
                        title={undoTitle(burst)}
                      >
                        <Undo2Icon />
                        Undo
                      </Button>
                    </>
                  ) : null}
                </div>
              </div>
            </div>

            <AnnotationList
              key={rallyNumber}
              annotations={annotations}
              onUpdate={onUpdate}
              onDelete={onDelete}
            />
          </>
        )}
      </div>
    </aside>
  )
}

/** The warning colour for a mark no rally will own. */
const UNOWNED_TEXT = "text-amber-600 dark:text-amber-400"

/**
 * Where the next press lands, receiver first and always on its own line —
 * never confused with the marks just made: "Next press → Rally 13 · 3:36", or
 * in a gap "Next press → Gap · 3:34" over an amber warning that no rally will
 * own it, with the rally it's beside — never claiming a rally that won't own
 * the mark (issue 1).
 */
function LandingText({ landing }: { landing: Landing }) {
  return (
    <span className="min-w-0" title={landing.text}>
      <span className="block truncate">
        Next press →{" "}
        <span
          className={cn(
            "font-medium",
            landing.owned ? "text-foreground" : UNOWNED_TEXT
          )}
        >
          {landing.receiver}
        </span>{" "}
        · {landing.clock}
      </span>
      {!landing.owned ? (
        <span className={cn("flex items-center gap-1 text-xs", UNOWNED_TEXT)}>
          <TriangleAlertIcon className="size-3.5 shrink-0" aria-hidden />
          <span className="truncate">
            {landing.context ? `${landing.context} · ` : ""}owned by no rally
          </span>
        </span>
      ) : null}
    </span>
  )
}

/**
 * Whether a scroll box has more below its fold, for the fade that says so —
 * re-read on scroll and whenever the box or its content resizes. `mounted`
 * says whether the box is rendered, so its listeners attach when it appears.
 */
function useMoreBelow(mounted: boolean) {
  const ref = useRef<HTMLDivElement>(null)
  const [moreBelow, setMoreBelow] = useState(false)
  const measure = useCallback(() => {
    const el = ref.current
    if (!el) return
    setMoreBelow(el.scrollTop + el.clientHeight < el.scrollHeight - 1)
  }, [])
  useEffect(() => {
    const el = ref.current
    if (!el) return
    measure()
    const observer = new ResizeObserver(measure)
    observer.observe(el)
    if (el.firstElementChild) observer.observe(el.firstElementChild)
    el.addEventListener("scroll", measure, { passive: true })
    return () => {
      observer.disconnect()
      el.removeEventListener("scroll", measure)
    }
  }, [measure, mounted])
  return { ref, moreBelow }
}

/**
 * The target rally's annotations (issue 4): a header with their verdict
 * counts, then one row per verdict per second — five Goods pressed at 0:20 are
 * "Good ×5 · 0:20", expanding to the five — in a region that scrolls on its
 * own with a fade while more is below, and the hint pinned under it. Clicking
 * a single mark opens its editor (issue #9).
 */
function AnnotationList({
  annotations,
  onUpdate,
  onDelete,
}: {
  annotations: SessionAnnotation[]
  onUpdate: AnnotationUpdate
  onDelete: (path: string, id: number) => void
}) {
  const [selectedId, setSelectedId] = useState<number | null>(null)
  const [openGroup, setOpenGroup] = useState<string | null>(null)
  const { ref, moreBelow } = useMoreBelow(annotations.length > 0)
  const groups = groupAnnotations(annotations)
  const editorFor = (a: SessionAnnotation) =>
    a.id === selectedId ? (
      <AnnotationEditor
        key={a.id}
        annotation={a}
        onUpdate={onUpdate}
        onDelete={(path, id) => {
          onDelete(path, id)
          setSelectedId(null)
        }}
      />
    ) : null
  return (
    <div className="flex min-h-32 flex-1 flex-col pt-4">
      <h3 className="mb-2 flex shrink-0 items-center gap-2 px-4 text-xs font-medium text-muted-foreground">
        Annotations
        <VerdictCounts
          counts={countVerdicts(annotations)}
          className="ml-auto"
        />
      </h3>
      {annotations.length === 0 ? (
        <p className="px-4 text-sm text-muted-foreground">
          Moments you mark in this rally collect here, pinned to their
          timestamps. Click one to tag an aspect or add a note.
        </p>
      ) : (
        <>
          <div className="relative min-h-0 flex-1">
            <div ref={ref} className="h-full overflow-y-auto px-4">
              <ul className="space-y-0.5 pb-2 text-sm">
                {groups.map((group) =>
                  group.marks.length === 1 ? (
                    <li key={group.key}>
                      <MarkRow
                        annotation={group.marks[0]}
                        open={group.marks[0].id === selectedId}
                        onToggle={() =>
                          setSelectedId((id) =>
                            id === group.marks[0].id ? null : group.marks[0].id
                          )
                        }
                      />
                      {editorFor(group.marks[0])}
                    </li>
                  ) : (
                    <li key={group.key}>
                      <GroupRow
                        group={group}
                        open={openGroup === group.key}
                        onToggle={() =>
                          setOpenGroup((k) =>
                            k === group.key ? null : group.key
                          )
                        }
                      />
                      {openGroup === group.key ? (
                        <ul className="ml-3 space-y-0.5 border-l pl-1.5">
                          {group.marks.map((a) => (
                            <li key={a.id}>
                              <MarkRow
                                annotation={a}
                                open={a.id === selectedId}
                                preciseTime
                                onToggle={() =>
                                  setSelectedId((id) =>
                                    id === a.id ? null : a.id
                                  )
                                }
                              />
                              {editorFor(a)}
                            </li>
                          ))}
                        </ul>
                      ) : null}
                    </li>
                  )
                )}
              </ul>
            </div>
            {/* More below the fold: fade the list's foot into the panel. */}
            <div
              aria-hidden
              className={cn(
                "pointer-events-none absolute inset-x-0 bottom-0 h-8 bg-gradient-to-t from-background to-transparent transition-opacity",
                moreBelow ? "opacity-100" : "opacity-0"
              )}
            />
          </div>
          <p className="shrink-0 border-t px-4 py-2 text-xs text-muted-foreground">
            Click a mark to tag an aspect or add a note.
          </p>
        </>
      )}
    </div>
  )
}

/** The editor's write path (issue #9): re-classify, tag, annotate. */
type AnnotationUpdate = (
  path: string,
  id: number,
  verdict: Verdict,
  aspect: string | null,
  note: string | null
) => void

/** "0:20.4" — a mark's time to the tenth, to tell apart marks in one second. */
function preciseClock(ms: number): string {
  return `${formatClock(ms)}.${Math.floor((ms % 1000) / 100)}`
}

/** One mark of the list: verdict, aspect, note glyph and time; opens its editor. */
function MarkRow({
  annotation: a,
  open,
  preciseTime = false,
  onToggle,
}: {
  annotation: SessionAnnotation
  open: boolean
  /** Show tenths — inside a group, where every mark shares the second. */
  preciseTime?: boolean
  onToggle: () => void
}) {
  return (
    <button
      type="button"
      onClick={onToggle}
      aria-expanded={open}
      title={
        open
          ? "Close the editor"
          : "Tag an aspect, add a note, change or delete this mark"
      }
      className={cn(
        "group/mark flex w-full items-center gap-2 rounded-md px-2 py-1 text-left outline-none hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring",
        open && "bg-muted"
      )}
    >
      <VerdictMark verdict={a.verdict} />
      <span>{VERDICT_LABEL[a.verdict]}</span>
      {a.aspect ? (
        <span className="min-w-0 truncate rounded-full border px-1.5 text-[11px] leading-4 text-muted-foreground">
          {a.aspect}
        </span>
      ) : (
        <span className="hidden rounded-full border border-dashed px-1.5 text-[11px] leading-4 text-muted-foreground group-hover/mark:inline">
          + aspect
        </span>
      )}
      {a.note ? (
        <StickyNoteIcon
          className="size-3.5 shrink-0 text-muted-foreground"
          aria-label="Has a note"
        />
      ) : null}
      <span className="ml-auto text-muted-foreground tabular-nums">
        {preciseTime ? preciseClock(a.globalMs) : formatClock(a.globalMs)}
      </span>
      <ChevronRightIcon
        className={cn(
          "size-3.5 shrink-0 text-muted-foreground transition-transform",
          open && "rotate-90"
        )}
      />
    </button>
  )
}

/**
 * Several marks of one verdict in one second, folded: "Good ×5 · 0:20", with a
 * note glyph if any carries one; expands to the marks themselves.
 */
function GroupRow({
  group,
  open,
  onToggle,
}: {
  group: AnnotationGroup
  open: boolean
  onToggle: () => void
}) {
  const label = VERDICT_LABEL[group.verdict]
  const count = group.marks.length
  return (
    <button
      type="button"
      onClick={onToggle}
      aria-expanded={open}
      title={
        open
          ? `Fold the ${count} ${label} marks back into one row`
          : `${count} ${label} marks at ${formatClock(group.globalMs)} — expand to tag or note each`
      }
      className={cn(
        "flex w-full items-center gap-2 rounded-md px-2 py-1 text-left outline-none hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring",
        open && "bg-muted/60"
      )}
    >
      <VerdictMark verdict={group.verdict} />
      <span>
        {label}
        <span className="ml-1 text-muted-foreground tabular-nums">
          ×{count}
        </span>
      </span>
      {group.marks.some((a) => a.note) ? (
        <StickyNoteIcon
          className="size-3.5 shrink-0 text-muted-foreground"
          aria-label="A mark has a note"
        />
      ) : null}
      <span className="ml-auto text-muted-foreground tabular-nums">
        {formatClock(group.globalMs)}
      </span>
      <ChevronRightIcon
        className={cn(
          "size-3.5 shrink-0 text-muted-foreground transition-transform",
          open && "rotate-90"
        )}
      />
    </button>
  )
}

/**
 * The header's one-line status, by priority: the rally is held after a mark
 * (follow the playhead now); the playhead is in the gap after it (the rally up
 * next, and a jump to it — or "that was the last"); the segmenter doubts its
 * boundaries; or nothing. Its box is a fixed height either way (see caller).
 */
function StatusSlot({
  rally,
  rallyNumber,
  ended,
  held,
  upNext,
  playheadPlace,
  onJumpNext,
  onFollowPlayhead,
}: {
  rally: SessionRally
  rallyNumber: number
  ended: boolean
  held: boolean
  upNext: UpNext | null
  playheadPlace: string
  onJumpNext: () => void
  onFollowPlayhead: () => void
}) {
  if (held) {
    return (
      <Button
        variant="outline"
        className="w-full justify-between"
        onMouseDown={keepFocus}
        onClick={onFollowPlayhead}
        title={`Showing rally ${rallyNumber} for a moment so your marks stay in view. Follow the playhead now.`}
      >
        <span className="min-w-0 truncate">
          <span className="text-muted-foreground">Playhead · </span>
          {playheadPlace}
        </span>
        <span className="flex items-center gap-1 text-muted-foreground">
          Follow
          <LocateFixedIcon className="size-3.5" />
        </span>
      </Button>
    )
  }
  if (ended) {
    return upNext ? (
      <Button
        variant="outline"
        className="w-full justify-between"
        onMouseDown={keepFocus}
        onClick={onJumpNext}
        title={`In the gap after rally ${rallyNumber}. Jump to rally ${upNext.number} now.`}
      >
        <span className="min-w-0 truncate tabular-nums">
          <span className="text-muted-foreground">Gap · </span>
          {upNextText(upNext)}
        </span>
        <span className="flex items-center gap-1 text-muted-foreground">
          Jump
          <SkipForwardIcon className="size-3.5" />
        </span>
      </Button>
    ) : (
      <p className="flex h-full w-full items-center rounded-md bg-muted px-2.5 text-xs text-muted-foreground">
        That was the last rally of the session.
      </p>
    )
  }
  if (rally.confidence < UNCERTAIN_CONFIDENCE) {
    return (
      <p
        className={`flex h-full w-full items-center gap-1.5 rounded-md px-2 text-xs ${UNCERTAIN_SOFT_BG} ${UNCERTAIN_TEXT}`}
      >
        <CircleHelpIcon className="size-3.5 shrink-0" />
        <span className="truncate">Uncertain boundaries — worth a check</span>
      </p>
    )
  }
  return null
}

/**
 * The inspector folded to a slim column (narrow player): the expand button,
 * the rally and how many marks it holds, then the actions a review needs
 * mid-play, each labelled with its key — flag, the three verdicts — over
 * where the next press lands (amber when no rally will own it) and, apart from
 * it, the marks just made with their Undo (so a press here is acknowledged as
 * in the full inspector, issue 13); the jump to the rally up next; and the
 * rally's marks condensed to one line per verdict per second (issue 14),
 * each opening the full inspector to tag or note it.
 */
function CollapsedInspector({
  rally,
  rallyNumber,
  ended,
  held,
  upNext,
  annotations,
  landing,
  burst,
  onExpand,
  onAnnotate,
  onUndoMark,
  onJumpNext,
  onFollowPlayhead,
  onToggleFlag,
}: {
  rally: SessionRally | null
  rallyNumber: number
  ended: boolean
  held: boolean
  upNext: UpNext | null
  annotations: SessionAnnotation[]
  landing: Landing | null
  burst: MarkBurst | null
  onExpand: () => void
  onAnnotate: (verdict: Verdict) => void
  onUndoMark: () => void
  onJumpNext: () => void
  onFollowPlayhead: () => void
  onToggleFlag: () => void
}) {
  const annotationCount = annotations.length
  return (
    <div className="flex min-h-0 w-full flex-col gap-1.5 p-2 @min-[68.75rem]/player:hidden">
      <Button
        variant="ghost"
        size="sm"
        className="justify-start"
        onMouseDown={keepFocus}
        onClick={onExpand}
        title="Expand the inspector — rally details, annotations, aspects and notes"
      >
        <PanelRightOpenIcon className="size-4" />
        Details
      </Button>
      {rally ? (
        <>
          <p
            className="px-1 pt-1 leading-tight tabular-nums"
            title={`Rally ${rallyNumber}${ended ? " (ended)" : ""} · ${marksText(annotationCount)}`}
          >
            <span
              className={cn(
                "block truncate text-sm font-medium",
                ended && "text-muted-foreground"
              )}
            >
              Rally {rallyNumber}
            </span>
            <span className="block truncate text-xs text-muted-foreground">
              {annotationCount > 0 ? marksText(annotationCount) : "No marks"}
            </span>
          </p>
          <Button
            variant={rally.flagged ? "default" : "outline"}
            className="justify-start px-2"
            onMouseDown={keepFocus}
            onClick={onToggleFlag}
            aria-pressed={rally.flagged}
            title={
              rally.flagged
                ? `Flagged — click (or X) to unflag rally ${rallyNumber}.`
                : `Flag rally ${rallyNumber} for the export reel (X).`
            }
          >
            <FlagIcon className="size-4" />
            {rally.flagged ? "Flagged" : "Flag"}
            <Kbd className="ml-auto">X</Kbd>
          </Button>
          <div className="my-1 h-px bg-border" />
          {VERDICTS.map((verdict) => (
            <Button
              key={verdict}
              variant="outline"
              className="justify-start gap-1.5 px-2"
              onMouseDown={keepFocus}
              onClick={() => onAnnotate(verdict)}
              title={verdictTitle(verdict, landing)}
            >
              <VerdictMark verdict={verdict} />
              <span className="min-w-0 truncate">{VERDICT_LABEL[verdict]}</span>
              <Kbd className="ml-auto">{VERDICT_KEY[verdict]}</Kbd>
            </Button>
          ))}
          {/* Fixed heights, so nothing below moves as marks come and go:
              where the next press lands, then the marks just made. */}
          <div
            className="flex flex-col text-xs text-muted-foreground tabular-nums"
            aria-live="polite"
          >
            <div className="flex h-12 flex-col justify-center px-1">
              {landing ? (
                <span className="min-w-0" title={landing.text}>
                  <span className="block truncate">Next press</span>
                  <span className="block truncate">
                    →{" "}
                    <span
                      className={cn(
                        "font-medium",
                        landing.owned ? "text-foreground" : UNOWNED_TEXT
                      )}
                    >
                      {landing.receiver}
                    </span>{" "}
                    · {landing.clock}
                  </span>
                  {!landing.owned ? (
                    <span
                      className={cn("flex items-center gap-1", UNOWNED_TEXT)}
                      title={`${landing.context ? `${landing.context} — ` : ""}owned by no rally`}
                    >
                      <TriangleAlertIcon className="size-3 shrink-0" />
                      <span className="truncate">no rally owns it</span>
                    </span>
                  ) : null}
                </span>
              ) : null}
            </div>
            <div className="flex h-7 items-center">
              {burst ? (
                <Button
                  variant="ghost"
                  size="sm"
                  className="w-full justify-start px-1.5"
                  onMouseDown={keepFocus}
                  onClick={onUndoMark}
                  aria-label={`Undo — ${burstText(burst)}`}
                  title={`${undoTitle(burst)} → ${burst.place}`}
                >
                  <Undo2Icon className="size-3.5" />
                  <span className="min-w-0 truncate">
                    Undo · {describeMarkBurst(burst)}
                  </span>
                </Button>
              ) : null}
            </div>
          </div>
        </>
      ) : null}
      {held ? (
        <Button
          variant="ghost"
          size="sm"
          className="justify-start"
          onMouseDown={keepFocus}
          onClick={onFollowPlayhead}
          title="Follow the playhead now"
        >
          <LocateFixedIcon className="size-3.5" />
          Follow playhead
        </Button>
      ) : upNext && (ended || rally === null) ? (
        <Button
          variant="ghost"
          size="sm"
          className="justify-start tabular-nums"
          onMouseDown={keepFocus}
          onClick={onJumpNext}
          title={`${upNextText(upNext)} — jump now`}
        >
          <SkipForwardIcon className="size-3.5" />
          <span className="min-w-0 truncate">Rally {upNext.number}</span>
          <span className="ml-auto text-muted-foreground">
            {upNext.inSeconds}s
          </span>
        </Button>
      ) : null}
      {rally && annotationCount > 0 ? (
        <CompactAnnotationList annotations={annotations} onOpen={onExpand} />
      ) : null}
    </div>
  )
}

/**
 * The rally's marks in the slim inspector (issue 14): one line per verdict
 * per second, as the full list folds them ("Good ×5 · 0:20"), scrolling on its
 * own; any line opens the full inspector, where marks are tagged and noted.
 */
function CompactAnnotationList({
  annotations,
  onOpen,
}: {
  annotations: SessionAnnotation[]
  onOpen: () => void
}) {
  const groups = groupAnnotations(annotations)
  return (
    <div className="mt-1 flex min-h-0 flex-1 flex-col border-t pt-2">
      <h3 className="mb-1 px-1 text-[11px] font-medium text-muted-foreground">
        Marks
      </h3>
      <ul className="min-h-0 flex-1 space-y-px overflow-y-auto text-xs">
        {groups.map((group) => (
          <li key={group.key}>
            <button
              type="button"
              onMouseDown={keepFocus}
              onClick={onOpen}
              title={`${VERDICT_LABEL[group.verdict]}${group.marks.length > 1 ? ` ×${group.marks.length}` : ""} at ${formatClock(group.globalMs)} — open the inspector to tag or note it`}
              className="flex h-6 w-full items-center gap-1 rounded-md px-1 text-left outline-none hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring"
            >
              <VerdictMark verdict={group.verdict} />
              <span className="min-w-0 truncate">
                {VERDICT_LABEL[group.verdict]}
                {group.marks.length > 1 ? (
                  <span className="text-muted-foreground">
                    {" "}
                    ×{group.marks.length}
                  </span>
                ) : null}
              </span>
              {group.marks.some((a) => a.note) ? (
                <StickyNoteIcon
                  className="size-3 shrink-0 text-muted-foreground"
                  aria-label="Has a note"
                />
              ) : null}
              <span className="ml-auto shrink-0 text-muted-foreground tabular-nums">
                {formatClock(group.globalMs)}
              </span>
            </button>
          </li>
        ))}
      </ul>
    </div>
  )
}

/**
 * No rally to inspect, and why: this recording is still being analyzed (only
 * then), detection failed, nothing was detected, or the playhead is before the
 * first rally (with the jump to it).
 */
function EmptyState({
  reason,
  upNext,
  onJumpNext,
}: {
  reason: InspectorEmptyReason
  upNext: UpNext | null
  onJumpNext: () => void
}) {
  const copy: Record<
    InspectorEmptyReason,
    { title: string; body: string; icon?: typeof Loader2Icon }
  > = {
    analyzing: {
      title: "Still analyzing…",
      body: "Rallies for this recording appear here as soon as detection finishes.",
      icon: Loader2Icon,
    },
    failed: {
      title: "Couldn’t detect rallies",
      body: "Rally detection failed for this recording. Re-analyze it from the ⋯ menu in the status bar to try again.",
      icon: TriangleAlertIcon,
    },
    "no-rallies": {
      title: "No rallies detected",
      body: "Nothing in this recording looked like play. Re-analyze it from the ⋯ menu in the status bar to try again.",
    },
    between: {
      title: "Before the first rally",
      body: "Play into a rally to inspect it.",
    },
  }
  const { title, body, icon: Icon } = copy[reason]
  return (
    <div className="p-4 text-sm text-muted-foreground">
      <p className="flex items-center gap-1.5 font-medium text-foreground">
        {Icon ? (
          <Icon
            className={cn(
              "size-3.5 shrink-0",
              reason === "analyzing" && "animate-spin",
              reason === "failed" && "text-amber-500"
            )}
          />
        ) : null}
        {title}
      </p>
      {reason === "between" && upNext ? (
        <Button
          variant="outline"
          className="mt-3 w-full justify-between"
          onMouseDown={keepFocus}
          onClick={onJumpNext}
          title={`Jump to rally ${upNext.number} now.`}
        >
          <span className="tabular-nums">{upNextText(upNext)}</span>
          <span className="flex items-center gap-1 text-muted-foreground">
            Jump
            <SkipForwardIcon className="size-3.5" />
          </span>
        </Button>
      ) : (
        <p className="mt-1 tabular-nums">{body}</p>
      )}
    </div>
  )
}

/**
 * Enrich the selected annotation (issue #9): re-classify its verdict, toggle an
 * aspect from the seeded vocabulary (clicking the set one clears it), and edit a
 * note. Verdict and aspect persist on click; the note persists on blur so a full
 * sentence is one write, not one per keystroke. Delete removes it.
 */
function AnnotationEditor({
  annotation,
  onUpdate,
  onDelete,
}: {
  annotation: SessionAnnotation
  onUpdate: (
    path: string,
    id: number,
    verdict: Verdict,
    aspect: string | null,
    note: string | null
  ) => void
  onDelete: (path: string, id: number) => void
}) {
  const { path, id, verdict, aspect, note } = annotation
  const [draftNote, setDraftNote] = useState(note ?? "")
  return (
    <div className="mt-1 space-y-2 rounded-md border p-2">
      <div className="grid grid-cols-3 gap-1.5">
        {VERDICTS.map((v) => (
          <Button
            key={v}
            variant={v === verdict ? "default" : "outline"}
            size="sm"
            className="px-1"
            onClick={() => onUpdate(path, id, v, aspect, note)}
          >
            <VerdictMark verdict={v} />
            <span className="min-w-0 truncate">{VERDICT_LABEL[v]}</span>
          </Button>
        ))}
      </div>
      <div>
        <p className="mb-1 text-[11px] text-muted-foreground">Aspect</p>
        <div className="flex flex-wrap gap-1">
          {ASPECTS.map((asp) => (
            <button
              key={asp}
              type="button"
              aria-pressed={asp === aspect}
              onClick={() =>
                onUpdate(path, id, verdict, asp === aspect ? null : asp, note)
              }
              className={`rounded-full border px-2 py-0.5 text-xs ${
                asp === aspect
                  ? "border-foreground bg-foreground text-background"
                  : "text-muted-foreground hover:bg-muted"
              }`}
            >
              {asp}
            </button>
          ))}
        </div>
      </div>
      <textarea
        value={draftNote}
        onChange={(e) => setDraftNote(e.target.value)}
        onBlur={() => {
          const next = draftNote.trim() || null
          if (next !== (note ?? null)) onUpdate(path, id, verdict, aspect, next)
        }}
        placeholder="Note (optional) — shot type goes here"
        rows={2}
        className="w-full resize-none rounded-md border bg-transparent px-2 py-1.5 text-sm placeholder:text-muted-foreground/70"
      />
      <Button
        variant="ghost"
        size="sm"
        className="w-full text-destructive hover:text-destructive"
        onClick={() => onDelete(path, id)}
      >
        <Trash2Icon className="size-4" />
        Delete annotation
      </Button>
    </div>
  )
}
