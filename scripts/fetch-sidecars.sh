#!/usr/bin/env bash
#
# Fetch self-contained (LGPL, statically linked) ffmpeg + ffprobe builds and
# install them into src-tauri/binaries/ under the names Tauri expects for a Rust
# target triple (see src-tauri/binaries/README.md). Both local dev and the
# release workflow call this so there is one source of truth.
#
# Usage:
#   scripts/fetch-sidecars.sh [target-triple]
#
# With no argument it targets the host triple (from `rustc -vV`), which is what
# CI wants on each runner. Pass a triple explicitly to cross-fetch.
set -euo pipefail

triple="${1:-$(rustc -vV | sed -n 's/^host: //p')}"
if [ -z "$triple" ]; then
  echo "error: could not determine target triple (is rustc installed?); pass one explicitly" >&2
  exit 1
fi

# BtbN publishes static GPL and LGPL builds for linux64/win64 under a rolling
# "latest" tag. We take LGPL (see ADR 0004). Pin to a dated release tag below if
# reproducible builds matter more than staying current.
base="https://github.com/BtbN/FFmpeg-Builds/releases/download/latest"

case "$triple" in
  x86_64-unknown-linux-gnu) asset="ffmpeg-master-latest-linux64-lgpl.tar.xz"; ext="" ;;
  x86_64-pc-windows-msvc)   asset="ffmpeg-master-latest-win64-lgpl.zip";      ext=".exe" ;;
  *) echo "error: no ffmpeg sidecar mapping for target triple: $triple" >&2; exit 1 ;;
esac

root="$(cd "$(dirname "$0")/.." && pwd)"
dest="$root/src-tauri/binaries"
mkdir -p "$dest"

# Relative temp dir under CWD so native extractors (7z/unzip on Windows) get a
# path they understand, avoiding git-bash <-> Windows path translation.
tmp=".ffmpeg-sidecar-tmp"
rm -rf "$tmp"; mkdir -p "$tmp"
trap 'rm -rf "$tmp"' EXIT

echo "fetching $asset for $triple ..."
curl -fL "$base/$asset" -o "$tmp/archive"

case "$asset" in
  *.tar.xz) tar -xf "$tmp/archive" -C "$tmp" ;;
  *.zip)
    if command -v unzip >/dev/null 2>&1; then
      unzip -q "$tmp/archive" -d "$tmp"
    elif command -v 7z >/dev/null 2>&1; then
      7z x -y "-o$tmp" "$tmp/archive" >/dev/null
    else
      tar -xf "$tmp/archive" -C "$tmp"  # bsdtar can unpack zip
    fi ;;
esac

bindir="$(dirname "$(find "$tmp" -type f -name "ffmpeg$ext" | head -n1)")"
if [ ! -f "$bindir/ffmpeg$ext" ] || [ ! -f "$bindir/ffprobe$ext" ]; then
  echo "error: ffmpeg/ffprobe not found in extracted archive" >&2
  exit 1
fi

cp "$bindir/ffmpeg$ext"  "$dest/ffmpeg-$triple$ext"
cp "$bindir/ffprobe$ext" "$dest/ffprobe-$triple$ext"
chmod +x "$dest/ffmpeg-$triple$ext" "$dest/ffprobe-$triple$ext" 2>/dev/null || true

echo "installed:"
ls -l "$dest/ffmpeg-$triple$ext" "$dest/ffprobe-$triple$ext"
