# Reference files in place; organize virtually; metadata in SQLite

The app never moves or copies the user's video files. It references each **recording** at its existing path and organizes them *virtually*: recordings are grouped into **sessions** automatically by capture date (same calendar day = one session). The grouping the user is missing becomes a property of the app's metadata, not of the disk.

Chosen over a managed library (copy/move files into app-owned folders) because recordings are hours-long and gigabytes each — copying wastes disk and import time, and moving originals is a footgun that breaks any other tool pointing at them. The user reviews inside the app, so physical reorganization buys nothing.

All metadata (sessions, timelines, rallies, annotations, flags) lives in a single **SQLite** database in the app's data directory — not JSON sidecars — because reviews are cross-recording relational queries ("all my `execution` mistakes this month").

Accepted weakness: a file moved/renamed *outside* the app breaks its path link. Mitigated by also storing file size + a quick hash to re-locate moved files rather than just reporting them missing.

This path-based model keeps a future "reference a cloud-mounted folder" feature open with no rewrite.
