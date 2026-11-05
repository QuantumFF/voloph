"use client"

import type { MouseEvent, ReactNode } from "react"
import { createPortal } from "react-dom"
import {
  ChevronLeftIcon,
  ChevronRightIcon,
  CircleHelpIcon,
  CrosshairIcon,
  MergeIcon,
  PencilIcon,
  PlusIcon,
  ScissorsIcon,
  ShrinkIcon,
  Trash2Icon,
  ZoomInIcon,
  ZoomOutIcon,
} from "lucide-react"

import { Button } from "@/components/ui/button"
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip"
import { formatClock } from "@/lib/format"
import { cn } from "@/lib/utils"
import {
  editBlockedReasons,
  type SessionRally,
} from "@/components/recording-player-transport"
import { Kbd } from "./kbd"
import { useTimelineView } from "./use-timeline-view"

/**
 * Keep keyboard focus on the player when a toolbar button is clicked (no
 * stuck-focused button catching the next keystroke); Tab focus still works.
 */
const keepFocus = (e: MouseEvent) => e.preventDefault()

/**
 * A key hint inside a button, the same cap on every action that has a key.
 * `className` says at which row width it shows (it drops when the row is
 * short of room, so the row keeps to one line — the title still names it).
 */
function Key({ children, className }: { children: string; className: string }) {
  return <Kbd className={cn("hidden", className)}>{children}</Kbd>
}

/** Key caps in the nav row, shown once the row has room for them. */
const NAV_KEY = "@min-[52rem]/nav:inline"
/** The view controls' key caps — the row's last to get room. */
const VIEW_KEY = "@min-[52rem]/nav:inline"
/** Key caps on the edit actions, in the nav row while correcting. */
const EDIT_KEY = "@min-[44rem]/nav:inline"
/** The edit actions' labels, in the nav row while correcting. */
const EDIT_LABEL = "@min-[36rem]/nav:inline"

/**
 * A disabled button can't show its own tooltip (it takes no pointer events), so
 * the reason it's unavailable — and how to enable it — rides on a wrapper.
 */
function WhyDisabled({
  reason,
  children,
}: {
  reason: string | null
  children: ReactNode
}) {
  return (
    <span className="inline-flex" title={reason ?? undefined}>
      {children}
    </span>
  )
}

/**
 * The strip's view controls — zoom out/in (- / =), Fit (0 or Z), and back to
 * the playhead (F) — at the left of the nav row, right above the strip they
 * act on, each with its key cap once the row has room; the state lives in the
 * player (`useTimelineView`). While correcting (`compact`) they shrink to
 * icons so the edit actions share the row. The playhead button takes an
 * accent only while the playhead is actually off-screen (`playheadLost`),
 * when it's the way back; bare, its tooltip still names its key.
 */
function ViewControls({ compact }: { compact: boolean }) {
  const view = useTimelineView()
  if (!view) return null
  const playheadTitle = view.playheadLost
    ? "The playhead is off-screen — scroll back to it and follow it again (F)"
    : "Center the timeline on the playhead; it follows playback (F)"
  return (
    <div
      role="group"
      aria-label="Timeline view"
      className="flex shrink-0 items-center gap-1"
    >
      <Button
        variant="outline"
        size={compact ? "icon" : "default"}
        className={cn(!compact && "px-2")}
        onMouseDown={keepFocus}
        onClick={view.zoomOut}
        disabled={!view.canZoomOut}
        aria-label="Zoom out"
        title="Zoom the timeline out — fit more of the session on screen (- or Alt+scroll)."
      >
        <ZoomOutIcon className="size-4" />
        {compact ? null : <Key className={VIEW_KEY}>-</Key>}
      </Button>
      <Button
        variant="outline"
        size={compact ? "icon" : "default"}
        className={cn(!compact && "px-2")}
        onMouseDown={keepFocus}
        onClick={view.zoomIn}
        disabled={!view.canZoomIn}
        aria-label="Zoom in"
        title="Zoom the timeline in — see finer detail around the playhead (= or Alt+scroll)."
      >
        <ZoomInIcon className="size-4" />
        {compact ? null : <Key className={VIEW_KEY}>=</Key>}
      </Button>
      <WhyDisabled
        reason={
          view.fitted
            ? "Already fitted — the whole session is in view (0 or Z)."
            : null
        }
      >
        <Button
          variant="outline"
          size={compact ? "icon" : "default"}
          onMouseDown={keepFocus}
          onClick={view.fit}
          disabled={view.fitted}
          aria-label="Fit the whole session"
          title="Fit the whole session to the timeline's width (0 or Z)."
        >
          <ShrinkIcon className="size-4" />
          {compact ? null : (
            <>
              Fit
              <Key className={VIEW_KEY}>0</Key>
            </>
          )}
        </Button>
      </WhyDisabled>
      {/* A real tooltip, not only a title: shrunk to its crosshair this is
          the one control whose meaning and key aren't otherwise on screen. */}
      <Tooltip delayDuration={300}>
        <TooltipTrigger asChild>
          <Button
            variant="outline"
            size={compact ? "icon" : "default"}
            onMouseDown={keepFocus}
            onClick={view.jumpToPlayhead}
            disabled={!view.canJumpToPlayhead}
            aria-label={`${
              view.playheadLost
                ? "Back to the playhead"
                : "Center on the playhead"
            } (F)`}
            // Off-screen: say so, so the way back is findable.
            className={cn(
              view.playheadLost &&
                "border-primary/60 bg-primary/10 text-foreground dark:border-primary/60 dark:bg-primary/15"
            )}
          >
            <CrosshairIcon className="size-4" />
            {compact ? null : (
              <>
                <span className="hidden @min-[60rem]/nav:inline">Playhead</span>
                <Key className={NAV_KEY}>F</Key>
              </>
            )}
          </Button>
        </TooltipTrigger>
        <TooltipContent side="top" sideOffset={6}>
          {playheadTitle}
        </TooltipContent>
      </Tooltip>
    </div>
  )
}

/**
 * The timeline strip's navigation row: the view controls (zoom, Fit, back to
 * the playhead) on the left; prev/next rally, next uncertain region, and the
 * toggle into correction mode (issue #7) on the right — each with its key cap.
 *
 * In correction mode the edit actions join this same row (the `EditToolbar`
 * renders into the slot it hosts) rather than adding a row of their own, so
 * the strip — and the video above it — never change height as edit mode
 * toggles. To make room the view controls and the rally/uncertain jumps shrink
 * to icons (their keys still work and their titles name them), and the jumps
 * drop out where the row is short.
 */
export function TimelineNav({
  hasRallies,
  canPrev,
  canNext,
  uncertainCount,
  editing,
  onPrevRally,
  onNextRally,
  onNextUncertain,
  onToggleEditing,
}: {
  hasRallies: boolean
  canPrev: boolean
  canNext: boolean
  uncertainCount: number
  editing: boolean
  onPrevRally: () => void
  onNextRally: () => void
  onNextUncertain: () => void
  onToggleEditing: () => void
}) {
  // The slot the edit actions render into (a callback ref: state, not a ref).
  const setEditSlot = useTimelineView()?.setEditSlot
  // While correcting, the jumps are bare icons, and only where there's room.
  const jumps = editing ? "hidden @min-[52rem]/nav:inline-flex" : undefined
  return (
    <div className="@container/nav">
      <div className="flex h-8 flex-nowrap items-center gap-2 overflow-hidden">
        <ViewControls compact={editing} />
        <div
          className={cn(
            "ml-auto flex shrink-0 items-center",
            editing ? "gap-1.5" : "gap-2"
          )}
        >
          <Button
            variant="outline"
            size={editing ? "icon" : "default"}
            className={jumps}
            onMouseDown={keepFocus}
            onClick={onPrevRally}
            disabled={!hasRallies && !canPrev}
            aria-label="Previous rally"
            title="Jump to the previous rally ([)."
          >
            <ChevronLeftIcon className="size-4" />
            {editing ? null : (
              <span className="hidden @min-[36rem]/nav:inline">Prev rally</span>
            )}
            {editing ? null : <Key className={NAV_KEY}>[</Key>}
          </Button>
          <Button
            variant="outline"
            size={editing ? "icon" : "default"}
            className={jumps}
            onMouseDown={keepFocus}
            onClick={onNextRally}
            disabled={!hasRallies && !canNext}
            aria-label="Next rally"
            title="Jump to the next rally (])."
          >
            {editing ? null : (
              <span className="hidden @min-[36rem]/nav:inline">Next rally</span>
            )}
            <ChevronRightIcon className="size-4" />
            {editing ? null : <Key className={NAV_KEY}>]</Key>}
          </Button>
          <WhyDisabled
            reason={
              uncertainCount === 0
                ? "Nothing uncertain — the segmenter is confident about every rally."
                : null
            }
          >
            <Button
              variant="outline"
              size={editing ? "icon" : "default"}
              className={jumps}
              onMouseDown={keepFocus}
              onClick={onNextUncertain}
              disabled={uncertainCount === 0}
              aria-label="Next uncertain region"
              title="Jump to the next uncertain region — a span the segmenter doubts, worth checking (U)."
            >
              <CircleHelpIcon className="size-4" />
              {editing ? null : (
                <span className="hidden @min-[30rem]/nav:inline">
                  Next uncertain
                </span>
              )}
              {editing ? null : <Key className={NAV_KEY}>U</Key>}
            </Button>
          </WhyDisabled>
          {/* The edit actions render here while correcting (`EditToolbar`). */}
          {editing && setEditSlot ? (
            <div
              ref={setEditSlot}
              className="flex min-w-0 items-center gap-1.5"
            />
          ) : null}
          <Button
            variant={editing ? "default" : "outline"}
            onMouseDown={keepFocus}
            onClick={onToggleEditing}
            disabled={!hasRallies}
            aria-pressed={editing}
            title={
              editing
                ? "Leave correction mode (E)."
                : "Correct the draft timeline: drag rally edges, split, merge, add, or delete (E)."
            }
          >
            <PencilIcon className="size-4" />
            {editing ? "Done" : "Edit timeline"}
            <Key className={NAV_KEY}>E</Key>
          </Button>
        </div>
      </div>
    </div>
  )
}

/**
 * The correction actions shown in edit mode (issue #7): the selected-rally
 * readout and the add / split / merge / delete actions, each resolved against
 * the recording that owns the selected rally. They render into the nav row's
 * slot (see `TimelineNav`), so edit mode adds no row; outside a player (no
 * view context) they fall back to a row of their own. The readout drops its
 * parts (never truncating) where the row is short. A greyed action says why on
 * hover (`editBlockedReasons`), and Split — greyed whenever the playhead isn't
 * strictly inside the selected rally — also says so beside the readout where
 * there's room. Delete is drawn in the destructive colour.
 */
export function EditToolbar({
  selected,
  selectedIndex,
  canSplit,
  canMerge,
  onAddAtPlayhead,
  onSplit,
  onMerge,
  onDelete,
}: {
  selected: SessionRally | null
  selectedIndex: number
  canSplit: boolean
  canMerge: boolean
  onAddAtPlayhead: () => void
  onSplit: () => void
  onMerge: () => void
  onDelete: () => void
}) {
  const view = useTimelineView()
  const blocked = editBlockedReasons(
    selected ? selectedIndex + 1 : null,
    canSplit,
    canMerge
  )
  const tools = (
    <>
      <span
        className="flex min-w-0 items-center gap-1.5 overflow-hidden text-sm whitespace-nowrap text-muted-foreground"
        title={
          selected
            ? `Rally ${selectedIndex + 1} selected · ${formatClock(selected.globalStart)}–${formatClock(selected.globalEnd)}${blocked.split ? ` · ${blocked.split}` : ""}`
            : "Drag a rally's edge to adjust it; click a rally (on the strip or in the rail) to select it."
        }
      >
        {selected ? (
          <>
            <span className="hidden shrink-0 font-medium text-foreground @min-[40rem]/nav:inline">
              Rally {selectedIndex + 1}
            </span>
            {blocked.split ? (
              <span className="hidden shrink-0 items-center gap-1 text-xs @min-[64rem]/nav:flex">
                <ScissorsIcon className="size-3.5" aria-hidden />
                needs the playhead inside
              </span>
            ) : null}
          </>
        ) : (
          <span className="hidden truncate @min-[56rem]/nav:inline">
            Select a rally, or drag an edge
          </span>
        )}
      </span>
      <Button
        variant="outline"
        onMouseDown={keepFocus}
        onClick={onAddAtPlayhead}
        aria-label="Add a rally"
        title="Add a rally over a span the segmenter missed (around the playhead, in the current recording) (A)."
      >
        <PlusIcon className="size-4" />
        <span className={cn("hidden", EDIT_LABEL)}>Add</span>
        <Key className={EDIT_KEY}>A</Key>
      </Button>
      <WhyDisabled reason={blocked.split}>
        <Button
          variant="outline"
          onMouseDown={keepFocus}
          onClick={onSplit}
          disabled={!canSplit}
          aria-label="Split the selected rally"
          title="Split the selected rally in two at the playhead (S)."
        >
          <ScissorsIcon className="size-4" />
          <span className={cn("hidden", EDIT_LABEL)}>Split</span>
          <Key className={EDIT_KEY}>S</Key>
        </Button>
      </WhyDisabled>
      <WhyDisabled reason={blocked.merge}>
        <Button
          variant="outline"
          onMouseDown={keepFocus}
          onClick={onMerge}
          disabled={!canMerge}
          aria-label="Merge with the next rally"
          title="Merge the selected rally with the next one in the same recording (C)."
        >
          <MergeIcon className="size-4" />
          <span className={cn("hidden", EDIT_LABEL)}>Merge</span>
          <Key className={EDIT_KEY}>C</Key>
        </Button>
      </WhyDisabled>
      <WhyDisabled reason={blocked.delete}>
        <Button
          variant="outline"
          onMouseDown={keepFocus}
          onClick={onDelete}
          disabled={!selected}
          aria-label="Delete the selected rally"
          className="text-destructive hover:bg-destructive/10 hover:text-destructive dark:hover:bg-destructive/20"
          title="Delete the selected rally — its span becomes a gap (Delete / Backspace)."
        >
          <Trash2Icon className="size-4" />
          <span className={cn("hidden", EDIT_LABEL)}>Delete</span>
          <Key className={EDIT_KEY}>Del</Key>
        </Button>
      </WhyDisabled>
    </>
  )
  // In a player: into the nav row's slot — or nothing for the moment until
  // the row has mounted it, rather than a row that would appear and vanish.
  if (view) return view.editSlot ? createPortal(tools, view.editSlot) : null
  return (
    <div className="@container/nav">
      <div className="flex h-8 min-w-0 flex-nowrap items-center justify-end gap-1.5 overflow-hidden">
        {tools}
      </div>
    </div>
  )
}
