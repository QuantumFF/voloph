//! The **teacher** rally segmenter (issue #105, ADR 0019): the most accurate of the
//! segmenters, and the heaviest — it needs the TrackNetV3 shuttle track and the
//! RTMPose-t player poses over the whole recording, so it wants a GPU.
//!
//! A port of `scripts/silver-ctx.py`'s predict path (the model in
//! `models/teacher-v1.json` is the v3 recipe — context logistic regression plus a
//! learned edge refiner — trained on TrackNet's non-overlapping tracks, which is what
//! the app computes). It must stay in step with that script, so the arithmetic that
//! decides outcomes mirrors numpy: hotspot removal and window statistics on the
//! integer shuttle track, numpy's linear percentile, float32 pose geometry and the
//! float32 score cut, pairwise summation for window means.
//!
//! On a 30 fps frame grid, every 0.5 s:
//! - **shuttle context** over windows from 8 s before to 2 s after: how often the
//!   shuttle is seen and moving, its speed, height and horizontal range, direction
//!   flips, and how far back / ahead the shuttle last vanished for 1.5 s;
//! - **player context** over the same windows: people seen, the two fastest players'
//!   speeds (body-heights/s), someone standing still, an arm raised;
//! - a logistic regression gives P(rally), smoothed into spans, then the edge refiner
//!   moves each edge to the best change point within ±2 s.
//!
//! Rally edges are strict (serve contact → shuttle down); padding belongs to playback
//! and export (#104, ADR 0020).

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use crate::learned::{self, Classifier, Logistic, Post, FPS, STEP};
use crate::segment::Rally;

/// The teacher segmenter's version (ADR 0019): bump with every retrained model that
/// passes gate v2, so untouched teacher drafts re-derive.
pub const VERSION: u32 = 1;

/// The TrackNet frame height the shuttle coordinates are in (512x288).
pub const TRACK_H: f64 = 288.0;
/// 10 s of detections in one 8 px cell = static background, not the shuttle.
const HOTSPOT_FRAMES: usize = 300;
const CELL: i64 = 8;
const WINDOWS_S: [(i64, i64); 4] = [(-8, -4), (-4, -2), (-2, 0), (0, 2)];
/// 1.5 s with no shuttle = "the shuttle is gone".
const GAP_RUN: usize = 45;
/// Look at most 30 s for a gap.
const GAP_CAP: i64 = 900;
/// Pose: people kept per frame, the frame size their pixels are in, and the rate.
pub const POSE_K: usize = 4;
pub const POSE_W: f64 = 640.0;
pub const POSE_H: f64 = 360.0;
pub const POSE_FPS: u32 = 5;
/// Pose score for a counted person — compared in float32, as numpy does.
const PERSON_MIN: f32 = 0.7;
const MOVE_STILL: f64 = 0.3;
const L_SH: usize = 5;
const R_SH: usize = 6;
const L_WR: usize = 9;
const R_WR: usize = 10;

const N_SHUTTLE: usize = WINDOWS_S.len() * 9 + 2;
const N_POSE: usize = WINDOWS_S.len() * 6;
const N_CONTEXT: usize = N_SHUTTLE + N_POSE;
const NSIG_EDGE: usize = 8;

/// A trained teacher (`models/teacher-v1.json`).
#[derive(Debug, serde::Deserialize)]
pub struct TeacherModel {
    #[serde(flatten)]
    context: Logistic,
    threshold: f64,
    post: Post,
    edge: Option<Logistic>,
    pose: bool,
}

impl TeacherModel {
    pub fn load(json: &str) -> Result<TeacherModel, String> {
        let m: TeacherModel =
            serde_json::from_str(json).map_err(|e| format!("teacher model: {e}"))?;
        if !m.pose {
            return Err("teacher model: trained without pose features".into());
        }
        m.context.check(N_CONTEXT, "teacher context model")?;
        if let Some(e) = &m.edge {
            e.check(learned::n_edge_features(NSIG_EDGE), "teacher edge model")?;
        }
        Ok(m)
    }

    /// The model bundled with the app.
    pub fn bundled() -> Result<&'static TeacherModel, String> {
        static MODEL: OnceLock<Result<TeacherModel, String>> = OnceLock::new();
        MODEL
            .get_or_init(|| TeacherModel::load(include_str!("../models/teacher-v1.json")))
            .as_ref()
            .map_err(Clone::clone)
    }
}

/// One TrackNet frame: whether the shuttle was seen, and where (512x288 pixels).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ShuttleFrame {
    pub visible: bool,
    pub x: i64,
    pub y: i64,
}

/// The players' poses at [`POSE_FPS`], up to [`POSE_K`] per frame (highest detector
/// score first), in 640x360 pixels — the layout the model was trained on
/// (`scripts/teacher-pose-app.py`). Missing people have a NaN box and score 0.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PoseArrays {
    pub fps: u32,
    /// x1, y1, x2, y2 per person.
    pub boxes: Vec<[[f32; 4]; POSE_K]>,
    pub scores: Vec<[f32; POSE_K]>,
    /// 17 COCO keypoints (x, y, score) per person.
    pub kps: Vec<[[[f32; 3]; 17]; POSE_K]>,
}

impl PoseArrays {
    /// The app's pose window (source-normalised) in the training layout — exactly
    /// `teacher-pose-app.py`'s conversion (float64 products stored as float32).
    pub fn from_window(win: &crate::pose::PoseWindow) -> PoseArrays {
        let n = win.frames.len();
        let mut out = PoseArrays {
            fps: win.fps.round() as u32,
            boxes: vec![[[f32::NAN; 4]; POSE_K]; n],
            scores: vec![[0.0; POSE_K]; n],
            kps: vec![[[[0.0; 3]; 17]; POSE_K]; n],
        };
        for (i, fr) in win.frames.iter().enumerate() {
            for (k, p) in fr.players.iter().take(POSE_K).enumerate() {
                let b = &p.bbox;
                let (x, y, w, h) = (
                    f64::from(b.x),
                    f64::from(b.y),
                    f64::from(b.w),
                    f64::from(b.h),
                );
                out.boxes[i][k] = [
                    (x * POSE_W) as f32,
                    (y * POSE_H) as f32,
                    ((x + w) * POSE_W) as f32,
                    ((y + h) * POSE_H) as f32,
                ];
                out.scores[i][k] = b.score;
                for (j, q) in p.keypoints.iter().enumerate() {
                    out.kps[i][k][j] = [
                        (f64::from(q.x) * POSE_W) as f32,
                        (f64::from(q.y) * POSE_H) as f32,
                        q.score,
                    ];
                }
            }
        }
        out
    }
}

/// Segment a recording with the teacher from its shuttle track (one entry per 30 fps
/// frame) and its pose arrays.
pub fn segment(model: &TeacherModel, track: &[ShuttleFrame], pose: &PoseArrays) -> Vec<Rally> {
    let n = track.len() as i64;
    if n == 0 || pose.scores.is_empty() {
        return Vec::new();
    }
    let tr = Track::new(track);
    let po = Pose::new(pose);
    let raw: Vec<f64> = (0..n)
        .step_by(STEP as usize)
        .map(|f| {
            let mut x = tr.features(f);
            x.extend(po.features(f * 1000 / FPS));
            model.context.prob(&x)
        })
        .collect();
    let sig = edge_signals(&tr, &po);
    learned::rallies_from_steps(
        &raw,
        model.threshold,
        &model.post,
        model.edge.as_ref().map(|e| e as &dyn Classifier),
        &sig,
    )
}

// ---------------------------------------------------------------------------------
// Shuttle track (silver-ctx.py load_track / features / _gap_dist / _flips)

struct Track {
    det: Vec<bool>,
    x: Vec<i64>,
    y: Vec<i64>,
    sp: Vec<f64>,
    flips: Vec<f64>,
    /// `gap[j]`: no shuttle in frames `j..j + GAP_RUN` (for `j <= n - GAP_RUN`).
    gap: Vec<bool>,
}

impl Track {
    fn new(frames: &[ShuttleFrame]) -> Track {
        let n = frames.len();
        let cell = |f: &ShuttleFrame| (f.x.div_euclid(CELL), f.y.div_euclid(CELL));
        let mut counts: HashMap<(i64, i64), usize> = HashMap::new();
        for f in frames.iter().filter(|f| f.visible) {
            *counts.entry(cell(f)).or_default() += 1;
        }
        let near: HashSet<(i64, i64)> = counts
            .iter()
            .filter(|(_, &c)| c >= HOTSPOT_FRAMES)
            .flat_map(|(&(i, j), _)| {
                (-1..=1).flat_map(move |dx| (-1..=1).map(move |dy| (i + dx, j + dy)))
            })
            .collect();
        let det: Vec<bool> = frames
            .iter()
            .map(|f| f.visible && !near.contains(&cell(f)))
            .collect();
        let (mut sp, mut vy) = (vec![0.0; n], vec![0.0; n]);
        for i in 1..n {
            if det[i] && det[i - 1] {
                let dx = (frames[i].x - frames[i - 1].x) as f64;
                let dy = (frames[i].y - frames[i - 1].y) as f64;
                sp[i] = dx.hypot(dy);
                vy[i] = dy;
            }
        }
        let flips = flips(&vy);
        let gap = if n >= GAP_RUN {
            let mut run = 0usize;
            let mut ends = vec![false; n];
            for i in 0..n {
                run = if det[i] { 0 } else { run + 1 };
                ends[i] = run >= GAP_RUN;
            }
            (0..=n - GAP_RUN).map(|j| ends[j + GAP_RUN - 1]).collect()
        } else {
            Vec::new()
        };
        Track {
            det,
            x: frames.iter().map(|f| f.x).collect(),
            y: frames.iter().map(|f| f.y).collect(),
            sp,
            flips,
            gap,
        }
    }

    fn len(&self) -> i64 {
        self.det.len() as i64
    }

    /// The 38 shuttle features at frame `f` (silver-ctx.py `features`).
    fn features(&self, f: i64) -> Vec<f64> {
        let n = self.len();
        let fc = f.clamp(0, n - 1);
        let mut x = Vec::with_capacity(N_SHUTTLE);
        for &(a, b) in &WINDOWS_S {
            let l = ((b - a) * FPS) as usize;
            let start = fc + a * FPS;
            let (mut det, mut moving, mut fast, mut fl) = (0usize, 0usize, 0usize, 0.0);
            let mut sp = Vec::with_capacity(l);
            let (mut ys, mut xs): (Vec<f64>, Vec<f64>) = (Vec::new(), Vec::new());
            for i in start..start + l as i64 {
                let inside = i >= 0 && i < n;
                let s = if inside { self.sp[i as usize] } else { 0.0 };
                sp.push(s);
                moving += (s > 2.0) as usize;
                fast += (s > 8.0) as usize;
                if inside {
                    let u = i as usize;
                    fl += self.flips[u];
                    if self.det[u] {
                        det += 1;
                        ys.push(self.y[u] as f64);
                        xs.push(self.x[u] as f64);
                    }
                }
            }
            let frac = |c: usize| c as f64 / l as f64;
            let min = |v: &[f64]| v.iter().copied().fold(f64::INFINITY, f64::min);
            let max = |v: &[f64]| v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let or0 = |v: f64| if v.is_finite() { v } else { 0.0 };
            x.extend([
                frac(det),
                frac(moving),
                frac(fast),
                percentile90(&mut sp),
                or0(min(&ys)),
                if ys.is_empty() {
                    0.0
                } else {
                    learned::np_sum(&ys) / ys.len() as f64
                },
                or0(max(&ys)),
                if xs.is_empty() {
                    0.0
                } else {
                    max(&xs) - min(&xs)
                },
                fl,
            ]);
        }
        let (back, fwd) = self.gap_dist(fc);
        x.extend([back, fwd]);
        x
    }

    /// Frames back to / forward to the nearest GAP_RUN-long run with no shuttle,
    /// capped at GAP_CAP (the recording's edge counts as a gap).
    fn gap_dist(&self, f: i64) -> (f64, f64) {
        let n = self.len();
        let starts = self.gap.len() as i64;
        let full = |j: i64| self.gap[j as usize];
        // backward: largest start j <= f - GAP_RUN + 1 with a full gap
        let q = (f - GAP_RUN as i64 + 1).clamp(-1, starts - 1);
        let mut back = f + 1;
        let mut j = q;
        while j >= 0 && f - j < GAP_CAP {
            if full(j) {
                back = f - j;
                break;
            }
            j -= 1;
        }
        // forward: smallest start j >= f with a full gap
        let mut fwd = n - f;
        let mut j = f.clamp(0, starts);
        while j < starts && j + GAP_RUN as i64 - 1 - f < GAP_CAP {
            if full(j) {
                fwd = j + GAP_RUN as i64 - 1 - f;
                break;
            }
            j += 1;
        }
        (back.min(GAP_CAP) as f64, fwd.min(GAP_CAP) as f64)
    }
}

/// 1 where the vertical direction reverses versus the last clear move (|vy| > 2).
fn flips(vy: &[f64]) -> Vec<f64> {
    let mut out = vec![0.0; vy.len()];
    let mut last: f64 = 0.0;
    for (i, &v) in vy.iter().enumerate() {
        let sg = if v.abs() > 2.0 { v.signum() } else { 0.0 };
        if sg != 0.0 && last != 0.0 && sg != last {
            out[i] = 1.0;
        }
        if sg != 0.0 {
            last = sg;
        }
    }
    out
}

/// numpy's `percentile(v, 90)` (linear), including its two-sided lerp.
fn percentile90(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    let pos = 0.9 * (v.len() - 1) as f64;
    let lo = pos.floor();
    let g = pos - lo;
    let lo = lo as usize;
    let (a, b) = (v[lo], v[(lo + 1).min(v.len() - 1)]);
    let d = b - a;
    if g >= 0.5 {
        b - d * (1.0 - g)
    } else {
        a + d * g
    }
}

// ---------------------------------------------------------------------------------
// Players (silver-ctx.py load_pose / pose_features)

struct Pose {
    fps: i64,
    n: Vec<f64>,
    fast1: Vec<f64>,
    fast2: Vec<f64>,
    still: Vec<f64>,
    raised: Vec<f64>,
}

impl Pose {
    fn new(p: &PoseArrays) -> Pose {
        let frames = p.scores.len();
        let fps32 = p.fps as f32;
        let ok: Vec<[bool; POSE_K]> = p
            .scores
            .iter()
            .map(|s| s.map(|v| v >= PERSON_MIN))
            .collect();
        let geom = |f: usize, k: usize| {
            let b = p.boxes[f][k];
            (
                (b[0] + b[2]) / 2.0,
                (b[1] + b[3]) / 2.0,
                (b[3] - b[1]).max(1.0),
            )
        };
        let mut out = Pose {
            fps: i64::from(p.fps),
            n: vec![0.0; frames],
            fast1: vec![0.0; frames],
            fast2: vec![0.0; frames],
            still: vec![0.0; frames],
            raised: vec![0.0; frames],
        };
        for f in 0..frames {
            let mut speed = [f64::NAN; POSE_K];
            if f > 0 {
                let prev: Vec<usize> = (0..POSE_K).filter(|&k| ok[f - 1][k]).collect();
                if !prev.is_empty() {
                    for k in (0..POSE_K).filter(|&k| ok[f][k]) {
                        let (cx, cy, bh) = geom(f, k);
                        let (mut best, mut bj) = (f32::INFINITY, prev[0]);
                        for &j in &prev {
                            let (px, py, _) = geom(f - 1, j);
                            let d = (px - cx).hypot(py - cy);
                            if d < best {
                                best = d;
                                bj = j;
                            }
                        }
                        let (_, _, pbh) = geom(f - 1, bj);
                        speed[k] = f64::from(best / ((bh + pbh) / 2.0) * fps32);
                    }
                }
            }
            let mut srt = speed.map(|s| if s.is_nan() { -1.0 } else { s });
            srt.sort_by(|a, b| b.total_cmp(a));
            out.fast1[f] = if srt[0] >= 0.0 { srt[0] } else { 0.0 };
            out.fast2[f] = if srt[1] >= 0.0 { srt[1] } else { 0.0 };
            let slow = (0..POSE_K)
                .filter(|&k| ok[f][k] && !speed[k].is_nan())
                .map(|k| speed[k])
                .fold(f64::NAN, |m, v| if m.is_nan() || v < m { v } else { m });
            out.still[f] = if (if slow.is_nan() { 1.0 } else { slow }) < MOVE_STILL {
                1.0
            } else {
                0.0
            };
            out.n[f] = ok[f].iter().filter(|&&o| o).count() as f64;
            let raised = (0..POSE_K).any(|k| {
                let kp = &p.kps[f][k];
                ok[f][k]
                    && np_minimum(kp[L_WR][1], kp[R_WR][1]) < np_minimum(kp[L_SH][1], kp[R_SH][1])
            });
            out.raised[f] = if raised { 1.0 } else { 0.0 };
        }
        out
    }

    /// The 24 player features at time `t_ms` (silver-ctx.py `pose_features`).
    fn features(&self, t_ms: i64) -> Vec<f64> {
        let len = self.n.len() as i64;
        let f = (t_ms * self.fps / 1000).clamp(0, len - 1);
        let mut x = Vec::with_capacity(N_POSE);
        for &(a, b) in &WINDOWS_S {
            let l = ((b - a) * self.fps) as usize;
            let start = f + a * self.fps;
            let window = |v: &[f64]| -> Vec<f64> {
                (start..start + l as i64)
                    .map(|i| {
                        if i >= 0 && i < len {
                            v[i as usize]
                        } else {
                            0.0
                        }
                    })
                    .collect()
            };
            let f1 = window(&self.fast1);
            for v in [&self.n, &self.fast1, &self.fast2, &self.still, &self.raised] {
                x.push(learned::np_sum(&window(v)) / l as f64);
            }
            x.push(f1.iter().copied().fold(f64::NEG_INFINITY, f64::max));
        }
        x
    }
}

/// `np.minimum`: NaN if either side is NaN.
fn np_minimum(a: f32, b: f32) -> f32 {
    if a.is_nan() || b.is_nan() {
        f32::NAN
    } else {
        a.min(b)
    }
}

// ---------------------------------------------------------------------------------
// Edge refiner signals (silver-ctx.py edge_signals)

fn edge_signals(tr: &Track, po: &Pose) -> Vec<[f64; NSIG_EDGE]> {
    let n = tr.len() as usize;
    let rep = (FPS / po.fps).max(1) as usize;
    let up = |v: &[f64], i: usize| v.get(i / rep).copied().unwrap_or(0.0);
    (0..n)
        .map(|i| {
            [
                if tr.det[i] { 1.0 } else { 0.0 },
                if tr.sp[i] > 2.0 { 1.0 } else { 0.0 },
                tr.sp[i].min(30.0) / 30.0,
                if tr.det[i] {
                    tr.y[i] as f64 / TRACK_H
                } else {
                    0.0
                },
                tr.flips[i],
                up(&po.fast1, i).min(5.0) / 5.0,
                up(&po.fast2, i).min(5.0) / 5.0,
                up(&po.still, i),
            ]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(visible: bool, x: i64, y: i64) -> ShuttleFrame {
        ShuttleFrame { visible, x, y }
    }

    #[test]
    fn percentile_matches_numpys_linear_rule() {
        // np.percentile(np.arange(60.), 90) == 53.1; np.percentile(np.arange(120.), 90) == 107.1
        let mut a: Vec<f64> = (0..60).map(f64::from).collect();
        assert!((percentile90(&mut a) - 53.1).abs() < 1e-12);
        let mut b: Vec<f64> = (0..120).map(f64::from).collect();
        assert!((percentile90(&mut b) - 107.1).abs() < 1e-12);
    }

    #[test]
    fn flips_mark_vertical_reversals_against_the_last_clear_move() {
        // up, small, down → a flip at the down move; small moves don't count.
        assert_eq!(
            flips(&[5.0, 1.0, -5.0, -3.0, 0.0, 4.0]),
            vec![0.0, 0.0, 1.0, 0.0, 0.0, 1.0]
        );
    }

    #[test]
    fn a_static_hotspot_is_not_the_shuttle() {
        // 300 frames parked in one cell → that cell and its neighbours are dropped.
        let mut f: Vec<ShuttleFrame> = (0..300).map(|_| frame(true, 100, 100)).collect();
        f.push(frame(true, 108, 100)); // neighbouring cell
        f.push(frame(true, 300, 200)); // elsewhere
        let t = Track::new(&f);
        assert!(!t.det[0] && !t.det[300] && t.det[301]);
    }

    #[test]
    fn gap_distance_counts_to_the_nearest_shuttle_less_run() {
        // 100 frames seen, 60 unseen, 100 seen.
        let f: Vec<ShuttleFrame> = (0..260)
            .map(|i| frame(!(100..160).contains(&i), 200, 100))
            .collect();
        // Also checked against silver-ctx.py _gap_dist for these frames.
        let t = Track::new(&f);
        assert_eq!(t.gap_dist(50), (51.0, 94.0));
        assert_eq!(t.gap_dist(200), (85.0, 60.0));
    }

    #[test]
    fn the_bundled_model_loads() {
        let m = TeacherModel::bundled().expect("bundled teacher model");
        assert!(m.edge.is_some());
    }
}
