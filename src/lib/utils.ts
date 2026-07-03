import { clsx, type ClassValue } from "clsx"
import { twMerge } from "tailwind-merge"

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs))
}

/** A session's capture day (`YYYY-MM-DD`) as a human date, e.g. "Sat, Jun 28, 2026". */
export function formatCaptureDay(day: string): string {
  const date = new Date(`${day}T00:00:00`)
  if (Number.isNaN(date.getTime())) return day
  return date.toLocaleDateString(undefined, {
    weekday: "short",
    month: "short",
    day: "numeric",
    year: "numeric",
  })
}
