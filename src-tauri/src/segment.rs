//! Local, hybrid rally segmentation: visual motion + audio confirmation (ADR 0006).
//!
//! Given a recording's audio (mono PCM) and a per-frame **motion** track, this
//! produces the draft **timeline**: the set of **rally** intervals, each with a
//! per-region **confidence**. Gaps are the derived complement and are never
//! represented here (ADR 0001, glossary).
//!
//! **Motion is the primary boundary signal; audio is confirmation** (ADR 0006).
//! A rally starts with explosive whole-court movement and ends when the players
//! decelerate after the point, so the rising and falling edges of the motion
//! envelope land where the rally boundaries actually are — something audio
//! cannot place, since a serve is preceded by shuttle-bouncing and chatter, and
//! a rally's end sounds identical to a mid-rally lull. Audio then **confirms**
//! that a moving span is a rally and not non-rally movement (players walking to
//! collect shuttles, towelling off): hit *count* over a multi-second span is
//! robust even though hit *timing* is not. A neighbouring court is out of frame,
//! so it never drives motion — sidestepping the audio bleed of the old approach.
//!
//! It is a **tunable heuristic, not a learned model** (ADR 0002). Every threshold
//! lives in [`Params`] with a documented meaning so the human tuning step can
//! adjust them against real footage without touching the algorithm (see
//! `docs/tuning-segmentation.md`). Defaults **err toward inclusion**: dropping a
//! real rally loses footage the user wanted, while keeping a little downtime is
//! cheap — so when unsure we keep the span as play, bridge brief lulls, and pad
//! each rally's edges. Assumes a roughly static camera (ADR 0006).
//!
//! The segmenter is a replaceable component behind the timeline (ADR 0002):
//! nothing downstream depends on how these intervals were produced.

/// A detected rally interval over a recording, in milliseconds from its start,
/// carrying a per-region confidence in `[0, 1]`. Low-confidence rallies surface
/// as "uncertain regions" on the timeline during review (ADR 0002).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Rally {
    pub start_ms: i64,
    pub end_ms: i64,
    pub confidence: f64,
}

/// A per-frame motion track: `energy[i]` is the mean absolute pixel difference
/// between sampled frame `i+1` and frame `i` (so sample `i` lands at time
/// `(i + 1) / fps` seconds). Produced by [`crate::media::extract_motion`].
#[derive(Debug, Clone)]
pub struct MotionTrack {
    pub fps: f64,
    pub energy: Vec<f64>,
}

/// Tunable thresholds for the heuristic, grouped here so the human tuning step
/// can adjust them against real badminton footage in one place. The defaults err
/// toward inclusion (ADR 0002).
#[derive(Debug, Clone)]
pub struct Params {
    // --- audio onset (hit) detection ---
    /// Energy-frame length in samples. Sets the temporal grain of onset
    /// detection; ~64 ms at 16 kHz resolves individual hits.
    pub frame: usize,
    /// Preceding-history window (ms) whose mean energy is the adaptive baseline
    /// a frame must rise above to count as an onset.
    pub baseline_ms: i64,
    /// A frame is an onset when its energy exceeds this multiple of the local
    /// baseline (a sharp rise = a hit, not ambient drift).
    pub onset_ratio: f64,
    /// Absolute floor, as a multiple of the whole clip's mean energy, below
    /// which a frame can never be an onset — suppresses spurious onsets in near
    /// silence where the baseline is ~0.
    pub onset_floor_ratio: f64,

    // --- visual motion (primary boundary signal) ---
    /// A block counts as movement when its mean motion energy reaches this
    /// multiple of the clip's mean motion. The dominant play/gap decision.
    pub motion_active_ratio: f64,

    // --- fusion / confirmation ---
    /// A motion span is kept as a rally only if it carries at least this many
    /// shuttle-hit onsets per second — audio confirming real play, filtering out
    /// non-rally movement (and motion-free audio bleed never reaches here).
    pub confirm_onsets_per_sec: f64,

    // --- structure ---
    /// Length of the analysis block, in milliseconds. The grain at which motion
    /// and onsets are judged, and the granularity of rally boundaries.
    pub block_ms: i64,
    /// Merge two motion runs separated by a gap no longer than this (ms). Brief
    /// lulls within a rally shouldn't shatter it (inclusion bias).
    pub bridge_gap_ms: i64,
    /// Discard a motion run shorter than this (ms) — too brief to be a rally.
    pub min_rally_ms: i64,
    /// Extend every rally edge by this much (ms) so the serve and the final shot
    /// are not clipped (inclusion bias). Overlaps created by padding are merged.
    pub pad_ms: i64,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            frame: 1024,
            baseline_ms: 1000,
            onset_ratio: 2.2,
            onset_floor_ratio: 0.75,
            motion_active_ratio: 1.2,
            confirm_onsets_per_sec: 0.2,
            block_ms: 500,
            bridge_gap_ms: 2900,
            min_rally_ms: 2000,
            pad_ms: 1200,
        }
    }
}

/// Segment a recording into draft rally intervals from its audio and motion
/// tracks, using the default tuned parameters (ADR 0006).
pub fn segment(samples: &[f32], sample_rate: u32, motion: &MotionTrack) -> Vec<Rally> {
    segment_with(samples, sample_rate, motion, &Params::default())
}

/// Segment with explicit parameters. Separated from [`segment`] so tests (and
/// the human tuning step) can exercise the algorithm at different thresholds.
pub fn segment_with(
    samples: &[f32],
    sample_rate: u32,
    motion: &MotionTrack,
    p: &Params,
) -> Vec<Rally> {
    let sr = sample_rate as f64;
    let total_ms = (samples.len() as f64 / sr * 1000.0) as i64;
    if samples.len() < p.frame * 2 {
        return Vec::new();
    }

    // 1. Per-frame audio energy (mean square), non-overlapping frames.
    let frame_ms = p.frame as f64 / sr * 1000.0;
    let energy: Vec<f64> = samples
        .chunks(p.frame)
        .map(|chunk| {
            let sum: f64 = chunk.iter().map(|&s| (s as f64) * (s as f64)).sum();
            sum / chunk.len() as f64
        })
        .collect();

    let clip_mean = mean(&energy).max(f64::MIN_POSITIVE);
    let floor = p.onset_floor_ratio * clip_mean;
    let baseline_frames = ((p.baseline_ms as f64) / frame_ms).round().max(1.0) as usize;

    // 2. Onset per frame: a sharp energy rise above the adaptive baseline and the
    //    absolute floor. The baseline is the mean of the preceding frames, so it
    //    tracks the local level rather than the global one.
    let mut onset = vec![false; energy.len()];
    let mut window_sum = 0.0;
    for i in 0..energy.len() {
        let baseline = if i == 0 {
            clip_mean
        } else {
            window_sum / i.min(baseline_frames) as f64
        };
        if energy[i] >= floor && energy[i] >= p.onset_ratio * baseline {
            onset[i] = true;
        }
        window_sum += energy[i];
        if i >= baseline_frames {
            window_sum -= energy[i - baseline_frames];
        }
    }

    // 3. Block grid (from the audio frames). Per block, count onsets.
    let frames_per_block = ((p.block_ms as f64) / frame_ms).round().max(1.0) as usize;
    let block_secs = (frames_per_block as f64 * frame_ms) / 1000.0;
    let block_ms = (block_secs * 1000.0).round() as i64;
    let block_onsets: Vec<usize> = onset
        .chunks(frames_per_block)
        .map(|chunk| chunk.iter().filter(|&&o| o).count())
        .collect();
    let num_blocks = block_onsets.len();

    // 4. Map the motion track onto the same block grid: each block's motion is
    //    the mean of the motion samples whose timestamp falls inside it.
    let motion_dt_ms = if motion.fps > 0.0 {
        1000.0 / motion.fps
    } else {
        f64::INFINITY
    };
    let mut motion_sum = vec![0.0; num_blocks];
    let mut motion_count = vec![0u32; num_blocks];
    for (j, &m) in motion.energy.iter().enumerate() {
        let t_ms = (j as f64 + 1.0) * motion_dt_ms;
        let b = (t_ms / block_ms.max(1) as f64) as usize;
        if b < num_blocks {
            motion_sum[b] += m;
            motion_count[b] += 1;
        }
    }
    let clip_motion_mean = mean(&motion.energy).max(f64::MIN_POSITIVE);
    let motion_threshold = p.motion_active_ratio * clip_motion_mean;

    // 5. A block is play when its motion clears the threshold (motion is primary).
    let active: Vec<bool> = (0..num_blocks)
        .map(|b| {
            let level = if motion_count[b] > 0 {
                motion_sum[b] / motion_count[b] as f64
            } else {
                0.0
            };
            level >= motion_threshold
        })
        .collect();

    // 6. Group consecutive active blocks into runs.
    let mut runs: Vec<(usize, usize)> = Vec::new(); // [start, end) half-open
    let mut i = 0;
    while i < active.len() {
        if active[i] {
            let start = i;
            while i < active.len() && active[i] {
                i += 1;
            }
            runs.push((start, i));
        } else {
            i += 1;
        }
    }

    // 7. Bridge runs separated by a short gap (inclusion bias). Track active vs
    //    total blocks per merged span so confidence reflects how much was real
    //    movement vs bridged downtime.
    let bridge_blocks = (p.bridge_gap_ms / block_ms.max(1)).max(0) as usize;
    let mut merged: Vec<(usize, usize, usize)> = Vec::new(); // (start, end, active_blocks)
    for (start, end) in runs {
        let active_blocks = end - start;
        if let Some(last) = merged.last_mut() {
            if start - last.1 <= bridge_blocks {
                last.1 = end;
                last.2 += active_blocks;
                continue;
            }
        }
        merged.push((start, end, active_blocks));
    }

    // 8. Confirm each span with audio and drop the too-short, then convert to
    //    padded ms intervals. Confidence is the share of the span that was real
    //    movement (bridged downtime lowers it → surfaces as an uncertain region).
    let min_blocks = (p.min_rally_ms / block_ms.max(1)).max(1) as usize;
    let mut rallies: Vec<Rally> = Vec::new();
    for (start, end, active_blocks) in merged {
        let span_blocks = end - start;
        if span_blocks < min_blocks {
            continue;
        }
        let onsets: usize = block_onsets[start..end].iter().sum();
        let span_secs = span_blocks as f64 * block_secs;
        // Audio confirmation: a moving span with too few hits is not a rally.
        if span_secs > 0.0 && (onsets as f64 / span_secs) < p.confirm_onsets_per_sec {
            continue;
        }
        let confidence = (active_blocks as f64 / span_blocks as f64).clamp(0.0, 1.0);
        let raw_start = start as i64 * block_ms;
        let raw_end = end as i64 * block_ms;
        rallies.push(Rally {
            start_ms: (raw_start - p.pad_ms).max(0),
            end_ms: (raw_end + p.pad_ms).min(total_ms),
            confidence,
        });
    }

    // 9. Padding can make neighbours overlap; coalesce them.
    merge_overlaps(rallies)
}

fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().sum::<f64>() / values.len() as f64
}

/// How many peak buckets the displayed waveform is reduced to. A fixed count
/// keeps the stored waveform tiny (one short array per recording) and lets the
/// timeline strip draw a fixed number of bars regardless of recording length —
/// the strip is only a few hundred pixels wide, so finer detail is invisible.
pub const WAVEFORM_BUCKETS: usize = 400;

/// Reduce a recording's audio to a compact waveform for the timeline strip: the
/// peak (max absolute) amplitude of each of [`WAVEFORM_BUCKETS`] equal-width time
/// buckets, normalized to `[0, 1]` against the loudest bucket so shuttle hits
/// show as visible spikes against the quiet of a gap (issue #6). An empty or
/// silent recording yields all-zero buckets.
pub fn waveform(samples: &[f32]) -> Vec<f32> {
    let mut peaks = vec![0.0f32; WAVEFORM_BUCKETS];
    if samples.is_empty() {
        return peaks;
    }
    for (i, peak) in peaks.iter_mut().enumerate() {
        let start = i * samples.len() / WAVEFORM_BUCKETS;
        let end = ((i + 1) * samples.len() / WAVEFORM_BUCKETS).max(start + 1);
        *peak = samples[start..end.min(samples.len())]
            .iter()
            .fold(0.0f32, |m, &s| m.max(s.abs()));
    }
    let max = peaks.iter().fold(0.0f32, |m, &p| m.max(p));
    if max > 0.0 {
        for p in &mut peaks {
            *p /= max;
        }
    }
    peaks
}

/// Coalesce intervals that overlap or touch after padding. Inputs are sorted by
/// construction (ascending start); the merged interval keeps the minimum
/// confidence of its parts, since a stitched span is no more certain than its
/// least-certain piece.
fn merge_overlaps(rallies: Vec<Rally>) -> Vec<Rally> {
    let mut out: Vec<Rally> = Vec::with_capacity(rallies.len());
    for r in rallies {
        if let Some(last) = out.last_mut() {
            if r.start_ms <= last.end_ms {
                last.end_ms = last.end_ms.max(r.end_ms);
                last.confidence = last.confidence.min(r.confidence);
                continue;
            }
        }
        out.push(r);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 16_000;
    const FPS: f64 = 5.0;

    /// Synthetic mono audio: faint hum throughout with sharp impulses ("hits")
    /// inside the given intervals at `hit_period_s` spacing — the rally audio
    /// signature without real footage.
    fn synth_audio(total_secs: f64, hits: &[(f64, f64)], hit_period_s: f64) -> Vec<f32> {
        let n = (total_secs * SR as f64) as usize;
        let mut buf: Vec<f32> = (0..n).map(|i| 0.0005 * ((i as f32 * 0.01).sin())).collect();
        for &(start, end) in hits {
            let mut t = start;
            while t < end {
                let idx = (t * SR as f64) as usize;
                for k in 0..400 {
                    if idx + k < n {
                        let env = (1.0 - k as f32 / 400.0).max(0.0);
                        buf[idx + k] += 0.9 * env * ((k as f32) * 0.5).sin();
                    }
                }
                t += hit_period_s;
            }
        }
        buf
    }

    /// Synthetic motion track: high energy inside the `active` intervals (a rally
    /// of whole-court movement), low elsewhere (a still gap).
    fn synth_motion(total_secs: f64, active: &[(f64, f64)]) -> MotionTrack {
        let n = (total_secs * FPS) as usize;
        let energy = (0..n)
            .map(|i| {
                let t = (i as f64 + 1.0) / FPS;
                if active.iter().any(|&(s, e)| t >= s && t < e) {
                    20.0
                } else {
                    0.5
                }
            })
            .collect();
        MotionTrack { fps: FPS, energy }
    }

    #[test]
    fn recovers_two_rallies_from_motion_and_audio() {
        let audio = synth_audio(40.0, &[(5.0, 15.0), (22.0, 32.0)], 0.66);
        let motion = synth_motion(40.0, &[(5.0, 15.0), (22.0, 32.0)]);
        let rallies = segment(&audio, SR, &motion);
        assert_eq!(rallies.len(), 2, "expected two rallies, got {rallies:?}");

        // Motion edges + padding place the boundaries; inclusion bias means we
        // never start late or end early.
        assert!(rallies[0].start_ms <= 5000, "rally 0 start {rallies:?}");
        assert!(
            (13_000..=16_500).contains(&rallies[0].end_ms),
            "rally 0 end {rallies:?}"
        );
        assert!(rallies[1].start_ms <= 22_000, "rally 1 start {rallies:?}");
        assert!(
            (30_000..=33_500).contains(&rallies[1].end_ms),
            "rally 1 end {rallies:?}"
        );
        assert!(rallies[0].confidence > 0.7, "confidence {rallies:?}");
    }

    #[test]
    fn stillness_and_silence_yield_no_rallies() {
        let audio = synth_audio(20.0, &[], 1.0);
        let motion = synth_motion(20.0, &[]);
        assert!(segment(&audio, SR, &motion).is_empty());
    }

    #[test]
    fn brief_lull_within_a_rally_is_bridged_not_split() {
        // Two movement bursts 2 s apart (< 2.5 s bridge) → one rally.
        let audio = synth_audio(30.0, &[(4.0, 10.0), (12.0, 20.0)], 0.6);
        let motion = synth_motion(30.0, &[(4.0, 10.0), (12.0, 20.0)]);
        let rallies = segment(&audio, SR, &motion);
        assert_eq!(rallies.len(), 1, "lull should bridge: {rallies:?}");
        assert!(
            rallies[0].confidence < 1.0,
            "bridged confidence {rallies:?}"
        );
    }

    #[test]
    fn motion_without_hits_is_not_a_rally() {
        // Whole-court movement but no shuttle hits (e.g. players walking around):
        // audio confirmation rejects it. This is the hybrid's key behaviour.
        let audio = synth_audio(20.0, &[], 1.0); // no hits
        let motion = synth_motion(20.0, &[(4.0, 16.0)]);
        assert!(segment(&audio, SR, &motion).is_empty());
    }

    #[test]
    fn hits_without_motion_are_ignored() {
        // Shuttle-like hits but no on-court motion (e.g. a neighbouring court
        // bleeding into the audio): motion is primary, so nothing is detected.
        let audio = synth_audio(20.0, &[(4.0, 16.0)], 0.6);
        let motion = synth_motion(20.0, &[]); // still court
        assert!(segment(&audio, SR, &motion).is_empty());
    }

    #[test]
    fn waveform_spikes_where_the_hits_are() {
        // Hits only in the back half → its buckets peak at 1.0, the silent front
        // half stays near zero. The strip can eyeball boundaries off this.
        let audio = synth_audio(20.0, &[(12.0, 18.0)], 0.3);
        let peaks = waveform(&audio);
        assert_eq!(peaks.len(), WAVEFORM_BUCKETS);
        let mid = WAVEFORM_BUCKETS / 2;
        let front_max = peaks[..mid].iter().fold(0.0f32, |m, &p| m.max(p));
        let back_max = peaks[mid..].iter().fold(0.0f32, |m, &p| m.max(p));
        assert!(back_max > front_max * 2.0, "front {front_max} back {back_max}");
        assert!((back_max - 1.0).abs() < 1e-6, "normalized to 1: {back_max}");
    }

    #[test]
    fn waveform_of_silence_is_all_zero() {
        let peaks = waveform(&synth_audio(5.0, &[], 1.0));
        assert_eq!(peaks.len(), WAVEFORM_BUCKETS);
        // Faint hum normalizes to a non-flat shape but never to a huge value;
        // mainly: no panic, fixed length, finite.
        assert!(peaks.iter().all(|p| p.is_finite() && (0.0..=1.0).contains(p)));
    }

    #[test]
    fn a_brief_burst_is_not_a_rally() {
        // A momentary movement (someone reaching across) is below min_rally_ms.
        let audio = synth_audio(20.0, &[(10.0, 10.3)], 0.1);
        let motion = synth_motion(20.0, &[(10.0, 10.3)]);
        assert!(segment(&audio, SR, &motion).is_empty());
    }
}
