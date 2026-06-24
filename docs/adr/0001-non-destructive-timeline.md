# Non-destructive timeline as the core model

The app's core is reviewing untouched recordings, not editing them. The ML and the user collaboratively produce a **timeline** (an ordered set of rallies and gaps) stored as metadata alongside each recording; playback skips gaps and jumps rally-to-rally by reading this timeline. Original files are never modified.

We chose this over destructive editing (rendering a shortened file up front) because review must start instantly — no multi-minute re-encode before watching — and because ML boundary errors must be cheap to correct (nudge a marker, not re-render). Re-encoding is slow, lossy, and storage-hungry; a timeline is a few KB.

**Export** (rendering a real cut file via ffmpeg) is retained as an optional downstream feature that consumes a timeline, not as the primary output.
