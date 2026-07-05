# Voloph

A desktop app for reviewing badminton games you recorded yourself.

Voloph exists to make starting a review **frictionless**: point it at the videos
off your camera and it skips the downtime between points, drops you straight into
the rallies, and lets you annotate and clip the moments worth studying — all
without a single keystroke ever touching the original files.

Reviewing is **non-destructive**. Nothing you do alters a recording. Every rally
boundary, verdict, flag, and note lives as metadata layered over the footage; the
video is played in place, exactly as it came off the camera.

---

## What it does

- **Imports your footage.** Point Voloph at a folder of video files. It groups
  them into **sessions** (one outing per day) by reading each recording's
  embedded capture date, and it re-homes recordings to the right day in the
  background as it learns their real dates.
- **Finds the rallies for you.** A background segmenter analyses each recording
  and produces a first-draft **timeline** of rally intervals. The downtime
  between rallies (walking to position, chasing the shuttle, water breaks) is a
  derived **gap** and is skipped automatically during playback.
- **Lets you correct the draft inline.** Where the machine is unsure it marks an
  **uncertain region** — "check this" — so cleaning up the timeline becomes
  "visit the few spots the machine doubts" rather than scrubbing the whole video.
  Five operations fix any draft: adjust a boundary, split, merge, add, delete.
- **Captures observations in the moment.** One keystroke drops a **verdict**
  (`good` / `bad` / `mistake`) pinned to the exact timestamp, no pause required.
  Enrich it later with an **aspect** (selection, execution, deception, footwork,
  positioning — your own vocabulary) and a free-text **note**.
- **Flags the rallies that matter.** A one-keystroke **flag** marks a rally as
  reel-worthy, independent of any annotation on it.
- **Surfaces what you're looking for.** The moment browser filters rallies across
  every session at once — "show my selection mistakes", "the long rallies", "the
  flagged ones" — and opens the right recording at the right timestamp.
- **Exports clean reels.** One engine, driven by a selection of rallies: **all**
  rallies → the condensed session (gaps removed, one portable file across every
  recording of the day); **flagged** rallies → a highlight reel; a **filter**
  (e.g. rallies containing a mistake) → a targeted study reel. The source is
  never modified.

---

## Platform

**Linux only.** Playback embeds libmpv in-process, rendered into a `GtkGLArea`
overlaid on the webview (see [ADR 0008](docs/adr/0008-embedded-libmpv-playback.md)),
so any codec plays and sparse-GOP seeking is fast. Non-Linux targets build with
inert playback stubs but are not a supported runtime.

---

## Architecture

Voloph is a [Tauri 2](https://tauri.app) app: a Rust backend behind a React
frontend rendered in the system webview (WebKitGTK).

| Layer          | Stack                                                                        |
| -------------- | --------------------------------------------------------------------------- |
| Frontend       | React 19 · TypeScript · Vite · Tailwind CSS v4 · shadcn/ui · lucide-react   |
| Shell          | Tauri 2 (Rust)                                                              |
| Playback       | Embedded **libmpv** via `GtkGLArea` (Linux)                                 |
| Metadata store | **SQLite** (`rusqlite`, bundled) in the app data dir                        |
| Media pipeline | Bundled **ffmpeg / ffprobe** sidecars for probing, segmentation, and export |

### How the pieces fit

- **Everything durable is metadata in SQLite.** Recordings, sessions, the draft
  timeline (rallies + per-region confidence + waveform), annotations, and flags.
  Recordings are referenced in place — Voloph never copies or rewrites the video.
- **A background media worker** drains a work queue without ever blocking
  playback: it refines each recording's capture date, probes it, then runs
  segmentation off the DB lock. Only quick DB touches take the lock; the slow
  ffmpeg and segmentation passes run unlocked, so the timeline and player stay
  responsive. Work left unfinished by a previous run resumes on startup.
- **Segmentation is hybrid audio + visual-motion** (see
  [ADR 0006](docs/adr/0006-hybrid-audio-visual-motion-segmentation.md)): motion
  energy from frame-to-frame differencing defines rally boundaries; audio
  hit-density confirms a moving span is really a rally. It's local, GPU-free,
  heuristic (not learned), and a replaceable component behind the timeline. It
  assumes a roughly static camera (tripod or propped) — the norm for
  self-recorded court video.
- **Export cuts and concatenates** the selected rally intervals with ffmpeg,
  stitching across file boundaries for a whole-session export, using
  hardware-accelerated encoders where available. Progress is streamed to the UI.

The Tauri command surface (`src-tauri/src/lib.rs`) is the contract between
frontend and backend: `scan_folder` / `rescan_folders`, `list_sessions`,
`recording_timeline`, the timeline edits (`update_rally`, `add_rally`,
`delete_rally`), `set_rally_flag`, the annotation commands, `filter_moments`,
`export_rallies` / `export_session`, and the `mpv_*` playback controls.

---

## Review controls

Review is keyboard-driven; press `?` in the player for the live cheat-sheet
(the same keymap backs both the handler and the sheet, so they never drift).

| Keys                    | Action                                    |
| ----------------------- | ----------------------------------------- |
| `Space` / `K`           | Play / pause                              |
| `←` `→`                 | Seek ∓ 5s (`Ctrl` ∓ 2.5s, `Shift` ∓ 10s) |
| `↑` `↓`                 | Volume up / down                          |
| `,` `.`                 | Frame step back / forward                 |
| `[` `]`                 | Previous / next rally                     |
| `U`                     | Jump to next uncertain region             |
| `L`                     | Loop the current rally                    |
| `G`                     | Toggle skipping gaps                      |
| `F`                     | Jump to playhead                          |
| `M`                     | Mute                                      |
| `1` `2` `3`             | Annotate `good` / `bad` / `mistake`       |
| `X`                     | Flag / unflag the current rally           |
| `Ctrl+-` `Ctrl+=`       | Playback speed down / up (`Ctrl+0` = 1×)  |
| scroll / `Alt`+scroll   | Scroll / zoom the timeline                |

---

## Development

### Prerequisites

- [Bun](https://bun.sh) (package manager and script runner)
- [Rust](https://www.rust-lang.org/tools/install) (stable toolchain)
- Tauri's Linux system dependencies — WebKitGTK, GTK 3, and libmpv development
  packages. See the [Tauri prerequisites guide](https://tauri.app/start/prerequisites/).

`ffmpeg` / `ffprobe` are bundled as sidecars (`src-tauri/binaries/`), so no
system ffmpeg is required.

### Setup

```bash
bun install
```

### Run

```bash
bun run tauri dev      # launch the app with hot-reload
```

### Other scripts

```bash
bun run tauri build    # produce a distributable bundle
bun run build          # type-check and build the frontend only
bun run typecheck      # tsc --noEmit
bun run lint           # eslint
bun run format         # prettier --write
bun run test           # vitest
```

---

## Project layout

```
src/                          Frontend (React + TypeScript)
  App.tsx                     Root: session list ⇄ recording player ⇄ moment browser
  components/
    session-list.tsx          Sessions and their recordings
    moment-browser.tsx        Cross-session rally filter (filter_moments)
    use-export.ts             Export flow + progress
    recording-player/         Player, timeline editing, annotations, keymap
src-tauri/                    Backend (Rust / Tauri)
  src/lib.rs                  Tauri command surface + background media worker
  src/db.rs                   SQLite schema and queries
  src/segment.rs              Rally/gap segmentation
  src/media.rs                ffmpeg/ffprobe probing and extraction
  src/export.rs               Rally-selection export engine
  src/mpv.rs                  Embedded libmpv playback (Linux)
  binaries/                   Bundled ffmpeg / ffprobe sidecars
docs/
  adr/                        Architecture Decision Records
  tuning-segmentation.md      Human-in-the-loop segmenter tuning guide
CONTEXT.md                    Domain model and shared vocabulary
```

---

## Documentation

- **[CONTEXT.md](CONTEXT.md)** — the domain model and the precise language the
  project uses (session, recording, rally, gap, timeline, annotation, verdict,
  aspect, flag, export). Read this first to understand how the app thinks.
- **[docs/adr/](docs/adr/)** — Architecture Decision Records capturing the key
  design choices and why they were made: non-destructive timeline, reference-in-place
  grouping, the bundled ffmpeg sidecar, embedded libmpv playback, hybrid
  segmentation, capture-date-from-metadata, and more.
- **[docs/tuning-segmentation.md](docs/tuning-segmentation.md)** — the
  human-in-the-loop loop for tuning the segmenter against real footage.
