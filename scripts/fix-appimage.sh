#!/usr/bin/env bash
#
# Strip host-coupled audio libraries from the built Linux AppImage and repack it.
#
# Why: the app links libmpv, which pulls libpipewire/libpulse/libjack. linuxdeploy
# bundles those into the AppImage, but they must come from the host. On the target
# machine the host's libjack (pipewire-jack) resolves PipeWire symbols against
# whatever libpipewire is first on the library path; a bundled *older* libpipewire
# is missing newer symbols (e.g. pw_log_topic_register), so the app dies at launch
# with "symbol lookup error: libjack.so.0: undefined symbol". Removing the bundled
# audio stack lets the host provide a self-consistent set. See ADR 0008.
#
# Prints the path of the fixed AppImage on stdout; diagnostics go to stderr.
set -euo pipefail
log() { echo "$@" >&2; }

bundle_dir="src-tauri/target/release/bundle/appimage"
appimage="$(find "$bundle_dir" -maxdepth 1 -name '*.AppImage' | head -n1)"
if [ -z "$appimage" ]; then
  log "error: no AppImage found under $bundle_dir"
  exit 1
fi
appimage="$(cd "$(dirname "$appimage")" && pwd)/$(basename "$appimage")"
log "fixing $appimage"

export APPIMAGE_EXTRACT_AND_RUN=1  # runners may lack FUSE
work="$(mktemp -d)"
( cd "$work" && "$appimage" --appimage-extract >/dev/null )

lib="$work/squashfs-root/usr/lib"
rm -f  "$lib"/libpipewire-0.3.so* \
       "$lib"/libpulse.so* \
       "$lib"/libpulsecommon-*.so \
       "$lib"/libjack.so* "$lib"/libjacknet.so* "$lib"/libjackserver.so*
rm -rf "$lib"/pipewire-0.3 "$lib"/spa-0.2
log "stripped bundled pipewire/pulse/jack from AppDir"

curl -fsSL -o "$work/appimagetool" \
  "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage"
chmod +x "$work/appimagetool"

out="$work/fixed.AppImage"
( cd "$work" && ARCH=x86_64 ./appimagetool --appimage-extract-and-run squashfs-root "$out" >&2 )

mv -f "$out" "$appimage"
chmod +x "$appimage"
log "repacked $appimage"
echo "$appimage"
