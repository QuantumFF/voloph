//! Occupancy **detection extraction** (ADR 0015 Stage 2, issue #83): a nano person
//! detector that turns a recording into a timestamped track of person bounding
//! boxes — the raw material for the occupancy signal that will *propose* candidate
//! play spans once fusion is wired (issue #84). This module owns everything from the
//! ffmpeg frame stream to the decoded, NMS-filtered boxes; it feeds nothing yet.
//!
//! **The model.** One nano-scale, Apache-2.0 detector — YOLOX-Nano, vendored under
//! `models/` with its provenance (see `models/README.md`) — run through ONNX Runtime
//! via the `ort` crate. YOLOX-Nano takes a `1x3x416x416` BGR, letterboxed, un-
//! normalized (raw 0–255) tensor and emits `1 x N x 85`: per anchor `[cx, cy, w, h,
//! obj, cls_0..cls_79]` in grid units, grid/stride-decoded with strides `[8,16,32]`.
//! We keep **class 0 (person)** only, score `= obj * cls_person`, threshold, and NMS.
//!
//! **Hardware-adaptive, no user choice (ADR 0015).** [`Detector::load`] registers the
//! GPU execution providers compiled in for this OS (CUDA on Linux, DirectML on
//! Windows) *ahead of* CPU. `ort`'s default behavior is a silent fallback: an EP that
//! fails to register (no GPU, missing driver) is skipped and the next one — ultimately
//! the always-present CPU provider — runs instead. The detections are identical across
//! providers; only wall-clock differs, so there is nothing to surface to the user.
//!
//! **Frames.** Mirroring [`crate::media::extract_motion`], frames come from the ffmpeg
//! sidecar (ADR 0004) at [`DETECT_FPS`], already letterboxed to the model's square
//! input by ffmpeg's `scale`+`pad`, streamed as raw `bgr24` so the whole video is
//! never buffered. Boxes are mapped back through the letterbox transform into
//! **source-frame-normalized** `[0,1]` coordinates, so the occupancy logic downstream
//! is independent of the model's input size or the source resolution.
//!
//! **Testing (ADR 0015).** Inference is deliberately untested — detector quality is
//! judged only by the eval harness on real footage. The pure helpers below (box
//! decode, NMS, the letterbox transform) carry unit tests, since they are ordinary
//! arithmetic with no model weights involved.

use std::io::Read as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use ort::execution_providers::ExecutionProviderDispatch;
use ort::session::{Session, SessionInputValue};
use ort::value::Tensor;

use crate::media::sidecar_path;

/// Frames per second the detection track is sampled at (ADR 0015: 2–5 fps). Person
/// occupancy changes slowly relative to shuttle motion, so a low rate captures who is
/// on court while keeping inference — the long pole of this pass — cheap. Chosen at
/// the low end of the band to stay inside the ~1.5× real-time CPU budget.
pub const DETECT_FPS: u32 = 3;

/// The square side the model consumes (YOLOX-Nano: 416). Frames are letterboxed to
/// `MODEL_SIZE x MODEL_SIZE` before inference. `pub(crate)` so the serve-cue pose
/// pass (issue #95) can letterbox its own source frames to the same input.
pub(crate) const MODEL_SIZE: u32 = 416;

/// The grayscale pad value (114) YOLOX's letterbox fills with — the byte form of
/// [`PAD_HEX`], exposed so the pose pass's in-Rust letterbox matches ffmpeg's.
pub(crate) const PAD_VALUE: u8 = 114;

/// Grayscale pad value YOLOX's `preproc` letterboxes with (114). ffmpeg's `pad`
/// takes it as a hex color; all three channels equal so BGR vs RGB is moot.
const PAD_HEX: &str = "0x727272";

/// The strides of YOLOX's three detection heads. Each head lays a grid over the
/// input at `MODEL_SIZE / stride` cells per side; the raw box coordinates are offsets
/// within that grid, decoded back to pixels by [`decode`].
const STRIDES: [u32; 3] = [8, 16, 32];

/// COCO class index for `person`. YOLOX emits 80 class scores per anchor; the
/// occupancy signal cares only about people, so every other class is discarded.
const PERSON_CLASS: usize = 0;

/// Keep an anchor only when `obj * cls_person` clears this. Deliberately low — the
/// zero-miss bar (ADR 0015) wants occupancy to *propose* generously; a spurious box
/// costs a downstream false positive, a dropped player risks a missed rally. NMS below
/// removes duplicate boxes on the same person that this admits. Public so the eval
/// harness can name the shipped floor when it extracts scratch tracks below it
/// (issue #93); every app path extracts at exactly this floor.
pub const SCORE_THRESHOLD: f32 = 0.35;

/// Two boxes overlapping more than this (IoU) are treated as the same detection by
/// [`nms`]; the lower-scoring one is dropped.
const NMS_IOU_THRESHOLD: f32 = 0.45;

/// One detected person box at one sampled instant, in **source-frame-normalized**
/// coordinates: every field is a fraction in `[0,1]` of the source frame's width
/// (x, w) or height (y, h), so it is independent of the model input size and the
/// recording's resolution. `score` is the detector's confidence (`obj * class`).
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Box {
    /// Left edge, fraction of source width.
    pub x: f32,
    /// Top edge, fraction of source height.
    pub y: f32,
    /// Width, fraction of source width.
    pub w: f32,
    /// Height, fraction of source height.
    pub h: f32,
    /// Detector confidence in `[0,1]`.
    pub score: f32,
}

/// The per-recording **detection track** (issue #83): the person boxes at each sampled
/// frame, in time order. `fps` is [`DETECT_FPS`]; sample `i` lands at `i / fps`
/// seconds. This is the occupancy raw material fusion (issue #84) will consume; today
/// it is only computed, dumped, and logged.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DetectionTrack {
    pub fps: f64,
    /// One entry per sampled frame; the inner vec is that frame's person boxes (empty
    /// when nobody was detected).
    pub samples: Vec<Vec<Box>>,
}

impl DetectionTrack {
    /// Convert to the pure [`crate::segment::OccupancyTrack`] the segmentation seam
    /// consumes (issue #84). Drops the detector score — a box reaching this track
    /// already cleared thresholding, and fusion reasons about *where* people are, not
    /// the detector's confidence — and widens the coordinates to `f64` so the pure
    /// seam carries no `f32`/`ort` residue. This is the one crossing from the
    /// detector's world into the segmenter's.
    pub fn to_occupancy_track(&self) -> crate::segment::OccupancyTrack {
        crate::segment::OccupancyTrack {
            fps: self.fps,
            samples: self
                .samples
                .iter()
                .map(|frame| {
                    frame
                        .iter()
                        .map(|b| crate::segment::DetBox {
                            x: f64::from(b.x),
                            y: f64::from(b.y),
                            w: f64::from(b.w),
                            h: f64::from(b.h),
                        })
                        .collect()
                })
                .collect(),
        }
    }
}

/// A loaded detector: the ONNX Runtime session plus the resolved input tensor name.
/// Building one probes the execution providers (GPU where present, else CPU); running
/// inference is [`Detector::infer`].
pub struct Detector {
    session: Session,
    input_name: String,
}

impl Detector {
    /// Load the vendored YOLOX-Nano model and build an inference session with GPU
    /// execution providers probed ahead of CPU (ADR 0015). A GPU EP that cannot
    /// register (no hardware/driver) is silently skipped by `ort`; CPU always runs.
    /// `model_path` is the `.onnx` file — [`vendored_model_path`] resolves the shipped
    /// one. Errors only when the model itself cannot be loaded, never for a missing
    /// GPU.
    pub fn load(model_path: &std::path::Path) -> Result<Detector, String> {
        let session = Session::builder()
            .map_err(|e| format!("ort session builder: {e}"))?
            .with_execution_providers(gpu_execution_providers())
            .map_err(|e| format!("ort execution providers: {e}"))?
            // The recording's motion pass already saturates cores; cap inference
            // threads so the two passes do not thrash the CPU against each other.
            .with_intra_threads(4)
            .map_err(|e| format!("ort intra threads: {e}"))?
            .commit_from_file(model_path)
            .map_err(|e| format!("ort could not load {}: {e}", model_path.display()))?;

        // YOLOX-Nano has a single input; take its name so we feed it by name rather
        // than hardcoding "images"/"input" (which differs across exports).
        let input_name = session
            .inputs
            .first()
            .map(|i| i.name.clone())
            .ok_or("model has no inputs")?;
        Ok(Detector { session, input_name })
    }

    /// Infer on one already-letterboxed `MODEL_SIZE` BGR frame and map the decoded
    /// person boxes straight into source-frame-normalized coordinates through
    /// `letterbox`. The serve-cue pose pass (issue #95) letterboxes its own source
    /// frames in Rust and reuses this to get the per-frame boxes it crops around,
    /// without touching the private [`PixelBox`] type.
    pub(crate) fn boxes_in_frame(
        &mut self,
        bgr: &[u8],
        score_floor: f32,
        letterbox: &Letterbox,
    ) -> Result<Vec<Box>, String> {
        let pixel_boxes = self.infer(bgr, score_floor)?;
        Ok(pixel_boxes
            .iter()
            .filter_map(|b| letterbox.to_source_norm(b))
            .collect())
    }

    /// Run the detector on one already-letterboxed `MODEL_SIZE x MODEL_SIZE` BGR frame
    /// (raw bytes, `H*W*3`), returning the decoded person boxes at or above
    /// `score_floor` in **model-input** pixel coordinates before letterbox
    /// back-mapping. The caller maps them into source-normalized space via
    /// [`Letterbox::to_source_norm`].
    fn infer(&mut self, bgr: &[u8], score_floor: f32) -> Result<Vec<PixelBox>, String> {
        let side = MODEL_SIZE as usize;
        debug_assert_eq!(bgr.len(), side * side * 3);
        // HWC bytes → CHW f32, no normalization (YOLOX consumes raw 0–255). Channel
        // order stays BGR, as the model was exported to expect.
        let mut chw = vec![0f32; 3 * side * side];
        let plane = side * side;
        for y in 0..side {
            for x in 0..side {
                let px = (y * side + x) * 3;
                chw[y * side + x] = bgr[px] as f32; // B
                chw[plane + y * side + x] = bgr[px + 1] as f32; // G
                chw[2 * plane + y * side + x] = bgr[px + 2] as f32; // R
            }
        }
        let shape = vec![1i64, 3, side as i64, side as i64];
        let tensor = Tensor::from_array((shape, chw)).map_err(|e| format!("ort tensor: {e}"))?;
        let input: SessionInputValue<'_> = tensor.into();
        let outputs = self
            .session
            .run(vec![(self.input_name.clone(), input)])
            .map_err(|e| format!("ort inference: {e}"))?;
        let (shape, data) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("ort output: {e}"))?;
        // Expect [1, N, 85].
        let dims: Vec<i64> = shape.iter().copied().collect();
        if dims.len() != 3 || dims[2] < (5 + PERSON_CLASS as i64 + 1) {
            return Err(format!("unexpected detector output shape {dims:?}"));
        }
        let n = dims[1] as usize;
        let cols = dims[2] as usize;
        Ok(decode(data, n, cols, score_floor))
    }
}

/// A decoded box in model-input pixel space (`0..MODEL_SIZE`), `xyxy` corners.
#[derive(Debug, Clone, Copy, PartialEq)]
struct PixelBox {
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    score: f32,
}

/// The GPU execution providers to try for this OS, most-preferred first. Each is left
/// on `ort`'s default silent-fallback behavior: if the EP was not compiled in (its
/// Cargo feature off) or the hardware/driver is absent, `ort` skips it and moves to the
/// next, ending at the always-present CPU provider — which needs no entry here. The
/// EP builder types exist in `ort`'s API regardless of the feature flag, so pushing
/// them unconditionally never fails to compile; only registration is conditional, and
/// that is handled silently at runtime. On CPU-only machines this list is simply
/// ignored, and detections are identical either way (ADR 0015 — no user choice).
pub(crate) fn gpu_execution_providers() -> Vec<ExecutionProviderDispatch> {
    #[cfg(target_os = "linux")]
    {
        vec![ort::execution_providers::CUDAExecutionProvider::default().build()]
    }
    #[cfg(target_os = "windows")]
    {
        vec![ort::execution_providers::DirectMLExecutionProvider::default().build()]
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        Vec::new()
    }
}

/// Decode YOLOX's raw `N x cols` output into scored person boxes in model-input pixel
/// space. Pure: it replays the grid/stride math (`xy = (raw + grid) * stride`,
/// `wh = exp(raw) * stride`) that `demo_postprocess` applies, keeps only anchors whose
/// `obj * cls_person` clears `score_floor` ([`SCORE_THRESHOLD`] on every app path),
/// and converts center-form to corners. The anchors are laid out head-major in the
/// order of [`STRIDES`], each head a `(MODEL_SIZE/stride)^2` grid in row-major
/// (y outer, x inner) order — the same order `demo_postprocess` concatenates them.
///
/// Lowering the floor only ever *appends* lower-scored boxes: [`nms`] visits boxes in
/// descending score order, so a sub-floor box can never suppress one the shipped
/// floor keeps — the `>= SCORE_THRESHOLD` subset of a low-floor decode is exactly the
/// default decode (the banding invariant issue #93's presence measurement rests on).
fn decode(data: &[f32], n: usize, cols: usize, score_floor: f32) -> Vec<PixelBox> {
    let mut out = Vec::new();
    let mut anchor = 0usize;
    for &stride in &STRIDES {
        let grid = MODEL_SIZE / stride;
        for gy in 0..grid {
            for gx in 0..grid {
                if anchor >= n {
                    break;
                }
                let base = anchor * cols;
                anchor += 1;
                let raw = &data[base..base + cols];
                let obj = raw[4];
                let cls = raw[5 + PERSON_CLASS];
                let score = obj * cls;
                if score < score_floor {
                    continue;
                }
                let cx = (raw[0] + gx as f32) * stride as f32;
                let cy = (raw[1] + gy as f32) * stride as f32;
                let w = raw[2].exp() * stride as f32;
                let h = raw[3].exp() * stride as f32;
                out.push(PixelBox {
                    x1: cx - w / 2.0,
                    y1: cy - h / 2.0,
                    x2: cx + w / 2.0,
                    y2: cy + h / 2.0,
                    score,
                });
            }
        }
    }
    nms(out)
}

/// Greedy non-maximum suppression: sort boxes by score, keep each unless it overlaps an
/// already-kept box by more than [`NMS_IOU_THRESHOLD`] IoU. Pure and order-independent
/// in result (ties are resolved by the stable sort but never change which cluster
/// survives). Collapses the several overlapping anchors YOLOX fires on one person into
/// one box.
fn nms(mut boxes: Vec<PixelBox>) -> Vec<PixelBox> {
    boxes.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    let mut kept: Vec<PixelBox> = Vec::new();
    for b in boxes {
        if kept.iter().all(|k| iou(k, &b) <= NMS_IOU_THRESHOLD) {
            kept.push(b);
        }
    }
    kept
}

/// Intersection-over-union of two corner-form boxes; `0.0` when disjoint.
fn iou(a: &PixelBox, b: &PixelBox) -> f32 {
    let ix1 = a.x1.max(b.x1);
    let iy1 = a.y1.max(b.y1);
    let ix2 = a.x2.min(b.x2);
    let iy2 = a.y2.min(b.y2);
    let iw = (ix2 - ix1).max(0.0);
    let ih = (iy2 - iy1).max(0.0);
    let inter = iw * ih;
    let area_a = (a.x2 - a.x1).max(0.0) * (a.y2 - a.y1).max(0.0);
    let area_b = (b.x2 - b.x1).max(0.0) * (b.y2 - b.y1).max(0.0);
    let union = area_a + area_b - inter;
    if union > 0.0 {
        inter / union
    } else {
        0.0
    }
}

/// The letterbox transform ffmpeg applies to fit a `src_w x src_h` source frame into
/// the square `MODEL_SIZE` input: a uniform `scale` (aspect preserved) then centered
/// padding. Carries just enough to invert it, mapping a model-space box back into
/// source-frame-normalized `[0,1]` coordinates.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Letterbox {
    scale: f32,
    pad_x: f32,
    pad_y: f32,
    src_w: f32,
    src_h: f32,
}

impl Letterbox {
    /// Build the transform for a source of `src_w x src_h`, matching ffmpeg's
    /// `scale=...:force_original_aspect_ratio=decrease` + centered `pad`.
    pub(crate) fn new(src_w: u32, src_h: u32) -> Letterbox {
        let side = MODEL_SIZE as f32;
        let (sw, sh) = (src_w as f32, src_h as f32);
        let scale = (side / sw).min(side / sh);
        let scaled_w = sw * scale;
        let scaled_h = sh * scale;
        Letterbox {
            scale,
            pad_x: (side - scaled_w) / 2.0,
            pad_y: (side - scaled_h) / 2.0,
            src_w: sw,
            src_h: sh,
        }
    }

    /// Map a model-input pixel `(mx, my)` back to the continuous **source pixel** it
    /// samples from (undo the centered pad, then the uniform scale). A coordinate
    /// outside `[0, src)` lands in the pad region. Used by the pose pass's in-Rust
    /// letterbox (issue #95).
    pub(crate) fn model_px_to_source(self, mx: f32, my: f32) -> (f32, f32) {
        ((mx - self.pad_x) / self.scale, (my - self.pad_y) / self.scale)
    }

    /// Map one model-space pixel box back into source-frame-normalized coordinates,
    /// clamping to `[0,1]` (a box may extend into the pad region). Returns `None` if
    /// the box collapses to zero area after clamping.
    fn to_source_norm(self, b: &PixelBox) -> Option<Box> {
        // Undo pad + scale to get source pixels, then normalize by source size.
        let sx1 = ((b.x1 - self.pad_x) / self.scale / self.src_w).clamp(0.0, 1.0);
        let sy1 = ((b.y1 - self.pad_y) / self.scale / self.src_h).clamp(0.0, 1.0);
        let sx2 = ((b.x2 - self.pad_x) / self.scale / self.src_w).clamp(0.0, 1.0);
        let sy2 = ((b.y2 - self.pad_y) / self.scale / self.src_h).clamp(0.0, 1.0);
        let w = sx2 - sx1;
        let h = sy2 - sy1;
        if w <= 0.0 || h <= 0.0 {
            return None;
        }
        Some(Box {
            x: sx1,
            y: sy1,
            w,
            h,
            score: b.score,
        })
    }
}

/// Resolve the vendored YOLOX-Nano model. Tauri copies `bundle.resources` into a
/// `resources/` folder beside the app executable in a release build, and the same
/// layout is used in `tauri dev`; the dev CLIs run from `target/<profile>/` where the
/// model is likewise expected at `resources/models/…` or `models/…`. We probe the
/// candidate layouts in order and return the first that exists, so both dev and release
/// resolve without a code change. Errors listing the tried paths when none is found.
pub fn vendored_model_path() -> Result<PathBuf, String> {
    const MODEL_FILE: &str = "yolox_nano.onnx";
    let mut tried: Vec<PathBuf> = Vec::new();
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|p| p.to_path_buf()));
    if let Some(dir) = &exe_dir {
        // Release/dev bundle layout: resources next to the executable.
        tried.push(dir.join("resources").join("models").join(MODEL_FILE));
        tried.push(dir.join("models").join(MODEL_FILE));
    }
    // Source-tree layout: running the dev CLI from the crate root / target dir.
    tried.push(PathBuf::from("models").join(MODEL_FILE));
    tried.push(PathBuf::from("src-tauri").join("models").join(MODEL_FILE));
    if let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") {
        tried.push(PathBuf::from(manifest).join("models").join(MODEL_FILE));
    }
    for p in &tried {
        if p.exists() {
            return Ok(p.clone());
        }
    }
    Err(format!(
        "vendored detector model not found; looked in: {}",
        tried
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// Probe a recording's source frame dimensions with the ffprobe sidecar, so the
/// letterbox back-mapping knows the source aspect. Returns `None` if ffprobe cannot
/// run or the stream declares no usable size — the caller then falls back to a square
/// assumption (still runnable, just less precise back-mapping).
fn probe_dimensions(path: &str) -> Option<(u32, u32)> {
    let output = Command::new(sidecar_path("ffprobe"))
        .args([
            "-v", "error", "-select_streams", "v:0",
            "-show_entries", "stream=width,height",
            "-of", "csv=s=x:p=0",
            path,
        ])
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let (w, h) = text.trim().split_once('x')?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// Extract the per-recording **detection track** (issue #83): stream the recording as
/// letterboxed `MODEL_SIZE` BGR frames from the ffmpeg sidecar at [`DETECT_FPS`], run
/// the detector on each, and collect the person boxes in source-normalized coordinates.
/// Parallel to [`crate::media::extract_motion`] and living entirely outside the pure
/// segmentation seam (ADR 0015) — it produces the occupancy raw material that fusion
/// consumes via [`DetectionTrack::to_occupancy_track`].
///
/// `on_progress` reports the footage position in ms as each frame is processed, so a
/// caller can pace an estimate exactly as motion extraction does. `detector` is loaded
/// once by the caller and reused across recordings. Errors if the sidecar cannot run,
/// exits non-zero, or yields no frames.
pub fn extract_detections(
    path: &str,
    detector: &mut Detector,
    on_progress: impl FnMut(i64),
) -> Result<DetectionTrack, String> {
    extract_detections_at(path, detector, DETECT_FPS, SCORE_THRESHOLD, on_progress)
}

/// [`extract_detections`] at an explicit sample rate and score floor instead of
/// [`DETECT_FPS`] / [`SCORE_THRESHOLD`] — the same ffmpeg-stream/letterbox/inference
/// path, only the `fps` filter and the decode floor differ. The app always samples
/// at [`DETECT_FPS`] with the shipped floor; other rates within ADR 0015's decided
/// 2–5 fps envelope and lower floors exist for the eval harness's headroom and
/// presence measurements (issue #93), which extract to a scratch cache and never
/// touch the app's tracks.
pub fn extract_detections_at(
    path: &str,
    detector: &mut Detector,
    fps: u32,
    score_floor: f32,
    mut on_progress: impl FnMut(i64),
) -> Result<DetectionTrack, String> {
    let (src_w, src_h) = probe_dimensions(path).unwrap_or((MODEL_SIZE, MODEL_SIZE));
    let letterbox = Letterbox::new(src_w, src_h);

    // Let ffmpeg do the aspect-preserving downscale + centered pad to the model's
    // square input, so we hand the detector exactly what YOLOX's `preproc` produces.
    let vf = format!(
        "fps={fps},scale={MODEL_SIZE}:{MODEL_SIZE}:force_original_aspect_ratio=decrease,\
         pad={MODEL_SIZE}:{MODEL_SIZE}:(ow-iw)/2:(oh-ih)/2:color={PAD_HEX}"
    );
    let mut child = Command::new(sidecar_path("ffmpeg"))
        .args([
            "-v", "error", "-nostats", "-i", path,
            "-an", // detection is video-only
            "-vf", &vf, "-f", "rawvideo", "-pix_fmt", "bgr24", "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("ffmpeg could not run: {e}"))?;

    let frame_size = (MODEL_SIZE * MODEL_SIZE * 3) as usize;
    let mut samples: Vec<Vec<Box>> = Vec::new();
    if let Some(mut stdout) = child.stdout.take() {
        let mut frame = vec![0u8; frame_size];
        let mut frames = 0u64;
        while stdout.read_exact(&mut frame).is_ok() {
            let pixel_boxes = detector.infer(&frame, score_floor)?;
            let boxes = pixel_boxes
                .iter()
                .filter_map(|b| letterbox.to_source_norm(b))
                .collect();
            samples.push(boxes);
            frames += 1;
            on_progress(frames as i64 * 1000 / i64::from(fps));
        }
    }

    let status = child.wait().map_err(|e| format!("ffmpeg wait failed: {e}"))?;
    if !status.success() {
        let stderr = child
            .stderr
            .take()
            .map(|mut s| {
                let mut buf = String::new();
                let _ = s.read_to_string(&mut buf);
                buf
            })
            .unwrap_or_default();
        return Err(format!("ffmpeg failed to extract frames: {stderr}"));
    }
    if samples.is_empty() {
        return Err("video yielded too few frames for detection".to_string());
    }

    Ok(DetectionTrack {
        fps: f64::from(fps),
        samples,
    })
}

/// Load the vendored detector and run it over one recording, degrading to `None` on
/// **every** failure path — no vendored model, ort init failure, ffmpeg or inference
/// error. This is the one load-run-degrade policy the zero-miss bar (ADR 0015) demands
/// of every occupancy consumer: fusion falls back to motion-proposes on `None`, so a
/// detector that cannot load or run costs precision, never a rally. Each failure is
/// reported once through `on_fail` in the caller's own log sink.
pub fn detections_or_none(path: &str, on_fail: impl Fn(&str)) -> Option<DetectionTrack> {
    detections_at_or_none(path, DETECT_FPS, SCORE_THRESHOLD, on_fail)
}

/// [`detections_or_none`] at an explicit sample rate and score floor — the same
/// load-run-degrade policy, for the eval harness's scratch extractions (issue #93).
pub fn detections_at_or_none(
    path: &str,
    fps: u32,
    score_floor: f32,
    on_fail: impl Fn(&str),
) -> Option<DetectionTrack> {
    let model_path = match vendored_model_path() {
        Ok(p) => p,
        Err(e) => {
            on_fail(&format!("model unavailable: {e}"));
            return None;
        }
    };
    let mut detector = match Detector::load(&model_path) {
        Ok(d) => d,
        Err(e) => {
            on_fail(&format!("detector load failed: {e}"));
            return None;
        }
    };
    match extract_detections_at(path, &mut detector, fps, score_floor, |_| {}) {
        Ok(track) => Some(track),
        Err(e) => {
            on_fail(&format!("extraction failed: {e}"));
            None
        }
    }
}

// ── `detect-dump` dev CLI shell ─────────────────────────────────────────────────
//
// A thin, untested dev tool (ADR 0015: detector quality is judged by the eval harness
// on real footage, not unit tests). It runs the detector on one real recording and
// prints per-sample box counts, positions, and sizes — the acceptance-criterion dump —
// plus the wall-clock vs recording-duration ratio. Mirrors the `eval-harness` shell:
// resolve a recording from the app DB (or take a direct `--file`), extract, report.

/// Run the `detect-dump` CLI. `args` is the process arguments **after** the program
/// name.
///
/// Usage: `detect-dump [--db PATH] [--library local|shared] [--file PATH] [--limit N] [RECORDING]`
/// - `--file PATH` — dump this media file directly, bypassing the DB (quickest path).
/// - `--db PATH` — the metadata DB to read (default: the app's own DB for this OS).
/// - `--library KIND` — which library to resolve the recording from (default: active).
/// - `--limit N` — print only the first N samples in full (the summary still covers all).
/// - `RECORDING` — a path substring; the first library recording whose path contains it.
pub fn run(args: Vec<String>) -> Result<(), String> {
    let opts = DumpOptions::parse(&args)?;
    if opts.help {
        print_dump_usage();
        return Ok(());
    }

    let (abs_path, duration_ms) = if let Some(file) = &opts.file {
        (file.clone(), None)
    } else {
        resolve_from_db(&opts)?
    };

    let model_path = vendored_model_path()?;
    println!("detect-dump — model {}", model_path.display());
    println!("recording    {abs_path}");

    let mut detector = Detector::load(&model_path)?;
    let started = std::time::Instant::now();
    let track = extract_detections(&abs_path, &mut detector, |_| {})?;
    let wall_ms = started.elapsed().as_millis() as i64;

    print_track(&track, opts.limit);
    print_summary(&track, wall_ms, duration_ms);
    Ok(())
}

/// Resolve a recording's absolute path (and its stored duration, if any) from the app
/// DB, honoring `--db`/`--library`/`RECORDING`. Reuses the same DB seams the eval
/// harness does.
fn resolve_from_db(opts: &DumpOptions) -> Result<(String, Option<i64>), String> {
    let db_path = match &opts.db {
        Some(p) => PathBuf::from(p),
        None => default_db_path()?,
    };
    if !db_path.exists() {
        return Err(format!(
            "no metadata database at {}\n(pass --db PATH or --file PATH)",
            db_path.display()
        ));
    }
    let conn =
        crate::db::open(&db_path).map_err(|e| format!("could not open {}: {e}", db_path.display()))?;
    let kind = match &opts.library {
        Some(k) => k.clone(),
        None => crate::db::active_kind(&conn).map_err(|e| e.to_string())?,
    };
    let library = crate::db::library_path_of(&conn, &kind)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("the '{kind}' library is not designated in {}", db_path.display()))?;

    let rows: Vec<(String, Option<i64>)> = {
        let mut stmt = conn
            .prepare("SELECT path, duration_ms FROM recordings WHERE library = ?1 ORDER BY path")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([&kind], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?)))
            .map_err(|e| e.to_string())?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| e.to_string())?;
        rows
    };
    if rows.is_empty() {
        return Err(format!("the '{kind}' library has no recordings"));
    }
    let (rel, duration_ms) = match &opts.filter {
        Some(f) => rows
            .iter()
            .find(|(p, _)| p.contains(f))
            .cloned()
            .ok_or_else(|| format!("no recording in '{kind}' matched \"{f}\""))?,
        None => rows[0].clone(),
    };
    Ok((crate::db::absolute(&library, &rel), duration_ms))
}

/// Print per-sample box counts, positions, and sizes — the acceptance-criterion dump.
/// Positions/sizes are the source-normalized `[0,1]` box fields. Capped at `limit`
/// samples printed in full (the summary that follows always covers the whole track).
fn print_track(track: &DetectionTrack, limit: Option<usize>) {
    println!(
        "\nper-sample detections ({} samples @ {} fps):",
        track.samples.len(),
        track.fps
    );
    let shown = limit.unwrap_or(track.samples.len()).min(track.samples.len());
    for (i, boxes) in track.samples.iter().take(shown).enumerate() {
        let t = i as f64 / track.fps;
        if boxes.is_empty() {
            println!("  [{i:>4}] t={t:6.2}s  0 boxes");
            continue;
        }
        print!("  [{i:>4}] t={t:6.2}s  {} box(es):", boxes.len());
        for b in boxes {
            print!(
                " {{x={:.3} y={:.3} w={:.3} h={:.3} s={:.2}}}",
                b.x, b.y, b.w, b.h, b.score
            );
        }
        println!();
    }
    if shown < track.samples.len() {
        println!("  … {} more samples (pass --limit 0 or a larger N to see all)", track.samples.len() - shown);
    }
}

/// Print the corpus-independent summary: total boxes, peak simultaneous count, the
/// fraction of samples with at least one person, and the wall-clock vs recording-
/// duration ratio (the ~1.5× real-time budget of ADR 0015 — this measures the
/// detection pass alone; the worker adds the audio+motion passes on top).
fn print_summary(track: &DetectionTrack, wall_ms: i64, duration_ms: Option<i64>) {
    let total: usize = track.samples.iter().map(Vec::len).sum();
    let peak = track.samples.iter().map(Vec::len).max().unwrap_or(0);
    let occupied = track.samples.iter().filter(|s| !s.is_empty()).count();
    let sampled_ms = (track.samples.len() as f64 / track.fps * 1000.0) as i64;
    let dur = duration_ms.filter(|&d| d > 0).unwrap_or(sampled_ms);
    println!("\n=== summary ===");
    println!("samples                : {}", track.samples.len());
    println!("total person boxes     : {total}");
    println!("peak simultaneous      : {peak}");
    println!(
        "samples with a person  : {occupied} / {} ({:.1}%)",
        track.samples.len(),
        100.0 * occupied as f64 / track.samples.len().max(1) as f64
    );
    println!("recording duration     : {:.1} s", dur as f64 / 1000.0);
    println!("detection wall-clock   : {:.1} s", wall_ms as f64 / 1000.0);
    if dur > 0 {
        println!(
            "wall / real-time ratio : {:.2}x  (ADR 0015 budget ~1.5x incl. audio+motion)",
            wall_ms as f64 / dur as f64
        );
    }
}

/// Parsed `detect-dump` options.
struct DumpOptions {
    db: Option<String>,
    library: Option<String>,
    file: Option<String>,
    filter: Option<String>,
    limit: Option<usize>,
    help: bool,
}

impl DumpOptions {
    fn parse(args: &[String]) -> Result<DumpOptions, String> {
        let mut o = DumpOptions {
            db: None,
            library: None,
            file: None,
            filter: None,
            limit: None,
            help: false,
        };
        let mut it = args.iter();
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "-h" | "--help" => o.help = true,
                "--db" => o.db = Some(it.next().ok_or("--db needs a path")?.clone()),
                "--file" => o.file = Some(it.next().ok_or("--file needs a path")?.clone()),
                "--library" => {
                    let k = it.next().ok_or("--library needs local|shared")?.clone();
                    if k != "local" && k != "shared" {
                        return Err(format!("--library must be local or shared, not {k}"));
                    }
                    o.library = Some(k);
                }
                "--limit" => {
                    let n = it.next().ok_or("--limit needs a number")?;
                    let n: usize = n.parse().map_err(|_| format!("--limit not a number: {n}"))?;
                    // 0 means "print all".
                    o.limit = if n == 0 { None } else { Some(n) };
                }
                other if other.starts_with("--") => return Err(format!("unknown flag {other}")),
                other => {
                    if o.filter.replace(other.to_string()).is_some() {
                        return Err("give at most one recording filter".to_string());
                    }
                }
            }
        }
        Ok(o)
    }
}

fn print_dump_usage() {
    println!(
        "detect-dump — run the occupancy detector on one recording and dump its track (issue #83)\n\n\
         USAGE:\n    detect-dump [--db PATH] [--library local|shared] [--file PATH] [--limit N] [RECORDING]\n\n\
         OPTIONS:\n    \
         --file PATH            dump this media file directly, bypassing the DB\n    \
         --db PATH              metadata DB to read (default: the app's own DB)\n    \
         --library local|shared which library to resolve from (default: the active one)\n    \
         --limit N              print only the first N samples (0 = all; default all)\n    \
         RECORDING              path substring; the first matching library recording\n    \
         -h, --help             show this help"
    );
}

/// The app's own metadata DB path for this OS — the same `voloph.db` the app opens.
/// Mirrors the eval harness so `detect-dump` reads the real library with no arguments.
fn default_db_path() -> Result<PathBuf, String> {
    const IDENTIFIER: &str = "com.quantumff.voloph";
    #[cfg(target_os = "linux")]
    let root = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).or_else(|| {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share"))
    });
    #[cfg(target_os = "macos")]
    let root = std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join("Library").join("Application Support"));
    #[cfg(target_os = "windows")]
    let root = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    let root: Option<PathBuf> = None;

    root.map(|r| r.join(IDENTIFIER).join("voloph.db"))
        .ok_or_else(|| "cannot resolve the app-data dir; pass --db PATH or --file PATH".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One anchor with a high person score decodes to a box centered at its grid cell,
    /// widths exp-scaled by the stride, and survives thresholding.
    #[test]
    fn decode_places_a_high_score_person_box() {
        // Single head is impossible to isolate (decode walks all three), but the first
        // anchor is grid cell (0,0) of stride 8. Build a full-length output where only
        // anchor 0 clears the threshold.
        let grid8 = (MODEL_SIZE / 8) as usize;
        let grid16 = (MODEL_SIZE / 16) as usize;
        let grid32 = (MODEL_SIZE / 32) as usize;
        let n = grid8 * grid8 + grid16 * grid16 + grid32 * grid32;
        let cols = 85;
        let mut data = vec![0f32; n * cols];
        // Anchor 0: cx,cy offset 0.5 within cell → pixel (4,4) at stride 8; w,h raw 0
        // → exp(0)*8 = 8; obj=1, person score=1.
        data[0] = 0.5; // raw cx
        data[1] = 0.5; // raw cy
        data[2] = 0.0; // raw w
        data[3] = 0.0; // raw h
        data[4] = 1.0; // obj
        data[5 + PERSON_CLASS] = 1.0; // person cls
        let boxes = decode(&data, n, cols, SCORE_THRESHOLD);
        assert_eq!(boxes.len(), 1, "only one anchor clears the threshold");
        let b = boxes[0];
        // center (4,4), size 8 → corners (0,0)-(8,8).
        assert!((b.x1 - 0.0).abs() < 1e-4, "{b:?}");
        assert!((b.y1 - 0.0).abs() < 1e-4, "{b:?}");
        assert!((b.x2 - 8.0).abs() < 1e-4, "{b:?}");
        assert!((b.y2 - 8.0).abs() < 1e-4, "{b:?}");
        assert!((b.score - 1.0).abs() < 1e-6);
    }

    /// Anchors below the score threshold are dropped entirely.
    #[test]
    fn decode_drops_low_score_anchors() {
        let grid8 = (MODEL_SIZE / 8) as usize;
        let grid16 = (MODEL_SIZE / 16) as usize;
        let grid32 = (MODEL_SIZE / 32) as usize;
        let n = grid8 * grid8 + grid16 * grid16 + grid32 * grid32;
        let cols = 85;
        let mut data = vec![0f32; n * cols];
        data[4] = 0.4; // obj
        data[5] = 0.4; // person → score 0.16 < 0.35
        assert!(decode(&data, n, cols, SCORE_THRESHOLD).is_empty());
    }

    /// A lowered floor admits the sub-threshold anchor the shipped floor drops —
    /// the scratch-extraction path of issue #93's presence measurement.
    #[test]
    fn decode_admits_sub_threshold_anchors_at_a_lower_floor() {
        let grid8 = (MODEL_SIZE / 8) as usize;
        let grid16 = (MODEL_SIZE / 16) as usize;
        let grid32 = (MODEL_SIZE / 32) as usize;
        let n = grid8 * grid8 + grid16 * grid16 + grid32 * grid32;
        let cols = 85;
        let mut data = vec![0f32; n * cols];
        data[4] = 0.4; // obj
        data[5] = 0.4; // person → score 0.16: below 0.35, above 0.10
        let boxes = decode(&data, n, cols, 0.10);
        assert_eq!(boxes.len(), 1);
        assert!((boxes[0].score - 0.16).abs() < 1e-6);
    }

    /// The `>= SCORE_THRESHOLD` subset of a low-floor decode is exactly the default
    /// decode: sub-floor boxes are appended by NMS's descending-score order, never
    /// suppressing a shipped box — the banding invariant the presence measurement
    /// (issue #93) rests on when it derives every floor from one 0.10 extraction.
    #[test]
    fn low_floor_decode_bands_down_to_the_default_decode() {
        let grid8 = (MODEL_SIZE / 8) as usize;
        let grid16 = (MODEL_SIZE / 16) as usize;
        let grid32 = (MODEL_SIZE / 32) as usize;
        let n = grid8 * grid8 + grid16 * grid16 + grid32 * grid32;
        let cols = 85;
        let mut data = vec![0f32; n * cols];
        // Anchor 0 (stride-8 cell (0,0)): confident box.
        data[0] = 0.5;
        data[1] = 0.5;
        data[4] = 1.0;
        data[5] = 0.9; // score 0.9
        // Anchor 1 (stride-8 cell (1,0)): sub-threshold box decoding to exactly
        // anchor 0's box (raw cx −0.5 cancels the grid offset) — NMS bait that the
        // confident box must suppress, not the other way around.
        data[cols] = -0.5;
        data[cols + 1] = 0.5;
        data[cols + 4] = 0.8;
        data[cols + 5] = 0.25; // score 0.2
        // A far-away sub-threshold box that survives NMS at the low floor.
        let far = 30 * cols;
        data[far] = 0.5;
        data[far + 1] = 0.5;
        data[far + 4] = 0.8;
        data[far + 5] = 0.25;
        let default = decode(&data, n, cols, SCORE_THRESHOLD);
        let low = decode(&data, n, cols, 0.10);
        assert!(low.len() > default.len(), "the low floor admits more boxes");
        let banded: Vec<PixelBox> = low
            .iter()
            .filter(|b| b.score >= SCORE_THRESHOLD)
            .copied()
            .collect();
        assert_eq!(banded, default);
    }

    /// A non-person class scoring high never survives — only class 0 is kept.
    #[test]
    fn decode_ignores_non_person_classes() {
        let grid8 = (MODEL_SIZE / 8) as usize;
        let grid16 = (MODEL_SIZE / 16) as usize;
        let grid32 = (MODEL_SIZE / 32) as usize;
        let n = grid8 * grid8 + grid16 * grid16 + grid32 * grid32;
        let cols = 85;
        let mut data = vec![0f32; n * cols];
        data[4] = 1.0; // obj
        data[5 + 1] = 1.0; // class 1 (bicycle), not person
        assert!(decode(&data, n, cols, SCORE_THRESHOLD).is_empty());
    }

    /// NMS collapses two heavily overlapping boxes to the higher-scoring one and keeps
    /// a disjoint box.
    #[test]
    fn nms_suppresses_overlaps_keeps_disjoint() {
        let a = PixelBox { x1: 0.0, y1: 0.0, x2: 10.0, y2: 10.0, score: 0.9 };
        let a2 = PixelBox { x1: 1.0, y1: 1.0, x2: 11.0, y2: 11.0, score: 0.8 }; // ~IoU 0.68
        let far = PixelBox { x1: 100.0, y1: 100.0, x2: 110.0, y2: 110.0, score: 0.7 };
        let kept = nms(vec![a2, far, a]);
        assert_eq!(kept.len(), 2, "one overlap suppressed, disjoint kept");
        // Highest score of the overlapping pair survives.
        assert!((kept[0].score - 0.9).abs() < 1e-6);
    }

    /// IoU of a box with itself is 1; disjoint boxes are 0.
    #[test]
    fn iou_bounds() {
        let a = PixelBox { x1: 0.0, y1: 0.0, x2: 10.0, y2: 10.0, score: 1.0 };
        let far = PixelBox { x1: 20.0, y1: 20.0, x2: 30.0, y2: 30.0, score: 1.0 };
        assert!((iou(&a, &a) - 1.0).abs() < 1e-6);
        assert_eq!(iou(&a, &far), 0.0);
    }

    /// A square source needs no letterbox: model-space maps straight to normalized
    /// coordinates by dividing by the side.
    #[test]
    fn letterbox_square_source_is_identity_scaled() {
        let lb = Letterbox::new(MODEL_SIZE, MODEL_SIZE);
        assert!((lb.scale - 1.0).abs() < 1e-6);
        assert_eq!(lb.pad_x, 0.0);
        assert_eq!(lb.pad_y, 0.0);
        let b = PixelBox { x1: 0.0, y1: 0.0, x2: 208.0, y2: 416.0, score: 0.5 };
        let n = lb.to_source_norm(&b).unwrap();
        assert!((n.x - 0.0).abs() < 1e-6);
        assert!((n.w - 0.5).abs() < 1e-6, "{n:?}"); // 208/416
        assert!((n.h - 1.0).abs() < 1e-6, "{n:?}");
    }

    /// A wide (16:9) source is padded top and bottom; a box in the padded (letterboxed)
    /// frame maps back into full-height source-normalized space, and the pad offset is
    /// removed.
    #[test]
    fn letterbox_wide_source_removes_vertical_pad() {
        // 1920x1080 → scale = 416/1920 = 0.21667; scaled height = 234; pad_y = 91.
        let lb = Letterbox::new(1920, 1080);
        assert!((lb.scale - 416.0 / 1920.0).abs() < 1e-6);
        assert!((lb.pad_x - 0.0).abs() < 1e-4);
        assert!((lb.pad_y - (416.0 - 1080.0 * (416.0 / 1920.0)) / 2.0).abs() < 1e-3);
        // A box exactly filling the scaled content region maps to the whole source.
        let content_top = lb.pad_y;
        let content_bottom = 416.0 - lb.pad_y;
        let b = PixelBox { x1: 0.0, y1: content_top, x2: 416.0, y2: content_bottom, score: 0.6 };
        let n = lb.to_source_norm(&b).unwrap();
        assert!((n.x - 0.0).abs() < 1e-4, "{n:?}");
        assert!((n.y - 0.0).abs() < 1e-4, "{n:?}");
        assert!((n.w - 1.0).abs() < 1e-4, "{n:?}");
        assert!((n.h - 1.0).abs() < 1e-4, "{n:?}");
    }
}
