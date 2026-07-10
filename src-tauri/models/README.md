# Vendored detector weights (ADR 0015 Stage 2, issue #83)

The occupancy signal (ADR 0015) is derived from a single **nano-scale person
detector** run through ONNX Runtime (`ort`) over frames the ffmpeg sidecar
decodes. This directory vendors that model so it ships with the app and the
`detect-dump` dev CLI, with its provenance recorded here.

## `yolox_nano.onnx`

- **Model:** YOLOX-Nano (Megvii-BaseDetection). Anchor-free single-stage detector,
  ~0.91M params, trained on COCO (80 classes; we filter to **class 0 = person**).
- **License:** **Apache-2.0** — see `LICENSE` in this directory (verbatim copy of
  the YOLOX repository's LICENSE). Apache-2.0 is a permissive license with no
  copyleft obligation, unlike the AGPL-licensed Ultralytics YOLOs the ADR
  explicitly excludes.
- **Source project:** https://github.com/Megvii-BaseDetection/YOLOX
- **Weights download URL:**
  https://github.com/Megvii-BaseDetection/YOLOX/releases/download/0.1.1rc0/yolox_nano.onnx
- **Release tag:** `0.1.1rc0` (the release that publishes the pre-exported ONNX
  demo weights).
- **SHA-256:** `c789161ed43c8269fcd4e67c67eeeb4e80c622da2eb296a20bc6007bd18a0b7d`
- **Producer (from the ONNX header):** `pytorch 1.7`

### Input / output contract (implemented in `src/detect.rs`)

- **Input:** `1x3x416x416` float32, **BGR** channel order, **letterboxed** (aspect
  preserved, padded with `114` to a square), **no** mean/std normalization (raw
  0–255 pixel values). This matches YOLOX's `preproc` (see the YOLOX repo
  `yolox/data/data_augment.py`).
- **Output:** `1 x N x 85` where the 85 columns are `[cx, cy, w, h, obj,
  cls_0..cls_79]`. Boxes are grid/stride-decoded with strides `[8, 16, 32]`
  (`demo_postprocess` in the YOLOX repo `yolox/utils/demo_utils.py`):
  `xy = (raw_xy + grid) * stride`, `wh = exp(raw_wh) * stride`. Final score is
  `obj * cls_person`; we keep class 0 only, threshold, and run NMS.

To re-vendor: re-download from the URL above, confirm the SHA-256 matches (or
record the new one), and keep this README and `LICENSE` in sync.
