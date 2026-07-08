#!/usr/bin/env bash
#
# Fetch a Windows libmpv build and install libmpv-2.dll into src-tauri/, where
# the Windows bundle picks it up as a resource placed next to the exe (ADR
# 0014). The Rust side links it with `raw-dylib`, so this DLL is the only
# artifact needed — no import library. Both local dev (on Windows) and the
# release workflow call this so there is one source of truth.
#
# For `cargo tauri dev` on Windows the loader must also find the DLL before
# any bundle exists: copy it beside the dev exe (src-tauri/target/debug/) or
# put its directory on PATH.
#
# Usage:
#   scripts/fetch-libmpv.sh [target-triple]
#
# With no argument it targets the host triple (from `rustc -vV`), which is what
# CI wants on the Windows runner. Pass a triple explicitly to cross-fetch.
set -euo pipefail

triple="${1:-$(rustc -vV | sed -n 's/^host: //p')}"
case "$triple" in
  x86_64-pc-windows-msvc) arch="x86_64" ;;
  *) echo "error: no libmpv mapping for target triple: $triple (only Windows bundles the DLL; Linux links the system libmpv)" >&2; exit 1 ;;
esac

# zhongfly/mpv-winbuild publishes shinchiro-style Windows builds on a rolling
# schedule; the "mpv-dev" asset carries libmpv-2.dll. Resolved through the
# latest-release API, like the BtbN ffmpeg fetch tracks "latest". Pin a release
# tag here if reproducible builds matter more than staying current.
api="https://api.github.com/repos/zhongfly/mpv-winbuild/releases/latest"
auth=()
if [ -n "${GITHUB_TOKEN:-}" ]; then
  auth=(-H "Authorization: Bearer $GITHUB_TOKEN")
fi
# The [0-9] after the arch excludes the x86-64-v3 variant asset
# (mpv-dev-x86_64-v3-…), which needs AVX2 and would crash older CPUs.
url="$(curl -fsSL "${auth[@]}" "$api" \
  | grep -o "https://[^\"]*/mpv-dev-${arch}-[0-9][^\"]*\\.7z" | head -n1)"
if [ -z "$url" ]; then
  echo "error: no mpv-dev-$arch asset found in the latest zhongfly/mpv-winbuild release" >&2
  exit 1
fi

root="$(cd "$(dirname "$0")/.." && pwd)"
dest="$root/src-tauri"

# Relative temp dir under CWD so native extractors (7z on Windows) get a path
# they understand, avoiding git-bash <-> Windows path translation.
tmp=".libmpv-tmp"
rm -rf "$tmp"; mkdir -p "$tmp"
trap 'rm -rf "$tmp"' EXIT

echo "fetching $(basename "$url") for $triple ..."
curl -fL "$url" -o "$tmp/archive.7z"

if command -v 7z >/dev/null 2>&1; then
  7z x -y "-o$tmp" "$tmp/archive.7z" >/dev/null
else
  tar -xf "$tmp/archive.7z" -C "$tmp"  # bsdtar can unpack 7z
fi

dll="$(find "$tmp" -type f -name "libmpv-2.dll" | head -n1)"
if [ -z "$dll" ]; then
  echo "error: libmpv-2.dll not found in extracted archive" >&2
  exit 1
fi

cp "$dll" "$dest/libmpv-2.dll"

echo "installed:"
ls -l "$dest/libmpv-2.dll"
