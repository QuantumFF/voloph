"use client"

import {
  ChevronLeftIcon,
  ChevronRightIcon,
  PencilIcon,
  PlusIcon,
  ScissorsIcon,
  Trash2Icon,
  TriangleAlertIcon,
} from "lucide-react"

import { Button } from "@/components/ui/button"
import { fileName, formatClock } from "@/lib/format"
import type { SessionRally } from "@/components/recording-player-transport"

/**
 * The timeline strip's navigation row: prev/next rally, next uncertain region,
 * and the toggle into correction mode (issue #7).
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
  return (
    <div className="flex flex-wrap items-center justify-end gap-2">
      <div className="flex items-center gap-2">
        <Button
          variant="outline"
          size="sm"
          onClick={onPrevRally}
          disabled={!hasRallies && !canPrev}
          title="Jump to the previous rally."
        >
          <ChevronLeftIcon className="size-4" />
          Prev rally
        </Button>
        <Button
          variant="outline"
          size="sm"
          onClick={onNextRally}
          disabled={!hasRallies && !canNext}
          title="Jump to the next rally."
        >
          Next rally
          <ChevronRightIcon className="size-4" />
        </Button>
        <Button
          variant="outline"
          size="sm"
          onClick={onNextUncertain}
          disabled={uncertainCount === 0}
          title="Jump to the next uncertain region — a span the segmenter doubts, worth checking."
        >
          <TriangleAlertIcon className="size-4" />
          Next uncertain
        </Button>
        <Button
          variant={editing ? "default" : "outline"}
          size="sm"
          onClick={onToggleEditing}
          disabled={!hasRallies}
          title="Correct the draft timeline: drag rally edges, split, merge, add, or delete."
        >
          <PencilIcon className="size-4" />
          {editing ? "Done editing" : "Edit timeline"}
        </Button>
      </div>
    </div>
  )
}

/**
 * The correction toolbar shown in edit mode (issue #7): the selected-rally
 * readout and the add / split / merge / delete actions, each resolved against
 * the recording that owns the selected rally.
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
  return (
    <div className="flex flex-wrap items-center gap-2 rounded-md border border-dashed bg-muted/40 px-3 py-2 text-sm text-muted-foreground">
      <span>
        {selected
          ? `Rally ${selectedIndex + 1} selected (${formatClock(
              selected.globalStart
            )}–${formatClock(selected.globalEnd)} · ${fileName(selected.path)})`
          : "Drag a rally's edge to adjust it, or click a rally to select it."}
      </span>
      <div className="ml-auto flex flex-wrap items-center gap-2">
        <Button
          variant="outline"
          size="sm"
          onClick={onAddAtPlayhead}
          title="Add a rally over a span the segmenter missed (around the playhead, in the current recording)."
        >
          <PlusIcon className="size-4" />
          Add at playhead
        </Button>
        <Button
          variant="outline"
          size="sm"
          onClick={onSplit}
          disabled={!canSplit}
          title="Split the selected rally in two at the playhead."
        >
          <ScissorsIcon className="size-4" />
          Split at playhead
        </Button>
        <Button
          variant="outline"
          size="sm"
          onClick={onMerge}
          disabled={!canMerge}
          title="Merge the selected rally with the next one in the same recording."
        >
          Merge with next
        </Button>
        <Button
          variant="outline"
          size="sm"
          onClick={onDelete}
          disabled={!selected}
          title="Delete the selected rally (its span becomes a gap)."
        >
          <Trash2Icon className="size-4" />
          Delete
        </Button>
      </div>
    </div>
  )
}
