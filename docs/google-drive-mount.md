# Using Google Drive as a shared library

Voloph has no Google Drive integration, and by design never will: "cloud support"
is a **mounted folder**, not a Drive API client (ADR
[0011](adr/0011-two-typed-libraries-relative-identity.md)). The app is purely
filesystem-based — libmpv and ffmpeg want real local paths for multi-gigabyte
files, so wedging OAuth and a download/cache layer in front of them is a
non-starter. Instead you mount your Drive as a normal folder with an external
tool, point Voloph's **shared library** at it, and everything downstream
(scanning, playback, export, session bundles) just works against paths.

This is what lets several people on several machines share one **shared
library** on Drive: each reaches the same folder however they like, and a
[session bundle](adr/0012-session-bundles-snapshot-handoff.md) resolves by each
recording's path *relative to that folder*, so mount locations never need to
match.

> **Linux only.** Voloph runs on Linux (see the README's Platform note), so the
> mounting tools below are the Linux ones. Google provides no official Drive
> client for Linux; the community tools here fill that gap.

## 1. Mount Google Drive as a folder

Two tools are common on Linux. **rclone** is the recommended one — its VFS cache
is what makes playback over Drive tolerable (see §4).

### rclone (recommended)

```bash
# One-time: authorise a remote named "gdrive".
rclone config          # → n(ew) → name it "gdrive" → "drive" → follow OAuth

# Mount it, with a read cache tuned for large-file playback.
mkdir -p ~/mnt/gdrive
rclone mount gdrive: ~/mnt/gdrive \
  --vfs-cache-mode full \
  --vfs-cache-max-size 20G \
  --dir-cache-time 1h &
```

`~/mnt/gdrive` now behaves like a local folder. Put your recordings in a
subfolder of it (e.g. `~/mnt/gdrive/voloph/`) — that subfolder is what you will
designate as the library.

### google-drive-ocamlfuse (alternative)

```bash
google-drive-ocamlfuse            # first run opens a browser to authorise
mkdir -p ~/mnt/gdrive
google-drive-ocamlfuse ~/mnt/gdrive
```

It has no equivalent of rclone's `--vfs-cache-mode full`, so playback is
choppier for large files; prefer rclone if you review off Drive often.

## 2. Point Voloph at the mounted folder

1. In the sessions homepage top bar, open **Libraries**.
2. Under **Shared library**, choose **Designate shared (network mount)**.
3. Pick the folder your recordings live in on the mount (e.g.
   `~/mnt/gdrive/voloph/`).

Voloph adopts every already-known recording found under that folder into the
shared library (with its review state intact), scans for new files, and makes
the shared library active.

> **Choose "network mount", not "local mount".** A FUSE/rclone Drive mount looks
> like an ordinary local disk to the filesystem — Voloph deliberately does **not**
> auto-detect locality and asks you instead (ADR 0011). Declaring it *network* is
> what turns on **staging** (§3). The only reason to pick "local mount" for a
> Drive folder is a fast, fully-cached mount where you would rather analyse in
> place — the uncommon case.

The mount folder and this local/network choice are **per-device** and never
travel in shared metadata. The machine hosting the files may declare the same
folder a *local* disk while your laptop declares its mount *network* — both are
correct.

## 3. Why "network" matters: staging

Analysis (probe + 16 kHz audio pass + 5 fps motion pass + waveform) reads each
recording in full **more than once** (`media.rs`). Streaming a multi-gigabyte
file across Drive that many times is slow and wasteful.

On a **network**-declared shared library, Voloph instead **stages** each
recording: copies it once into a bounded local scratch area, runs every analysis
pass against the local copy, writes the results to SQLite, then evicts the copy.
It copies the next file while analysing the current one, so a session far larger
than the staging budget still processes within it (see
[`staging.rs`](../src-tauri/src/staging.rs)).

- Staging is **skipped** when a session bundle already carries the analysis —
  receiving a shared review needs no probe, segmentation, or staging at all.
- The default staging budget is **8 GiB**. Override it at launch with
  `VOLOPH_STAGING_BUDGET` (bytes) if your scratch disk is tighter or roomier:
  ```bash
  VOLOPH_STAGING_BUDGET=$((16 * 1024 * 1024 * 1024)) bun run tauri dev
  ```
- A **local**-declared mount never stages — analysis runs in place against the
  original.

## 4. Playback quality over Drive

**Staging only covers analysis — playback streams the file straight off the
mount.** So playback smoothness is entirely down to your mount, not Voloph (ADR
0011 leaves this deliberately to this guide rather than making it an app
feature).

- Use **rclone with `--vfs-cache-mode full`** (as in §1). Without a VFS cache,
  seeking scrubs badly and libmpv may stall waiting on Drive.
- Give the cache real headroom — `--vfs-cache-max-size 20G` or more if you review
  long sessions, so the recording you are watching stays resident.
- Expect the **first** play of a fresh recording to buffer while rclone pulls it
  into the cache; subsequent seeks within it are then local-fast.
- If playback is still choppy, copy that day's session to your **local library**
  first, review it there, then copy it back — the local library is the home of
  the "quick same-day review of one file" journey precisely for this.

## 5. Sharing with others on the same Drive

Once everyone designates the **same** Drive folder as their shared library:

- **Share** a session to write a metadata-only bundle into the shared library.
- Others **discover** it on their next scan and can **receive** it — their review
  becomes the shared timeline/annotations/flags, with staging skipped.

Bundles reference recordings by their path *relative to the shared library
folder*, so this works no matter where each person mounted Drive. Sharing is
refused while the *local* library is active (recipients can't reach local files)
— switch to the shared library first. See ADR
[0012](adr/0012-session-bundles-snapshot-handoff.md).

## Troubleshooting

| Symptom | Likely cause / fix |
| --- | --- |
| Recordings don't appear after designating | The mount wasn't ready when you scanned. Confirm files list in a terminal (`ls ~/mnt/gdrive/voloph`), then **Refresh**. |
| Analysis re-streams the whole file each pass (slow) | The mount is declared **local**. Re-designate as **network mount** to enable staging. |
| Playback stalls or seeks badly | No VFS cache. Remount rclone with `--vfs-cache-mode full` and a generous `--vfs-cache-max-size`. |
| Staging fills the disk | Lower `VOLOPH_STAGING_BUDGET`. The area is wiped on launch, so an interrupted run leaves nothing behind. |
| Known recordings reported unresolved after a remount | The mount path changed. Re-designate the shared library at its new mount folder; recordings re-link by quick hash + size. |
