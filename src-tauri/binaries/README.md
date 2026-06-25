# ffmpeg / ffprobe sidecars

The in-app player uses these binaries to probe codecs and to transcode
web-incompatible recordings (e.g. iPhone HEVC) to H.264/AAC on the fly. They are
declared in `../tauri.conf.json` under `bundle.externalBin` and invoked from
`../src/media.rs` via the Tauri shell plugin's sidecar API.

## Naming

Tauri resolves a sidecar by appending the Rust target triple to the configured
name. Each platform needs its own binary:

```
ffmpeg-<target-triple>[.exe]
ffprobe-<target-triple>[.exe]
```

Find the triple with `rustc -vV | grep host`. The committed binaries cover
`x86_64-unknown-linux-gnu`.

## Sourcing per platform

Drop a static (or self-contained) ffmpeg/ffprobe build for the target into this
directory under the names above:

- **Linux** `x86_64-unknown-linux-gnu` — e.g. John Van Sickle's static builds
  (<https://johnvansickle.com/ffmpeg/>), or copy the system `ffmpeg`/`ffprobe`.
- **macOS** `aarch64-apple-darwin` / `x86_64-apple-darwin` — e.g.
  <https://evermeet.cx/ffmpeg/>.
- **Windows** `x86_64-pc-windows-msvc` — e.g. gyan.dev builds; add the `.exe`
  suffix.

Prefer a statically linked build so the sidecar does not depend on system
shared libraries at runtime.

## Licensing

ffmpeg ships as LGPL or GPL builds. The chosen build's license and its linkage
must be tracked before distributing (see `../../docs/adr/0004-bundle-ffmpeg-sidecar.md`).
Pick an LGPL build unless a GPL-only feature is required.
