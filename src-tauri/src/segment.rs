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
///
/// v4 (ADR 0015 Stage 2, issue #84): occupancy joins the fusion — the detection
/// track now *proposes* candidate play spans, materially altering the draft on any
/// recording where a person detector runs. The staleness machinery (#80) silently
/// re-drafts untouched recordings on this bump.
pub const SEGMENTER_VERSION: u32 = 4;

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

/// One detected person box in **source-frame-normalized** `[0,1]` coordinates —
/// the pure mirror of [`crate::detect::Box`], carried here so the segmentation
/// seam stays independent of `ort` and the detector (ADR 0015: all fusion logic is
/// pure and deterministic behind the seam). The detector's score is dropped: a box
/// reaching this track already cleared detection thresholding, and fusion reasons
/// about *where* people are, not how sure the detector was.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DetBox {
    /// Left edge, fraction of source width.
    pub x: f64,
    /// Top edge, fraction of source height.
    pub y: f64,
    /// Width, fraction of source width.
    pub w: f64,
    /// Height, fraction of source height.
    pub h: f64,
}

impl DetBox {
    /// Box area (fraction of frame). The depth proxy under the static-camera
    /// assumption (ADR 0015): a near player fills more frame than a far one.
    fn area(&self) -> f64 {
        self.w * self.h
    }

    /// Vertical center in `[0,1]` (0 = top of frame). The near player (large box)
    /// sits lower in frame than the far player, so this splits the halves without
    /// any court-line geometry.
    fn cy(&self) -> f64 {
        self.y + self.h / 2.0
    }
}

/// The per-recording **occupancy track**: the person boxes at each sampled frame,
/// in time order. The pure mirror of [`crate::detect::DetectionTrack`] (ADR 0015
/// Stage 2, issue #84). `fps` is the detection sample rate; sample `i` lands at
/// `i / fps` seconds. Occupancy *proposes* candidate play spans — one near + one
/// far player, kinetically active, in opposite halves — that motion then edges and
/// audio then modulates. An absent or empty track is legal and means "detector did
/// not run": fusion falls back to motion-proposes (pre-#84 behavior), never losing
/// a rally to a missing signal (the zero-miss bar, ADR 0015).
#[derive(Debug, Clone)]
pub struct OccupancyTrack {
    pub fps: f64,
    /// One entry per sampled frame; the inner vec is that frame's person boxes
    /// (empty when nobody was detected).
    pub samples: Vec<Vec<DetBox>>,
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

    // --- occupancy (ADR 0015 Stage 2, issue #84: occupancy proposes) ---
    /// Fraction of the frame diagonal a person box's center may drift over the whole
    /// recording and still count as **static** — furniture, not a player. A net-side
    /// bystander stands nearly still for minutes; a real player criss-crosses their
    /// half. A box track whose center range stays under this is dropped before the
    /// near/far split so a spectator never proposes play (ADR 0015: bystanders are
    /// dropped by staticness, not geometry). Raise it to drop more near-still boxes;
    /// lower it toward 0 to trust almost everything the detector emits.
    pub occupancy_static_frac: f64,
    /// How kinetically active a proposed span's people must be for occupancy to call
    /// it "rally plausible": the fraction of the span's occupied samples in which a
    /// tracked player's box center moved at least [`Params::occupancy_static_frac`]
    /// between consecutive samples. Below it the people are present but milling
    /// (collecting shuttles, towelling), so occupancy proposes no span there — the
    /// mechanism that attacks the expensive false positives (ADR 0015). Motion still
    /// independently proposes, so this never *deletes* a rally, only withholds an
    /// occupancy proposal.
    pub occupancy_active_frac: f64,
    /// The fraction of a candidate span's samples that must show **opposite-half
    /// occupancy** (one near/lower box and one far/upper box, split per-recording by
    /// box-size clustering) for occupancy to propose a rally there. Below 1.0 so a
    /// deep retreat that briefly leaves only one player visible does not sink the
    /// proposal — only-one-visible must never read as rally-over (ADR 0015). Raise
    /// toward 1.0 to demand both players almost always present; lower to tolerate
    /// more single-player stretches.
    pub occupancy_opposite_frac: f64,

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
            occupancy_static_frac: 0.02,
            occupancy_active_frac: 0.3,
            occupancy_opposite_frac: 0.5,
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
pub fn segment(
    samples: &[f32],
    sample_rate: u32,
    motion: &MotionTrack,
    occupancy: Option<&OccupancyTrack>,
) -> Segmentation {
    segment_with(samples, sample_rate, motion, occupancy, &Params::default())
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
    occupancy: Option<&OccupancyTrack>,
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

    // 5b. Occupancy *proposes* candidate play spans (ADR 0015 Stage 2, issue #84):
    //     one near + one far player (per-recording box-size split), kinetically
    //     active, in opposite halves; a net-side bystander is dropped by staticness;
    //     only-one-player-visible never ends a proposal. An absent/empty track
    //     yields no proposals and the fusion is exactly the pre-#84 motion-proposes
    //     path — a failed detector never loses a rally (the zero-miss bar).
    let occ_active = occupancy_blocks(occupancy, num_blocks, block_ms.max(1), p);

    // 5c. Fuse the two proposers as a **union**: a block is a candidate rally block
    //     if motion fired there OR occupancy proposed it. The union can only *add*
    //     spans, never delete one — so no single signal drops a rally on its own
    //     (ADR 0015). Motion still places edges: where motion is active the block
    //     boundaries come from the motion envelope; occupancy contributes blocks
    //     motion missed (a rally too subtle for frame-differencing but with two
    //     players rallying on court). The downstream run/bridge/pad machinery is
    //     unchanged — it now runs over the fused candidate mask.
    let candidate: Vec<bool> = active
        .iter()
        .zip(occ_active.iter())
        .map(|(&m, &o)| m || o)
        .collect();

    // 6. Group consecutive candidate blocks into runs.
    let runs = mask_runs(&candidate); // [start, end) half-open

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
        if candidate[start..end].iter().any(|&a| a) {
            continue; // motion or occupancy proposed here → already a span above
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

/// Map an [`OccupancyTrack`] onto the block grid, returning per-block "rally
/// plausible" flags — the occupancy *proposal* (ADR 0015 Stage 2, issue #84).
///
/// The pipeline, all pure and deterministic:
/// 1. **Staticness.** Discard boxes belonging to a furniture-like position: a
///    net-side bystander stands nearly still for the whole recording, so any box
///    whose spatial cell shows a center-drift range below
///    [`Params::occupancy_static_frac`] over the recording is dropped before the
///    split (ADR 0015: bystanders die by staticness, not geometry).
/// 2. **Near/far split.** Under the static camera, box *area* is a depth proxy;
///    the split is the midpoint between the recording's smallest and largest
///    surviving box areas (a per-recording clustering with no court geometry). A
///    below-midpoint box is "far" (small, sits high); above is "near" (large, sits
///    low).
/// 3. **Per-sample plausibility.** A sample is "two-player" when it holds a near
///    box *and* a far box in opposite halves (near lower in frame than far). A
///    sample with only one visible player is *not* counted against the span —
///    only-one-visible must never read as rally-over (ADR 0015).
/// 4. **Kinetic activity.** Across a run of occupied samples, the players must
///    actually move: the fraction of samples in which a surviving box center shifts
///    at least [`Params::occupancy_static_frac`] from the previous sample must reach
///    [`Params::occupancy_active_frac`]. Milling about (collecting shuttles) fails
///    this and proposes nothing.
/// 5. **Span → blocks.** A maximal run of occupied samples that is both kinetically
///    active and opposite-half-occupied on at least [`Params::occupancy_opposite_frac`]
///    of its samples marks its blocks plausible.
///
/// Returns an all-`false` vector when `occupancy` is `None`, empty, or never finds a
/// two-player configuration — the caller then falls back to motion-proposes.
fn occupancy_blocks(
    occupancy: Option<&OccupancyTrack>,
    num_blocks: usize,
    block_ms: i64,
    p: &Params,
) -> Vec<bool> {
    let mut plausible = vec![false; num_blocks];
    let occ = match occupancy {
        Some(o) if !o.samples.is_empty() && o.fps > 0.0 => o,
        _ => return plausible,
    };

    // 1. Staticness: find furniture positions. Cluster every box center by spatial
    //    proximity across the whole recording; a persistent cluster whose members
    //    never wander more than `static_frac` (in either axis) is furniture — a
    //    net-side bystander, not a criss-crossing player. A box near such a centroid
    //    is dropped from every sample before the split. Proximity clustering (rather
    //    than a fixed grid) keeps a lightly-jittering box in one cluster, so a real
    //    player's small frame-to-frame wiggle is not mistaken for a still fixture.
    let static_positions = static_furniture_positions(occ, p.occupancy_static_frac);
    let is_furniture = |b: &DetBox| {
        let (cx, cy) = (b.x + b.w / 2.0, b.cy());
        static_positions
            .iter()
            .any(|&(sx, sy)| (cx - sx).abs() <= p.occupancy_static_frac && (cy - sy).abs() <= p.occupancy_static_frac)
    };

    // 2. Near/far split by area, over the surviving (non-furniture) boxes.
    let areas: Vec<f64> = occ
        .samples
        .iter()
        .flat_map(|s| s.iter())
        .filter(|b| !is_furniture(b))
        .map(|b| b.area())
        .collect();
    if areas.is_empty() {
        return plausible;
    }
    let min_area = areas.iter().cloned().fold(f64::INFINITY, f64::min);
    let max_area = areas.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let split = (min_area + max_area) / 2.0;

    // 3. Per-sample flags: two-player (near+far, opposite halves) and the surviving
    //    box centers, so kinetic movement can be measured sample-to-sample.
    let dt_ms = 1000.0 / occ.fps;
    let n = occ.samples.len();
    let mut two_player = vec![false; n];
    let mut occupied: Vec<bool> = Vec::with_capacity(n);
    let mut centers: Vec<Vec<(f64, f64)>> = Vec::with_capacity(n);
    for s in &occ.samples {
        let live: Vec<&DetBox> = s.iter().filter(|b| !is_furniture(b)).collect();
        occupied.push(!live.is_empty());
        centers.push(
            live.iter()
                .map(|b| (b.x + b.w / 2.0, b.cy()))
                .collect(),
        );
        // Near = large box (area ≥ split) sitting lower; far = small box higher.
        let near = live.iter().find(|b| b.area() >= split);
        let far = live.iter().find(|b| b.area() < split);
        let idx = centers.len() - 1;
        if let (Some(near), Some(far)) = (near, far) {
            // Opposite halves: the near player sits *substantially* lower in frame
            // than the far one — separated across the court midline, not merely a
            // hair apart. `MIN_HALF_SEPARATION` (fraction of frame height) is the gap
            // below which the two are treated as converged / in the same half, so a
            // pair milling together at the net never reads as opposite-half play.
            const MIN_HALF_SEPARATION: f64 = 0.2;
            two_player[idx] = near.cy() - far.cy() >= MIN_HALF_SEPARATION;
        }
    }

    // 4/5. Walk runs of occupied samples; propose the run's blocks when it is both
    //      kinetically active and opposite-half-occupied often enough.
    let mut i = 0;
    while i < n {
        if !occupied[i] {
            i += 1;
            continue;
        }
        let start = i;
        while i < n && occupied[i] {
            i += 1;
        }
        let end = i; // [start, end) run of occupied samples
        let span = end - start;
        if span == 0 {
            continue;
        }
        // Kinetic: fraction of samples where some center moved ≥ static_frac from
        // the previous sample.
        let mut moved = 0usize;
        for k in (start + 1)..end {
            if centers_moved(&centers[k - 1], &centers[k], p.occupancy_static_frac) {
                moved += 1;
            }
        }
        let active_frac = if span > 1 {
            moved as f64 / (span - 1) as f64
        } else {
            0.0
        };
        let opp_frac = two_player[start..end].iter().filter(|&&t| t).count() as f64 / span as f64;
        if active_frac >= p.occupancy_active_frac && opp_frac >= p.occupancy_opposite_frac {
            // Mark the blocks this sample run spans as plausible.
            let t0 = start as f64 * dt_ms;
            let t1 = (end as f64) * dt_ms;
            let b0 = ((t0 / block_ms as f64) as usize).min(num_blocks);
            let b1 = ((t1 / block_ms as f64).ceil() as usize).min(num_blocks);
            plausible[b0..b1].iter_mut().for_each(|p| *p = true);
        }
    }
    plausible
}

/// The centroids of every **furniture** cluster: box positions that persist across
/// much of the recording yet never drift more than `frac` (either axis) — a net-side
/// bystander, not a player. Greedy single-linkage clustering assigns each box center
/// to the first existing cluster whose centroid is within a generous **linkage
/// radius**, else opens a new one; the radius is deliberately much larger than `frac`
/// (`LINK_RADIUS`) so a moving player's frame-to-frame steps stay in one cluster
/// rather than fragmenting into several tight-but-static-looking ones. A cluster is
/// furniture when it is seen in at least a quarter of samples (persistent — a
/// transient false detection is not furniture) *and* its total center span stays
/// within `frac` on both axes (near-still over the whole recording). A real player
/// criss-crosses their half, so their cluster's span dwarfs `frac` and they are never
/// dropped. Deterministic: samples and boxes are visited in fixed order.
fn static_furniture_positions(occ: &OccupancyTrack, frac: f64) -> Vec<(f64, f64)> {
    // A player's between-sample step is far larger than furniture's jitter; link
    // within this radius so one player is one cluster, then judge staticness by span.
    const LINK_RADIUS: f64 = 0.2;
    // Per cluster: (count, seed_cx, seed_cy, min_cx, max_cx, min_cy, max_cy).
    struct Cluster {
        count: usize,
        cx: f64,
        cy: f64,
        minx: f64,
        maxx: f64,
        miny: f64,
        maxy: f64,
    }
    let mut clusters: Vec<Cluster> = Vec::new();
    for s in &occ.samples {
        for b in s {
            let (cx, cy) = (b.x + b.w / 2.0, b.cy());
            let hit = clusters
                .iter_mut()
                .find(|c| (cx - c.cx).abs() <= LINK_RADIUS && (cy - c.cy).abs() <= LINK_RADIUS);
            match hit {
                Some(c) => {
                    c.count += 1;
                    c.minx = c.minx.min(cx);
                    c.maxx = c.maxx.max(cx);
                    c.miny = c.miny.min(cy);
                    c.maxy = c.maxy.max(cy);
                    // Track the running centroid so linkage follows a drifting player.
                    c.cx = (c.minx + c.maxx) / 2.0;
                    c.cy = (c.miny + c.maxy) / 2.0;
                }
                None => clusters.push(Cluster {
                    count: 1,
                    cx,
                    cy,
                    minx: cx,
                    maxx: cx,
                    miny: cy,
                    maxy: cy,
                }),
            }
        }
    }
    let persistence = (occ.samples.len() / 4).max(3);
    clusters
        .into_iter()
        .filter(|c| c.count >= persistence && (c.maxx - c.minx) <= frac && (c.maxy - c.miny) <= frac)
        .map(|c| ((c.minx + c.maxx) / 2.0, (c.miny + c.maxy) / 2.0))
        .collect()
}

/// Whether any player moved between two samples: the minimum center-to-center
/// distance from a previous-sample box to a current-sample box exceeds `frac`. Uses
/// the nearest match so a two-box sample is not called "moved" just because the box
/// order differs; an added/removed box (a player entering/leaving frame) is not on
/// its own counted as motion.
fn centers_moved(prev: &[(f64, f64)], cur: &[(f64, f64)], frac: f64) -> bool {
    if prev.is_empty() || cur.is_empty() {
        return false;
    }
    cur.iter().any(|&(cx, cy)| {
        let nearest = prev
            .iter()
            .map(|&(px, py)| ((cx - px).powi(2) + (cy - py).powi(2)).sqrt())
            .fold(f64::INFINITY, f64::min);
        nearest > frac
    })
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
        let rallies = segment(&audio, SR, &motion, None).rallies;
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
        assert!(segment(&audio, SR, &motion, None).rallies.is_empty());
    }

    #[test]
    fn brief_lull_within_a_rally_is_bridged_not_split() {
        // Two movement bursts 2 s apart (< 2.5 s bridge) → one rally.
        let audio = synth_audio(30.0, &[(4.0, 10.0), (12.0, 20.0)], 0.6);
        let motion = synth_motion(30.0, &[(4.0, 10.0), (12.0, 20.0)]);
        let rallies = segment(&audio, SR, &motion, None).rallies;
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
        let rallies = segment(&audio, SR, &motion, None).rallies;
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
        assert!(segment(&audio, SR, &motion, None).rallies.is_empty());
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
        assert!(segment(&audio, SR, &motion, None).rallies.is_empty());
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
        let seg = segment(&audio, SR, &motion, None);
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
        let seg = segment(&audio, SR, &motion, None);
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
        let seg = segment(&audio, SR, &motion, None);
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
        let seg = segment(&audio, SR, &motion, None);
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
        let seg = segment(&audio, SR, &motion, None);
        assert!(seg.rallies.is_empty());
        assert!(seg.verdicts.is_empty(), "{:?}", seg.verdicts);
    }

    #[test]
    fn verdict_span_times_are_the_raw_unpadded_boundaries() {
        // A kept span's verdict reports the block boundaries the segmenter saw,
        // not the padded rally interval — the honest view for diagnosis.
        let audio = synth_audio(40.0, &[(5.0, 15.0)], 0.66);
        let motion = synth_motion(40.0, &[(5.0, 15.0)]);
        let seg = segment(&audio, SR, &motion, None);
        assert_eq!(seg.verdicts.len(), 1, "{:?}", seg.verdicts);
        let span = &seg.verdicts[0];
        assert_eq!(span.verdict, GateVerdict::Kept);
        let rally = &seg.rallies[0];
        // Padding widens the rally past the raw span on both edges.
        assert!(rally.start_ms <= span.start_ms, "{seg:?}");
        assert!(rally.end_ms >= span.end_ms, "{seg:?}");
    }

    // --- Occupancy proposes (ADR 0015 Stage 2, issue #84) --------------------
    //
    // The third synthetic track. Occupancy *proposes* candidate play spans that
    // motion then edges and audio then modulates; a failed detector (absent track)
    // falls back to motion-proposes and never loses a rally (the zero-miss bar).
    // Each test fabricates a detection track, same pattern as `synth_motion`.

    const DET_FPS: f64 = 3.0;

    /// A person box centered at `(cx, cy)` (frame fractions) with the given `area`
    /// (fraction of frame), square-ish. `area` is the depth proxy: near player big,
    /// far player small.
    fn dbox(cx: f64, cy: f64, area: f64) -> DetBox {
        let side = area.sqrt();
        DetBox {
            x: (cx - side / 2.0).clamp(0.0, 1.0),
            y: (cy - side / 2.0).clamp(0.0, 1.0),
            w: side,
            h: side,
        }
    }

    /// Build an [`OccupancyTrack`] of `total_secs` at [`DET_FPS`] by evaluating
    /// `per_sample(i, t)` for each sampled frame → that frame's boxes. Lets each test
    /// script exactly who is on court, where, and how they move.
    fn synth_occupancy(
        total_secs: f64,
        per_sample: impl Fn(usize, f64) -> Vec<DetBox>,
    ) -> OccupancyTrack {
        let n = (total_secs * DET_FPS) as usize;
        let samples = (0..n).map(|i| per_sample(i, i as f64 / DET_FPS)).collect();
        OccupancyTrack {
            fps: DET_FPS,
            samples,
        }
    }

    /// A far player (small box, high in frame) and a near player (large box, low in
    /// frame), both jittering each sample so the span reads kinetically active — the
    /// canonical "rally plausible" configuration inside `[s, e)`.
    fn active_rally_sample(i: usize, t: f64, s: f64, e: f64) -> Vec<DetBox> {
        if t < s || t >= e {
            return vec![];
        }
        let jitter = if i % 2 == 0 { 0.06 } else { -0.06 };
        vec![
            dbox(0.5 + jitter, 0.25, 0.02), // far: small, high
            dbox(0.5 - jitter, 0.75, 0.10), // near: large, low
        ]
    }

    #[test]
    fn two_active_players_in_opposite_halves_propose_a_rally() {
        // Motion is silent (no whole-court frame-differencing), audio has hits, and
        // occupancy sees two kinetically-active players in opposite halves. Occupancy
        // must *propose* the span even though motion never fired — the mechanism that
        // finds a rally motion is too subtle for.
        let audio = synth_audio(20.0, &[(4.0, 16.0)], 0.6);
        let motion = synth_motion(20.0, &[]); // motion never fires
        let occ = synth_occupancy(20.0, |i, t| active_rally_sample(i, t, 4.0, 16.0));
        let rallies = segment(&audio, SR, &motion, Some(&occ)).rallies;
        assert_eq!(rallies.len(), 1, "occupancy should propose a rally: {rallies:?}");
        // Occupancy places the (unedged-by-motion) span roughly over [4,16); padding
        // widens it. Never starts late / ends early (inclusion bias).
        assert!(rallies[0].start_ms <= 4_000, "start {rallies:?}");
        assert!(rallies[0].end_ms >= 16_000, "end {rallies:?}");
    }

    #[test]
    fn converging_players_off_half_yield_a_gap() {
        // Both players present and moving, but they drift into the *same* half
        // (converging at the net / wandering off their half) — never opposite halves.
        // Occupancy must not propose play; with motion also silent, the result is a
        // gap (no rally).
        let audio = synth_audio(20.0, &[(4.0, 16.0)], 0.6);
        let motion = synth_motion(20.0, &[]);
        let occ = synth_occupancy(20.0, |i, t| {
            if t < 4.0 || t >= 16.0 {
                return vec![];
            }
            let jitter = if i % 2 == 0 { 0.06 } else { -0.06 };
            // Both boxes sit in the lower half (cy ~0.7) — same half, so no
            // opposite-half configuration ever forms.
            vec![
                dbox(0.4 + jitter, 0.70, 0.02),
                dbox(0.6 - jitter, 0.72, 0.10),
            ]
        });
        let rallies = segment(&audio, SR, &motion, Some(&occ)).rallies;
        assert!(
            rallies.is_empty(),
            "same-half players must not propose a rally: {rallies:?}"
        );
    }

    #[test]
    fn a_static_box_near_the_net_is_ignored() {
        // A bystander stands nearly motionless by the net for the whole recording —
        // furniture. With no second, active, opposite-half player, occupancy proposes
        // nothing; motion is silent too, so no rally. The static box is dropped by
        // staticness, not geometry.
        let audio = synth_audio(20.0, &[], 1.0);
        let motion = synth_motion(20.0, &[]);
        let occ = synth_occupancy(20.0, |_i, _t| {
            // One box, mid-frame (net side), never moving.
            vec![dbox(0.5, 0.5, 0.05)]
        });
        let rallies = segment(&audio, SR, &motion, Some(&occ)).rallies;
        assert!(
            rallies.is_empty(),
            "a lone static net-side box is furniture, not play: {rallies:?}"
        );
    }

    #[test]
    fn near_player_disappearing_mid_span_keeps_the_rally() {
        // A real rally where the near player retreats deep and leaves frame for a
        // stretch mid-span (the camera cuts off the baseline). Only the far player is
        // visible during that stretch. Only-one-visible must NEVER end the span —
        // the rally continues across the disappearance as one span.
        let audio = synth_audio(24.0, &[(4.0, 20.0)], 0.6);
        let motion = synth_motion(24.0, &[]);
        let occ = synth_occupancy(24.0, |i, t| {
            if t < 4.0 || t >= 20.0 {
                return vec![];
            }
            let jitter = if i % 2 == 0 { 0.06 } else { -0.06 };
            let far = dbox(0.5 + jitter, 0.25, 0.02);
            // Near player leaves frame for the middle third of the rally.
            if (10.0..14.0).contains(&t) {
                vec![far] // only the far player visible
            } else {
                vec![far, dbox(0.5 - jitter, 0.75, 0.10)]
            }
        });
        let rallies = segment(&audio, SR, &motion, Some(&occ)).rallies;
        assert_eq!(
            rallies.len(),
            1,
            "a deep retreat must not shatter or end the rally: {rallies:?}"
        );
        assert!(rallies[0].start_ms <= 4_000, "start {rallies:?}");
        assert!(rallies[0].end_ms >= 20_000, "end {rallies:?}");
    }

    #[test]
    fn absent_occupancy_falls_back_to_motion_proposes() {
        // Degradation (the zero-miss bar): the detector could not run, so no
        // occupancy track. Fusion must behave exactly as pre-#84 — motion proposes.
        // A moving span is kept whether or not occupancy exists; passing `None` must
        // never lose the rally motion found.
        let audio = synth_audio(20.0, &[(4.0, 16.0)], 0.6);
        let motion = synth_motion(20.0, &[(4.0, 16.0)]);
        let with_none = segment(&audio, SR, &motion, None).rallies;
        let empty = OccupancyTrack { fps: DET_FPS, samples: vec![] };
        let with_empty = segment(&audio, SR, &motion, Some(&empty)).rallies;
        assert_eq!(with_none.len(), 1, "motion still proposes: {with_none:?}");
        // An empty track is the same as no track — a failed detector changes nothing.
        assert_eq!(with_none, with_empty, "empty occ must equal None");
    }

    #[test]
    fn occupancy_never_deletes_a_motion_rally() {
        // Occupancy sees nothing rally-plausible (players converging, same half), but
        // motion clearly fired. The union must keep motion's rally — no single signal
        // deletes a rally on its own (ADR 0015).
        let audio = synth_audio(20.0, &[(4.0, 16.0)], 0.6);
        let motion = synth_motion(20.0, &[(4.0, 16.0)]); // motion fires
        let occ = synth_occupancy(20.0, |_i, _t| vec![dbox(0.5, 0.5, 0.05)]); // furniture only
        let rallies = segment(&audio, SR, &motion, Some(&occ)).rallies;
        assert_eq!(
            rallies.len(),
            1,
            "motion's rally must survive an unhelpful occupancy track: {rallies:?}"
        );
    }
}
