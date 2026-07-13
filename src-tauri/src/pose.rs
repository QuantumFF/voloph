//! Serve-cue **pose extraction** (ADR 0017, issue #95 round 4): a nano RTMPose-t
//! keypoint estimator run only in *densified windows* around candidate span starts,
//! to decide whether a proposal shows a serve. This is a deliberately **new signal
//! class** — 0/22 v5 residual misses trace to detector absence, so a bigger detector
//! stays closed (ADR 0015); posture is the fresh axis. The module is observational:
//! nothing here runs on the shipped app path until the gated v6 build clears the
//! kill criterion (`eval-harness --serve` is the only caller).
//!
//! **The model.** RTMPose-t (`rtmpose-t_simcc-body7`, OpenMMLab MMPose), a top-down
//! 17-keypoint SimCC estimator, vendored under `models/rtmpose_t.onnx` (Apache-2.0 —
//! the popular Ultralytics pose models are AGPL) and run through `ort`, exactly like
//! the YOLOX detector in [`crate::detect`]. It takes a `1x3x256x192` **RGB**,
//! ImageNet-normalized, top-down crop of one person box (MMPose center-scale, pad
//! `1.25`, aspect `192:256`) and emits SimCC — `simcc_x` `1x17x384` and `simcc_y`
//! `1x17x512`; each keypoint is the `argmax` of its row over the split ratio `2.0`
//! (pixels in the `192x256` input), the row max its confidence. See
//! `models/README.md` for the full contract.
//!
//! **The pipeline.** For each densified frame we letterbox the source frame to the
//! detector's `416` input in Rust (matching ffmpeg's `scale`+`pad`), run YOLOX to get
//! the person boxes, take the two highest-scored (singles: two players on court), and
//! run the pose model on a top-down crop of each. Keypoints are mapped back through
//! the crop transform into **source-frame-normalized** `[0,1]` coordinates, so the
//! serve-cue heuristic downstream is independent of resolution and crop size.
//!
//! **Testing (mirroring ADR 0015).** Inference is judged by the eval harness on real
//! footage, not unit tests; the pure geometry here — the crop transform, the SimCC
//! decode, bilinear sampling, the letterbox — carries unit tests, being ordinary
//! arithmetic with no model weights involved.

use std::io::Read as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use ort::session::{Session, SessionInputValue};
use ort::value::Tensor;

use crate::detect::{self, Box, Letterbox, MODEL_SIZE, PAD_VALUE, SCORE_THRESHOLD};
use crate::media::sidecar_path;

/// Pose model input width (MMPose `256x192` → W=192).
const POSE_W: usize = 192;
/// Pose model input height (H=256).
const POSE_H: usize = 256;
/// ImageNet channel means (RGB) the model was normalized with.
const MEAN: [f32; 3] = [123.675, 116.28, 103.53];
/// ImageNet channel std-devs (RGB).
const STD: [f32; 3] = [58.395, 57.12, 57.375];
/// COCO body keypoint count.
pub const NUM_KP: usize = 17;
/// SimCC x-axis bin count (`POSE_W * split_ratio`).
const SIMCC_X_LEN: usize = 384;
/// SimCC y-axis bin count (`POSE_H * split_ratio`).
const SIMCC_Y_LEN: usize = 512;
/// SimCC split ratio: a decoded bin index divided by this gives the input-pixel coord.
const SPLIT_RATIO: f32 = 2.0;
/// MMPose top-down crop padding around the person box.
const CROP_PAD: f32 = 1.25;
/// Players kept per frame (singles: two on court). The two highest-scored person
/// boxes; the serve-cue heuristic looks for a serve posture on *either*.
const MAX_PLAYERS: usize = 2;

// ── COCO-17 keypoint indices (the serve-cue heuristic names these) ──────────────
pub const KP_L_SHOULDER: usize = 5;
pub const KP_R_SHOULDER: usize = 6;
pub const KP_L_ELBOW: usize = 7;
pub const KP_R_ELBOW: usize = 8;
pub const KP_L_WRIST: usize = 9;
pub const KP_R_WRIST: usize = 10;
pub const KP_L_HIP: usize = 11;
pub const KP_R_HIP: usize = 12;
pub const KP_L_ANKLE: usize = 15;
pub const KP_R_ANKLE: usize = 16;

/// One decoded keypoint in **source-frame-normalized** coordinates: `x` a fraction of
/// source width, `y` of source height, `score` the SimCC confidence in `[0,1]`.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Keypoint {
    pub x: f32,
    pub y: f32,
    pub score: f32,
}

/// One person's pose at one instant: the detector box it was cropped from (source-
/// normalized, carrying the detector score) and its 17 COCO keypoints.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PlayerPose {
    pub bbox: Box,
    pub keypoints: [Keypoint; NUM_KP],
}

impl PlayerPose {
    /// Vertical centre of the person box, `[0,1]` of source height — the near/far
    /// court split axis (a camera from one baseline puts the near court low in frame).
    pub fn box_center_y(&self) -> f32 {
        self.bbox.y + self.bbox.h / 2.0
    }
}

/// The players' poses at one densified frame. `t_ms` is the frame's position in the
/// recording (window start + frame offset), so callers can key it to a span edge.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PoseFrame {
    pub t_ms: i64,
    pub players: Vec<PlayerPose>,
}

/// A densified-window pose track: the per-frame poses across one window, at `fps`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PoseWindow {
    pub fps: f64,
    pub frames: Vec<PoseFrame>,
}

/// A loaded pose estimator: the `ort` session and its input tensor name. Built like
/// [`crate::detect::Detector`] — GPU execution providers probed ahead of CPU, silent
/// fallback to CPU where no GPU is present.
pub struct PoseEstimator {
    session: Session,
    input_name: String,
}

impl PoseEstimator {
    /// Load the vendored RTMPose-t model into an inference session. Errors only when
    /// the model file itself cannot be loaded, never for a missing GPU.
    pub fn load(model_path: &std::path::Path) -> Result<PoseEstimator, String> {
        let session = Session::builder()
            .map_err(|e| format!("ort session builder: {e}"))?
            .with_execution_providers(detect::gpu_execution_providers())
            .map_err(|e| format!("ort execution providers: {e}"))?
            .with_intra_threads(4)
            .map_err(|e| format!("ort intra threads: {e}"))?
            .commit_from_file(model_path)
            .map_err(|e| format!("ort could not load {}: {e}", model_path.display()))?;
        let input_name = session
            .inputs
            .first()
            .map(|i| i.name.clone())
            .ok_or("pose model has no inputs")?;
        Ok(PoseEstimator { session, input_name })
    }

    /// Run the pose model on one prepared `3x256x192` RGB-normalized CHW crop and
    /// decode the 17 keypoints in **crop-input pixel** coordinates (`x` in `0..192`,
    /// `y` in `0..256`) with a confidence in `[0,1]`. The caller maps the pixels back
    /// to source-normalized space through the [`CropRect`] that built the input.
    fn infer_crop(&mut self, chw: Vec<f32>) -> Result<[(f32, f32, f32); NUM_KP], String> {
        debug_assert_eq!(chw.len(), 3 * POSE_H * POSE_W);
        let shape = vec![1i64, 3, POSE_H as i64, POSE_W as i64];
        let tensor = Tensor::from_array((shape, chw)).map_err(|e| format!("ort tensor: {e}"))?;
        let input: SessionInputValue<'_> = tensor.into();
        let outputs = self
            .session
            .run(vec![(self.input_name.clone(), input)])
            .map_err(|e| format!("ort inference: {e}"))?;
        if outputs.len() < 2 {
            return Err(format!("pose model gave {} outputs, expected 2", outputs.len()));
        }
        // Assign the two SimCC heads by their last dim (x=384, y=512) rather than by
        // position, so the decode is robust to export output ordering.
        let (sa_shape, sa) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("ort output 0: {e}"))?;
        let (sb_shape, sb) = outputs[1]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("ort output 1: {e}"))?;
        let sa_last = *sa_shape.last().unwrap_or(&0) as usize;
        let sb_last = *sb_shape.last().unwrap_or(&0) as usize;
        let (simcc_x, simcc_y) = if sa_last == SIMCC_X_LEN && sb_last == SIMCC_Y_LEN {
            (sa, sb)
        } else if sa_last == SIMCC_Y_LEN && sb_last == SIMCC_X_LEN {
            (sb, sa)
        } else {
            return Err(format!(
                "unexpected SimCC output dims {sa_last} / {sb_last} (want {SIMCC_X_LEN} / {SIMCC_Y_LEN})"
            ));
        };
        Ok(decode_simcc(simcc_x, simcc_y))
    }
}

/// Decode the two SimCC heads into 17 `(x_px, y_px, score)` triples. Pure: per
/// keypoint, `argmax` of the x-row and y-row over [`SPLIT_RATIO`] give the input-pixel
/// coordinates, and the mean of the two row maxima is the confidence. Split out for a
/// unit test.
fn decode_simcc(simcc_x: &[f32], simcc_y: &[f32]) -> [(f32, f32, f32); NUM_KP] {
    let mut out = [(0.0f32, 0.0f32, 0.0f32); NUM_KP];
    for (k, slot) in out.iter_mut().enumerate() {
        let (ix, vx) = argmax(&simcc_x[k * SIMCC_X_LEN..(k + 1) * SIMCC_X_LEN]);
        let (iy, vy) = argmax(&simcc_y[k * SIMCC_Y_LEN..(k + 1) * SIMCC_Y_LEN]);
        *slot = (
            ix as f32 / SPLIT_RATIO,
            iy as f32 / SPLIT_RATIO,
            ((vx + vy) / 2.0).clamp(0.0, 1.0),
        );
    }
    out
}

/// Index and value of the maximum of a slice (first max on ties). `(0, 0.0)` for empty.
fn argmax(row: &[f32]) -> (usize, f32) {
    let mut best_i = 0usize;
    let mut best_v = f32::NEG_INFINITY;
    for (i, &v) in row.iter().enumerate() {
        if v > best_v {
            best_v = v;
            best_i = i;
        }
    }
    if best_v.is_finite() {
        (best_i, best_v)
    } else {
        (0, 0.0)
    }
}

/// The top-down crop rectangle in **source pixels** for one person box: the box
/// centre, its size expanded to the pose input's `192:256` aspect and then padded by
/// [`CROP_PAD`], matching MMPose's `TopDownGetBboxCenterScale`. It both samples the
/// input crop and maps decoded keypoints back to source-normalized coordinates.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CropRect {
    cx: f32,
    cy: f32,
    cw: f32,
    ch: f32,
    src_w: f32,
    src_h: f32,
}

impl CropRect {
    /// Build the crop for a source-normalized `bbox` over a `src_w x src_h` source.
    pub(crate) fn for_box(bbox: &Box, src_w: u32, src_h: u32) -> CropRect {
        let (sw, sh) = (src_w as f32, src_h as f32);
        let cx = (bbox.x + bbox.w / 2.0) * sw;
        let cy = (bbox.y + bbox.h / 2.0) * sh;
        let mut w = bbox.w * sw;
        let mut h = bbox.h * sh;
        // Expand to the model's aspect ratio (never crop the person away): grow the
        // short side to match W/H = 192/256.
        let aspect = POSE_W as f32 / POSE_H as f32;
        if w > aspect * h {
            h = w / aspect;
        } else {
            w = aspect * h;
        }
        CropRect {
            cx,
            cy,
            cw: w * CROP_PAD,
            ch: h * CROP_PAD,
            src_w: sw,
            src_h: sh,
        }
    }

    /// Source-pixel coordinate for input-crop pixel `(u, v)` (`u` in `0..POSE_W`,
    /// `v` in `0..POSE_H`), pixel-centre sampled.
    fn source_px(&self, u: f32, v: f32) -> (f32, f32) {
        let sx = self.cx - self.cw / 2.0 + (u + 0.5) * self.cw / POSE_W as f32;
        let sy = self.cy - self.ch / 2.0 + (v + 0.5) * self.ch / POSE_H as f32;
        (sx, sy)
    }

    /// Map a decoded keypoint at crop-pixel `(kx, ky)` to source-normalized `[0,1]`.
    pub(crate) fn to_source_norm(&self, kx: f32, ky: f32) -> (f32, f32) {
        let sx = self.cx - self.cw / 2.0 + kx * self.cw / POSE_W as f32;
        let sy = self.cy - self.ch / 2.0 + ky * self.ch / POSE_H as f32;
        (
            (sx / self.src_w).clamp(0.0, 1.0),
            (sy / self.src_h).clamp(0.0, 1.0),
        )
    }
}

/// Bilinearly sample a `bgr24` source frame at continuous pixel `(fx, fy)`, clamping
/// to the frame edge. Returns `[B, G, R]` as `f32` in `0..255`.
fn sample_bgr(frame: &[u8], w: usize, h: usize, fx: f32, fy: f32) -> [f32; 3] {
    let fx = fx.clamp(0.0, (w - 1) as f32);
    let fy = fy.clamp(0.0, (h - 1) as f32);
    let x0 = fx.floor() as usize;
    let y0 = fy.floor() as usize;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let dx = fx - x0 as f32;
    let dy = fy - y0 as f32;
    let mut out = [0.0f32; 3];
    for c in 0..3 {
        let p00 = frame[(y0 * w + x0) * 3 + c] as f32;
        let p10 = frame[(y0 * w + x1) * 3 + c] as f32;
        let p01 = frame[(y1 * w + x0) * 3 + c] as f32;
        let p11 = frame[(y1 * w + x1) * 3 + c] as f32;
        let top = p00 + (p10 - p00) * dx;
        let bot = p01 + (p11 - p01) * dx;
        out[c] = top + (bot - top) * dy;
    }
    out
}

/// Letterbox a `bgr24` source frame into the detector's `MODEL_SIZE` square BGR input
/// in Rust, matching ffmpeg's `scale=…:decrease` + centered `pad` with [`PAD_VALUE`].
/// Lets the pose pass reuse the vendored detector without a second ffmpeg decode.
fn letterbox_to_model(frame: &[u8], src_w: usize, src_h: usize, lb: &Letterbox) -> Vec<u8> {
    let side = MODEL_SIZE as usize;
    let mut out = vec![PAD_VALUE; side * side * 3];
    for my in 0..side {
        for mx in 0..side {
            let (sx, sy) = lb.model_px_to_source(mx as f32, my as f32);
            // Outside the scaled image sits in the pad region — leave PAD_VALUE.
            if sx < 0.0 || sy < 0.0 || sx > (src_w - 1) as f32 || sy > (src_h - 1) as f32 {
                continue;
            }
            let bgr = sample_bgr(frame, src_w, src_h, sx, sy);
            let px = (my * side + mx) * 3;
            out[px] = bgr[0].round().clamp(0.0, 255.0) as u8;
            out[px + 1] = bgr[1].round().clamp(0.0, 255.0) as u8;
            out[px + 2] = bgr[2].round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

/// Build the pose model's `3x256x192` RGB-normalized CHW input for one crop from a
/// `bgr24` source frame: bilinearly sample the crop, swap BGR→RGB, apply ImageNet
/// normalization ([`MEAN`]/[`STD`]).
fn build_pose_input(frame: &[u8], src_w: usize, src_h: usize, crop: &CropRect) -> Vec<f32> {
    let plane = POSE_H * POSE_W;
    let mut chw = vec![0f32; 3 * plane];
    for v in 0..POSE_H {
        for u in 0..POSE_W {
            let (sx, sy) = crop.source_px(u as f32, v as f32);
            let bgr = sample_bgr(frame, src_w, src_h, sx, sy);
            // BGR → RGB, then normalize per channel.
            let rgb = [bgr[2], bgr[1], bgr[0]];
            let idx = v * POSE_W + u;
            for c in 0..3 {
                chw[c * plane + idx] = (rgb[c] - MEAN[c]) / STD[c];
            }
        }
    }
    chw
}

/// Resolve the vendored RTMPose-t model, probing the same dev/release layouts as the
/// detector ([`crate::detect::vendored_model_path`]).
pub fn vendored_pose_model_path() -> Result<PathBuf, String> {
    detect::vendored_model_file("rtmpose_t.onnx", "pose")
}

/// Probe the pose pass's *decoded* source frame size — width/height parsed
/// robustly (see [`detect::parse_two_dims`]) and swapped when a ±90°/270° display rotation
/// makes ffmpeg autorotate the raw output. `None` if ffprobe cannot run. Cheap
/// (header-only), so the harness probes it **once per recording** and threads the
/// result into [`extract_pose_window`], never per window.
pub(crate) fn probe_source_dimensions(path: &str) -> Option<(u32, u32)> {
    let out = Command::new(sidecar_path("ffprobe"))
        .args([
            "-v", "error", "-select_streams", "v:0",
            "-show_entries", "stream=width,height", "-of", "csv=p=0", path,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let (mut w, mut h) = detect::parse_two_dims(&String::from_utf8_lossy(&out.stdout))?;
    if probe_rotation(path).is_some_and(|r| r.rem_euclid(180) == 90) {
        std::mem::swap(&mut w, &mut h);
    }
    Some((w, h))
}

/// Best-effort display-matrix rotation in degrees (e.g. `-90`, `90`), or `None` when
/// the stream declares none. Reads the **stream-level** side data from the container
/// header (`stream_side_data`) — instant; the frame-level `side_data` form scans the
/// whole file (minutes on a multi-GB recording).
fn probe_rotation(path: &str) -> Option<i32> {
    let out = Command::new(sidecar_path("ffprobe"))
        .args([
            "-v", "error", "-select_streams", "v:0",
            "-show_entries", "stream_side_data=rotation", "-of", "csv=p=0", path,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .split(|c: char| !(c.is_ascii_digit() || c == '-'))
        .find_map(|s| s.parse::<i32>().ok())
}

/// Extract a densified-window pose track: decode `[start_ms, start_ms + dur_ms)` at
/// `fps` (targeted 10–15 fps decode, scratch-only per ADR 0017), run the detector on
/// each frame, and run the pose model on the two highest-scored person boxes. Returns
/// the per-frame poses in source-normalized coordinates. `detector` and `pose` are
/// loaded once by the caller and reused across windows.
///
/// Seeks with `-ss` before `-i` (fast, frame-approximate) — window edges need not be
/// exact for a serve-detectability measurement. Errors if the sidecar cannot run or
/// yields no frames.
#[allow(clippy::too_many_arguments)]
pub fn extract_pose_window(
    path: &str,
    start_ms: i64,
    dur_ms: i64,
    fps: u32,
    src_w: u32,
    src_h: u32,
    detector: &mut detect::Detector,
    pose: &mut PoseEstimator,
) -> Result<PoseWindow, String> {
    // The pose pass decodes at *source* resolution (unlike detection, which lets
    // ffmpeg letterbox to a fixed square), so it needs the true decoded frame size.
    // `src_w`/`src_h` come from the caller's once-per-recording
    // [`probe_source_dimensions`] — its own robust probe, since some ffprobe builds
    // emit a trailing separator (`1920x1080x`) that the detector's parser drops to
    // `None` (harmless there, a fatal frame-size mismatch here). Kept separate so
    // shipped detection stays byte-identical (ADR 0017).
    let (sw, sh) = (src_w as usize, src_h as usize);
    let lb = Letterbox::new(src_w, src_h);

    let ss = format!("{:.3}", start_ms as f64 / 1000.0);
    let t = format!("{:.3}", dur_ms.max(0) as f64 / 1000.0);
    let vf = format!("fps={fps}");
    let mut child = Command::new(sidecar_path("ffmpeg"))
        .args([
            "-v", "error", "-nostats",
            "-ss", &ss, "-i", path, "-t", &t,
            "-an",
            "-vf", &vf, "-f", "rawvideo", "-pix_fmt", "bgr24", "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("ffmpeg could not run: {e}"))?;

    let frame_size = sw * sh * 3;
    let mut frames: Vec<PoseFrame> = Vec::new();
    if let Some(mut stdout) = child.stdout.take() {
        let mut frame = vec![0u8; frame_size];
        let mut i = 0i64;
        while stdout.read_exact(&mut frame).is_ok() {
            let t_ms = start_ms + i * 1000 / i64::from(fps);
            i += 1;

            let model_bgr = letterbox_to_model(&frame, sw, sh, &lb);
            let mut boxes = detector.boxes_in_frame(&model_bgr, SCORE_THRESHOLD, &lb)?;
            boxes.sort_by(|a, b| {
                b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal)
            });
            boxes.truncate(MAX_PLAYERS);

            let mut players = Vec::with_capacity(boxes.len());
            for bbox in boxes {
                let crop = CropRect::for_box(&bbox, src_w, src_h);
                let input = build_pose_input(&frame, sw, sh, &crop);
                let decoded = pose.infer_crop(input)?;
                let mut keypoints = [Keypoint { x: 0.0, y: 0.0, score: 0.0 }; NUM_KP];
                for (kp, &(kx, ky, score)) in keypoints.iter_mut().zip(decoded.iter()) {
                    let (x, y) = crop.to_source_norm(kx, ky);
                    *kp = Keypoint { x, y, score };
                }
                players.push(PlayerPose { bbox, keypoints });
            }
            frames.push(PoseFrame { t_ms, players });
        }
    }

    crate::media::wait_ffmpeg(&mut child, "ffmpeg failed to extract window")?;
    if frames.is_empty() {
        return Err("window yielded no frames".to_string());
    }
    Ok(PoseWindow {
        fps: f64::from(fps),
        frames,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argmax_picks_first_maximum() {
        assert_eq!(argmax(&[0.1, 0.9, 0.9, 0.2]), (1, 0.9));
        assert_eq!(argmax(&[]), (0, 0.0));
    }

    #[test]
    fn simcc_decode_maps_bins_to_pixels_over_split_ratio() {
        // Put keypoint 0's x-peak at bin 100 and y-peak at bin 200.
        let mut sx = vec![0.0f32; NUM_KP * SIMCC_X_LEN];
        let mut sy = vec![0.0f32; NUM_KP * SIMCC_Y_LEN];
        sx[100] = 0.8;
        sy[200] = 0.6;
        let kp = decode_simcc(&sx, &sy);
        assert_eq!(kp[0].0, 100.0 / SPLIT_RATIO); // 50 px
        assert_eq!(kp[0].1, 200.0 / SPLIT_RATIO); // 100 px
        assert!((kp[0].2 - 0.7).abs() < 1e-6); // mean of the two peaks
    }

    #[test]
    fn crop_expands_to_model_aspect_and_pads() {
        // A wide box (aspect > 192/256) keeps its width and grows its height, then
        // both are padded by CROP_PAD.
        let b = Box { x: 0.25, y: 0.4, w: 0.5, h: 0.1, score: 0.9 };
        let crop = CropRect::for_box(&b, 1000, 1000);
        // width px = 0.5*1000 = 500; expected height = 500 / (192/256) = 666.67
        assert!((crop.cw - 500.0 * CROP_PAD).abs() < 1e-3);
        assert!((crop.ch - (500.0 / (POSE_W as f32 / POSE_H as f32)) * CROP_PAD).abs() < 1e-2);
        // Centre is the box centre in px.
        assert!((crop.cx - 500.0).abs() < 1e-3);
        assert!((crop.cy - 450.0).abs() < 1e-3);
    }

    #[test]
    fn crop_source_norm_roundtrips_center() {
        let b = Box { x: 0.2, y: 0.2, w: 0.4, h: 0.4, score: 0.9 };
        let crop = CropRect::for_box(&b, 800, 600);
        // The crop-centre pixel maps back to the box centre, source-normalized.
        let (nx, ny) = crop.to_source_norm(POSE_W as f32 / 2.0, POSE_H as f32 / 2.0);
        assert!((nx - 0.4).abs() < 1e-3);
        assert!((ny - 0.4).abs() < 1e-3);
    }

    #[test]
    fn sample_bgr_interpolates_between_pixels() {
        // 2x1 frame: left pixel BGR (0,0,0), right pixel (100,100,100).
        let frame = [0u8, 0, 0, 100, 100, 100];
        let mid = sample_bgr(&frame, 2, 1, 0.5, 0.0);
        assert!((mid[0] - 50.0).abs() < 1e-3);
    }
}
