# Vendored model weights (ADR 0015 Stage 2, issue #83; ADR 0019, issue #105)

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
round 4 (the `eval-harness --serve` mode); the v6 segmenter never runs it. Since
ADR 0019 the **teacher** segmenter runs it in the app over the whole recording for
its player features (see `teacher-v1.json`).

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

## `tracknet_v3.onnx` and `inpaintnet_v3.onnx` (ADR 0019, issue #105)

The **teacher** rally segmenter (ADR 0019) starts from a per-frame shuttlecock
track: TrackNetV3's two networks, run by `src/tracknet.rs` exactly as TrackNetV3's
own `predict.py --eval_mode nonoverlap` runs them offline
(`scripts/teacher-tracknet.py`).

- **Models:** TrackNetV3 (Chen & Wang, "TrackNetV3: Enhancing ShuttleCock Tracking
  with Augmentations and Trajectory Rectification", 2023). *TrackNet* is a U-Net
  heatmap tracker (~11.3M params, `seq_len 8`, `bg_mode 'concat'`); *InpaintNet* a
  small 1-D conv net (`seq_len 16`) that fills gaps in the predicted trajectory.
- **License:** **MIT** — see `LICENSE-tracknetv3` in this directory (verbatim copy
  of the TrackNetV3 repository's LICENSE, which explicitly covers the *pretrained
  model checkpoints*).
- **Source project:** https://github.com/qaz812345/TrackNetV3, commit `6eda442`
  (`6eda442ada1740573f200f836d93edc9a541ee86`).
- **Checkpoints:** `ckpts/TrackNet_best.pt` (SHA-256
  `df867641a02712b021f04548ff4b1208ddfdb47f629ab2094ceb978667e83b1a`) and
  `ckpts/InpaintNet_best.pt` (SHA-256
  `5749b66b8002f3ad9e0af841604004706fc796df30599e6bf01952696009688c`), from the
  checkpoint zip linked in the project README.
- **Export:** `scripts/export-tracknet-onnx.py` (torch 2.11, legacy TorchScript
  exporter, opset 17, dynamic batch); it checks each file with ONNX Runtime against
  torch on random input (max abs difference ~1e-6). TrackNet's first convolution
  is widened to 28 input channels behind an in-graph zero `Pad` of `x` (the same
  sums plus zero terms; the input stays 27 channels) because ONNX Runtime's WebGPU
  provider computes a 27-input-channel convolution wrongly (ADR 0021). Against the
  unpadded export it differs by < 1e-6 on CPU.
- **`tracknet_v3.onnx` SHA-256:** `f58666df173a461396865d07282e14d7f2cce9238564cf2bd21c5f1d1ea57e1f`
- **`inpaintnet_v3.onnx` SHA-256:** `40aa41beaf3d15aed95ea31673f540db28f210a1828d8e221384a285ff8283ee`

### Input / output contract (implemented in `src/tracknet.rs`)

- **TrackNet input `x`:** `N x 27 x 288 x 512` float32 (NCHW). 27 channels = the
  background frame then 8 consecutive frames, **RGB** each, in that order
  (`[bg_R, bg_G, bg_B, f0_R, f0_G, f0_B, …, f7_B]`), every value `u8 / 255`
  (no mean/std). The background is the per-pixel median of every frame in the
  120 s chunk, truncated to `u8`. Frames are the recording at 30 fps scaled to
  512x288.
- **TrackNet output `y`:** `N x 8 x 288 x 512` float32 sigmoid heatmaps, one per
  input frame. A frame's point is the centre of the bounding box of the largest
  (by box area) 8-connected blob of `y > 0.5`, or "not visible" when none.
- **InpaintNet inputs:** `x` `N x 16 x 2` float32 — 16 consecutive points as
  `(x / 512, y / 288)` (an invisible frame is `(0, 0)`); `m` `N x 16 x 1` float32 —
  1 for frames to inpaint (TrackNetV3's `generate_inpaint_mask`), else 0.
- **InpaintNet output `y`:** `N x 16 x 2` float32 sigmoid coordinates in the same
  normalization. Used only where `m = 1`; a point under `COOR_TH` (≈ 0.0851) on
  both axes means "not visible".

To re-vendor: run `scripts/export-tracknet-onnx.py --tracknet <checkout>` with a
python that has torch, onnx and onnxruntime, record the new SHA-256s here, and
keep `LICENSE-tracknetv3` in sync.

## student-v1.json

The learned **student** rally segmenter (ADR 0019, issues #100/#105), read by
`src/student.rs`. Not a third-party model: trained in this repo by
`scripts/student.py` (gradient-boosted trees, exported as plain JSON by
`silver-ctx.export_trees`) on the three signals v6 already computes — motion energy,
audio level + hit onsets, YOLOX person boxes (`signals-dump`) — with labels from the
dev gold files plus teacher v3 silver; the gate-v2 test recordings were never used.
Frozen as `eval/models/student-v1.json` (sha256 `72e88972…`); gate v2 GREEN (span
precision 0.991, recall ≥ 3 s 0.962, edges ≤ 1 s 81.5 %). The file carries its
feature layout (`signals`, `windows_s`); the app refuses a layout it does not compute.
Parity: the Rust port reproduces `scripts/student.py` on all 18 corpus recordings
(908/908 rallies identical).

## teacher-v1.json

The learned **teacher** rally segmenter (ADR 0019, #105), read by `src/teacher.rs`.
Trained in this repo by `scripts/silver-ctx.py train --edges` (logistic-regression
context model + logistic edge refiner, plain JSON) on the shuttle tracks the app itself
computes (`tracknet-dump`, TrackNetV3 non-overlapping mode, see `tracknet_v3.onnx`) and
the app's RTMPose-t player poses, with the gate-v2 test recordings held out. Copy kept
as `eval/models/teacher-v1-app-tracks.json`. On the (already seen) gate-v2 test set:
span precision 0.976, recall ≥ 3 s 0.971, edges ≤ 1 s 88 % (the blind result for the
same recipe on offline tracks: 0.992 / 0.971 / 87 %). Parity: identical to Python on
all 18 recordings given the same track and poses (971/971 rallies); the full in-app
pipeline matches the offline run on the test recordings (123/127 rallies identical, all
within 0.5 s).
