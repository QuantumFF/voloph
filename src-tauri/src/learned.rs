//! Pieces shared by the learned segmenters (ADR 0019) — the student
//! ([`crate::student`]) and the teacher ([`crate::teacher`]) — ported from the
//! training code (`scripts/silver-ctx.py`, `scripts/student.py`): the saved classifier
//! formats, the per-step → span post-processing, the edge refiner, and the confidence a
//! draft rally carries. Both segmenters work on a 30 fps frame grid with a context step
//! of 0.5 s, and both must reproduce the offline timelines, so where numpy's arithmetic
//! decides an outcome (pairwise summation in the edge search) this mirrors it.

/// The frame grid both learned segmenters work on (the teacher's TrackNet rate).
pub(crate) const FPS: i64 = 30;
/// One context step: 0.5 s.
pub(crate) const STEP: i64 = 15;
// Edge refiner (silver-ctx.py's EDGE_*).
const EDGE_BIN: i64 = 6;
const EDGE_OFFS: std::ops::Range<i64> = -8..8;
const EDGE_PAD: i64 = 60;
const EDGE_WIN: i64 = 60;
/// Confidence ceiling: strictly below the hand-corrected sentinel
/// (`db::CORRECTED_CONFIDENCE`), so a machine draft never reads as hand-touched.
const MAX_CONFIDENCE: f64 = 0.99;

/// Number of edge-refiner features for `n` per-frame signals.
pub(crate) const fn n_edge_features(n: usize) -> usize {
    (EDGE_OFFS.end - EDGE_OFFS.start) as usize * n
}

/// A saved binary classifier: P(rally) for one feature row.
pub(crate) trait Classifier {
    fn prob(&self, x: &[f64]) -> f64;
    /// Refuse a model whose shape does not fit `n_features` inputs.
    fn check(&self, n_features: usize, what: &str) -> Result<(), String>;
}

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

impl Classifier for Trees {
    fn prob(&self, x: &[f64]) -> f64 {
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

/// A logistic regression on standardised features, as saved by `silver-ctx.fit`.
#[derive(Debug, serde::Deserialize)]
pub(crate) struct Logistic {
    mu: Vec<f64>,
    sd: Vec<f64>,
    coef: Vec<f64>,
    intercept: f64,
}

impl Classifier for Logistic {
    fn prob(&self, x: &[f64]) -> f64 {
        let mut z = 0.0;
        for i in 0..self.coef.len() {
            z += (x[i] - self.mu[i]) / self.sd[i] * self.coef[i];
        }
        1.0 / (1.0 + (-(z + self.intercept)).exp())
    }

    fn check(&self, n_features: usize, what: &str) -> Result<(), String> {
        if self.mu.len() != n_features
            || self.sd.len() != n_features
            || self.coef.len() != n_features
        {
            return Err(format!(
                "{what}: expects {n_features} features, model has {}",
                self.coef.len()
            ));
        }
        Ok(())
    }
}

/// Span post-processing saved with a model: smoothing window (steps), join spans
/// closer than `merge_ms`, drop spans shorter than `min_ms`.
#[derive(Debug, serde::Deserialize)]
pub(crate) struct Post {
    pub smooth: usize,
    pub merge_ms: i64,
    pub min_ms: i64,
}

/// Per-step rally probabilities → strict rallies: smooth, threshold, build spans,
/// tidy, refine edges, then a confidence per rally (0.5 = the threshold). `sig` is
/// the per-frame signal matrix the edge refiner reads.
pub(crate) fn rallies_from_steps<const N: usize>(
    raw: &[f64],
    threshold: f64,
    post: &Post,
    edge: Option<&dyn Classifier>,
    sig: &[[f64; N]],
) -> Vec<crate::segment::Rally> {
    let n = sig.len() as i64;
    let pr = smooth(raw, post.smooth);
    let on: Vec<bool> = pr.iter().map(|&p| p >= threshold).collect();
    let mut rallies = tidy(spans(&on, n), post.merge_ms, post.min_ms);
    if let Some(edge) = edge {
        rallies = refine_edges(edge, sig, &rallies)
            .into_iter()
            .filter(|r| r[1] - r[0] >= post.min_ms)
            .collect();
    }
    rallies
        .into_iter()
        .map(|[s0, e0]| {
            let sel: Vec<f64> = pr
                .iter()
                .enumerate()
                .filter(|(k, _)| {
                    let t = (*k as i64 * STEP) as f64 * 1000.0 / FPS as f64;
                    t >= s0 as f64 && t < e0 as f64
                })
                .map(|(_, &p)| p)
                .collect();
            let v = if sel.is_empty() {
                threshold
            } else {
                np_sum(&sel) / sel.len() as f64
            };
            let c = if v >= threshold {
                0.5 + 0.5 * (v - threshold) / (1.0 - threshold)
            } else {
                0.5 * v / threshold
            };
            crate::segment::Rally {
                start_ms: s0,
                end_ms: e0,
                confidence: ((c * 1000.0).round() / 1000.0).clamp(0.0, MAX_CONFIDENCE),
            }
        })
        .collect()
}

/// numpy's `convolve(p, ones(k) / k, "same")` for odd `k`.
pub(crate) fn smooth(p: &[f64], k: usize) -> Vec<f64> {
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
pub(crate) fn spans(on: &[bool], n_frames: i64) -> Vec<[i64; 2]> {
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
pub(crate) fn tidy(rallies: Vec<[i64; 2]>, merge_ms: i64, min_ms: i64) -> Vec<[i64; 2]> {
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

/// Row `i` of a signal matrix, zero outside the recording.
pub(crate) fn padded<const N: usize>(sig: &[[f64; N]], i: i64) -> [f64; N] {
    if i < 0 || i >= sig.len() as i64 {
        [0.0; N]
    } else {
        sig[i as usize]
    }
}

/// Prefix sums over `sig` zero-padded by `pad` rows each side (row `i` = sum of rows
/// `< i`), summed sequentially like numpy's `cumsum`.
pub(crate) fn prefix_sums<const N: usize>(sig: &[[f64; N]], pad: i64) -> Vec<[f64; N]> {
    let len = sig.len() + 2 * pad as usize;
    let mut cum = vec![[0.0; N]; len + 1];
    for i in 0..len {
        let row = padded(sig, i as i64 - pad);
        for c in 0..N {
            cum[i + 1][c] = cum[i][c] + row[c];
        }
    }
    cum
}

/// silver-ctx.py `edge_features`: means of every signal in EDGE_BIN-frame bins at
/// offsets EDGE_OFFS around frame `f` (zero outside the recording).
fn edge_features<const N: usize>(cum: &[[f64; N]], f: i64) -> Vec<f64> {
    let len = cum.len() as i64 - 1;
    let mut x = Vec::with_capacity(n_edge_features(N));
    for o in EDGE_OFFS {
        let lo = (f + EDGE_PAD + o * EDGE_BIN).clamp(0, len) as usize;
        let hi = (lo as i64 + EDGE_BIN).clamp(0, len) as usize;
        for c in 0..N {
            x.push((cum[hi][c] - cum[lo][c]) / EDGE_BIN as f64);
        }
    }
    x
}

/// silver-ctx.py `refine_edges`: move each edge to the change point within ±EDGE_WIN
/// frames that best fits the per-frame "in a rally?" probabilities.
fn refine_edges<const N: usize>(
    edge: &dyn Classifier,
    sig: &[[f64; N]],
    rallies: &[[i64; 2]],
) -> Vec<[i64; 2]> {
    let n = sig.len() as i64;
    let cum = prefix_sums(sig, EDGE_PAD);
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

/// numpy's pairwise summation (`np.sum` / `.mean` on a contiguous float64 array), so
/// sums that decide an argmax or a threshold match the training pipeline bit for bit.
pub(crate) fn np_sum(a: &[f64]) -> f64 {
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
        assert_eq!(
            spans(&[true, false, true, false, false], 75),
            vec![[0, 1267]]
        );
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
    fn logistic_standardises_then_weighs() {
        let m = Logistic {
            mu: vec![1.0],
            sd: vec![2.0],
            coef: vec![4.0],
            intercept: -1.0,
        };
        // z = (3 - 1) / 2 * 4 - 1 = 3
        assert!((m.prob(&[3.0]) - 1.0 / (1.0 + (-3.0f64).exp())).abs() < 1e-15);
        assert!(m.check(1, "m").is_ok() && m.check(2, "m").is_err());
    }
}
