//! **Shuttle tracking** (ADR 0019, issue #105): the first stage of the selectable
//! *teacher* rally segmenter — TrackNetV3 run over a whole recording, giving one
//! shuttle position per 30 fps frame. It is the in-app port of what
//! `scripts/teacher-tracknet.py --eval-mode nonoverlap` produces offline (TrackNetV3's
//! own `predict.py` on 120 s proxies, stitched), and returns the same per-frame track:
//! frame `i` at `i / 30` s, `visible` + `x, y` in TrackNet's 512x288 input pixels.
//!
//! **The models.** TrackNetV3 (MIT, github.com/qaz812345/TrackNetV3) — two networks
//! exported to ONNX by `scripts/export-tracknet-onnx.py` and vendored in `models/`
//! (`tracknet_v3.onnx`, `inpaintnet_v3.onnx`; contract in `models/README.md`):
//! - *TrackNet* takes 8 consecutive RGB frames plus the background (the per-pixel
//!   median frame) as 27 channels `[median, f0, …, f7] x RGB`, each `u8 / 255`, and
//!   emits 8 sigmoid heatmaps `288x512`.
//! - *InpaintNet* takes 16 consecutive normalized coordinates plus a 0/1 mask of the
//!   frames to repair, and emits 16 coordinates; it fills short gaps in a trajectory.
//!
//! **Mirroring `predict.py` (nonoverlap mode, not `--large_video`).** The recording is
//! decoded once by the ffmpeg sidecar at `fps=30,scale=512:288` as RGB and cut into
//! [`CHUNK_FRAMES`] chunks (chunk `k` starts at frame `k * 3600`). Each chunk is
//! handled exactly as `predict.py` handles one proxy video:
//! 1. the background is `np.median` over **every** frame of the chunk (the in-memory
//!    `Shuttlecock_Trajectory_Dataset` path; `max_sample_num` only applies to
//!    `--large_video`), truncated to `u8` — [`median_frame`];
//! 2. TrackNet runs on non-overlapping 8-frame windows, the last one padded by
//!    repeating the chunk's last frame — [`windows`];
//! 3. each heatmap is thresholded at `> 0.5` and reduced to the centre of the bounding
//!    box with the largest *box* area among its 8-connected blobs, as `cv2.findContours`
//!    + `cv2.boundingRect` do it — [`heatmap_point`];
//! 4. `generate_inpaint_mask` marks gaps between visible points ([`inpaint_mask`]),
//!    InpaintNet runs on non-overlapping 16-frame windows of the coordinates
//!    (normalized by 512/288), its output replaces the masked frames only, and points
//!    under `COOR_TH` on both axes are dropped — [`inpaint_chunk`].
//!
//! Chunks are stitched and trimmed to the probed duration as the script's `stitch()`
//! does (the fps filter can pad a variable-frame-rate source's tail).
//!
//! **Deviations from the offline run, by design.** Offline, ffmpeg re-encoded each
//! chunk to libx264 (crf 18) and OpenCV decoded that proxy; the app never re-encodes,
//! so its pixels differ by the proxy's compression noise — and, for 10-bit (HLG)
//! sources, by OpenCV's mis-decode of the 10-bit H.264 proxies (G/B ~33 levels off),
//! which the app does not reproduce. The offline segment muxer also cut its first
//! chunk at frame 3850, not 3600 (see [`track_recording_chunked`]); the app uses the
//! regular 3600-frame layout. Nonoverlap TrackNet is sensitive to where its 8-frame
//! windows fall, so the two tracks agree on visibility but not frame-for-frame.
//!
//! **Memory.** The median needs every frame of a chunk before the chunk's first window
//! can run, so up to one chunk of frames (3600 x 442 KB ≈ 1.6 GB) is held at once. The
//! frame buffers are pooled: as windows of chunk `k` are consumed their buffers go back
//! to the decoder thread for chunk `k + 1`, so decoding overlaps inference without a
//! second chunk in memory.
//!
//! **Testing (as ADR 0015).** Model quality is judged on real footage (`tracknet-dump`
//! against the offline CSVs); the pure pieces — median, heatmap → point, inpaint mask,
//! window indexing, coordinate round-trip — carry unit tests checked against the
//! Python functions.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;

use ort::session::{Session, SessionInputValue};
use ort::value::Tensor;

use crate::media::sidecar_path;

/// The track's frame rate: the offline proxies were cut at 30 fps.
pub const FPS: u32 = 30;
/// TrackNet's input width (`utils/general.py` `WIDTH`); track `x` is in these pixels.
pub const WIDTH: usize = 512;
/// TrackNet's input height (`HEIGHT`); track `y` is in these pixels.
pub const HEIGHT: usize = 288;
/// Frames per chunk: the offline run cut 120 s proxies, and every per-video step of
/// `predict.py` (median, windows, inpaint mask) is scoped to one of them.
pub const CHUNK_FRAMES: usize = 120 * FPS as usize;

const PLANE: usize = WIDTH * HEIGHT;
const FRAME_BYTES: usize = PLANE * 3;
/// TrackNet window (checkpoint `param_dict['seq_len']`).
const SEQ_LEN: usize = 8;
/// TrackNet input channels: background + 8 frames, RGB each (`bg_mode='concat'`).
const IN_CH: usize = (SEQ_LEN + 1) * 3;
/// InpaintNet window (its checkpoint's `seq_len`).
const INPAINT_LEN: usize = 16;
/// TrackNet windows per inference call. Kept modest — the full-resolution U-Net
/// activations of one window run to hundreds of MB, and the GPU may be shared — and
/// every call is padded to this size so the CUDA EP never re-tunes its convolutions
/// for a ragged last batch.
const TRACKNET_BATCH: usize = 4;
/// InpaintNet windows per call (a chunk is 225 windows of 16 tiny vectors).
const INPAINT_BATCH: usize = 64;
/// Frame buffers in the decode pool: one chunk plus a few batches of read-ahead.
const POOL_FRAMES: usize = CHUNK_FRAMES + 4 * TRACKNET_BATCH * SEQ_LEN;

/// One frame of the shuttle track, in TrackNet's 512x288 input pixels. `x == y == 0`
/// exactly when `visible` is false (TrackNetV3's convention: `Visibility = 0` iff the
/// predicted point is the origin).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ShuttlePoint {
    pub visible: bool,
    pub x: i32,
    pub y: i32,
}

impl ShuttlePoint {
    fn at(x: i32, y: i32) -> ShuttlePoint {
        ShuttlePoint {
            visible: !(x == 0 && y == 0),
            x,
            y,
        }
    }
}

/// A whole recording's shuttle track: `points[i]` is frame `i`, at `i / fps` seconds.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ShuttleTrack {
    pub fps: f64,
    pub points: Vec<ShuttlePoint>,
}

/// The two loaded networks. Built like [`crate::detect::Detector`]: the GPU execution
/// providers for this OS are tried ahead of CPU, falling back silently — and, unlike a
/// bare `ort` fallback, also when a GPU provider *registers* but cannot create the
/// session (CUDA libraries installed, no usable device).
pub struct ShuttleTracker {
    tracknet: Session,
    inpaintnet: Session,
    provider: String,
}

impl ShuttleTracker {
    /// Load both vendored models (see [`crate::detect::vendored_model_file`]).
    pub fn load_vendored() -> Result<ShuttleTracker, String> {
        let tn = crate::detect::vendored_model_file("tracknet_v3.onnx", "TrackNet")?;
        let ip = crate::detect::vendored_model_file("inpaintnet_v3.onnx", "InpaintNet")?;
        ShuttleTracker::load(&tn, &ip, true)
    }

    /// Load the two models from explicit paths. `gpu` false builds CPU-only sessions
    /// (the dev CLI's `--cpu`, for timing the fallback path on a GPU machine).
    pub fn load(tracknet: &Path, inpaintnet: &Path, gpu: bool) -> Result<ShuttleTracker, String> {
        if gpu {
            let built = build_session(tracknet, true)
                .and_then(|t| Ok((t, build_session(inpaintnet, true)?)));
            match built {
                Ok((tracknet, inpaintnet)) => {
                    return Ok(ShuttleTracker {
                        tracknet,
                        inpaintnet,
                        provider: probe_execution_provider(),
                    })
                }
                Err(e) => log::warn!("shuttle tracker: GPU session failed, using CPU: {e}"),
            }
        }
        Ok(ShuttleTracker {
            tracknet: build_session(tracknet, false)?,
            inpaintnet: build_session(inpaintnet, false)?,
            provider: if gpu {
                "CPU (GPU session failed)"
            } else {
                "CPU (forced)"
            }
            .to_string(),
        })
    }

    /// The execution provider the sessions run on, for diagnostics: a GPU provider's
    /// name, or `CPU` with the reason when no GPU provider took.
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// TrackNet on `n` windows of `IN_CH x HEIGHT x WIDTH` input, padded to
    /// [`TRACKNET_BATCH`]; returns the `n x SEQ_LEN x HEIGHT x WIDTH` heatmaps.
    fn run_tracknet(&mut self, mut x: Vec<f32>, n: usize) -> Result<Vec<f32>, String> {
        let per = IN_CH * PLANE;
        debug_assert_eq!(x.len(), n * per);
        x.resize(TRACKNET_BATCH * per, 0.0);
        let shape = vec![
            TRACKNET_BATCH as i64,
            IN_CH as i64,
            HEIGHT as i64,
            WIDTH as i64,
        ];
        let tensor = Tensor::from_array((shape, x)).map_err(|e| format!("ort tensor: {e}"))?;
        let input: SessionInputValue<'_> = tensor.into();
        let outputs = self
            .tracknet
            .run(vec![("x".to_string(), input)])
            .map_err(|e| format!("TrackNet inference: {e}"))?;
        let (_, y) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("TrackNet output: {e}"))?;
        let want = n * SEQ_LEN * PLANE;
        if y.len() < want {
            return Err(format!("TrackNet gave {} values, expected {want}", y.len()));
        }
        Ok(y[..want].to_vec())
    }

    /// InpaintNet on `n` windows of `INPAINT_LEN` coordinates `x` (`n x 16 x 2`) and
    /// masks `m` (`n x 16 x 1`), padded to [`INPAINT_BATCH`]; returns `n x 16 x 2`.
    fn run_inpaintnet(
        &mut self,
        mut x: Vec<f32>,
        mut m: Vec<f32>,
        n: usize,
    ) -> Result<Vec<f32>, String> {
        x.resize(INPAINT_BATCH * INPAINT_LEN * 2, 0.0);
        m.resize(INPAINT_BATCH * INPAINT_LEN, 0.0);
        let b = INPAINT_BATCH as i64;
        let len = INPAINT_LEN as i64;
        let xt =
            Tensor::from_array((vec![b, len, 2], x)).map_err(|e| format!("ort tensor: {e}"))?;
        let mt =
            Tensor::from_array((vec![b, len, 1], m)).map_err(|e| format!("ort tensor: {e}"))?;
        let outputs = self
            .inpaintnet
            .run(vec![
                ("x".to_string(), SessionInputValue::from(xt)),
                ("m".to_string(), SessionInputValue::from(mt)),
            ])
            .map_err(|e| format!("InpaintNet inference: {e}"))?;
        let (_, y) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("InpaintNet output: {e}"))?;
        let want = n * INPAINT_LEN * 2;
        if y.len() < want {
            return Err(format!(
                "InpaintNet gave {} values, expected {want}",
                y.len()
            ));
        }
        Ok(y[..want].to_vec())
    }
}

fn build_session(model: &Path, gpu: bool) -> Result<Session, String> {
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(8);
    let mut builder = Session::builder().map_err(|e| format!("ort session builder: {e}"))?;
    if gpu {
        builder = builder
            .with_execution_providers(crate::detect::gpu_execution_providers())
            .map_err(|e| format!("ort execution providers: {e}"))?;
    }
    builder
        .with_intra_threads(threads)
        .map_err(|e| format!("ort intra threads: {e}"))?
        .commit_from_file(model)
        .map_err(|e| format!("ort could not load {}: {e}", model.display()))
}

/// Which execution provider a GPU-enabled session gets on this machine: the name of
/// the first GPU provider that registers (probed on a throwaway builder with
/// `error_on_failure`), else `"CPU"` with each GPU provider's registration error (e.g.
/// a missing CUDA 12 runtime). Diagnostic only — the real sessions keep `ort`'s silent
/// fallback.
pub fn probe_execution_provider() -> String {
    let mut why = Vec::new();
    for ep in crate::detect::gpu_execution_providers() {
        let name = format!("{ep:?}");
        let name = name.split_whitespace().next().unwrap_or("GPU").to_string();
        match Session::builder()
            .and_then(|b| b.with_execution_providers([ep.clone().error_on_failure()]))
        {
            Ok(_) => return name,
            Err(e) => why.push(format!("{name}: {e}")),
        }
    }
    if why.is_empty() {
        "CPU".to_string()
    } else {
        format!("CPU ({})", why.join("; "))
    }
}

// ── Pure pieces (unit-tested against TrackNetV3's Python) ───────────────────────

/// The per-pixel, per-channel median of `frames` (each `FRAME_BYTES`-long RGB), as
/// `np.median(frames, 0).astype('uint8')`: the middle value for an odd count, the
/// truncated mean of the two middle values for an even count. Exact, via a 256-bin
/// histogram per element, computed in row bands across threads.
pub(crate) fn median_frame(frames: &[&[u8]], frame_bytes: usize) -> Vec<u8> {
    let n = frames.len();
    let mut out = vec![0u8; frame_bytes];
    if n == 0 {
        return out;
    }
    assert!(n <= u16::MAX as usize, "median over more than 65535 frames");
    const BAND: usize = 1536; // one 512 px RGB row
    let threads = std::thread::available_parallelism()
        .map(|t| t.get())
        .unwrap_or(4)
        .min(8);
    let per_thread = frame_bytes.div_ceil(threads).div_ceil(BAND) * BAND;
    std::thread::scope(|s| {
        for (t, out_part) in out.chunks_mut(per_thread.max(1)).enumerate() {
            let base = t * per_thread.max(1);
            s.spawn(move || {
                let mut hist = vec![0u16; BAND * 256];
                for (b, out_band) in out_part.chunks_mut(BAND).enumerate() {
                    let off = base + b * BAND;
                    let len = out_band.len();
                    hist[..len * 256].iter_mut().for_each(|c| *c = 0);
                    for f in frames {
                        for (e, &v) in f[off..off + len].iter().enumerate() {
                            hist[e * 256 + v as usize] += 1;
                        }
                    }
                    for (e, o) in out_band.iter_mut().enumerate() {
                        *o = median_of_hist(&hist[e * 256..(e + 1) * 256], n);
                    }
                }
            });
        }
    });
    out
}

/// The median of `n` values given their histogram, as `np.median(...).astype(uint8)`.
fn median_of_hist(hist: &[u16], n: usize) -> u8 {
    let value_at = |rank: usize| -> usize {
        let mut cum = 0usize;
        for (v, &c) in hist.iter().enumerate() {
            cum += c as usize;
            if cum > rank {
                return v;
            }
        }
        255
    };
    if n % 2 == 1 {
        value_at(n / 2) as u8
    } else {
        ((value_at(n / 2 - 1) + value_at(n / 2)) / 2) as u8
    }
}

/// The non-overlapping windows `predict.py` feeds over a sequence of `n` frames
/// (`Shuttlecock_Trajectory_Dataset(sliding_step=len, padding=True)`): window `w`
/// holds frames `w*len ..`, the last one padded by repeating frame `n - 1`.
pub(crate) fn windows(n: usize, len: usize) -> Vec<Vec<usize>> {
    (0..n)
        .step_by(len)
        .map(|i| (0..len).map(|f| (i + f).min(n - 1)).collect())
        .collect()
}

/// How many leading entries of a window `predict()` keeps: it walks the window and
/// stops at the first repeated frame index (the padding).
fn kept(window: &[usize]) -> usize {
    1 + window.windows(2).take_while(|p| p[1] != p[0]).count()
}

/// One heatmap (`HEIGHT x WIDTH`, sigmoid) → the point `predict()` reports:
/// threshold `> 0.5`; no foreground → `(0, 0)`; else the 8-connected blob whose
/// **bounding box** (`cv2.boundingRect`: `w = max - min + 1`) has the largest area —
/// on a tie the blob found *last* in raster order (by its first pixel), because
/// `cv2.findContours` lists outer contours in reverse discovery order and `predict_location`
/// keeps the first strict maximum — and its centre `(x + w // 2, y + h // 2)`. Blobs
/// nested in another's hole (dropped by `RETR_EXTERNAL`) always have a strictly
/// smaller box than their encloser, so including them never changes the pick.
pub(crate) fn heatmap_point(heat: &[f32], w: usize, h: usize) -> (i32, i32) {
    debug_assert_eq!(heat.len(), w * h);
    let fg: Vec<usize> = heat
        .iter()
        .enumerate()
        .filter(|(_, &v)| v > 0.5)
        .map(|(i, _)| i)
        .collect();
    if fg.is_empty() {
        return (0, 0);
    }
    // 0 = background, 1 = unvisited foreground, 2 = visited.
    let mut state = vec![0u8; w * h];
    for &p in &fg {
        state[p] = 1;
    }
    let mut stack = Vec::new();
    let mut best: Option<(usize, usize, usize, usize)> = None; // x, y, bw, bh
    let mut best_area = 0usize;
    for &start in &fg {
        if state[start] != 1 {
            continue;
        }
        state[start] = 2;
        stack.push(start);
        let (mut x0, mut y0, mut x1, mut y1) = (start % w, start / w, start % w, start / w);
        while let Some(p) = stack.pop() {
            let (px, py) = (p % w, p / w);
            x0 = x0.min(px);
            x1 = x1.max(px);
            y0 = y0.min(py);
            y1 = y1.max(py);
            for ny in py.saturating_sub(1)..=(py + 1).min(h - 1) {
                for nx in px.saturating_sub(1)..=(px + 1).min(w - 1) {
                    let q = ny * w + nx;
                    if state[q] == 1 {
                        state[q] = 2;
                        stack.push(q);
                    }
                }
            }
        }
        let (bw, bh) = (x1 - x0 + 1, y1 - y0 + 1);
        if bw * bh >= best_area {
            best_area = bw * bh;
            best = Some((x0, y0, bw, bh));
        }
    }
    let (x, y, bw, bh) = best.expect("non-empty foreground has a blob");
    ((x + bw / 2) as i32, (y + bh / 2) as i32)
}

/// `generate_inpaint_mask(pred_dict, th_h)` from TrackNetV3's `test.py`, ported
/// literally (including its `i > 1` guard): 1 marks frames in an invisible run that
/// InpaintNet should fill — a run bounded by points below the top `th_h` pixels, or a
/// leading run whose first visible point is.
pub(crate) fn inpaint_mask(ys: &[i32], vis: &[bool], th_h: f64) -> Vec<u8> {
    let n = vis.len();
    let mut mask = vec![0u8; n];
    let (mut i, mut j) = (0usize, 0usize);
    let above = |k: usize| f64::from(ys[k]) > th_h;
    while j < n {
        while i + 1 < n && vis[i] {
            i += 1;
        }
        j = i;
        while j + 1 < n && !vis[j] {
            j += 1;
        }
        if j == i {
            break;
        } else if i == 0 && above(j) {
            mask[..j].iter_mut().for_each(|m| *m = 1);
        } else if i > 1 && above(i - 1) && above(j) {
            mask[i..j].iter_mut().for_each(|m| *m = 1);
        }
        i = j;
    }
    mask
}

/// `COOR_TH` from `utils/general.py` (`DELTA_T * 50`), at the `f32` precision torch
/// compares it in.
fn coor_th() -> f32 {
    let delta_t = 1.0 / ((HEIGHT * HEIGHT + WIDTH * WIDTH) as f64).sqrt();
    (delta_t * 50.0) as f32
}

/// A pixel coordinate normalized as the coordinate dataset does it (`f64` divide, then
/// torch's `.float()`).
fn norm(v: i32, size: usize) -> f32 {
    (f64::from(v) / size as f64) as f32
}

/// A normalized coordinate back to pixels as `predict()` does it: `int(c * size)` in
/// `float32`, truncating.
fn denorm(c: f32, size: usize) -> i32 {
    (c * size as f32) as i32
}

/// The InpaintNet stage over one chunk's TrackNet points, as `predict.py` runs it in
/// nonoverlap mode. `infer` maps `n` windows of `(x, m)` to `n x 16 x 2` outputs.
pub(crate) fn inpaint_chunk(
    pts: &[ShuttlePoint],
    mut infer: impl FnMut(Vec<f32>, Vec<f32>, usize) -> Result<Vec<f32>, String>,
) -> Result<Vec<ShuttlePoint>, String> {
    let n = pts.len();
    if n == 0 {
        return Ok(Vec::new());
    }
    let ys: Vec<i32> = pts.iter().map(|p| p.y).collect();
    let vis: Vec<bool> = pts.iter().map(|p| p.visible).collect();
    let mask = inpaint_mask(&ys, &vis, HEIGHT as f64 * 0.05);
    let th = coor_th();
    let wins = windows(n, INPAINT_LEN);
    let mut out = Vec::with_capacity(n);
    for batch in wins.chunks(INPAINT_BATCH) {
        let mut x = Vec::with_capacity(batch.len() * INPAINT_LEN * 2);
        let mut m = Vec::with_capacity(batch.len() * INPAINT_LEN);
        for win in batch {
            for &f in win {
                x.push(norm(pts[f].x, WIDTH));
                x.push(norm(pts[f].y, HEIGHT));
                m.push(f32::from(mask[f]));
            }
        }
        let y = infer(x.clone(), m.clone(), batch.len())?;
        for (b, win) in batch.iter().enumerate() {
            for f in 0..kept(win) {
                let k = b * INPAINT_LEN + f;
                let mk = m[k];
                let mut cx = y[2 * k] * mk + x[2 * k] * (1.0 - mk);
                let mut cy = y[2 * k + 1] * mk + x[2 * k + 1] * (1.0 - mk);
                if cx < th && cy < th {
                    cx = 0.0;
                    cy = 0.0;
                }
                out.push(ShuttlePoint::at(denorm(cx, WIDTH), denorm(cy, HEIGHT)));
            }
        }
    }
    Ok(out)
}

/// `(u8 as f64 / 255.0) as f32` for every byte: the dataset normalizes in `float64`
/// and the model input is cast to `float32`.
fn unit_lut() -> [f32; 256] {
    let mut lut = [0f32; 256];
    for (v, slot) in lut.iter_mut().enumerate() {
        *slot = (v as f64 / 255.0) as f32;
    }
    lut
}

/// Append one interleaved RGB frame as three planar channels to a model input.
fn push_planes(dst: &mut Vec<f32>, rgb: &[u8], lut: &[f32; 256]) {
    for c in 0..3 {
        dst.extend(rgb[c..].iter().step_by(3).map(|&v| lut[v as usize]));
    }
}

// ── The pipeline ─────────────────────────────────────────────────────────────────

/// Track the shuttle through a whole recording. Decodes with the ffmpeg sidecar at
/// `fps=30,scale=512:288`, runs [`CHUNK_FRAMES`]-frame chunks through TrackNet and
/// InpaintNet exactly as `predict.py --eval_mode nonoverlap` runs one proxy, stitches,
/// and trims frames at or past the probed duration (as `teacher-tracknet.py` does).
///
/// `on_progress` reports the footage position in ms as windows complete. Errors if the
/// sidecar cannot run or fails, inference fails, no frame decodes, or the decoded
/// length falls more than a second short of (or 3 s past) the probed duration — the
/// offline stitch's sanity check.
pub fn track_recording(
    path: &str,
    tracker: &mut ShuttleTracker,
    on_progress: impl FnMut(i64),
) -> Result<ShuttleTrack, String> {
    track_recording_chunked(path, tracker, CHUNK_FRAMES, on_progress)
}

/// [`track_recording`] with the **first** chunk `first_chunk` frames long instead of
/// [`CHUNK_FRAMES`] (later chunks unchanged). For parity measurements only: the
/// offline run's segment muxer cut its first proxy at frame 3850, not 3600 (libx264
/// does not flag the forced I-frame at 120 s as a cut point, so the cut waits for the
/// IDR 250 frames later), which shifts every later chunk's windows. `tracknet-dump
/// --first-chunk 3850` reproduces that layout; the app always uses [`CHUNK_FRAMES`].
pub fn track_recording_chunked(
    path: &str,
    tracker: &mut ShuttleTracker,
    first_chunk: usize,
    mut on_progress: impl FnMut(i64),
) -> Result<ShuttleTrack, String> {
    let first_chunk = first_chunk.max(1);
    let pool_frames = POOL_FRAMES.max(first_chunk + POOL_FRAMES - CHUNK_FRAMES);
    let duration_s = probe_duration_s(path);
    let vf = format!("fps={FPS},scale={WIDTH}:{HEIGHT}");
    let mut child = Command::new(sidecar_path("ffmpeg"))
        .args([
            "-v", "error", "-nostats", "-i", path, "-an", "-vf", &vf, "-f", "rawvideo", "-pix_fmt",
            "rgb24", "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("ffmpeg could not run: {e}"))?;
    let mut stdout = child.stdout.take().ok_or("ffmpeg gave no stdout")?;

    // Decoder thread: fills pooled frame buffers, hands them over in order.
    let (filled_tx, filled_rx) = mpsc::sync_channel::<Vec<u8>>(pool_frames);
    let (free_tx, free_rx) = mpsc::channel::<Vec<u8>>();
    let decoder = std::thread::spawn(move || {
        let mut allocated = 0usize;
        loop {
            let mut buf = match free_rx.try_recv() {
                Ok(b) => b,
                Err(_) if allocated < pool_frames => {
                    allocated += 1;
                    vec![0u8; FRAME_BYTES]
                }
                Err(_) => match free_rx.recv() {
                    Ok(b) => b,
                    Err(_) => return, // consumer gone
                },
            };
            if stdout.read_exact(&mut buf).is_err() || filled_tx.send(buf).is_err() {
                return; // end of stream (a partial last frame is dropped), or consumer gone
            }
        }
    });

    let result = (|| -> Result<Vec<ShuttlePoint>, String> {
        let mut points: Vec<ShuttlePoint> = Vec::new();
        let mut chunk: Vec<Vec<u8>> = Vec::with_capacity(CHUNK_FRAMES);
        let lut = unit_lut();
        loop {
            let next = filled_rx.recv().ok();
            let end = next.is_none();
            if let Some(f) = next {
                chunk.push(f);
            }
            let target = if points.is_empty() {
                first_chunk
            } else {
                CHUNK_FRAMES
            };
            if chunk.len() == target || (end && !chunk.is_empty()) {
                let base = points.len();
                let tn = tracknet_chunk(tracker, &mut chunk, &lut, &free_tx, |done| {
                    on_progress((base + done) as i64 * 1000 / i64::from(FPS))
                })?;
                points.extend(inpaint_chunk(&tn, |x, m, n| {
                    tracker.run_inpaintnet(x, m, n)
                })?);
                chunk.clear();
            }
            if end {
                return Ok(points);
            }
        }
    })();

    // Unblock the decoder if we bailed out early: it may be waiting to send, for a free
    // buffer, or on ffmpeg's pipe.
    drop(filled_rx);
    drop(free_tx);
    if result.is_err() {
        let _ = child.kill();
    }
    let _ = decoder.join();
    let waited = crate::media::wait_ffmpeg(&mut child, "ffmpeg failed to decode frames");
    let mut points = result?;
    waited?;
    if points.is_empty() {
        return Err("video yielded no frames for shuttle tracking".to_string());
    }
    if let Some(d) = duration_s {
        let expected = d * f64::from(FPS);
        let decoded = points.len() as f64;
        if decoded < expected - f64::from(FPS) || decoded > expected + 3.0 * f64::from(FPS) {
            return Err(format!(
                "decoded {decoded} frames, expected ~{expected:.0} from the {d:.3} s duration"
            ));
        }
        // Tail padding past the recording's end (variable-frame-rate sources).
        let keep = points
            .iter()
            .enumerate()
            .take_while(|(i, _)| (*i as f64) < expected)
            .count();
        points.truncate(keep);
    }
    Ok(ShuttleTrack {
        fps: f64::from(FPS),
        points,
    })
}

/// TrackNet over one chunk: median background, non-overlapping padded windows in
/// batches, each heatmap reduced to a point. Frame buffers are handed back to the
/// decoder through `free` as soon as their windows are built. `progress` gets the
/// number of chunk frames done.
fn tracknet_chunk(
    tracker: &mut ShuttleTracker,
    chunk: &mut [Vec<u8>],
    lut: &[f32; 256],
    free: &mpsc::Sender<Vec<u8>>,
    mut progress: impl FnMut(usize),
) -> Result<Vec<ShuttlePoint>, String> {
    let n = chunk.len();
    let median = {
        let refs: Vec<&[u8]> = chunk.iter().map(|f| f.as_slice()).collect();
        median_frame(&refs, FRAME_BYTES)
    };
    let mut median_planes = Vec::with_capacity(3 * PLANE);
    push_planes(&mut median_planes, &median, lut);

    let wins = windows(n, SEQ_LEN);
    let mut points = Vec::with_capacity(n);
    for batch in wins.chunks(TRACKNET_BATCH) {
        let mut x = Vec::with_capacity(TRACKNET_BATCH * IN_CH * PLANE);
        for win in batch {
            x.extend_from_slice(&median_planes);
            for &f in win {
                push_planes(&mut x, &chunk[f], lut);
            }
        }
        // Every frame of these windows is now in `x`: recycle the buffers.
        for win in batch {
            for &f in &win[..kept(win)] {
                let _ = free.send(std::mem::take(&mut chunk[f]));
            }
        }
        let y = tracker.run_tracknet(x, batch.len())?;
        for (b, win) in batch.iter().enumerate() {
            for f in 0..kept(win) {
                let off = (b * SEQ_LEN + f) * PLANE;
                let (px, py) = heatmap_point(&y[off..off + PLANE], WIDTH, HEIGHT);
                points.push(ShuttlePoint::at(px, py));
            }
        }
        progress(points.len());
    }
    Ok(points)
}

/// The recording's container duration in seconds (ffprobe `format=duration`), the
/// figure `teacher-tracknet.py` trims its stitch to. `None` when unavailable.
fn probe_duration_s(path: &str) -> Option<f64> {
    let out = Command::new(sidecar_path("ffprobe"))
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
            path,
        ])
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s: f64 = String::from_utf8_lossy(&out.stdout).trim().parse().ok()?;
    (s > 0.0).then_some(s)
}

// ── `tracknet-dump` dev CLI ──────────────────────────────────────────────────────

/// Run the `tracknet-dump` dev CLI: track one recording and write the shuttle track
/// in `teacher-tracknet.py`'s CSV format (`frame,t_ms,visible,x,y`), printing wall
/// time, the real-time ratio and the execution provider.
///
/// Usage: `tracknet-dump --file PATH --out CSV [--cpu] [--models DIR] [--first-chunk N]`
/// - `--cpu` — CPU-only sessions (time the no-GPU path on a GPU machine).
/// - `--models DIR` — load `tracknet_v3.onnx` / `inpaintnet_v3.onnx` from `DIR`.
/// - `--first-chunk N` — first chunk `N` frames long (3850 reproduces the offline
///   run's chunk layout; see [`track_recording_chunked`]).
pub fn dump_cli(args: Vec<String>) -> Result<(), String> {
    let (mut file, mut out, mut cpu, mut models) = (None, None, false, None::<PathBuf>);
    let mut first_chunk = CHUNK_FRAMES;
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        let mut val = |name: &str| it.next().ok_or(format!("{name} needs a value"));
        match a.as_str() {
            "--file" => file = Some(val("--file")?),
            "--out" => out = Some(val("--out")?),
            "--models" => models = Some(PathBuf::from(val("--models")?)),
            "--cpu" => cpu = true,
            "--first-chunk" => {
                first_chunk = val("--first-chunk")?
                    .parse()
                    .map_err(|e| format!("--first-chunk: {e}"))?
            }
            "-h" | "--help" => {
                println!(
                    "tracknet-dump --file PATH --out CSV [--cpu] [--models DIR] [--first-chunk N]"
                );
                return Ok(());
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let file = file.ok_or("--file is required")?;
    let out = out.ok_or("--out is required")?;
    let (tn, ip) = match &models {
        Some(dir) => (dir.join("tracknet_v3.onnx"), dir.join("inpaintnet_v3.onnx")),
        None => (
            crate::detect::vendored_model_file("tracknet_v3.onnx", "TrackNet")?,
            crate::detect::vendored_model_file("inpaintnet_v3.onnx", "InpaintNet")?,
        ),
    };
    let mut tracker = ShuttleTracker::load(&tn, &ip, !cpu)?;
    let provider = tracker.provider().to_string();
    eprintln!("execution provider: {provider}");
    let started = std::time::Instant::now();
    let track = track_recording_chunked(&file, &mut tracker, first_chunk, |_| {})?;
    let wall = started.elapsed().as_secs_f64();

    let mut csv = String::from("frame,t_ms,visible,x,y\n");
    for (i, p) in track.points.iter().enumerate() {
        let t_ms = (i as f64 * 1000.0 / f64::from(FPS)).round() as i64;
        csv.push_str(&format!(
            "{i},{t_ms},{},{},{}\n",
            u8::from(p.visible),
            p.x,
            p.y
        ));
    }
    let tmp = format!("{out}.tmp");
    std::fs::write(&tmp, csv)
        .and_then(|()| std::fs::rename(&tmp, &out))
        .map_err(|e| format!("{out}: {e}"))?;
    let footage = track.points.len() as f64 / f64::from(FPS);
    let visible = track.points.iter().filter(|p| p.visible).count();
    println!(
        "{} frames ({footage:.0} s, {:.1} % visible) in {wall:.1} s = {:.2}x real-time on {provider} -> {out}",
        track.points.len(),
        100.0 * visible as f64 / track.points.len().max(1) as f64,
        footage / wall.max(1e-9)
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_matches_numpy_odd_and_even() {
        // np.median([[3,7,1,200],[5,2,1,100],[4,9,2,0]], 0) -> [4, 7, 1, 100]
        let a = [3u8, 7, 1, 200];
        let b = [5u8, 2, 1, 100];
        let c = [4u8, 9, 2, 0];
        assert_eq!(median_frame(&[&a, &b, &c], 4), vec![4, 7, 1, 100]);
        // Even count: mean of the two middle values, truncated by astype('uint8'):
        // np.median([[3,7,1,200],[5,2,1,100],[4,9,2,0],[0,255,2,255]], 0).astype('uint8')
        //   -> [3.5, 8.0, 1.5, 150.0] -> [3, 8, 1, 150]
        let d = [0u8, 255, 2, 255];
        assert_eq!(median_frame(&[&a, &b, &c, &d], 4), vec![3, 8, 1, 150]);
    }

    #[test]
    fn median_covers_a_full_frame_across_bands() {
        // Element e of frame k holds (e + k) % 251: every band and thread boundary sees
        // its own value set. Brute-force the reference.
        let n = 6;
        let frames: Vec<Vec<u8>> = (0..n)
            .map(|k| {
                (0..FRAME_BYTES)
                    .map(|e| ((e * 7 + k * 31) % 251) as u8)
                    .collect()
            })
            .collect();
        let refs: Vec<&[u8]> = frames.iter().map(|f| f.as_slice()).collect();
        let got = median_frame(&refs, FRAME_BYTES);
        for e in (0..FRAME_BYTES).step_by(997) {
            let mut v: Vec<u32> = frames.iter().map(|f| f[e] as u32).collect();
            v.sort();
            assert_eq!(got[e] as u32, (v[2] + v[3]) / 2, "element {e}");
        }
    }

    #[test]
    fn windows_pad_the_last_with_the_final_frame() {
        // Shuttlecock_Trajectory_Dataset(seq_len=8, sliding_step=8, padding=True) over
        // 19 frames: ids [0..7], [8..15], [16,17,18,18,18,18,18,18].
        let w = windows(19, 8);
        assert_eq!(w.len(), 3);
        assert_eq!(w[0], (0..8).collect::<Vec<_>>());
        assert_eq!(w[2], vec![16, 17, 18, 18, 18, 18, 18, 18]);
        assert_eq!(kept(&w[0]), 8);
        assert_eq!(kept(&w[2]), 3);
        // A single leftover frame: [16] * 8 keeps one.
        let w = windows(17, 8);
        assert_eq!(w[2], vec![16; 8]);
        assert_eq!(kept(&w[2]), 1);
        // Exact multiple: no padding window.
        assert_eq!(windows(16, 8).len(), 2);
    }

    fn heat(w: usize, h: usize, on: &[(usize, usize)]) -> Vec<f32> {
        let mut v = vec![0.1f32; w * h];
        for &(x, y) in on {
            v[y * w + x] = 0.9;
        }
        v
    }

    #[test]
    fn heatmap_empty_and_threshold() {
        assert_eq!(heatmap_point(&vec![0.5f32; 64], 8, 8), (0, 0)); // > 0.5, not >=
        let mut v = vec![0.0f32; 64];
        v[3 * 8 + 5] = 0.5001;
        assert_eq!(heatmap_point(&v, 8, 8), (5, 3));
    }

    #[test]
    fn heatmap_picks_largest_bounding_box_not_pixel_count() {
        // A diagonal of 4 px (8-connected, box 4x4 = 16) beats a solid 3x3 (box 9).
        // cv2: rects [(5,4,3,3), (0,0,4,4)] -> picks (0,0,4,4) -> centre (2, 2).
        let mut on = vec![(0, 0), (1, 1), (2, 2), (3, 3)];
        for y in 4..7 {
            for x in 5..8 {
                on.push((x, y));
            }
        }
        assert_eq!(heatmap_point(&heat(10, 8, &on), 10, 8), (2, 2));
    }

    #[test]
    fn heatmap_ties_go_to_the_blob_found_last_in_raster_order() {
        // Two 2x1 blobs with equal boxes; cv2.findContours returns the lower one first,
        // so predict_location keeps it: rects [(1,5,2,1), (6,1,2,1)] -> (1+1, 5+0).
        let on = [(6, 1), (7, 1), (1, 5), (2, 5)];
        assert_eq!(heatmap_point(&heat(10, 8, &on), 10, 8), (2, 5));
        // Same row: the right one starts later in raster order.
        let on = [(1, 2), (2, 2), (6, 2), (7, 2)];
        assert_eq!(heatmap_point(&heat(10, 8, &on), 10, 8), (7, 2));
    }

    #[test]
    fn heatmap_centre_truncates_and_touches_borders() {
        // Box (0,0,1,1): centre (0,0) — invisible by TrackNet's convention.
        assert_eq!(heatmap_point(&heat(6, 4, &[(0, 0)]), 6, 4), (0, 0));
        // Box x 3..5 (w 3), y 2..3 (h 2) at the bottom-right edge: (3 + 1, 2 + 1).
        let on = [(3, 2), (4, 2), (5, 3)];
        assert_eq!(heatmap_point(&heat(6, 4, &on), 6, 4), (4, 3));
    }

    #[test]
    fn inpaint_mask_matches_generate_inpaint_mask() {
        // Each case checked against test.generate_inpaint_mask with th_h=14.4.
        let case = |vis: &[u8], ys: &[i32]| {
            let v: Vec<bool> = vis.iter().map(|&b| b == 1).collect();
            inpaint_mask(ys, &v, 14.4)
        };
        // gap bounded by low points (y > th) on both sides -> filled
        assert_eq!(
            case(&[1, 1, 0, 0, 1, 1], &[50, 60, 0, 0, 70, 80]),
            vec![0, 0, 1, 1, 0, 0]
        );
        // a point above the threshold before the gap -> not filled
        assert_eq!(
            case(&[1, 1, 0, 0, 1, 1], &[50, 10, 0, 0, 70, 80]),
            vec![0, 0, 0, 0, 0, 0]
        );
        // leading gap whose first visible point is low -> mask[:j] = 1
        assert_eq!(case(&[0, 0, 1, 1], &[0, 0, 40, 40]), vec![1, 1, 0, 0]);
        // the i > 1 quirk: a gap starting at index 1 is never filled
        assert_eq!(case(&[1, 0, 0, 1], &[90, 0, 0, 90]), vec![0, 0, 0, 0]);
        // trailing gap: j stops at the last frame, whose y is 0 -> not filled
        assert_eq!(case(&[1, 1, 1, 0, 0], &[90, 90, 90, 0, 0]), vec![0; 5]);
        // all visible / all invisible / empty
        assert_eq!(case(&[1, 1, 1], &[90, 90, 90]), vec![0; 3]);
        assert_eq!(case(&[0, 0, 0], &[0, 0, 0]), vec![0; 3]);
        assert_eq!(case(&[], &[]), Vec::<u8>::new());
        // two gaps
        assert_eq!(
            case(&[1, 1, 0, 1, 0, 0, 1], &[30, 30, 0, 30, 0, 0, 30]),
            vec![0, 0, 1, 0, 1, 1, 0]
        );
    }

    #[test]
    fn coordinate_round_trip_is_exact_and_coor_th_matches() {
        // int(np.float32(np.float64(v) / size) * size) == v for every pixel on both axes
        // (checked in numpy), so untouched TrackNet points pass InpaintNet unchanged.
        assert!((0..=288).all(|y| denorm(norm(y, HEIGHT), HEIGHT) == y));
        assert!((0..=512).all(|x| denorm(norm(x, WIDTH), WIDTH) == x));
        // utils.general.COOR_TH = 0.08511479854731926
        assert!((coor_th() - 0.085_114_8).abs() < 1e-7);
    }

    #[test]
    fn inpaint_chunk_blends_masked_frames_and_thresholds() {
        // 5 frames: a gap at 2..=3 bounded by low points gets InpaintNet's output; the
        // rest pass through; an output under COOR_TH on both axes becomes invisible.
        let pts = [
            ShuttlePoint::at(100, 100),
            ShuttlePoint::at(110, 100),
            ShuttlePoint::at(0, 0),
            ShuttlePoint::at(0, 0),
            ShuttlePoint::at(10, 20), // under COOR_TH (43.6 px, 24.5 px) -> dropped
        ];
        let got = inpaint_chunk(&pts, |x, _m, n| {
            assert_eq!(n, 1);
            assert_eq!(x.len(), INPAINT_LEN * 2);
            // A model that answers (0.25, 0.5) everywhere.
            Ok((0..INPAINT_LEN).flat_map(|_| [0.25f32, 0.5]).collect())
        })
        .unwrap();
        assert_eq!(got.len(), 5);
        assert_eq!(got[0], ShuttlePoint::at(100, 100));
        assert_eq!(got[2], ShuttlePoint::at(128, 144));
        assert_eq!(got[3], ShuttlePoint::at(128, 144));
        assert_eq!(
            got[4],
            ShuttlePoint {
                visible: false,
                x: 0,
                y: 0
            }
        );
    }
}
