#!/usr/bin/env bash
#
# Fetch the vendored RTMPose-t serve-cue pose model (ADR 0017, issue #95 round 4)
# and install it as src-tauri/models/rtmpose_t.onnx. OpenMMLab publishes a
# pre-exported ONNX SDK bundle (a zip); we download it, verify its SHA-256,
# extract the `end2end.onnx`, and verify the extracted model's SHA-256. See
# src-tauri/models/README.md for provenance and the input/output contract.
#
# Usage: scripts/fetch-pose-model.sh
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
models_dir="$here/src-tauri/models"
url="https://download.openmmlab.com/mmpose/v1/projects/rtmposev1/onnx_sdk/rtmpose-t_simcc-body7_pt-body7_420e-256x192-026a1439_20230504.zip"
bundle_sha="937003a70832d9cc34ea16927f504792f3133e92dda1b9c626236bbbe9e805cb"
model_sha="a6c2f6a3896a4d51131d14d7a80a3d08b50f559af5a58a45d5b098aef510a70f"
onnx_in_zip="rtmpose-t_simcc-body7_pt-body7_420e-256x192-026a1439_20230504/end2end.onnx"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "downloading RTMPose-t bundle…"
curl -sL -o "$tmp/bundle.zip" "$url"

got="$(sha256sum "$tmp/bundle.zip" | cut -d' ' -f1)"
if [ "$got" != "$bundle_sha" ]; then
  echo "error: bundle SHA-256 mismatch: got $got, want $bundle_sha" >&2
  exit 1
fi

# The onnx lives under a dated top-level dir inside the zip; find it rather than
# hard-coding the timestamp folder.
unzip -q -o "$tmp/bundle.zip" -d "$tmp/x"
src="$(find "$tmp/x" -type f -name end2end.onnx -path "*$onnx_in_zip" | head -n1)"
if [ -z "$src" ]; then
  echo "error: end2end.onnx not found in bundle" >&2
  exit 1
fi

mkdir -p "$models_dir"
cp "$src" "$models_dir/rtmpose_t.onnx"

got="$(sha256sum "$models_dir/rtmpose_t.onnx" | cut -d' ' -f1)"
if [ "$got" != "$model_sha" ]; then
  echo "error: rtmpose_t.onnx SHA-256 mismatch: got $got, want $model_sha" >&2
  exit 1
fi

echo "installed $models_dir/rtmpose_t.onnx (sha256 ok)"
