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
//! a rally's end sounds identical to a mid-rally lull. Audio then **modulates
//! confidence**, but never deletes a span (ADR 0015, issue #79): under the
//! zero-miss bar no single signal may drop a rally on its own. A moving span
//! with enough shuttle-hit onsets keeps its motion-derived confidence; one with
//! too few is still kept but marked uncertain, surfacing as a doubtful region
//! for review rather than being silently removed. Hit *count* over a multi-second
//! span is robust even though hit *timing* is not. A neighbouring court is out of
//! frame, so it never drives motion — sidestepping the audio bleed of old work.
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

/// The segmenter's identity, stamped into a published Analysis (ADR 0013) so a
/// future, meaningfully better segmenter can spot stale Analyses. Ignored today;
/// bump on a change that materially alters the draft timeline it produces.
pub const SEGMENTER_VERSION: u32 = 3;

/// A detected rally interval over a recording, in milliseconds from its start,
/// carrying a per-region confidence in `[0, 1]`. Low-confidence rallies surface
/// as "uncertain regions" on the timeline during review (ADR 0002).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Rally {
    pub start_ms: i64,
    pub end_ms: i64,
    pub confidence: f64,
}

/// The gate a candidate span hit during segmentation — the Stage 0 diagnostic of
/// ADR 0015. Every span the segmenter weighs gets exactly one, so running one bad
/// recording answers "which gate is eating rallies". Since audio was demoted to a
/// confidence modulator (issue #79, Stage 1), only the motion gate can lose a
/// rally outright ([`GateVerdict::MotionNeverFired`]); audio can only mark one
/// doubtful ([`GateVerdict::UnconfirmedByAudio`]). Observational only — the
/// verdict never feeds back into the draft.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateVerdict {
    /// Motion fired, the span was long enough, and audio confirmed real play — it
    /// became a rally at its motion-derived confidence.
    Kept,
    /// Motion fired and the span was long enough, but too few shuttle-hit onsets
    /// to confirm play. The span is still kept — audio never deletes a rally
    /// (issue #79) — with its confidence capped at
    /// [`Params::unconfirmed_confidence`], so this marks a doubtful rally, not a
    /// dropped one. Still the verdict to watch on noisy footage (ADR 0015).
    UnconfirmedByAudio,
    /// A candidate span too brief to be a rally (below [`Params::min_rally_ms`]).
    TooShort,
    /// Audio confirmed play here, but the motion envelope never crossed its
    /// threshold, so the span never entered the rally pass — the motion gate's
    /// silent counterpart to [`GateVerdict::UnconfirmedByAudio`]. Invisible without
    /// this instrumentation, since motion is primary.
    MotionNeverFired,
}

impl GateVerdict {
    /// Stable, hyphenated label for the per-span diagnostic log line.
    pub fn label(self) -> &'static str {
        match self {
            GateVerdict::Kept => "kept",
            GateVerdict::UnconfirmedByAudio => "unconfirmed-by-audio",
            GateVerdict::TooShort => "too-short",
            GateVerdict::MotionNeverFired => "motion-never-fired",
        }
    }
}

/// One candidate span and the [`GateVerdict`] the segmenter reached for it, in
/// milliseconds from the recording start at the raw (unpadded) block boundaries
/// the segmenter actually saw. Diagnostic output of Stage 0 (ADR 0015).
#[derive(Debug, Clone, PartialEq)]
pub struct SpanVerdict {
    pub start_ms: i64,
    pub end_ms: i64,
    pub verdict: GateVerdict,
}

/// The pure segmentation output: the draft rally intervals plus a per-span gate
/// [`GateVerdict`] for every candidate span the segmenter considered (ADR 0015 Stage
/// 0). The seam carries the verdicts out so the worker can log them without
/// reaching into gate internals — no I/O happens inside the seam.
#[derive(Debug, Clone, PartialEq)]
pub struct Segmentation {
    pub rallies: Vec<Rally>,
    pub verdicts: Vec<SpanVerdict>,
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

    // --- fusion / confidence modulation ---
    /// Onsets-per-second at or above which audio *confirms* a motion span as real
    /// play, leaving its motion-derived confidence intact. Below it the span is
    /// still kept (audio never deletes a rally — ADR 0015, issue #79); its
    /// confidence is instead pulled down toward [`Params::unconfirmed_confidence`]
    /// so a hit-less moving span surfaces as an uncertain region rather than
    /// vanishing. This is a confidence knob now, not an inclusion gate.
    pub confirm_onsets_per_sec: f64,
    /// The confidence ceiling for a motion span audio does **not** confirm (onset
    /// density below [`Params::confirm_onsets_per_sec`]). Kept below the review
    /// UI's uncertain threshold — `UNCERTAIN_CONFIDENCE` (0.5) in
    /// `src/components/recording-player-transport.ts`, duplicated here as a bare
    /// number since it lives across the Rust/TS boundary; keep this strictly under
    /// it — so such a span reliably surfaces as an amber "check this" region during
    /// review, never a silent deletion (issue #79).
    pub unconfirmed_confidence: f64,

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
            motion_active_ratio: 1.1,
            confirm_onsets_per_sec: 0.2,
            unconfirmed_confidence: 0.3,
            block_ms: 500,
            bridge_gap_ms: 2900,
            min_rally_ms: 1500,
            pad_ms: 1200,
        }
    }
}

/// Segment a recording into a draft [`Segmentation`] — rally intervals plus the
/// per-span gate verdicts (ADR 0015 Stage 0) — from its audio and motion tracks,
/// using the default tuned parameters (ADR 0006).
pub fn segment(samples: &[f32], sample_rate: u32, motion: &MotionTrack) -> Segmentation {
    segment_with(samples, sample_rate, motion, &Params::default())
}

/// Segment with explicit parameters. This is the pure seam: signal tracks +
/// params in, rally intervals and per-span verdicts out, no I/O. Separated from
/// [`segment`] so tests (and the human tuning step) can exercise the algorithm at
/// different thresholds. The verdicts are a strictly additional observation over
/// the old rally-only output — they never alter the draft (ADR 0015 Stage 0).
pub fn segment_with(
    samples: &[f32],
    sample_rate: u32,
    motion: &MotionTrack,
    p: &Params,
) -> Segmentation {
    let sr = sample_rate as f64;
    let total_ms = (samples.len() as f64 / sr * 1000.0) as i64;
    if samples.len() < p.frame * 2 {
        return Segmentation {
            rallies: Vec::new(),
            verdicts: Vec::new(),
        };
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
    let runs = mask_runs(&active); // [start, end) half-open

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

    // 8. Drop the too-short, then convert to padded ms intervals. Confidence
    //    starts as the share of the span that was real movement (bridged downtime
    //    lowers it → surfaces as an uncertain region), then audio *modulates* it.
    //    Every span the segmenter weighs here records its gate verdict (ADR 0015
    //    Stage 0) at the raw block boundaries.
    //
    //    Audio no longer hard-gates (ADR 0015, issue #79): under the zero-miss bar
    //    no single signal may delete a rally on its own. A moving span with too
    //    few shuttle-hit onsets is still kept — its confidence is instead capped
    //    at `unconfirmed_confidence` (below the review UI's uncertain threshold)
    //    so it surfaces as a doubtful region rather than vanishing. Hits present
    //    leave the motion-derived confidence intact. (Motion-free audio bleed
    //    never reaches here — with no motion there is no span to modulate.)
    let min_blocks = (p.min_rally_ms / block_ms.max(1)).max(1) as usize;
    let mut rallies: Vec<Rally> = Vec::new();
    let mut verdicts: Vec<SpanVerdict> = Vec::new();
    for (start, end, active_blocks) in merged {
        let span_blocks = end - start;
        let raw_start = start as i64 * block_ms;
        let raw_end = end as i64 * block_ms;
        if span_blocks < min_blocks {
            verdicts.push(SpanVerdict {
                start_ms: raw_start,
                end_ms: raw_end,
                verdict: GateVerdict::TooShort,
            });
            continue;
        }
        let onsets: usize = block_onsets[start..end].iter().sum();
        let span_secs = span_blocks as f64 * block_secs;
        let mut confidence = (active_blocks as f64 / span_blocks as f64).clamp(0.0, 1.0);
        let confirmed = span_secs > 0.0 && (onsets as f64 / span_secs) >= p.confirm_onsets_per_sec;
        if !confirmed {
            confidence = confidence.min(p.unconfirmed_confidence);
        }
        verdicts.push(SpanVerdict {
            start_ms: raw_start,
            end_ms: raw_end,
            verdict: if confirmed {
                GateVerdict::Kept
            } else {
                GateVerdict::UnconfirmedByAudio
            },
        });
        rallies.push(Rally {
            start_ms: (raw_start - p.pad_ms).max(0),
            end_ms: (raw_end + p.pad_ms).min(total_ms),
            confidence,
        });
    }

    // 8b. Motion-never-fired (ADR 0015 Stage 0): audio-confirmed regions the motion
    //     gate never opened for. Motion is primary, so these never reach the pass
    //     above and a rally lost to the motion gate would be invisible — this is the
    //     symmetric counterpart to `unconfirmed-by-audio`. Group the shuttle-hit blocks
    //     with the same bridging, then flag only spans that are rally-length,
    //     audio-confirmed, and carry no motion (a span with motion already spoke
    //     above). Shorter or unconfirmed audio is not a candidate rally.
    let audio_hot: Vec<bool> = block_onsets.iter().map(|&n| n > 0).collect();
    let audio_spans = bridge_runs(mask_runs(&audio_hot), bridge_blocks);
    for (start, end) in audio_spans {
        let span_blocks = end - start;
        if span_blocks < min_blocks {
            continue;
        }
        if active[start..end].iter().any(|&a| a) {
            continue; // motion fired here → already covered by a span above
        }
        let onsets: usize = block_onsets[start..end].iter().sum();
        let span_secs = span_blocks as f64 * block_secs;
        if span_secs > 0.0 && (onsets as f64 / span_secs) < p.confirm_onsets_per_sec {
            continue; // audio itself never confirmed play here
        }
        verdicts.push(SpanVerdict {
            start_ms: start as i64 * block_ms,
            end_ms: end as i64 * block_ms,
            verdict: GateVerdict::MotionNeverFired,
        });
    }
    verdicts.sort_by_key(|v| v.start_ms);

    // 9. Padding can make neighbours overlap; coalesce them.
    Segmentation {
        rallies: merge_overlaps(rallies),
        verdicts,
    }
}

/// Merge `[start, end)` runs separated by a gap of at most `bridge_blocks` blocks
/// into single spans (inclusion bias — a brief lull shouldn't shatter a span).
/// Runs are assumed ascending and non-overlapping, as [`mask_runs`] produces them.
fn bridge_runs(runs: Vec<(usize, usize)>, bridge_blocks: usize) -> Vec<(usize, usize)> {
    let mut spans: Vec<(usize, usize)> = Vec::with_capacity(runs.len());
    for (start, end) in runs {
        if let Some(last) = spans.last_mut() {
            if start - last.1 <= bridge_blocks {
                last.1 = end;
                continue;
            }
        }
        spans.push((start, end));
    }
    spans
}

/// Group a per-block boolean mask into maximal `[start, end)` runs of `true`.
fn mask_runs(mask: &[bool]) -> Vec<(usize, usize)> {
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < mask.len() {
        if mask[i] {
            let start = i;
            while i < mask.len() && mask[i] {
                i += 1;
            }
            runs.push((start, i));
        } else {
            i += 1;
        }
    }
    runs
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
        let rallies = segment(&audio, SR, &motion).rallies;
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
        assert!(segment(&audio, SR, &motion).rallies.is_empty());
    }

    #[test]
    fn brief_lull_within_a_rally_is_bridged_not_split() {
        // Two movement bursts 2 s apart (< 2.5 s bridge) → one rally.
        let audio = synth_audio(30.0, &[(4.0, 10.0), (12.0, 20.0)], 0.6);
        let motion = synth_motion(30.0, &[(4.0, 10.0), (12.0, 20.0)]);
        let rallies = segment(&audio, SR, &motion).rallies;
        assert_eq!(rallies.len(), 1, "lull should bridge: {rallies:?}");
        assert!(
            rallies[0].confidence < 1.0,
            "bridged confidence {rallies:?}"
        );
    }

    #[test]
    fn motion_without_hits_is_kept_but_uncertain() {
        // Whole-court movement but no shuttle hits (e.g. players walking around):
        // audio can no longer *delete* the span (issue #79, ADR 0015). Under the
        // zero-miss bar no single signal drops a rally on its own — the worst
        // audio does is mark it doubtful. So the motion span survives as a rally
        // whose confidence is pushed into the uncertain zone (below the review
        // UI's UNCERTAIN_CONFIDENCE = 0.5), surfacing as an amber "check this".
        let audio = synth_audio(20.0, &[], 1.0); // no hits
        let motion = synth_motion(20.0, &[(4.0, 16.0)]);
        let rallies = segment(&audio, SR, &motion).rallies;
        assert_eq!(
            rallies.len(),
            1,
            "span must be kept, not dropped: {rallies:?}"
        );
        assert!(
            rallies[0].confidence < 0.5,
            "unconfirmed span should surface as uncertain: {rallies:?}"
        );
    }

    #[test]
    fn hits_without_motion_are_ignored() {
        // Shuttle-like hits but no on-court motion (e.g. a neighbouring court
        // bleeding into the audio): motion is the only signal that *proposes* a
        // rally, so with no on-court movement there is nothing to keep. Audio
        // modulates confidence but never conjures a span (issue #79).
        let audio = synth_audio(20.0, &[(4.0, 16.0)], 0.6);
        let motion = synth_motion(20.0, &[]); // still court
        assert!(segment(&audio, SR, &motion).rallies.is_empty());
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
        assert!(
            back_max > front_max * 2.0,
            "front {front_max} back {back_max}"
        );
        assert!((back_max - 1.0).abs() < 1e-6, "normalized to 1: {back_max}");
    }

    #[test]
    fn waveform_of_silence_is_all_zero() {
        let peaks = waveform(&synth_audio(5.0, &[], 1.0));
        assert_eq!(peaks.len(), WAVEFORM_BUCKETS);
        // Faint hum normalizes to a non-flat shape but never to a huge value;
        // mainly: no panic, fixed length, finite.
        assert!(peaks
            .iter()
            .all(|p| p.is_finite() && (0.0..=1.0).contains(p)));
    }

    #[test]
    fn a_brief_burst_is_not_a_rally() {
        // A momentary movement (someone reaching across) is below min_rally_ms.
        let audio = synth_audio(20.0, &[(10.0, 10.3)], 0.1);
        let motion = synth_motion(20.0, &[(10.0, 10.3)]);
        assert!(segment(&audio, SR, &motion).rallies.is_empty());
    }

    // --- Per-span gate verdicts (ADR 0015 Stage 0) ---------------------------
    //
    // Each test drives the same synthetic tracks as the behaviour tests above,
    // but through the widened seam, and asserts the diagnostic verdict a rally's
    // fate produces. The seam only observes: every test also confirms the draft
    // rallies match the un-instrumented behaviour.

    fn verdict_kinds(v: &[SpanVerdict]) -> Vec<GateVerdict> {
        v.iter().map(|s| s.verdict).collect()
    }

    #[test]
    fn each_kept_rally_records_a_kept_verdict() {
        let audio = synth_audio(40.0, &[(5.0, 15.0), (22.0, 32.0)], 0.66);
        let motion = synth_motion(40.0, &[(5.0, 15.0), (22.0, 32.0)]);
        let seg = segment(&audio, SR, &motion);
        assert_eq!(seg.rallies.len(), 2, "{:?}", seg.rallies);
        assert_eq!(
            verdict_kinds(&seg.verdicts),
            vec![GateVerdict::Kept, GateVerdict::Kept],
            "two rallies → two kept verdicts: {:?}",
            seg.verdicts
        );
    }

    #[test]
    fn motion_without_hits_records_unconfirmed_by_audio() {
        // Whole-court movement but no shuttle hits: since #79 the audio gate no
        // longer drops the span — it is kept as a doubtful rally and the verdict
        // records that audio never confirmed it, still the signal to watch on
        // noisy footage.
        let audio = synth_audio(20.0, &[], 1.0);
        let motion = synth_motion(20.0, &[(4.0, 16.0)]);
        let seg = segment(&audio, SR, &motion);
        assert_eq!(seg.rallies.len(), 1, "{:?}", seg.rallies);
        assert_eq!(
            verdict_kinds(&seg.verdicts),
            vec![GateVerdict::UnconfirmedByAudio],
            "{:?}",
            seg.verdicts
        );
    }

    #[test]
    fn hits_without_motion_records_motion_never_fired() {
        // Shuttle-like hits confirming play, but the court never moved (a rally the
        // motion gate would silently miss). The audio surfaces it as a candidate.
        let audio = synth_audio(20.0, &[(4.0, 16.0)], 0.6);
        let motion = synth_motion(20.0, &[]);
        let seg = segment(&audio, SR, &motion);
        assert!(seg.rallies.is_empty(), "{:?}", seg.rallies);
        assert_eq!(
            verdict_kinds(&seg.verdicts),
            vec![GateVerdict::MotionNeverFired],
            "{:?}",
            seg.verdicts
        );
    }

    #[test]
    fn a_sub_threshold_run_records_too_short() {
        // A momentary burst with hits: a motion run forms and audio is present,
        // but it is below min_rally_ms, so the length gate drops it.
        let audio = synth_audio(20.0, &[(10.0, 10.3)], 0.1);
        let motion = synth_motion(20.0, &[(10.0, 10.3)]);
        let seg = segment(&audio, SR, &motion);
        assert!(seg.rallies.is_empty(), "{:?}", seg.rallies);
        assert_eq!(
            verdict_kinds(&seg.verdicts),
            vec![GateVerdict::TooShort],
            "{:?}",
            seg.verdicts
        );
    }

    #[test]
    fn stillness_and_silence_yield_no_verdicts() {
        let audio = synth_audio(20.0, &[], 1.0);
        let motion = synth_motion(20.0, &[]);
        let seg = segment(&audio, SR, &motion);
        assert!(seg.rallies.is_empty());
        assert!(seg.verdicts.is_empty(), "{:?}", seg.verdicts);
    }

    #[test]
    fn verdict_span_times_are_the_raw_unpadded_boundaries() {
        // A kept span's verdict reports the block boundaries the segmenter saw,
        // not the padded rally interval — the honest view for diagnosis.
        let audio = synth_audio(40.0, &[(5.0, 15.0)], 0.66);
        let motion = synth_motion(40.0, &[(5.0, 15.0)]);
        let seg = segment(&audio, SR, &motion);
        assert_eq!(seg.verdicts.len(), 1, "{:?}", seg.verdicts);
        let span = &seg.verdicts[0];
        assert_eq!(span.verdict, GateVerdict::Kept);
        let rally = &seg.rallies[0];
        // Padding widens the rally past the raw span on both edges.
        assert!(rally.start_ms <= span.start_ms, "{seg:?}");
        assert!(rally.end_ms >= span.end_ms, "{seg:?}");
    }
}
