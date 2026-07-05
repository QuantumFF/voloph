# ffmpeg / ffprobe sidecars

The in-app player uses these binaries to probe codecs and to transcode
web-incompatible recordings (e.g. iPhone HEVC) to H.264/AAC on the fly. They are
declared in `../tauri.conf.json` under `bundle.externalBin` so Tauri bundles
them beside the app executable, and `../src/media.rs` resolves them there and
runs them directly (probe via `ffprobe`, streaming transcode via `ffmpeg`).

## Naming

Tauri resolves a sidecar by appending the Rust target triple to the configured
name. Each platform needs its own binary:

```
ffmpeg-<target-triple>[.exe]
ffprobe-<target-triple>[.exe]
```

Find the triple with `rustc -vV | grep host`.

## Getting them

The binaries are **not committed** (`ffmpeg-*` / `ffprobe-*` are gitignored);
only this README is tracked. Populate them for your host triple with:

```
scripts/fetch-sidecars.sh          # host triple, via `rustc -vV`
scripts/fetch-sidecars.sh <triple> # cross-fetch a specific target
```

The script pulls a static **LGPL** build from
[BtbN/FFmpeg-Builds](https://github.com/BtbN/FFmpeg-Builds/releases) and installs
it here under the names above. The release workflow runs the same script per
runner, so a fresh clone needs one `scripts/fetch-sidecars.sh` before its first
`tauri dev` / `tauri build`.

Other sources, if you provision by hand instead: John Van Sickle static builds
for Linux (<https://johnvansickle.com/ffmpeg/>), gyan.dev for Windows (add the
`.exe` suffix). Prefer a statically linked build so the sidecar does not depend
on system shared libraries at runtime.

## Licensing

ffmpeg ships as LGPL or GPL builds. The chosen build's license and its linkage
must be tracked before distributing (see `../../docs/adr/0004-bundle-ffmpeg-sidecar.md`).
Pick an LGPL build unless a GPL-only feature is required.
