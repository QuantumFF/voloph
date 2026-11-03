//! The **student** rally segmenter (issue #105): a learned model on the three cheap
//! signals v6 already computes — motion energy, audio energy + hit onsets, and the
//! person-detector boxes — so in the app it costs about what v6 costs.
//!
//! It is a port of `scripts/student.py` (trained there, distilled from the TrackNetV3
//! teacher's timelines plus hand-verified gold, issue #100) and must stay in step with
//! it: the model file (`models/student-v1.json`) carries the feature layout it was
//! trained on, and [`StudentModel::load`] refuses a layout this code does not compute.
//! Where numpy's arithmetic decides an outcome — rounding half to even when mapping
//! frames to motion samples, float32 box geometry, pairwise summation in the edge
//! search — this mirrors it, so the in-app timeline matches the offline one.
//!
//! The pipeline, on a 30 fps frame grid:
//! 1. **Signals** per frame: log motion vs the recording's median, audio level vs its
//!    median, hit onset, people counted, fastest / second-fastest player speed (box
//!    centres, body-heights/s), someone standing still.
//! 2. **Context features** every 0.5 s: window means of every signal, window max /
//!    spread of motion, audio and player speed, and the distance to the nearest quiet
//!    stretch, over windows from 8 s before to 4 s after.
//! 3. **Rally probability** from a gradient-boosted tree ensemble, smoothed over 2.5 s,
//!    thresholded into spans; spans < 3 s apart are joined, spans < 1.5 s dropped.
//! 4. **Edge refiner:** a second tree ensemble says "in a rally?" every 0.1 s from local
//!    signals; each edge moves to the change point within ±2 s that best fits it.
//!
//! Rally edges are strict (serve contact → shuttle down); padding belongs to playback
//! and export (#104).

use std::sync::OnceLock;

/// The student segmenter's version (ADR 0019): bump with every retrained model that
/// passes gate v2, so untouched student drafts re-derive.
pub const VERSION: u32 = 1;

use crate::detect::DetectionTrack;
use crate::segment::Rally;

/// The frame grid every signal is resampled onto (the teacher's TrackNet rate).
const FPS: i64 = 30;
/// One context step: 0.5 s.
const STEP: i64 = 15;
const WINDOWS_S: [(i64, i64); 5] = [(-8, -4), (-4, -2), (-2, 0), (0, 2), (2, 4)];
const SIGNALS: [&str; 7] = [
    "motion",
    "audio_level",
    "onset",
    "people",
    "fast1",
    "fast2",
    "still",
];
const NSIG: usize = SIGNALS.len();
/// Detector score for a counted person.
const PERSON_MIN: f32 = 0.5;
/// Body-heights/s below which a player counts as standing still.
const MOVE_STILL: f64 = 0.3;
/// Boxes kept per detector sample, highest score first.
const KEEP_BOXES: usize = 6;
const MAX_COLS: [usize; 4] = [0, 1, 4, 5];
const STD_COLS: [usize; 2] = [0, 1];
const QUIET_FRAMES: usize = 2 * FPS as usize;
const QUIET_CAP_S: f64 = 30.0;
/// Context padding: the widest window reaches 8 s back.
const PAD: i64 = 8 * FPS;
// Edge refiner (shared with silver-ctx.py's EDGE_*).
const EDGE_BIN: i64 = 6;
const EDGE_OFFS: std::ops::Range<i64> = -8..8;
const EDGE_PAD: i64 = 60;
const EDGE_WIN: i64 = 60;
/// Confidence ceiling: strictly below the hand-corrected sentinel
/// (`db::CORRECTED_CONFIDENCE`), so a machine draft never reads as hand-touched.
const MAX_CONFIDENCE: f64 = 0.99;

#[derive(Debug, serde::Deserialize)]
struct Tree {
    feature: Vec<usize>,
    threshold: Vec<f64>,
    left: Vec<usize>,
    right: Vec<usize>,
    leaf: Vec<bool>,
    value: Vec<f64>,
    missing_left: Vec<bool>,
}

/// A gradient-boosted tree ensemble as exported by `silver-ctx.export_trees`: a row
/// goes left when its feature is `<=` the node threshold (NaN follows `missing_left`).
#[derive(Debug, serde::Deserialize)]
pub(crate) struct Trees {
    baseline: f64,
    trees: Vec<Tree>,
}

impl Trees {
    pub(crate) fn prob(&self, x: &[f64]) -> f64 {
        let mut z = 0.0;
        for t in &self.trees {
            let mut node = 0;
            while !t.leaf[node] {
                let v = x[t.feature[node]];
                let left = if v.is_nan() {
                    t.missing_left[node]
                } else {
                    v <= t.threshold[node]
                };
                node = if left { t.left[node] } else { t.right[node] };
            }
            z += t.value[node];
        }
        1.0 / (1.0 + (-(self.baseline + z)).exp())
    }

    fn check(&self, n_features: usize, what: &str) -> Result<(), String> {
        for t in &self.trees {
            let n = t.leaf.len();
            let ok = [
                t.feature.len(),
                t.threshold.len(),
                t.left.len(),
                t.right.len(),
                t.value.len(),
                t.missing_left.len(),
            ]
            .iter()
            .all(|&l| l == n);
            if !ok || n == 0 {
                return Err(format!("{what}: malformed tree"));
            }
            for i in 0..n {
                if !t.leaf[i] && (t.feature[i] >= n_features || t.left[i] >= n || t.right[i] >= n) {
                    return Err(format!(
                        "{what}: tree node out of range (expects {n_features} features)"
                    ));
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, serde::Deserialize)]
struct Post {
    smooth: usize,
    merge_ms: i64,
    min_ms: i64,
}

/// A trained student (`models/student-v1.json`).
#[derive(Debug, serde::Deserialize)]
pub struct StudentModel {
    #[serde(flatten)]
    context: Trees,
    threshold: f64,
    post: Post,
    edge: Option<Trees>,
    signals: Vec<String>,
    windows_s: Vec<(i64, i64)>,
}

const N_CONTEXT: usize = WINDOWS_S.len() * (NSIG + MAX_COLS.len() + STD_COLS.len()) + 2;
const N_EDGE: usize = (EDGE_OFFS.end - EDGE_OFFS.start) as usize * NSIG;

impl StudentModel {
    /// Parse a model file, refusing one trained on a feature layout this code does not
    /// compute (a renamed signal or a changed window would silently mis-score).
    pub fn load(json: &str) -> Result<StudentModel, String> {
        let m: StudentModel =
            serde_json::from_str(json).map_err(|e| format!("student model: {e}"))?;
        if m.signals != SIGNALS || m.windows_s != WINDOWS_S {
            return Err("student model: trained on a different signal/window layout".into());
        }
        m.context.check(N_CONTEXT, "student context model")?;
        if let Some(e) = &m.edge {
            e.check(N_EDGE, "student edge model")?;
        }
        Ok(m)
    }

    /// The model bundled with the app.
    pub fn bundled() -> Result<&'static StudentModel, String> {
        static MODEL: OnceLock<Result<StudentModel, String>> = OnceLock::new();
        MODEL
            .get_or_init(|| StudentModel::load(include_str!("../models/student-v1.json")))
            .as_ref()
            .map_err(Clone::clone)
    }
}

/// The student's inputs: what the analysis pass already extracts for v6.
pub struct Inputs<'a> {
    /// Recording length from the decoded audio.
    pub duration_ms: i64,
    pub motion_fps: f64,
    /// `media::extract_motion` energy; sample `i` at `(i + 1) / fps` s.
    pub motion: &'a [f64],
    pub audio_frame_ms: f64,
    /// `segment::audio_frames` energy and onsets.
    pub audio_energy: &'a [f64],
    pub audio_onset: &'a [bool],
    pub detect: &'a DetectionTrack,
}

/// Segment a recording with the student. Returns strict rally intervals with a
/// per-rally confidence (0.5 = the model's threshold; amber below it in review).
pub fn segment(model: &StudentModel, inp: &Inputs) -> Vec<Rally> {
    let sig = signals(inp);
    let n = sig.len() as i64;
    if n == 0 {
        return Vec::new();
    }
    let steps: Vec<i64> = (0..n).step_by(STEP as usize).collect();
    let ctx = Context::new(&sig);
    let raw: Vec<f64> = steps
        .iter()
        .map(|&f| model.context.prob(&ctx.features(f)))
        .collect();
    let pr = smooth(&raw, model.post.smooth);
    let on: Vec<bool> = pr.iter().map(|&p| p >= model.threshold).collect();
    let mut rallies = tidy(spans(&on, n), model.post.merge_ms, model.post.min_ms);
    if let Some(edge) = &model.edge {
        rallies = refine_edges(edge, &sig, &rallies)
            .into_iter()
            .filter(|r| r[1] - r[0] >= model.post.min_ms)
            .collect();
    }
    let thr = model.threshold;
    rallies
        .into_iter()
        .map(|[s0, e0]| {
            let sel: Vec<f64> = steps
                .iter()
                .zip(&pr)
                .filter(|(&f, _)| {
                    let t = f as f64 * 1000.0 / FPS as f64;
                    t >= s0 as f64 && t < e0 as f64
                })
                .map(|(_, &p)| p)
                .collect();
            let v = if sel.is_empty() {
                thr
            } else {
                np_sum(&sel) / sel.len() as f64
            };
            let c = if v >= thr {
                0.5 + 0.5 * (v - thr) / (1.0 - thr)
            } else {
                0.5 * v / thr
            };
            Rally {
                start_ms: s0,
                end_ms: e0,
                confidence: ((c * 1000.0).round() / 1000.0).clamp(0.0, MAX_CONFIDENCE),
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------------
// 1. Signals

fn median(v: &[f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    let m = s.len() / 2;
    if s.len() % 2 == 1 {
        s[m]
    } else {
        (s[m - 1] + s[m]) / 2.0
    }
}

/// Per detector sample: people counted, fastest and second-fastest box-centre speed
/// (body-heights/s, nearest-centre matching to the previous sample), someone still.
/// Box geometry is float32, as in the training pipeline (numpy keeps float32).
fn player_signals(track: &DetectionTrack) -> (Vec<f64>, Vec<f64>, Vec<f64>, Vec<f64>) {
    let aspect = (16.0f64 / 9.0) as f32;
    let fps = track.fps as f32;
    let frames: Vec<Vec<crate::detect::Box>> = track
        .samples
        .iter()
        .map(|s| {
            let mut b = s.clone();
            b.sort_by(|a, c| {
                c.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            b.truncate(KEEP_BOXES);
            b
        })
        .collect();
    let d = frames.len();
    let (mut people, mut fast1, mut fast2, mut still) =
        (vec![0.0; d], vec![0.0; d], vec![0.0; d], vec![0.0; d]);
    let geom =
        |b: &crate::detect::Box| ((b.x + b.w / 2.0) * aspect, b.y + b.h / 2.0, b.h.max(1e-3));
    for f in 0..d {
        let ok: Vec<usize> = (0..frames[f].len())
            .filter(|&k| frames[f][k].score >= PERSON_MIN)
            .collect();
        people[f] = ok.len() as f64;
        let mut speed = vec![-1.0f64; KEEP_BOXES];
        if f > 0 {
            let prev: Vec<usize> = (0..frames[f - 1].len())
                .filter(|&k| frames[f - 1][k].score >= PERSON_MIN)
                .collect();
            if !prev.is_empty() {
                for &k in &ok {
                    let (cx, cy, bh) = geom(&frames[f][k]);
                    let (mut best, mut bj) = (f32::INFINITY, prev[0]);
                    for &j in &prev {
                        let (px, py, _) = geom(&frames[f - 1][j]);
                        let dist = (px - cx).hypot(py - cy);
                        if dist < best {
                            best = dist;
                            bj = j;
                        }
                    }
                    let (_, _, pbh) = geom(&frames[f - 1][bj]);
                    speed[k] = f64::from(best / ((bh + pbh) / 2.0) * fps);
                }
            }
        }
        let mut srt = speed.clone();
        srt.sort_by(|a, b| b.total_cmp(a));
        fast1[f] = srt[0].max(0.0);
        fast2[f] = srt[1].max(0.0);
        let slow = ok
            .iter()
            .map(|&k| speed[k])
            .filter(|&s| s >= 0.0)
            .fold(f64::INFINITY, f64::min);
        still[f] = if slow < MOVE_STILL { 1.0 } else { 0.0 };
    }
    (people, fast1, fast2, still)
}

/// The (n, NSIG) per-frame signal matrix at [`FPS`]. Motion and audio energy pass
/// through float32 first, as the training pipeline stored them.
fn signals(inp: &Inputs) -> Vec<[f64; NSIG]> {
    let n = (inp.duration_ms * FPS / 1000).max(0) as usize;
    if n == 0 || inp.motion.is_empty() || inp.audio_energy.is_empty() {
        return Vec::new();
    }
    let m: Vec<f64> = inp.motion.iter().map(|&v| f64::from(v as f32)).collect();
    let m_med = median(&m);
    let e: Vec<f64> = inp
        .audio_energy
        .iter()
        .map(|&v| (f64::from(v as f32) + 1e-12).ln())
        .collect();
    let e_med = median(&e);
    let (people, f1, f2, still) = player_signals(inp.detect);
    let df = inp.detect.fps;
    let clip = |i: i64, len: usize| i.clamp(0, len as i64 - 1) as usize;
    (0..n)
        .map(|f| {
            let t = f as f64 / FPS as f64;
            let mi = clip((t * inp.motion_fps).round_ties_even() as i64 - 1, m.len());
            let motion = ((m[mi] + 1e-6) / (m_med + 1e-6)).ln().clamp(-5.0, 5.0);
            let ai = clip((t * 1000.0 / inp.audio_frame_ms) as i64, e.len());
            let level = (e[ai] - e_med).clamp(-10.0, 10.0);
            let onset = if inp.audio_onset.get(ai).copied().unwrap_or(false) {
                1.0
            } else {
                0.0
            };
            let (p, a, b, s) = if people.is_empty() {
                (0.0, 0.0, 0.0, 0.0)
            } else {
                let di = clip((t * df) as i64, people.len());
                (people[di], f1[di].min(5.0), f2[di].min(5.0), still[di])
            };
            [motion, level, onset, p, a, b, s]
        })
        .collect()
}

// ---------------------------------------------------------------------------------
// 2. Context features

struct Context<'a> {
    sig: &'a [[f64; NSIG]],
    /// Prefix sums over the zero-padded signal matrix (row `i` = sum of rows `< i`).
    cum: Vec<[f64; NSIG]>,
    quiet_starts: Vec<i64>,
}

impl<'a> Context<'a> {
    fn new(sig: &'a [[f64; NSIG]]) -> Context<'a> {
        let len = sig.len() + 2 * PAD as usize;
        let mut cum = vec![[0.0; NSIG]; len + 1];
        for i in 0..len {
            let row = padded(sig, i as i64 - PAD);
            for c in 0..NSIG {
                cum[i + 1][c] = cum[i][c] + row[c];
            }
        }
        // A quiet stretch starts at i when QUIET_FRAMES frames from i have below-median
        // motion and no onset.
        let q: Vec<bool> = sig.iter().map(|r| r[0] < 0.0 && r[2] == 0.0).collect();
        let mut quiet_starts = Vec::new();
        let mut run = 0usize;
        for (i, &qi) in q.iter().enumerate() {
            run = if qi { run + 1 } else { 0 };
            if run >= QUIET_FRAMES {
                quiet_starts.push((i + 1 - QUIET_FRAMES) as i64);
            }
        }
        Context {
            sig,
            cum,
            quiet_starts,
        }
    }

    fn features(&self, f: i64) -> Vec<f64> {
        let len = self.cum.len() as i64 - 1;
        let mut x = Vec::with_capacity(N_CONTEXT);
        for &(a, b) in &WINDOWS_S {
            let lo = (f + PAD + a * FPS).clamp(0, len) as usize;
            let hi = (f + PAD + b * FPS).clamp(0, len) as usize;
            let w = (hi as f64 - lo as f64).max(1.0);
            for c in 0..NSIG {
                x.push((self.cum[hi][c] - self.cum[lo][c]) / w);
            }
        }
        for &(a, b) in &WINDOWS_S {
            let (start, l) = self.window(f, a, b);
            for &c in &MAX_COLS {
                x.push(
                    (start..start + l)
                        .map(|i| padded(self.sig, i)[c])
                        .fold(f64::NEG_INFINITY, f64::max),
                );
            }
        }
        for &(a, b) in &WINDOWS_S {
            let (start, l) = self.window(f, a, b);
            for &c in &STD_COLS {
                let v: Vec<f64> = (start..start + l).map(|i| padded(self.sig, i)[c]).collect();
                let mean = v.iter().sum::<f64>() / l as f64;
                x.push((v.iter().map(|y| (y - mean) * (y - mean)).sum::<f64>() / l as f64).sqrt());
            }
        }
        let k = self.quiet_starts.partition_point(|&s| s <= f);
        x.push(if k > 0 {
            ((f - self.quiet_starts[k - 1]) as f64 / FPS as f64).min(QUIET_CAP_S)
        } else {
            QUIET_CAP_S
        });
        let k2 = self.quiet_starts.partition_point(|&s| s < f);
        x.push(if k2 < self.quiet_starts.len() {
            ((self.quiet_starts[k2] - f) as f64 / FPS as f64).min(QUIET_CAP_S)
        } else {
            QUIET_CAP_S
        });
        x
    }

    /// The `[a, b)`-second window around frame `f`, as a start in *unpadded* frame
    /// coordinates (may be negative) and a length, clamped like numpy's sliding view.
    fn window(&self, f: i64, a: i64, b: i64) -> (i64, i64) {
        let l = (b - a) * FPS;
        let n_views = self.sig.len() as i64 + 2 * PAD - l + 1;
        let idx = (f + PAD + a * FPS).clamp(0, n_views - 1);
        (idx - PAD, l)
    }
}

/// Row `i` of the signal matrix, zero outside the recording.
fn padded(sig: &[[f64; NSIG]], i: i64) -> [f64; NSIG] {
    if i < 0 || i >= sig.len() as i64 {
        [0.0; NSIG]
    } else {
        sig[i as usize]
    }
}

// ---------------------------------------------------------------------------------
// 3. Spans

/// numpy's `convolve(p, ones(k) / k, "same")` for odd `k`.
fn smooth(p: &[f64], k: usize) -> Vec<f64> {
    if k <= 1 {
        return p.to_vec();
    }
    let w = 1.0 / k as f64;
    let h = (k / 2) as i64;
    (0..p.len() as i64)
        .map(|i| {
            ((i - h)..=(i + h))
                .filter(|&j| j >= 0 && j < p.len() as i64)
                .map(|j| p[j as usize] * w)
                .sum()
        })
        .collect()
}

/// Per-step decisions → `[start_ms, end_ms]` spans; each step covers ±STEP/2 frames
/// and a single-step hole inside play is filled (silver-ctx.py `spans`).
fn spans(on: &[bool], n_frames: i64) -> Vec<[i64; 2]> {
    let mut on = on.to_vec();
    if on.len() > 2 {
        let hole: Vec<bool> = (1..on.len() - 1)
            .map(|i| !on[i] && on[i - 1] && on[i + 1])
            .collect();
        for (i, h) in hole.into_iter().enumerate() {
            on[i + 1] |= h;
        }
    }
    let ms = |frames: i64| (frames as f64 * 1000.0 / FPS as f64).round() as i64;
    let mut out = Vec::new();
    let mut k = 0;
    while k < on.len() {
        if !on[k] {
            k += 1;
            continue;
        }
        let mut j = k;
        while j + 1 < on.len() && on[j + 1] {
            j += 1;
        }
        let s = (k as i64 * STEP - STEP / 2).max(0);
        let e = (j as i64 * STEP + STEP - STEP / 2).min(n_frames);
        out.push([ms(s), ms(e)]);
        k = j + 1;
    }
    out
}

/// Join spans less than `merge_ms` apart, then drop those shorter than `min_ms`.
fn tidy(rallies: Vec<[i64; 2]>, merge_ms: i64, min_ms: i64) -> Vec<[i64; 2]> {
    let mut out: Vec<[i64; 2]> = Vec::new();
    for [s, e] in rallies {
        match out.last_mut() {
            Some(last) if s - last[1] < merge_ms => last[1] = e,
            _ => out.push([s, e]),
        }
    }
    out.retain(|r| r[1] - r[0] >= min_ms);
    out
}

// ---------------------------------------------------------------------------------
// 4. Edge refiner

/// silver-ctx.py `edge_features`: means of every signal in EDGE_BIN-frame bins at
/// offsets EDGE_OFFS around each frame (zero outside the recording).
fn edge_features(cum: &[[f64; NSIG]], f: i64) -> Vec<f64> {
    let len = cum.len() as i64 - 1;
    let mut x = Vec::with_capacity(N_EDGE);
    for o in EDGE_OFFS {
        let lo = (f + EDGE_PAD + o * EDGE_BIN).clamp(0, len) as usize;
        let hi = (lo as i64 + EDGE_BIN).clamp(0, len) as usize;
        for c in 0..NSIG {
            x.push((cum[hi][c] - cum[lo][c]) / EDGE_BIN as f64);
        }
    }
    x
}

/// silver-ctx.py `refine_edges`: move each edge to the change point within ±EDGE_WIN
/// frames that best fits the per-frame "in a rally?" probabilities.
fn refine_edges(edge: &Trees, sig: &[[f64; NSIG]], rallies: &[[i64; 2]]) -> Vec<[i64; 2]> {
    let n = sig.len() as i64;
    let len = sig.len() + 2 * EDGE_PAD as usize;
    let mut cum = vec![[0.0; NSIG]; len + 1];
    for i in 0..len {
        let row = padded(sig, i as i64 - EDGE_PAD);
        for c in 0..NSIG {
            cum[i + 1][c] = cum[i][c] + row[c];
        }
    }
    let mut out = Vec::new();
    for &[s0, e0] in rallies {
        let mut new = [0i64; 2];
        for (slot, (t, is_start)) in [(s0, true), (e0, false)].into_iter().enumerate() {
            let f0 = t * FPS / 1000;
            let fr: Vec<i64> = ((f0 - EDGE_WIN - 45).max(0)..(f0 + EDGE_WIN + 45).min(n))
                .step_by(3)
                .collect();
            let q: Vec<f64> = fr
                .iter()
                .map(|&f| edge.prob(&edge_features(&cum, f)).clamp(1e-3, 1.0 - 1e-3))
                .collect();
            let li: Vec<f64> = q.iter().map(|v| v.ln()).collect();
            let lo: Vec<f64> = q.iter().map(|v| (1.0 - v).ln()).collect();
            let mut best: Option<(f64, i64)> = None;
            for k in 0..fr.len() {
                if (fr[k] - f0).abs() > EDGE_WIN {
                    continue;
                }
                let ll = if is_start {
                    np_sum(&lo[..k]) + np_sum(&li[k..])
                } else {
                    np_sum(&li[..k]) + np_sum(&lo[k..])
                };
                if best.is_none_or(|(b, _)| ll > b) {
                    best = Some((ll, fr[k]));
                }
            }
            new[slot] = match best {
                Some((_, f)) => (f as f64 * 1000.0 / FPS as f64) as i64,
                None => t,
            };
        }
        if new[1] - new[0] >= 500 {
            out.push(new);
        }
    }
    out
}

/// numpy's pairwise summation (`np.sum` on a contiguous float64 array), so sums that
/// decide an argmax match the training pipeline bit for bit.
fn np_sum(a: &[f64]) -> f64 {
    const BLOCK: usize = 128;
    let n = a.len();
    if n < 8 {
        let mut r = -0.0;
        for &v in a {
            r += v;
        }
        r
    } else if n <= BLOCK {
        let mut r = [0.0f64; 8];
        r.copy_from_slice(&a[..8]);
        let mut i = 8;
        while i < n - n % 8 {
            for j in 0..8 {
                r[j] += a[i + j];
            }
            i += 8;
        }
        let mut res = ((r[0] + r[1]) + (r[2] + r[3])) + ((r[4] + r[5]) + (r[6] + r[7]));
        for &v in &a[i..] {
            res += v;
        }
        res
    } else {
        let mut n2 = n / 2;
        n2 -= n2 % 8;
        np_sum(&a[..n2]) + np_sum(&a[n2..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn np_sum_matches_sequential_sum_on_exact_values() {
        let v: Vec<f64> = (0..300).map(|i| i as f64).collect();
        assert_eq!(np_sum(&v), (0..300).sum::<i64>() as f64);
        assert_eq!(np_sum(&[]), 0.0);
        assert_eq!(np_sum(&[1.5, 2.5]), 4.0);
    }

    #[test]
    fn spans_fill_single_step_holes_and_map_steps_to_ms() {
        // on, off, on → one span over steps 0–2, ending 8 frames past step 2 (frame 38);
        // same as silver-ctx.py spans().
        let s = spans(&[true, false, true, false, false], 75);
        assert_eq!(s, vec![[0, 1267]]);
        assert!(spans(&[false; 4], 60).is_empty());
    }

    #[test]
    fn tidy_joins_close_spans_and_drops_short_ones() {
        let r = tidy(vec![[0, 2000], [4000, 6000], [20000, 20800]], 3000, 1500);
        assert_eq!(r, vec![[0, 6000]]);
    }

    #[test]
    fn smooth_is_a_centred_zero_padded_mean() {
        let s = smooth(&[1.0, 1.0, 1.0, 1.0, 1.0], 5);
        assert!(
            (s[0] - 0.6).abs() < 1e-12 && (s[2] - 1.0).abs() < 1e-12 && (s[4] - 0.6).abs() < 1e-12
        );
    }

    #[test]
    fn trees_follow_threshold_and_missing_direction() {
        let t = Trees {
            baseline: 0.0,
            trees: vec![Tree {
                feature: vec![0, 0, 0],
                threshold: vec![1.0, 0.0, 0.0],
                left: vec![1, 0, 0],
                right: vec![2, 0, 0],
                leaf: vec![false, true, true],
                value: vec![0.0, -2.0, 2.0],
                missing_left: vec![false, false, false],
            }],
        };
        let p = |x: f64| t.prob(&[x]);
        assert!(p(1.0) < 0.5 && p(1.5) > 0.5 && p(f64::NAN) > 0.5);
    }

    #[test]
    fn the_bundled_model_loads() {
        let m = StudentModel::bundled().expect("bundled student model");
        assert!(m.threshold > 0.0 && m.edge.is_some());
    }

    #[test]
    fn silence_and_stillness_yield_no_rallies() {
        let m = StudentModel::bundled().unwrap();
        let motion = vec![1.0; 5 * 60];
        let energy = vec![1e-6; 60_000 / 64];
        let onset = vec![false; energy.len()];
        let detect = DetectionTrack {
            fps: 3.0,
            samples: vec![Vec::new(); 180],
        };
        let r = segment(
            m,
            &Inputs {
                duration_ms: 60_000,
                motion_fps: 5.0,
                motion: &motion,
                audio_frame_ms: 64.0,
                audio_energy: &energy,
                audio_onset: &onset,
                detect: &detect,
            },
        );
        assert!(r.is_empty(), "{r:?}");
    }
}
