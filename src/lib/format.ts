/**
 * Shared display formatters. One home for the small string helpers the
 * library and the player both need, so they cannot drift apart per file.
 */

/** In-rally clock for the rail and inspector: `m:ss`. */
export function formatClock(ms: number): string {
  const total = Math.round(ms / 1000)
  const m = Math.floor(total / 60)
  const s = total % 60
  return `${m}:${s.toString().padStart(2, "0")}`
}

/**
 * Session-global timecode for the transport bar: `mm:ss` under an hour,
 * `h:mm:ss` past it, so the position display reads naturally for both a short
 * clip and a long session.
 */
export function formatTimecode(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000))
  const h = Math.floor(total / 3600)
  const m = Math.floor((total % 3600) / 60)
  const s = total % 60
  if (h > 0) {
    return `${h}:${m.toString().padStart(2, "0")}:${s.toString().padStart(2, "0")}`
  }
  return `${m}:${s.toString().padStart(2, "0")}`
}

/** Coarse footage length for the session stats line: `Nm` or `Nh MMm`. */
export function formatDuration(ms: number): string {
  // Round to whole minutes first so 59m30s carries into the hour instead of
  // rendering as "60m" / "1h 60m".
  const totalMinutes = Math.round(ms / 60000)
  const h = Math.floor(totalMinutes / 60)
  const m = totalMinutes % 60
  if (h > 0) return `${h}h ${m.toString().padStart(2, "0")}m`
  return `${m}m`
}

/** Human-readable file size: `0 B`, `12 B`, `3.4 MB`, … */
export function formatSize(bytes: number): string {
  if (bytes <= 0) return "0 B"
  const units = ["B", "KB", "MB", "GB", "TB"]
  const i = Math.min(
    Math.floor(Math.log(bytes) / Math.log(1024)),
    units.length - 1
  )
  return `${(bytes / 1024 ** i).toFixed(i === 0 ? 0 : 1)} ${units[i]}`
}

/** The last path segment, tolerating both `/` and `\` separators. */
export function fileName(path: string): string {
  const parts = path.split(/[\\/]/)
  return parts[parts.length - 1] || path
}
