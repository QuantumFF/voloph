# Non-destructive timeline as the core model

The app's core is reviewing untouched recordings, not editing them. The ML and the user collaboratively produce a **timeline** (an ordered set of rallies and gaps) stored as metadata alongside each recording; playback skips gaps and jumps rally-to-rally by reading this timeline. Review never modifies a file. The one deliberate exception is a single, one-time codec normalization at import — a web-incompatible recording is transcoded in place to a playable codec (see ADR 0005) — which is import housekeeping, not editing; nothing the user does while reviewing alters the file.

We chose this over destructive editing (rendering a shortened file up front) because review must start instantly — no multi-minute re-encode before watching — and because ML boundary errors must be cheap to correct (nudge a marker, not re-render). Re-encoding is slow, lossy, and storage-hungry; a timeline is a few KB.

**Export** (rendering a real cut file via ffmpeg) is retained as an optional downstream feature that consumes a timeline, not as the primary output.
