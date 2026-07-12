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

## `rtmpose_t.onnx` (ADR 0017, issue #95 round 4)

The **serve cue** (ADR 0017) is a nano-scale **pose** model — a *new signal
class*, not a bigger detector — run only in densified windows around candidate
span starts to decide whether a proposal shows a serve. It is observational in
round 4 (the `eval-harness --serve` mode); the shipped app never runs it until
the v6 build clears the kill criterion.

- **Model:** RTMPose-t (`rtmpose-t_simcc-body7`, OpenMMLab MMPose). Top-down
  17-keypoint (COCO body) SimCC pose estimator, ~3.4M params. Nano-class, and
  crucially **Apache-2.0** — the popular Ultralytics pose models are AGPL, the
  same trap ADR 0015/0017 dodge for the detector.
- **License:** **Apache-2.0** — see `LICENSE-rtmpose` in this directory (verbatim
  copy of the MMPose repository's LICENSE).
- **Source project:** https://github.com/open-mmlab/mmpose (RTMPose)
- **Weights download URL** (pre-exported ONNX SDK bundle):
  https://download.openmmlab.com/mmpose/v1/projects/rtmposev1/onnx_sdk/rtmpose-t_simcc-body7_pt-body7_420e-256x192-026a1439_20230504.zip
  — the vendored `rtmpose_t.onnx` is the `end2end.onnx` extracted from that zip.
- **Bundle SHA-256:** `937003a70832d9cc34ea16927f504792f3133e92dda1b9c626236bbbe9e805cb`
- **`rtmpose_t.onnx` SHA-256:** `a6c2f6a3896a4d51131d14d7a80a3d08b50f559af5a58a45d5b098aef510a70f`
- **Producer (from the ONNX header):** `pytorch 1.9`, opset 11

### Input / output contract (implemented in `src/pose.rs`)

- **Input:** `1x3x256x192` float32 (NCHW, H=256 W=192), **RGB** channel order, a
  **top-down affine crop** of one person box (MMPose `TopDownGetBboxCenterScale`
  padding `1.25`, aspect `192:256`), then ImageNet **normalization**: subtract
  mean `[123.675, 116.28, 103.53]`, divide by std `[58.395, 57.12, 57.375]`
  (RGB). Contract read from the bundle's `pipeline.json`.
- **Output:** SimCC — `simcc_x` `1 x 17 x 384` and `simcc_y` `1 x 17 x 512`. Each
  keypoint's location is `argmax` of its row divided by the `simcc_split_ratio`
  `2.0` (giving pixels in the `192 x 256` input), with the row's max as the
  keypoint confidence. Keypoints are mapped back through the crop transform into
  source-frame-normalized `[0,1]` coordinates.

To re-vendor: `scripts/fetch-pose-model.sh` downloads the bundle, verifies the
bundle SHA, extracts `end2end.onnx` to `models/rtmpose_t.onnx`, and verifies its
SHA. Keep this README and `LICENSE-rtmpose` in sync with any new weights.
