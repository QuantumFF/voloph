//! The **eval harness** (ADR 0015): a dev tool — not app UI — that referees the
//! segmenter against fully hand-corrected timelines. Given the recordings whose
//! timelines a human has corrected into truth (**gold**), it re-runs the *current*
//! segmenter on each and prints the three acceptance-bar numbers the ADR set:
//!
//! - **missed rallies** — gold rallies with no overlapping draft rally (the worst
//!   error: play hidden with no marker; the ADR's hard `≈ 0` constraint);
//! - **false positives per hour** — draft rallies overlapping no gold rally (a
//!   bounded one-keystroke-delete cost);
//! - **median boundary error** — how far the matched draft edges land from the gold
//!   edges, in seconds (the ~1 s bounded cost).
//!
//! The heart is a **pure scoring function** ([`score`]) over interval sets: it takes
//! the draft rallies, the gold rallies, and the recording duration, and returns the
//! metrics — no DB, no ffmpeg, no I/O, so it is exhaustively unit-testable with
//! synthetic intervals. Everything below it ([`run`]) is a thin, untested shell:
//! open the DB, gather the gold corpus, re-run the segmenter, feed the pure core,
//! print. Re-running as new footage arrives is a single command.
//!
//! **Gold vs. machine draft are separable artifacts** (ADR 0015). Gold is the
//! recording's *current* DB timeline, kept only when it shows hand-correction — the
//! confidence sentinel [`crate::db::CORRECTED_CONFIDENCE`] an inline edit stamps on a
//! rally. A recording with no such rally is **skipped and reported**, never scored
//! against unverified gold. The machine draft is produced fresh by re-running the
//! segmenter here, so rebuilding with a changed segmenter and re-running the harness
//! re-measures it — the eval gate of Stages 1–2. (Reading the machine draft from the
//! published Analysis instead would only ever measure the version frozen there before
//! the human corrected it; a gold recording's Analysis never regenerates.)

use std::path::PathBuf;

use rusqlite::Connection;

use crate::db::{self, CORRECTED_CONFIDENCE};
use crate::media;
use crate::segment;

// ── Pure scoring core ────────────────────────────────────────────────────────
//
// Interval math only. No DB, no ffmpeg, no I/O — the entire acceptance bar reduces
// to overlap and boundary arithmetic over two interval sets, so it is unit-tested
// in isolation from the shell that feeds it real rallies.

/// A rally interval in milliseconds from the recording start. Both the segmenter's
/// draft [`crate::segment::Rally`] and a gold DB rally collapse to this — the scorer
/// cares only about where the play spans sit, never how they were produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interval {
    pub start_ms: i64,
    pub end_ms: i64,
}

impl Interval {
    /// Milliseconds of overlap between two intervals, `0` when they are disjoint or
    /// merely touch. Touching edges (`a.end == b.start`) are *not* overlap: a rally
    /// that ends exactly where the next begins is two rallies, not one.
    fn overlap_ms(&self, other: &Interval) -> i64 {
        (self.end_ms.min(other.end_ms) - self.start_ms.max(other.start_ms)).max(0)
    }

    /// Whether two intervals share any positive span of time.
    fn overlaps(&self, other: &Interval) -> bool {
        self.overlap_ms(other) > 0
    }
}

/// The scored verdict for one recording (or, aggregated, a whole corpus): the three
/// acceptance-bar numbers plus the counts they are derived from. `boundary_errors_ms`
/// is carried out so a corpus aggregate can pool every recording's edge errors and
/// take one honest median, rather than averaging per-recording medians.
#[derive(Debug, Clone, PartialEq)]
pub struct Score {
    /// Gold rallies scored against.
    pub gold_count: usize,
    /// Draft rallies the segmenter produced.
    pub draft_count: usize,
    /// Gold rallies with at least one overlapping draft rally (`gold_count - misses`).
    pub matched: usize,
    /// Gold rallies with **no** overlapping draft rally — play the segmenter hid.
    pub misses: usize,
    /// Draft rallies overlapping **no** gold rally — spurious rallies.
    pub false_positives: usize,
    /// [`Self::false_positives`] normalized to the recording's duration.
    pub false_positives_per_hour: f64,
    /// Every boundary error, in ms: for each matched gold rally, the absolute start
    /// and end offset from its best-overlapping draft rally (two samples per match).
    pub boundary_errors_ms: Vec<i64>,
    /// Median of [`Self::boundary_errors_ms`], in seconds; `None` when nothing matched.
    pub median_boundary_error_secs: Option<f64>,
}

/// Score a draft timeline against a gold one (ADR 0015). Pure: interval sets and a
/// duration in, metrics out. `duration_ms` normalizes the false-positive count to
/// per-hour; when it is non-positive (unknown) the span is taken from the intervals
/// themselves so the number stays finite.
///
/// Matching is overlap-based and asymmetric to the metric:
/// - a gold rally is **missed** when no draft rally overlaps it;
/// - a draft rally is a **false positive** when it overlaps no gold rally;
/// - for boundary error, each matched gold rally is paired with its
///   **best-overlapping** draft rally (ties broken by earliest start, then end, so
///   the result is deterministic), contributing that pair's start and end offsets.
pub fn score(draft: &[Interval], gold: &[Interval], duration_ms: i64) -> Score {
    let mut misses = 0usize;
    let mut boundary_errors_ms: Vec<i64> = Vec::new();
    for g in gold {
        match best_match(g, draft) {
            Some(d) => {
                boundary_errors_ms.push((g.start_ms - d.start_ms).abs());
                boundary_errors_ms.push((g.end_ms - d.end_ms).abs());
            }
            None => misses += 1,
        }
    }
    let false_positives = draft.iter().filter(|d| !gold.iter().any(|g| d.overlaps(g))).count();

    let false_positives_per_hour = per_hour(false_positives, effective_ms(duration_ms, draft, gold));

    Score {
        gold_count: gold.len(),
        draft_count: draft.len(),
        matched: gold.len() - misses,
        misses,
        false_positives,
        false_positives_per_hour,
        median_boundary_error_secs: median(&boundary_errors_ms).map(|ms| ms / 1000.0),
        boundary_errors_ms,
    }
}

/// The draft rally that best covers `gold` — the most overlapping one — or `None`
/// when none overlaps it at all (a miss). Ties in overlap resolve to the earliest
/// start, then the earliest end, so the pairing never depends on input order.
fn best_match<'a>(gold: &Interval, draft: &'a [Interval]) -> Option<&'a Interval> {
    draft
        .iter()
        .filter(|d| d.overlaps(gold))
        .max_by(|a, b| {
            let by_overlap = a.overlap_ms(gold).cmp(&b.overlap_ms(gold));
            // Larger overlap wins; on a tie prefer the earlier (smaller) start, then
            // the earlier end — so reverse the position comparisons under `max_by`.
            by_overlap
                .then_with(|| b.start_ms.cmp(&a.start_ms))
                .then_with(|| b.end_ms.cmp(&a.end_ms))
        })
}

/// Milliseconds in an hour — the divisor for every per-hour rate.
const MS_PER_HOUR: f64 = 3_600_000.0;

/// A count normalized to per-hour over `span_ms`, or `0.0` when the span is
/// non-positive (so an unknown duration never yields a divide-by-zero / infinity).
fn per_hour(count: usize, span_ms: i64) -> f64 {
    let hours = span_ms as f64 / MS_PER_HOUR;
    if hours > 0.0 {
        count as f64 / hours
    } else {
        0.0
    }
}

/// The recording's length in ms for the per-hour rate: its known `duration_ms`, or —
/// when that is unknown (`<= 0`) — the latest edge across both timelines, so a
/// missing duration degrades to "the footage we can see" rather than a zero span.
fn effective_ms(duration_ms: i64, draft: &[Interval], gold: &[Interval]) -> i64 {
    if duration_ms > 0 {
        duration_ms
    } else {
        draft
            .iter()
            .chain(gold)
            .map(|i| i.end_ms)
            .max()
            .unwrap_or(0)
    }
}

/// The median of a slice of millisecond errors, or `None` when it is empty. Even
/// counts average the two central values. Does not mutate the caller's data.
fn median(values: &[i64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let mid = sorted.len() / 2;
    Some(if sorted.len() % 2 == 1 {
        sorted[mid] as f64
    } else {
        (sorted[mid - 1] + sorted[mid]) as f64 / 2.0
    })
}

// ── CLI shell ─────────────────────────────────────────────────────────────────
//
// Thin and untested (its logic is DB reads, ffmpeg calls, and printing; the judgement
// lives in the pure core above). Gathers gold from the DB, re-runs the segmenter, and
// reports per-recording and aggregate numbers.

/// One recording as the corpus needs it, resolved to an absolute path for ffmpeg.
struct CorpusRecording {
    abs_path: String,
    rel_path: String,
    duration_ms: Option<i64>,
    segment_state: String,
    gold: Vec<Interval>,
    /// Whether the timeline shows hand-correction — the confidence sentinel on at
    /// least one rally. Only these are scored; the rest are reported as skipped.
    hand_corrected: bool,
}

/// Run the eval harness. `args` is the process arguments **after** the program name.
///
/// Usage: `eval-harness [--db PATH] [--library local|shared] [--sweep] [RECORDING]`
/// - `--db PATH` — the metadata DB to read (default: the app's own DB for this OS).
/// - `--library KIND` — which library's recordings to score (default: the active one).
/// - `--sweep` — instead of one scored run, extract each gold recording's signal
///   tracks once and re-score the pure [`segment::segment_with`] seam across a grid
///   of occupancy parameters (ADR 0016), printing one line per configuration.
/// - `--trace` — diagnose every missed gold rally at the default parameters.
/// - `--fp-trace` — classify every draft span (false positives and true positives
///   alike) by signal support and price the candidate suppression rules (issue #92).
/// - `--headroom` — price the candidate velocity firing rule and 5 fps detector
///   sampling against the v5 residual misses (issue #93); `--scratch DIR` sets
///   where the 5 fps tracks are cached (default: the system temp dir).
/// - `--edges` — split v5's boundary errors by which signal placed each draft
///   edge, plus the motion-near-gold-edge trim-viability counts (issue #93).
/// - `--presence` — quantify presence headroom in the residual missed windows
///   from sub-threshold detections and continuity bridging (issue #93 round 2);
///   shares `--scratch` with `--headroom`.
/// - `--spans` — span-score separation: integrated continuity+movement(+audio)
///   features over gold rallies, duration-matched out-of-gold windows, and v5's
///   FP spans, swept through the declared score grid (issue #93 round 3).
/// - `RECORDING` — a path substring; score only recordings whose path contains it
///   (default: the whole library).
pub fn run(args: Vec<String>) -> Result<(), String> {
    let opts = Options::parse(&args)?;
    if opts.help {
        print_usage();
        return Ok(());
    }

    let db_path = match opts.db {
        Some(p) => PathBuf::from(p),
        None => default_db_path()?,
    };
    if !db_path.exists() {
        return Err(format!(
            "no metadata database at {}\n(pass --db PATH to point at one)",
            db_path.display()
        ));
    }
    let conn = db::open(&db_path).map_err(|e| format!("could not open {}: {e}", db_path.display()))?;

    let kind = match opts.library {
        Some(k) => k,
        None => db::active_kind(&conn).map_err(|e| e.to_string())?,
    };
    let library = db::library_path_of(&conn, &kind)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("the '{kind}' library is not designated in {}", db_path.display()))?;

    let mut recordings = gather_corpus(&conn, &kind, &library).map_err(|e| e.to_string())?;
    if let Some(filter) = &opts.filter {
        recordings.retain(|r| r.abs_path.contains(filter) || r.rel_path.contains(filter));
        if recordings.is_empty() {
            return Err(format!("no recording in the '{kind}' library matched \"{filter}\""));
        }
    }

    println!(
        "eval harness — segmenter v{} against '{kind}' library ({})\n",
        segment::SEGMENTER_VERSION,
        library
    );
    if opts.sweep {
        sweep_and_report(&recordings);
    } else if opts.trace {
        trace_and_report(&recordings);
    } else if opts.fp_trace {
        fp_trace_and_report(&recordings);
    } else if opts.headroom || opts.presence {
        let scratch = opts
            .scratch
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("voloph-eval-scratch"));
        if opts.headroom {
            headroom_and_report(&recordings, &scratch);
        } else {
            presence_and_report(&recordings, &scratch);
        }
    } else if opts.edges {
        edges_and_report(&recordings);
    } else if opts.spans {
        spans_and_report(&recordings);
    } else {
        score_and_report(&recordings);
    }
    Ok(())
}

/// Read every recording in the `kind` library, resolve its absolute path, and load
/// its gold timeline (the current DB rallies) together with whether that timeline
/// shows hand-correction (the confidence sentinel).
fn gather_corpus(
    conn: &Connection,
    kind: &str,
    library: &str,
) -> rusqlite::Result<Vec<CorpusRecording>> {
    let rows: Vec<(i64, String, Option<i64>, String)> = {
        let mut stmt = conn.prepare(
            "SELECT id, path, duration_ms, segment_state FROM recordings
             WHERE library = ?1 ORDER BY path",
        )?;
        let rows = stmt
            .query_map([kind], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };

    let mut out = Vec::with_capacity(rows.len());
    for (id, rel_path, duration_ms, segment_state) in rows {
        let mut stmt = conn.prepare(
            "SELECT start_ms, end_ms, confidence FROM rallies
             WHERE recording_id = ?1 ORDER BY start_ms",
        )?;
        let mut gold = Vec::new();
        let mut hand_corrected = false;
        let mapped = stmt.query_map([id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, f64>(2)?))
        })?;
        for row in mapped {
            let (start_ms, end_ms, confidence) = row?;
            if confidence >= CORRECTED_CONFIDENCE {
                hand_corrected = true;
            }
            gold.push(Interval { start_ms, end_ms });
        }
        out.push(CorpusRecording {
            abs_path: db::absolute(library, &rel_path),
            rel_path,
            duration_ms,
            segment_state,
            gold,
            hand_corrected,
        });
    }
    Ok(out)
}

/// Score each gold recording, print its numbers, and print the corpus aggregate.
/// Recordings that are not segmented or not hand-corrected are reported as skipped;
/// a recording whose audio/motion the segmenter cannot re-extract is reported as an
/// error. Neither ever contributes to the numbers.
fn score_and_report(recordings: &[CorpusRecording]) {
    let mut scored = 0usize;
    let mut skipped = 0usize;
    let mut errored = 0usize;
    let mut total_misses = 0usize;
    let mut total_false_positives = 0usize;
    let mut total_ms = 0i64;
    let mut all_boundary_errors: Vec<i64> = Vec::new();

    for rec in recordings {
        if rec.segment_state != "ready" {
            println!("SKIP  {}  (not segmented: {})", rec.rel_path, rec.segment_state);
            skipped += 1;
            continue;
        }
        if !rec.hand_corrected {
            println!("SKIP  {}  (no hand-corrected timeline — not gold)", rec.rel_path);
            skipped += 1;
            continue;
        }

        let (draft, duration_ms) = match rerun_segmenter(&rec.abs_path) {
            Ok(v) => v,
            Err(e) => {
                println!("ERR   {}  ({e})", rec.rel_path);
                errored += 1;
                continue;
            }
        };
        let duration_ms = rec.duration_ms.filter(|&d| d > 0).unwrap_or(duration_ms);

        let s = score(&draft, &rec.gold, duration_ms);
        print_recording(&rec.rel_path, &s);

        scored += 1;
        total_misses += s.misses;
        total_false_positives += s.false_positives;
        total_ms += duration_ms;
        all_boundary_errors.extend(s.boundary_errors_ms);
    }

    println!(
        "\n=== aggregate over {scored} scored recording(s) — {skipped} skipped, {errored} error(s) ==="
    );
    if scored == 0 {
        println!("no gold recordings to score");
        return;
    }
    let fp_per_hour = per_hour(total_false_positives, total_ms);
    println!("footage scored        : {:.2} h", total_ms as f64 / MS_PER_HOUR);
    println!("missed rallies        : {total_misses}   (ADR 0015 target ≈ 0)");
    println!(
        "false positives       : {total_false_positives}  ({fp_per_hour:.2} / hour)"
    );
    match median(&all_boundary_errors).map(|ms| ms / 1000.0) {
        Some(secs) => println!("median boundary error : {secs:.2} s"),
        None => println!("median boundary error : n/a (nothing matched)"),
    }
}

/// Re-run the current segmenter on the recording at `abs_path`, returning its draft
/// intervals and the duration derived from the decoded audio. Surfaces any
/// extraction failure as an error string so the caller can report and skip it.
fn rerun_segmenter(abs_path: &str) -> Result<(Vec<Interval>, i64), String> {
    let samples = media::extract_pcm(abs_path)?;
    let energy = media::extract_motion(abs_path, |_| {})?; // dev CLI: no progress UI
    let motion = segment::MotionTrack {
        fps: f64::from(media::MOTION_FPS),
        energy,
    };
    // The harness re-runs the exact production fusion (ADR 0015): occupancy proposes
    // when the detector is available, else motion-proposes. A detector that cannot
    // load/run (missing model, ort init failure) yields no occupancy track — analysis
    // still completes and no rally is lost (the zero-miss bar).
    let occupancy = extract_occupancy(abs_path);
    let seg = segment::segment(
        &samples,
        media::SEGMENT_SAMPLE_RATE,
        &motion,
        occupancy.as_ref(),
    );
    let draft = seg
        .rallies
        .into_iter()
        .map(|r| Interval {
            start_ms: r.start_ms,
            end_ms: r.end_ms,
        })
        .collect();
    let duration_ms = (samples.len() as f64 / media::SEGMENT_SAMPLE_RATE as f64 * 1000.0) as i64;
    Ok((draft, duration_ms))
}

/// Run the occupancy detector over a recording and return the pure track for fusion,
/// or `None` if the detector is unavailable for any reason (the shared degradation
/// policy in [`crate::detect::detections_or_none`]). The harness must still score a
/// draft when the detector cannot run — failures are printed, not fatal, so the
/// harness numbers reflect the motion-proposes fallback honestly.
fn extract_occupancy(abs_path: &str) -> Option<segment::OccupancyTrack> {
    crate::detect::detections_or_none(abs_path, |why| {
        eprintln!("      (occupancy disabled — {why})");
    })
    .map(|track| track.to_occupancy_track())
}

// ── Parameter sweep ───────────────────────────────────────────────────────────
//
// The tuning loop of ADR 0016: extraction (ffmpeg + detector) is the expensive
// part and depends on no parameter, so each gold recording's tracks are pulled
// once and the pure `segment_with` seam is re-scored across the whole occupancy
// grid from cache. Same shell/core split as the single run: all judgement stays
// in `score`.

/// One gold recording's extracted signal tracks, cached so the sweep re-runs only
/// the pure seam per configuration.
struct CachedTracks {
    rel_path: String,
    samples: Vec<f32>,
    motion: segment::MotionTrack,
    occupancy: Option<segment::OccupancyTrack>,
    gold: Vec<Interval>,
    duration_ms: i64,
}

/// The occupancy grid swept (issue #91): every combination of the four ADR 0016
/// knobs plus `occupancy_static_frac` (the movement floor doubles as the firing
/// rule's speed demand, so it shapes the in-rally vs gap density separation),
/// bracketing each default.
const SWEEP_RATIO: &[f64] = &[1.0, 1.5, 2.0, 2.5];
const SWEEP_AREA_CAP_K: &[f64] = &[4.0, 8.0, 16.0];
const SWEEP_WINDOW_MS: &[i64] = &[1500, 2000, 3000, 4000];
const SWEEP_DENSITY: &[f64] = &[0.3, 0.4, 0.5, 0.6];
const SWEEP_STATIC_FRAC: &[f64] = &[0.02, 0.05];

/// Score one parameter set against every cached recording, aggregated.
fn score_config(tracks: &[CachedTracks], p: &segment::Params) -> (usize, usize, f64, Option<f64>, usize) {
    let mut misses = 0usize;
    let mut false_positives = 0usize;
    let mut total_ms = 0i64;
    let mut draft_count = 0usize;
    let mut boundary_errors: Vec<i64> = Vec::new();
    for t in tracks {
        let seg = segment::segment_with(
            &t.samples,
            media::SEGMENT_SAMPLE_RATE,
            &t.motion,
            t.occupancy.as_ref(),
            p,
        );
        let draft: Vec<Interval> = seg
            .rallies
            .iter()
            .map(|r| Interval { start_ms: r.start_ms, end_ms: r.end_ms })
            .collect();
        let s = score(&draft, &t.gold, t.duration_ms);
        misses += s.misses;
        false_positives += s.false_positives;
        total_ms += t.duration_ms;
        draft_count += s.draft_count;
        boundary_errors.extend(s.boundary_errors_ms);
    }
    let fp_per_hour = per_hour(false_positives, total_ms);
    let med = median(&boundary_errors).map(|ms| ms / 1000.0);
    (misses, false_positives, fp_per_hour, med, draft_count)
}

/// Extract every gold recording's tracks once, then print one aggregate line per
/// grid configuration plus the occupancy-disabled baseline. Sorted best-first
/// (fewest misses, then FP/h) so the frontier reads off the top.
fn sweep_and_report(recordings: &[CorpusRecording]) {
    let mut tracks: Vec<CachedTracks> = Vec::new();
    for rec in recordings {
        if rec.segment_state != "ready" || !rec.hand_corrected {
            println!("SKIP  {}  (not gold)", rec.rel_path);
            continue;
        }
        let samples = match media::extract_pcm(&rec.abs_path) {
            Ok(s) => s,
            Err(e) => {
                println!("ERR   {}  ({e})", rec.rel_path);
                continue;
            }
        };
        let energy = match media::extract_motion(&rec.abs_path, |_| {}) {
            Ok(e) => e,
            Err(e) => {
                println!("ERR   {}  ({e})", rec.rel_path);
                continue;
            }
        };
        let duration_ms = rec
            .duration_ms
            .filter(|&d| d > 0)
            .unwrap_or((samples.len() as f64 / media::SEGMENT_SAMPLE_RATE as f64 * 1000.0) as i64);
        println!("CACHE {}", rec.rel_path);
        tracks.push(CachedTracks {
            rel_path: rec.rel_path.clone(),
            samples,
            motion: segment::MotionTrack {
                fps: f64::from(media::MOTION_FPS),
                energy,
            },
            occupancy: extract_occupancy(&rec.abs_path),
            gold: rec.gold.clone(),
            duration_ms,
        });
    }
    if tracks.is_empty() {
        println!("no gold recordings to sweep");
        return;
    }
    let gold_total: usize = tracks.iter().map(|t| t.gold.len()).sum();
    println!(
        "\nsweeping {} configuration(s) over {} recording(s), {} gold rallies",
        SWEEP_STATIC_FRAC.len()
            * SWEEP_RATIO.len()
            * SWEEP_AREA_CAP_K.len()
            * SWEEP_WINDOW_MS.len()
            * SWEEP_DENSITY.len(),
        tracks.len(),
        gold_total
    );

    // The occupancy-disabled baseline: what motion alone proposes. A config's
    // draft-count delta against this is the "occupancy contributed spans" signal.
    let baseline_tracks: Vec<CachedTracks> = tracks
        .iter()
        .map(|t| CachedTracks {
            rel_path: t.rel_path.clone(),
            samples: t.samples.clone(),
            motion: segment::MotionTrack {
                fps: t.motion.fps,
                energy: t.motion.energy.clone(),
            },
            occupancy: None,
            gold: t.gold.clone(),
            duration_ms: t.duration_ms,
        })
        .collect();
    let p0 = segment::Params::default();
    let (b_miss, b_fp, b_fph, b_med, b_draft) = score_config(&baseline_tracks, &p0);
    println!(
        "baseline (occupancy off): miss {b_miss} | fp {b_fp} ({b_fph:.1}/h) | med {} | draft {b_draft}\n",
        fmt_med(b_med)
    );

    let mut rows: Vec<(usize, f64, String)> = Vec::new();
    for &static_frac in SWEEP_STATIC_FRAC {
        for &ratio in SWEEP_RATIO {
            for &cap_k in SWEEP_AREA_CAP_K {
                for &window_ms in SWEEP_WINDOW_MS {
                    for &density in SWEEP_DENSITY {
                        let p = segment::Params {
                            occupancy_static_frac: static_frac,
                            occupancy_ratio: ratio,
                            occupancy_area_cap_k: cap_k,
                            occupancy_window_ms: window_ms,
                            occupancy_density: density,
                            ..segment::Params::default()
                        };
                        let (miss, fp, fph, med, draft) = score_config(&tracks, &p);
                        rows.push((
                            miss,
                            fph,
                            format!(
                                "static {static_frac:.3} | ratio {ratio:>3.1} | cap {cap_k:>4.1} | win {window_ms:>4} | dens {density:.2}  →  miss {miss:>2} | fp {fp:>3} ({fph:>5.1}/h) | med {} | draft {draft} ({:+})",
                                fmt_med(med),
                                draft as i64 - b_draft as i64
                            ),
                        ));
                    }
                }
            }
        }
    }
    rows.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
    for (_, _, line) in &rows {
        println!("{line}");
    }
}

/// A median-boundary-error option as a fixed-width cell.
fn fmt_med(med: Option<f64>) -> String {
    match med {
        Some(s) => format!("{s:.2}s"),
        None => "  n/a".to_string(),
    }
}

// ── Miss trace ────────────────────────────────────────────────────────────────
//
// The #85-style diagnosis, mechanized (issue #91's acceptance demands every
// residual miss individually traced): for each gold rally the draft misses at the
// default parameters, report what the occupancy pipeline saw inside its window —
// how often anyone / two people survived the filters, how often the firing rule
// fired, and the peak windowed density — and name the stage that lost it.

/// Trace every missed gold rally at the default parameters. Reuses the same
/// per-recording extraction as a scored run.
fn trace_and_report(recordings: &[CorpusRecording]) {
    let p = segment::Params::default();
    let mut total_missed = 0usize;
    for rec in recordings {
        if rec.segment_state != "ready" || !rec.hand_corrected {
            continue;
        }
        let (samples, motion, occupancy) = match extract_tracks(&rec.abs_path) {
            Ok(t) => t,
            Err(e) => {
                println!("ERR   {}  ({e})", rec.rel_path);
                continue;
            }
        };
        let seg = segment::segment_with(
            &samples,
            media::SEGMENT_SAMPLE_RATE,
            &motion,
            occupancy.as_ref(),
            &p,
        );
        let draft: Vec<Interval> = seg
            .rallies
            .iter()
            .map(|r| Interval { start_ms: r.start_ms, end_ms: r.end_ms })
            .collect();
        let firing = occupancy.as_ref().map(|o| (segment::occupancy_firing(o, &p), o.fps));
        println!("TRACE {}", rec.rel_path);
        for g in &rec.gold {
            if draft.iter().any(|d| d.overlaps(g)) {
                continue;
            }
            total_missed += 1;
            match &firing {
                None => println!(
                    "        miss {:>7.1}s–{:>7.1}s  (no occupancy track — detector did not run)",
                    g.start_ms as f64 / 1000.0,
                    g.end_ms as f64 / 1000.0
                ),
                Some((f, fps)) => {
                    let lo = ((g.start_ms as f64 / 1000.0) * fps) as usize;
                    let hi = (((g.end_ms as f64 / 1000.0) * fps) as usize).min(f.len());
                    let window = &f[lo.min(f.len())..hi];
                    let n = window.len().max(1);
                    let any = window.iter().filter(|s| s.live >= 1).count();
                    let two = window.iter().filter(|s| s.live >= 2).count();
                    let fired = window.iter().filter(|s| s.fired).count();
                    // Peak windowed density inside the rally, at the same window
                    // width the block judge uses.
                    let w_samples = ((p.occupancy_window_ms as f64 / 1000.0) * fps).round() as usize;
                    let half = (w_samples / 2).max(1);
                    let peak = (lo..hi)
                        .map(|c| {
                            let a = c.saturating_sub(half);
                            let b = (c + half + 1).min(f.len());
                            f[a..b].iter().filter(|s| s.fired).count() as f64 / (b - a).max(1) as f64
                        })
                        .fold(0.0, f64::max);
                    let verdict = if two * 2 < n {
                        "detector/filters: two players rarely survive"
                    } else if (fired as f64) < 0.2 * n as f64 {
                        "firing rule: structure present, fires too rarely"
                    } else if peak < p.occupancy_density {
                        "density: fires, but never densely enough"
                    } else {
                        "coverage: dense firing, span lost downstream (min-rally/bridge)"
                    };
                    println!(
                        "        miss {:>7.1}s–{:>7.1}s  any {:>3.0}% | two {:>3.0}% | fired {:>3.0}% | peak dens {:.2}  →  {verdict}",
                        g.start_ms as f64 / 1000.0,
                        g.end_ms as f64 / 1000.0,
                        100.0 * any as f64 / n as f64,
                        100.0 * two as f64 / n as f64,
                        100.0 * fired as f64 / n as f64,
                        peak
                    );
                }
            }
        }
    }
    println!("\n{total_missed} missed gold rally(ies) traced at default parameters");
}

/// Extract the three signal tracks for one recording — the shared shell step of a
/// scored run, a sweep cache, and a trace.
fn extract_tracks(
    abs_path: &str,
) -> Result<(Vec<f32>, segment::MotionTrack, Option<segment::OccupancyTrack>), String> {
    let samples = media::extract_pcm(abs_path)?;
    let energy = media::extract_motion(abs_path, |_| {})?;
    Ok((
        samples,
        segment::MotionTrack {
            fps: f64::from(media::MOTION_FPS),
            energy,
        },
        extract_occupancy(abs_path),
    ))
}

// ── FP trace (issue #92) ──────────────────────────────────────────────────────
//
// The FP-composition measurement of issue #92: classify every draft span on the
// gold corpus — false positives and true positives alike — by signal support
// (block provenance, occupancy firing density, audio verdict), then price each
// obvious suppression rule as a measured pair: FP/h removed vs gold rallies lost.
// The recall cost is simulated exactly (drop the suppressed spans, re-run the
// pure scorer), never assumed (ADR 0015: no precision is bought by paying
// recall). Purely observational — the draft is never altered and
// `SEGMENTER_VERSION` stays 5.

/// Which proposer(s) put a draft span on the timeline. The union fusion collapses
/// the motion and occupancy masks into one candidate mask, so this is re-read per
/// span from [`segment::FusionBlocks`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Provenance {
    /// Motion fired somewhere in the span; occupancy proposed none of its blocks.
    MotionOnly,
    /// Occupancy proposed blocks; motion never fired inside the span.
    OccupancyOnly,
    /// Both proposers contributed blocks.
    Mixed,
}

impl Provenance {
    /// Stable label for report rows.
    fn label(self) -> &'static str {
        match self {
            Provenance::MotionOnly => "motion-only",
            Provenance::OccupancyOnly => "occ-only",
            Provenance::Mixed => "mixed",
        }
    }
}

/// One draft span classified by signal support (issue #92): who proposed its
/// blocks, whether audio confirmed it, how densely the occupancy firing rule
/// fired inside it, and whether it is a false positive against gold.
#[derive(Debug, Clone, PartialEq)]
struct SpanSupport {
    span: Interval,
    provenance: Provenance,
    /// Fraction of the span's blocks that were motion-active / occupancy-proposed.
    /// The padded edges and bridged downtime count in the denominator, so these
    /// read as "how much of what the user sees is signal-backed".
    motion_frac: f64,
    occupancy_frac: f64,
    /// Audio confirmed play in the span ([`segment::GateVerdict::Kept`]).
    audio_confirmed: bool,
    /// Fraction of detector samples inside the span that fired the occupancy
    /// rule; `None` when no track covers the span. Finer-grained than
    /// [`Self::occupancy_frac`]: sub-threshold firing shows up here first.
    fired_fraction: Option<f64>,
    /// Overlaps no gold rally — a false positive.
    is_fp: bool,
}

impl SpanSupport {
    /// The six-way support class this span falls in (provenance × audio).
    fn class(&self) -> (Provenance, bool) {
        (self.provenance, self.audio_confirmed)
    }
}

/// The six support classes in report order, with their row labels.
const SUPPORT_CLASSES: [(Provenance, bool); 6] = [
    (Provenance::MotionOnly, false),
    (Provenance::MotionOnly, true),
    (Provenance::Mixed, false),
    (Provenance::Mixed, true),
    (Provenance::OccupancyOnly, false),
    (Provenance::OccupancyOnly, true),
];

/// A support class's row label, e.g. `motion-only / unconfirmed`.
fn class_label(class: (Provenance, bool)) -> String {
    format!(
        "{} / {}",
        class.0.label(),
        if class.1 { "confirmed" } else { "unconfirmed" }
    )
}

/// Classify one draft span against the provenance masks, the gate verdicts, and
/// gold. Pure — interval and mask arithmetic only, so it is unit-testable with
/// synthetic fixtures. `firing` is the occupancy per-sample diagnostic with its
/// sample rate, absent when the detector did not run.
fn classify_span(
    span: Interval,
    blocks: &segment::FusionBlocks,
    verdicts: &[segment::SpanVerdict],
    firing: Option<(&[segment::OccupancySample], f64)>,
    gold: &[Interval],
) -> SpanSupport {
    // Blocks overlapping the padded span: block b covers [b·block_ms, (b+1)·block_ms).
    let bm = blocks.block_ms.max(1);
    let lo = ((span.start_ms.max(0)) / bm) as usize;
    let hi = (span.end_ms.max(0) as u64).div_ceil(bm as u64) as usize;
    let hi = hi.min(blocks.motion.len()).min(blocks.occupancy.len());
    let lo = lo.min(hi);
    let total = (hi - lo).max(1);
    let motion_blocks = blocks.motion[lo..hi].iter().filter(|&&m| m).count();
    let occupancy_blocks = blocks.occupancy[lo..hi].iter().filter(|&&o| o).count();
    let provenance = match (motion_blocks > 0, occupancy_blocks > 0) {
        (true, false) => Provenance::MotionOnly,
        (false, true) => Provenance::OccupancyOnly,
        (true, true) => Provenance::Mixed,
        // A rally span exists because some block fired, so this only happens on
        // degenerate fixtures; read it as the motion-proposes fallback.
        (false, false) => Provenance::MotionOnly,
    };

    // Audio verdict: the rally-producing gate verdicts sit at the raw unpadded
    // block boundaries inside the padded span, so match by overlap. A merged span
    // covering several raw spans is confirmed when any of them was.
    let audio_confirmed = verdicts.iter().any(|v| {
        matches!(v.verdict, segment::GateVerdict::Kept)
            && span.overlaps(&Interval {
                start_ms: v.start_ms,
                end_ms: v.end_ms,
            })
    });

    SpanSupport {
        span,
        provenance,
        motion_frac: motion_blocks as f64 / total as f64,
        occupancy_frac: occupancy_blocks as f64 / total as f64,
        audio_confirmed,
        fired_fraction: firing.and_then(|(f, fps)| fired_fraction(span, f, fps)),
        is_fp: !gold.iter().any(|g| span.overlaps(g)),
    }
}

/// Fraction of the detector samples inside `span` that fired the occupancy rule,
/// or `None` when the track holds no sample there (span beyond the track's end,
/// or an empty track).
fn fired_fraction(span: Interval, firing: &[segment::OccupancySample], fps: f64) -> Option<f64> {
    if fps <= 0.0 {
        return None;
    }
    let lo = (((span.start_ms.max(0) as f64) / 1000.0) * fps) as usize;
    let hi = ((((span.end_ms.max(0) as f64) / 1000.0) * fps) as usize).min(firing.len());
    let lo = lo.min(hi);
    let window = &firing[lo..hi];
    if window.is_empty() {
        return None;
    }
    Some(window.iter().filter(|s| s.fired).count() as f64 / window.len() as f64)
}

/// One traced recording: every draft span classified, plus what the pure scorer
/// needs to re-score a suppressed draft (gold and duration).
struct TracedRecording {
    spans: Vec<SpanSupport>,
    gold: Vec<Interval>,
    duration_ms: i64,
}

/// The measured price of one candidate suppression rule over the corpus: spans
/// matching `suppress` are dropped and the remainder re-scored, so the recall
/// cost is exact, not assumed. Returns
/// `(fp_removed, fp_per_hour_remaining, gold_lost)` where `gold_lost` is the
/// misses *added* relative to the unsuppressed draft.
fn price_rule(
    recs: &[TracedRecording],
    suppress: impl Fn(&SpanSupport) -> bool,
) -> (usize, f64, usize) {
    let mut baseline_misses = 0usize;
    let mut misses = 0usize;
    let mut fp_removed = 0usize;
    let mut fp_remaining = 0usize;
    let mut total_ms = 0i64;
    for rec in recs {
        let full: Vec<Interval> = rec.spans.iter().map(|s| s.span).collect();
        let kept: Vec<Interval> = rec
            .spans
            .iter()
            .filter(|s| !suppress(s))
            .map(|s| s.span)
            .collect();
        baseline_misses += score(&full, &rec.gold, rec.duration_ms).misses;
        let s = score(&kept, &rec.gold, rec.duration_ms);
        misses += s.misses;
        fp_remaining += s.false_positives;
        fp_removed += rec.spans.iter().filter(|s| s.is_fp && suppress(s)).count();
        total_ms += rec.duration_ms;
    }
    (
        fp_removed,
        per_hour(fp_remaining, total_ms),
        misses - baseline_misses,
    )
}

/// Classify every draft span on the gold corpus by signal support, print the
/// FP/TP composition per recording and aggregated, and price the candidate
/// suppression rules (issue #92). Same extraction shell as a scored run; all
/// judgement lives in [`classify_span`], [`fired_fraction`], and [`price_rule`].
fn fp_trace_and_report(recordings: &[CorpusRecording]) {
    let p = segment::Params::default();
    let mut traced: Vec<TracedRecording> = Vec::new();
    for rec in recordings {
        if rec.segment_state != "ready" || !rec.hand_corrected {
            continue;
        }
        let (samples, motion, occupancy) = match extract_tracks(&rec.abs_path) {
            Ok(t) => t,
            Err(e) => {
                println!("ERR   {}  ({e})", rec.rel_path);
                continue;
            }
        };
        let seg = segment::segment_with(
            &samples,
            media::SEGMENT_SAMPLE_RATE,
            &motion,
            occupancy.as_ref(),
            &p,
        );
        let blocks = segment::fusion_blocks(
            &samples,
            media::SEGMENT_SAMPLE_RATE,
            &motion,
            occupancy.as_ref(),
            &p,
        );
        let firing = occupancy
            .as_ref()
            .map(|o| (segment::occupancy_firing(o, &p), o.fps));
        let duration_ms = rec.duration_ms.filter(|&d| d > 0).unwrap_or(
            (samples.len() as f64 / media::SEGMENT_SAMPLE_RATE as f64 * 1000.0) as i64,
        );
        let spans: Vec<SpanSupport> = seg
            .rallies
            .iter()
            .map(|r| {
                classify_span(
                    Interval {
                        start_ms: r.start_ms,
                        end_ms: r.end_ms,
                    },
                    &blocks,
                    &seg.verdicts,
                    firing.as_ref().map(|(f, fps)| (f.as_slice(), *fps)),
                    &rec.gold,
                )
            })
            .collect();

        println!("FPTRACE {}  ({} draft spans)", rec.rel_path, spans.len());
        print_class_table(&spans, duration_ms, "        ");
        traced.push(TracedRecording {
            spans,
            gold: rec.gold.clone(),
            duration_ms,
        });
    }
    if traced.is_empty() {
        println!("no gold recordings to trace");
        return;
    }

    let all_spans: Vec<SpanSupport> = traced.iter().flat_map(|t| t.spans.iter().cloned()).collect();
    let total_ms: i64 = traced.iter().map(|t| t.duration_ms).sum();
    println!(
        "\n=== aggregate over {} recording(s), {} draft spans, {:.2} h ===",
        traced.len(),
        all_spans.len(),
        total_ms as f64 / MS_PER_HOUR
    );
    print_class_table(&all_spans, total_ms, "");

    // The obvious candidate rules (issue #92), priced exactly. "no occupancy
    // firing" is the strictest reading: not one detector sample fired in the span
    // (an absent track also reads as unsupported).
    println!("\n=== candidate suppression rules (measured ceilings) ===");
    type Rule = Box<dyn Fn(&SpanSupport) -> bool>;
    let rules: [(&str, Rule); 4] = [
        (
            "suppress motion-only & audio-unconfirmed",
            Box::new(|s: &SpanSupport| s.provenance == Provenance::MotionOnly && !s.audio_confirmed),
        ),
        (
            "suppress motion-only & unconfirmed & zero occupancy firing",
            Box::new(|s: &SpanSupport| {
                s.provenance == Provenance::MotionOnly
                    && !s.audio_confirmed
                    && s.fired_fraction.is_none_or(|f| f == 0.0)
            }),
        ),
        (
            "suppress every audio-unconfirmed span",
            Box::new(|s: &SpanSupport| !s.audio_confirmed),
        ),
        (
            "suppress every motion-only span",
            Box::new(|s: &SpanSupport| s.provenance == Provenance::MotionOnly),
        ),
    ];
    for (name, rule) in &rules {
        let (fp_removed, fph_remaining, gold_lost) = price_rule(&traced, rule);
        println!(
            "{name:<62} →  -{fp_removed} FP  →  {fph_remaining:.1} FP/h remaining | {gold_lost} gold rally(ies) lost"
        );
    }
}

/// Print the six-class FP/TP composition table for one span set. `indent`
/// prefixes every row (the per-recording block indents, the aggregate does not).
/// The fired% medians are split FP vs TP — whether false positives fire the
/// occupancy rule less densely than real rallies is exactly the demotion
/// question the issue #92 design session weighs.
fn print_class_table(spans: &[SpanSupport], duration_ms: i64, indent: &str) {
    println!(
        "{indent}{:<28} {:>4} {:>9} {:>5}   med fired% FP | TP",
        "class", "FP", "FP/h", "TP"
    );
    for class in SUPPORT_CLASSES {
        let members: Vec<&SpanSupport> = spans.iter().filter(|s| s.class() == class).collect();
        if members.is_empty() {
            continue;
        }
        let fp = members.iter().filter(|s| s.is_fp).count();
        let tp = members.len() - fp;
        let fired_cell = |want_fp: bool| {
            let mut fired: Vec<i64> = members
                .iter()
                .filter(|s| s.is_fp == want_fp)
                .filter_map(|s| s.fired_fraction)
                .map(|f| (f * 100.0).round() as i64)
                .collect();
            fired.sort_unstable();
            match median(&fired) {
                Some(m) => format!("{m:>3.0}%"),
                None => " n/a".to_string(),
            }
        };
        println!(
            "{indent}{:<28} {:>4} {:>9.2} {:>5}   {} | {}",
            class_label(class),
            fp,
            per_hour(fp, duration_ms),
            tp,
            fired_cell(true),
            fired_cell(false)
        );
    }
}

// ── Headroom measurement (issue #93) ──────────────────────────────────────────
//
// The candidate-direction measurement behind the v5 residuals (22 misses, 1.50 s
// median boundary error): how much a velocity-keyed firing rule and/or a 5 fps
// detector rate would buy, measured observationally on the gold corpus in the
// style of the #85 checkpoint. Nothing here alters the draft: the shipped rule,
// `DETECT_FPS`, and every `Params` default stay untouched; the 5 fps tracks live
// only in a scratch cache the app never reads.

/// Velocity thresholds priced for the candidate rule, in frame fractions per
/// second (a per-sample box-center step of `v / fps`). The shipped movement bool
/// demands a step above `occupancy_static_frac` = 0.02/sample — 0.06/s at 3 fps,
/// 0.10/s at 5 fps — so the grid brackets it on both sides.
const HEADROOM_SPEEDS: &[f64] = &[0.05, 0.10, 0.15, 0.25, 0.40, 0.60];

/// The scratch re-extraction rate: the top of ADR 0015's decided 2–5 fps envelope.
const HEADROOM_FPS: u32 = 5;

/// A gold rally at or under this duration is "short" — the miss tail the #91
/// trace identified (3–5 s rallies whose firing never separates from chatter).
const SHORT_RALLY_MS: i64 = 5_000;

/// A candidate per-sample firing rule, priced against the shipped one.
#[derive(Clone, Copy)]
enum FiringRule {
    /// The shipped v5 rule: size structure + the movement bool.
    Current,
    /// Size structure + box-center velocity at or above this (fractions/second).
    Velocity(f64),
}

impl FiringRule {
    /// Row label for the headroom tables.
    fn label(self) -> String {
        match self {
            FiringRule::Current => "current (step>0.02/sample)".to_string(),
            FiringRule::Velocity(v) => format!("velocity >= {v:.2}/s"),
        }
    }
}

/// Every rule the headroom mode prices: the shipped one, then the velocity grid.
fn headroom_rules() -> Vec<FiringRule> {
    std::iter::once(FiringRule::Current)
        .chain(HEADROOM_SPEEDS.iter().map(|&v| FiringRule::Velocity(v)))
        .collect()
}

/// Per-sample fired flags for one rule over one track. `Current` reads the
/// shipped [`segment::occupancy_firing`]; `Velocity` keys the same filtered
/// samples on step magnitude × fps through the kinematics sibling seam
/// ([`segment::occupancy_kinematics`]) — never a parallel reimplementation.
fn rule_flags(occ: &segment::OccupancyTrack, p: &segment::Params, rule: FiringRule) -> Vec<bool> {
    match rule {
        FiringRule::Current => segment::occupancy_firing(occ, p)
            .iter()
            .map(|s| s.fired)
            .collect(),
        FiringRule::Velocity(v) => segment::occupancy_kinematics(occ, p)
            .iter()
            .map(|k| k.size_structure && k.max_step.is_some_and(|d| d * occ.fps >= v))
            .collect(),
    }
}

/// Whether sample `i` of a track at `fps` lands inside any gold rally.
fn sample_in_gold(i: usize, fps: f64, gold: &[Interval]) -> bool {
    let t_ms = (i as f64 / fps * 1000.0) as i64;
    gold.iter().any(|g| t_ms >= g.start_ms && t_ms < g.end_ms)
}

/// Firing density in the window of `half` samples each side of `center`, judged
/// on the samples the window actually holds (clipped at the track edges) — the
/// window shape `occupancy_blocks` judges, at sample rather than block centers.
fn window_density(fired: &[bool], center: usize, half: usize) -> f64 {
    if fired.is_empty() {
        return 0.0;
    }
    let lo = center.saturating_sub(half);
    let hi = (center + half + 1).min(fired.len());
    fired[lo..hi].iter().filter(|&&f| f).count() as f64 / (hi - lo).max(1) as f64
}

/// Peak windowed density over window centers in the sample range `[lo, hi)`.
fn peak_density(fired: &[bool], lo: usize, hi: usize, half: usize) -> f64 {
    (lo..hi.min(fired.len()))
        .map(|c| window_density(fired, c, half))
        .fold(0.0, f64::max)
}

/// Sample-index range `[lo, hi)` an interval covers on a track of `len` samples.
fn sample_range(iv: Interval, fps: f64, len: usize) -> (usize, usize) {
    let lo = ((iv.start_ms.max(0) as f64 / 1000.0) * fps) as usize;
    let hi = ((((iv.end_ms.max(0)) as f64 / 1000.0) * fps) as usize).min(len);
    (lo.min(hi), hi)
}

/// Half-window in samples for [`segment::Params::occupancy_window_ms`] at `fps` —
/// the same width the trace mode probes peak density with.
fn half_window(p: &segment::Params, fps: f64) -> usize {
    let w_samples = ((p.occupancy_window_ms as f64 / 1000.0) * fps).round() as usize;
    (w_samples / 2).max(1)
}

/// One rule's pooled #85-style measurements over the corpus at one rate.
#[derive(Default)]
struct RuleStats {
    in_gold_fired: usize,
    in_gold_total: usize,
    out_fired: usize,
    out_total: usize,
    /// Peak windowed density per short (≤ [`SHORT_RALLY_MS`]) gold rally.
    short_peaks: Vec<f64>,
    /// Out-of-gold sample-centered windows at or above the default proposal
    /// density — the FP pressure a lowered bar would admit.
    out_windows_proposing: usize,
    out_windows: usize,
    /// Missed windows (v5 defaults, 3 fps) whose peak density clears the default
    /// proposal threshold under this rule.
    missed_proposing: usize,
    missed_total: usize,
}

impl RuleStats {
    /// Pool one recording's flags into the stats. `missed` is the fixed missed-window
    /// list (from the v5-default 3 fps draft), probed on this track's flags.
    fn add(
        &mut self,
        fired: &[bool],
        fps: f64,
        gold: &[Interval],
        missed: &[Interval],
        p: &segment::Params,
    ) {
        let half = half_window(p, fps);
        for (i, &f) in fired.iter().enumerate() {
            if sample_in_gold(i, fps, gold) {
                self.in_gold_total += 1;
                self.in_gold_fired += f as usize;
            } else {
                self.out_total += 1;
                self.out_fired += f as usize;
                self.out_windows += 1;
                self.out_windows_proposing +=
                    (window_density(fired, i, half) >= p.occupancy_density) as usize;
            }
        }
        for g in gold {
            if g.end_ms - g.start_ms <= SHORT_RALLY_MS {
                let (lo, hi) = sample_range(*g, fps, fired.len());
                self.short_peaks.push(peak_density(fired, lo, hi, half));
            }
        }
        for m in missed {
            let (lo, hi) = sample_range(*m, fps, fired.len());
            self.missed_total += 1;
            self.missed_proposing +=
                (peak_density(fired, lo, hi, half) >= p.occupancy_density) as usize;
        }
    }
}

/// One gold recording's tracks for the headroom measurement: the 3 fps occupancy
/// the app extracts, plus the 5 fps scratch track, plus the missed-gold windows of
/// the v5-default 3 fps draft (the fixed 22 the report probes).
struct HeadroomRecording {
    rel_path: String,
    samples: Vec<f32>,
    motion: segment::MotionTrack,
    occ3: Option<segment::OccupancyTrack>,
    occ5: Option<segment::OccupancyTrack>,
    gold: Vec<Interval>,
    duration_ms: i64,
    missed: Vec<Interval>,
}

/// Load a recording's scratch detection track at `fps` and `score_floor`,
/// extracting and caching it under `scratch_dir` on first use. Keyed by relative
/// path + rate (+ floor when lowered, so the shipped-floor files headroom already
/// cached stay valid); the app's own tracks and Analyses are never touched
/// (issue #93). Degrades to `None` exactly as [`extract_occupancy`] does. Returns
/// the score-carrying [`crate::detect::DetectionTrack`] — the presence measurement
/// bands it by floor before crossing into the pure seam.
fn scratch_track(
    scratch_dir: &std::path::Path,
    rel_path: &str,
    abs_path: &str,
    fps: u32,
    score_floor: f32,
) -> Option<crate::detect::DetectionTrack> {
    let stem = rel_path.replace(['/', '\\'], "_");
    let file = if score_floor == crate::detect::SCORE_THRESHOLD {
        scratch_dir.join(format!("{stem}@{fps}fps.json"))
    } else {
        scratch_dir.join(format!("{stem}@{fps}fps@f{score_floor:.2}.json"))
    };
    if let Ok(text) = std::fs::read_to_string(&file) {
        match serde_json::from_str::<crate::detect::DetectionTrack>(&text) {
            Ok(track) => return Some(track),
            Err(e) => eprintln!("      (scratch cache unreadable, re-extracting — {e})"),
        }
    }
    let track = crate::detect::detections_at_or_none(abs_path, fps, score_floor, |why| {
        eprintln!("      ({fps} fps occupancy unavailable — {why})");
    })?;
    if let Ok(json) = serde_json::to_string(&track) {
        if let Err(e) =
            std::fs::create_dir_all(scratch_dir).and_then(|()| std::fs::write(&file, json))
        {
            eprintln!("      (could not cache scratch track at {} — {e})", file.display());
        }
    }
    Some(track)
}

/// Score the corpus at the default parameters through the pure seam with each
/// recording's occupancy track chosen by `occ` — the misses / FP/h / median line
/// for one rate.
fn score_at_rate<'a>(
    recs: &'a [HeadroomRecording],
    p: &segment::Params,
    occ: impl Fn(&'a HeadroomRecording) -> Option<&'a segment::OccupancyTrack>,
) -> (usize, usize, f64, Option<f64>) {
    let mut misses = 0usize;
    let mut fps_count = 0usize;
    let mut total_ms = 0i64;
    let mut errors: Vec<i64> = Vec::new();
    for r in recs {
        let seg = segment::segment_with(&r.samples, media::SEGMENT_SAMPLE_RATE, &r.motion, occ(r), p);
        let draft: Vec<Interval> = seg
            .rallies
            .iter()
            .map(|x| Interval { start_ms: x.start_ms, end_ms: x.end_ms })
            .collect();
        let s = score(&draft, &r.gold, r.duration_ms);
        misses += s.misses;
        fps_count += s.false_positives;
        total_ms += r.duration_ms;
        errors.extend(s.boundary_errors_ms);
    }
    (
        misses,
        fps_count,
        per_hour(fps_count, total_ms),
        median(&errors).map(|ms| ms / 1000.0),
    )
}

/// The headroom report (issue #93): the #85-style rule table at 3 fps and 5 fps,
/// the v5-defaults score at both rates, and the per-missed-window peak densities.
fn headroom_and_report(recordings: &[CorpusRecording], scratch_dir: &std::path::Path) {
    let p = segment::Params::default();
    let mut recs: Vec<HeadroomRecording> = Vec::new();
    for rec in recordings {
        if rec.segment_state != "ready" || !rec.hand_corrected {
            println!("SKIP  {}  (not gold)", rec.rel_path);
            continue;
        }
        let (samples, motion, occ3) = match extract_tracks(&rec.abs_path) {
            Ok(t) => t,
            Err(e) => {
                println!("ERR   {}  ({e})", rec.rel_path);
                continue;
            }
        };
        let occ5 = scratch_track(
            scratch_dir,
            &rec.rel_path,
            &rec.abs_path,
            HEADROOM_FPS,
            crate::detect::SCORE_THRESHOLD,
        )
        .map(|t| t.to_occupancy_track());
        let duration_ms = rec.duration_ms.filter(|&d| d > 0).unwrap_or(
            (samples.len() as f64 / media::SEGMENT_SAMPLE_RATE as f64 * 1000.0) as i64,
        );
        // The fixed missed-window list every rule is probed on: what the shipped
        // draft misses at the defaults on the app's own 3 fps tracks.
        let seg = segment::segment_with(&samples, media::SEGMENT_SAMPLE_RATE, &motion, occ3.as_ref(), &p);
        let draft: Vec<Interval> = seg
            .rallies
            .iter()
            .map(|r| Interval { start_ms: r.start_ms, end_ms: r.end_ms })
            .collect();
        let missed: Vec<Interval> = rec
            .gold
            .iter()
            .filter(|g| !draft.iter().any(|d| d.overlaps(g)))
            .copied()
            .collect();
        println!("CACHE {}  ({} missed at v5 defaults)", rec.rel_path, missed.len());
        recs.push(HeadroomRecording {
            rel_path: rec.rel_path.clone(),
            samples,
            motion,
            occ3,
            occ5,
            gold: rec.gold.clone(),
            duration_ms,
            missed,
        });
    }
    if recs.is_empty() {
        println!("no gold recordings to measure");
        return;
    }

    // v5-defaults score at each rate — the "did 5 fps alone move the bar" line.
    println!("\n=== v5-defaults score by detector rate ===");
    let (m3, f3, fh3, med3) = score_at_rate(&recs, &p, |r| r.occ3.as_ref());
    println!("3 fps (app tracks)     : miss {m3} | fp {f3} ({fh3:.1}/h) | med {}", fmt_med(med3));
    let (m5, f5, fh5, med5) = score_at_rate(&recs, &p, |r| r.occ5.as_ref());
    println!("{HEADROOM_FPS} fps (scratch tracks) : miss {m5} | fp {f5} ({fh5:.1}/h) | med {}", fmt_med(med5));

    // The #85-style rule table, per rate.
    type OccOf = fn(&HeadroomRecording) -> Option<&segment::OccupancyTrack>;
    let rates: [(&str, OccOf); 2] = [
        ("3 fps (app tracks)", |r| r.occ3.as_ref()),
        ("5 fps (scratch tracks)", |r| r.occ5.as_ref()),
    ];
    for (rate_label, occ_of) in rates {
        println!("\n=== firing-rule headroom @ {rate_label} ===");
        println!(
            "{:<28} {:>8} {:>8} {:>14} {:>10} {:>12} {:>10}",
            "rule", "in-gold%", "out%", "short med-peak", "short>=dens", "out win>=dens", "missed>=dens"
        );
        for rule in headroom_rules() {
            let mut stats = RuleStats::default();
            for r in &recs {
                let Some(occ) = occ_of(r) else { continue };
                let fired = rule_flags(occ, &p, rule);
                stats.add(&fired, occ.fps, &r.gold, &r.missed, &p);
            }
            let pct = |n: usize, d: usize| 100.0 * n as f64 / d.max(1) as f64;
            let mut peaks: Vec<i64> = stats.short_peaks.iter().map(|&d| (d * 100.0).round() as i64).collect();
            peaks.sort_unstable();
            let short_ge = stats.short_peaks.iter().filter(|&&d| d >= p.occupancy_density).count();
            println!(
                "{:<28} {:>7.0}% {:>7.1}% {:>14} {:>10} {:>11.1}% {:>10}",
                rule.label(),
                pct(stats.in_gold_fired, stats.in_gold_total),
                pct(stats.out_fired, stats.out_total),
                median(&peaks).map_or("n/a".to_string(), |m| format!("{:.2}", m / 100.0)),
                format!("{short_ge}/{}", stats.short_peaks.len()),
                pct(stats.out_windows_proposing, stats.out_windows),
                format!("{}/{}", stats.missed_proposing, stats.missed_total),
            );
        }
    }

    // Every missed window individually: peak windowed density per rule and rate —
    // "would the density judge (>= 0.50) open here" read per residual miss.
    println!("\n=== missed gold windows: peak windowed density per rule ===");
    let rules = headroom_rules();
    let header: Vec<String> = rules.iter().map(|r| match r {
        FiringRule::Current => "cur".to_string(),
        FiringRule::Velocity(v) => format!("v{v:.2}"),
    }).collect();
    println!("{:<44} {:>6}   3fps: {}   {HEADROOM_FPS}fps: {}", "window", "dur", header.join(" "), header.join(" "));
    for r in &recs {
        for m in &r.missed {
            let mut cells: Vec<String> = Vec::new();
            for occ in [r.occ3.as_ref(), r.occ5.as_ref()] {
                for rule in &rules {
                    cells.push(match occ {
                        None => " n/a".to_string(),
                        Some(o) => {
                            let fired = rule_flags(o, &p, *rule);
                            let (lo, hi) = sample_range(*m, o.fps, fired.len());
                            format!("{:.2}", peak_density(&fired, lo, hi, half_window(&p, o.fps)))
                        }
                    });
                }
            }
            let (head, tail) = cells.split_at(rules.len());
            println!(
                "{:<44} {:>5.1}s   {}         {}",
                format!("{} {:.1}s–{:.1}s", r.rel_path, m.start_ms as f64 / 1000.0, m.end_ms as f64 / 1000.0),
                (m.end_ms - m.start_ms) as f64 / 1000.0,
                head.join(" "),
                tail.join(" ")
            );
        }
    }
}

// ── Boundary-edge provenance (issue #93) ──────────────────────────────────────
//
// The third diagnostic: split v5's per-boundary errors by which signal placed
// each draft edge, to confirm or refute that occupancy-edged spans own the
// 1.50 s median — and, for the occupancy-edged boundaries, whether motion
// activity exists near the true gold edge at all (the number that decides if
// motion-trimming is viable). Purely observational.

/// Neighbourhood radii (ms) probed around a gold edge for nearby motion activity.
const TRIM_RADII_MS: [i64; 3] = [500, 1000, 2000];

/// One corpus's boundary errors split by the proposer that placed each draft
/// edge, plus every occupancy-edged boundary's gold edge time for the
/// trim-viability probe.
#[derive(Default)]
struct BoundarySplit {
    motion_edged_errs: Vec<i64>,
    occupancy_edged_errs: Vec<i64>,
    /// `(gold_edge_ms, error_ms)` per occupancy-edged boundary.
    occupancy_gold_edges: Vec<i64>,
}

/// The first and last candidate-active block (motion OR occupancy) covered by a
/// padded draft span — the blocks that placed its edges — or `None` when no block
/// in the span is active (degenerate fixtures only).
fn edge_blocks(span: Interval, blocks: &segment::FusionBlocks) -> Option<(usize, usize)> {
    let bm = blocks.block_ms.max(1);
    let lo = (span.start_ms.max(0) / bm) as usize;
    let hi = (span.end_ms.max(0) as u64).div_ceil(bm as u64) as usize;
    let hi = hi.min(blocks.motion.len()).min(blocks.occupancy.len());
    let lo = lo.min(hi);
    let active = |b: usize| blocks.motion[b] || blocks.occupancy[b];
    let first = (lo..hi).find(|&b| active(b))?;
    let last = (lo..hi).rev().find(|&b| active(b))?;
    Some((first, last))
}

/// Attribute each matched gold rally's two boundary errors to the signal that
/// placed the corresponding draft edge. Motion active on the edge block means
/// motion placed it (motion edges spans wherever it fires; the union only adds
/// occupancy blocks beyond motion's) — otherwise the edge is occupancy's, at
/// block grain, and its gold edge feeds the trim-viability probe. Pure.
fn split_boundary_errors(
    draft: &[Interval],
    gold: &[Interval],
    blocks: &segment::FusionBlocks,
    split: &mut BoundarySplit,
) {
    for g in gold {
        let Some(d) = best_match(g, draft) else { continue };
        let Some((first, last)) = edge_blocks(*d, blocks) else { continue };
        for (edge_block, gold_edge, err) in [
            (first, g.start_ms, (g.start_ms - d.start_ms).abs()),
            (last, g.end_ms, (g.end_ms - d.end_ms).abs()),
        ] {
            if blocks.motion[edge_block] {
                split.motion_edged_errs.push(err);
            } else {
                split.occupancy_edged_errs.push(err);
                split.occupancy_gold_edges.push(gold_edge);
            }
        }
    }
}

/// Whether any motion-active block's center lies within `radius_ms` of `t_ms` —
/// "is there a motion boundary to trim this occupancy edge to".
fn motion_near(blocks: &segment::FusionBlocks, t_ms: i64, radius_ms: i64) -> bool {
    let bm = blocks.block_ms.max(1);
    blocks
        .motion
        .iter()
        .enumerate()
        .any(|(b, &m)| m && ((b as i64 * bm + bm / 2) - t_ms).abs() <= radius_ms)
}

/// The edge-provenance report (issue #93): per-boundary errors split by the
/// signal that placed each draft edge, plus the motion-near-gold-edge viability
/// counts for the occupancy-edged boundaries.
fn edges_and_report(recordings: &[CorpusRecording]) {
    let p = segment::Params::default();
    let mut split = BoundarySplit::default();
    let mut viability = [0usize; TRIM_RADII_MS.len()];
    let mut all_errors: Vec<i64> = Vec::new();
    for rec in recordings {
        if rec.segment_state != "ready" || !rec.hand_corrected {
            continue;
        }
        let (samples, motion, occupancy) = match extract_tracks(&rec.abs_path) {
            Ok(t) => t,
            Err(e) => {
                println!("ERR   {}  ({e})", rec.rel_path);
                continue;
            }
        };
        let seg = segment::segment_with(&samples, media::SEGMENT_SAMPLE_RATE, &motion, occupancy.as_ref(), &p);
        let blocks = segment::fusion_blocks(&samples, media::SEGMENT_SAMPLE_RATE, &motion, occupancy.as_ref(), &p);
        let draft: Vec<Interval> = seg
            .rallies
            .iter()
            .map(|r| Interval { start_ms: r.start_ms, end_ms: r.end_ms })
            .collect();
        let before = split.occupancy_gold_edges.len();
        split_boundary_errors(&draft, &rec.gold, &blocks, &mut split);
        for &edge in &split.occupancy_gold_edges[before..] {
            for (i, &radius) in TRIM_RADII_MS.iter().enumerate() {
                viability[i] += motion_near(&blocks, edge, radius) as usize;
            }
        }
        all_errors.extend(score(&draft, &rec.gold, rec.duration_ms.unwrap_or(0)).boundary_errors_ms);
        println!("EDGES {}", rec.rel_path);
    }
    let n_motion = split.motion_edged_errs.len();
    let n_occ = split.occupancy_edged_errs.len();
    if n_motion + n_occ == 0 {
        println!("no matched boundaries to split");
        return;
    }
    println!("\n=== boundary errors by edge provenance (v5 defaults) ===");
    println!(
        "all boundaries        : {:>4}  median {}",
        all_errors.len(),
        fmt_med(median(&all_errors).map(|ms| ms / 1000.0))
    );
    println!(
        "motion-edged          : {:>4}  median {}",
        n_motion,
        fmt_med(median(&split.motion_edged_errs).map(|ms| ms / 1000.0))
    );
    println!(
        "occupancy-edged       : {:>4}  median {}",
        n_occ,
        fmt_med(median(&split.occupancy_edged_errs).map(|ms| ms / 1000.0))
    );
    println!("\n=== motion activity near the gold edge (occupancy-edged boundaries) ===");
    for (i, &radius) in TRIM_RADII_MS.iter().enumerate() {
        println!(
            "within ±{:>4} ms       : {:>4} / {}  ({:.0}%)",
            radius,
            viability[i],
            n_occ,
            100.0 * viability[i] as f64 / n_occ.max(1) as f64
        );
    }
}

// ── Presence measurement (issue #93, round 2) ─────────────────────────────────
//
// The presence-headroom measurement behind the miss tail: the headroom round
// showed the 22 residual missed windows are presence-starved — two structured,
// moving boxes don't appear often enough at any per-sample rule or rate — and
// presence has two untested sources. **Sub-threshold detections**: the shipped
// extraction keeps only boxes scoring ≥ 0.35 and the cached track type drops the
// score, so whether the detector emits usable lower-confidence boxes in those
// windows is invisible today. **Continuity**: a box seen at one sample vanishing
// from the next says nothing about the player leaving — per-sample independence
// throws presence away between samples. Both are simulated observationally on
// scratch tracks (score floor 0.10, both rates) and measured through the real
// filter pipeline (`segment::occupancy_kinematics` / `occupancy_firing`) — never
// a parallel reimplementation, and never feeding back into the draft.

/// The lowered score floor scratch tracks are extracted at. Every reported floor
/// is a *filter* over this one extraction — sound because a lower decode floor
/// only appends boxes (see `detect::decode`'s banding invariant).
const PRESENCE_FLOOR: f32 = 0.10;

/// Cumulative admission floors the presence tables report: the shipped floor,
/// then adding the 0.20–0.35 band, then also the 0.10–0.20 band.
const PRESENCE_FLOORS: [f32; 3] = [crate::detect::SCORE_THRESHOLD, 0.20, PRESENCE_FLOOR];

/// Bridge depths priced for continuity-bridged presence, in samples (0 = the
/// per-sample status quo). At 3 fps, k = 3 persists a vanished box for one second.
const PRESENCE_KS: [usize; 4] = [0, 1, 2, 3];

/// A carried box is *re-detected* (its persistence chain ends) when a real box of
/// the current sample overlaps it by at least this IoU — the standard association
/// bar of overlap trackers, generous enough that a player's between-sample step
/// at 3–5 fps keeps matching their own box.
const BRIDGE_MATCH_IOU: f64 = 0.3;

/// Intersection-over-union of two normalized boxes; `0.0` when disjoint.
fn det_iou(a: &segment::DetBox, b: &segment::DetBox) -> f64 {
    let ix = (a.x + a.w).min(b.x + b.w) - a.x.max(b.x);
    let iy = (a.y + a.h).min(b.y + b.h) - a.y.max(b.y);
    let inter = ix.max(0.0) * iy.max(0.0);
    let union = a.w * a.h + b.w * b.h - inter;
    if union > 0.0 {
        inter / union
    } else {
        0.0
    }
}

/// Band a score-carrying detection track down to the boxes clearing `floor`,
/// crossing into the pure seam's [`segment::OccupancyTrack`]. At the shipped
/// floor this reproduces the app's own track exactly (the decode banding
/// invariant); lower floors admit the sub-threshold bands (issue #93).
fn occupancy_at_floor(track: &crate::detect::DetectionTrack, floor: f32) -> segment::OccupancyTrack {
    segment::OccupancyTrack {
        fps: track.fps,
        samples: track
            .samples
            .iter()
            .map(|frame| {
                frame
                    .iter()
                    .filter(|b| b.score >= floor)
                    .map(|b| segment::DetBox {
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

/// Simulate temporal continuity over a track: every box persists (unchanged
/// geometry) into up to `k` following samples, dying early when a real detection
/// re-finds it ([`BRIDGE_MATCH_IOU`]). `k = 0` returns the track as-is. The
/// augmented track then flows through the real filter pipeline, so a persisted
/// box is still subject to staticness and the area cap; a persisted box never
/// moves, so the firing rule's movement demand keeps resting on real detections.
fn bridge_track(occ: &segment::OccupancyTrack, k: usize) -> segment::OccupancyTrack {
    // (box, samples since it was really seen); age 0 = seen this sample.
    let mut carried: Vec<(segment::DetBox, usize)> = Vec::new();
    let samples = occ
        .samples
        .iter()
        .map(|real| {
            let mut augmented = real.clone();
            let mut next: Vec<(segment::DetBox, usize)> = Vec::new();
            for &(b, age) in &carried {
                if age + 1 > k || real.iter().any(|r| det_iou(r, &b) >= BRIDGE_MATCH_IOU) {
                    continue; // chain exhausted, or the player was re-detected
                }
                augmented.push(b);
                next.push((b, age + 1));
            }
            for &r in real {
                next.push((r, 0));
            }
            carried = next;
            augmented
        })
        .collect();
    segment::OccupancyTrack {
        fps: occ.fps,
        samples,
    }
}

/// Fraction of the samples in `[lo, hi)` whose flag is set — per-missed-window
/// presence. `0.0` for an empty range (window beyond the track).
fn flag_fraction(flags: &[bool], lo: usize, hi: usize) -> f64 {
    let hi = hi.min(flags.len());
    let lo = lo.min(hi);
    if lo == hi {
        return 0.0;
    }
    flags[lo..hi].iter().filter(|&&f| f).count() as f64 / (hi - lo) as f64
}

/// One gold recording's material for the presence measurement: the fixed missed
/// windows of the v5-default draft on the app's own tracks, the app-path 3 fps
/// occupancy (for the banding consistency line), and the score-carrying low-floor
/// scratch tracks at both rates.
struct PresenceRecording {
    rel_path: String,
    gold: Vec<Interval>,
    missed: Vec<Interval>,
    occ3: Option<segment::OccupancyTrack>,
    low3: Option<crate::detect::DetectionTrack>,
    low5: Option<crate::detect::DetectionTrack>,
}

/// One simulated variant's pooled measurements: presence (the two-structured-boxes
/// flag) and the shipped firing rule, both on the same modified track, plus the
/// per-missed-window numbers in corpus order.
struct VariantStats {
    presence: RuleStats,
    fired: RuleStats,
    /// Per missed window: fraction of its samples with two-structured-boxes presence.
    window_presence: Vec<f64>,
    /// Per missed window: peak windowed density of the shipped rule's firing.
    window_fired_peak: Vec<f64>,
}

/// Measure one (floor, k) variant across the corpus at one rate. `low_of` picks
/// the rate's low-floor track. Presence and firing are read through the real
/// pipeline seams on the banded, bridged track.
fn measure_variant(
    recs: &[PresenceRecording],
    low_of: impl Fn(&PresenceRecording) -> Option<&crate::detect::DetectionTrack>,
    floor: f32,
    k: usize,
    p: &segment::Params,
) -> VariantStats {
    let mut vs = VariantStats {
        presence: RuleStats::default(),
        fired: RuleStats::default(),
        window_presence: Vec::new(),
        window_fired_peak: Vec::new(),
    };
    for rec in recs {
        let Some(low) = low_of(rec) else {
            // Keep the per-window vectors aligned with the corpus order.
            vs.window_presence.extend(rec.missed.iter().map(|_| 0.0));
            vs.window_fired_peak.extend(rec.missed.iter().map(|_| 0.0));
            continue;
        };
        let track = bridge_track(&occupancy_at_floor(low, floor), k);
        let presence: Vec<bool> = segment::occupancy_kinematics(&track, p)
            .iter()
            .map(|s| s.size_structure)
            .collect();
        let fired: Vec<bool> = segment::occupancy_firing(&track, p)
            .iter()
            .map(|s| s.fired)
            .collect();
        vs.presence.add(&presence, track.fps, &rec.gold, &rec.missed, p);
        vs.fired.add(&fired, track.fps, &rec.gold, &rec.missed, p);
        let half = half_window(p, track.fps);
        for m in &rec.missed {
            let (lo, hi) = sample_range(*m, track.fps, presence.len());
            vs.window_presence.push(flag_fraction(&presence, lo, hi));
            vs.window_fired_peak.push(peak_density(&fired, lo, hi, half));
        }
    }
    vs
}

/// Row label for an admission floor.
fn floor_label(floor: f32) -> String {
    if floor == crate::detect::SCORE_THRESHOLD {
        format!(">={floor:.2} (shipped)")
    } else {
        format!(">={floor:.2}")
    }
}

/// The presence-headroom report (issue #93, round 2): per-score-band presence,
/// continuity-bridged presence, and the both-sides separation table, at both
/// rates, all on scratch tracks banded from one low-floor extraction per rate.
fn presence_and_report(recordings: &[CorpusRecording], scratch_dir: &std::path::Path) {
    let p = segment::Params::default();
    let mut recs: Vec<PresenceRecording> = Vec::new();
    for rec in recordings {
        if rec.segment_state != "ready" || !rec.hand_corrected {
            println!("SKIP  {}  (not gold)", rec.rel_path);
            continue;
        }
        let (samples, motion, occ3) = match extract_tracks(&rec.abs_path) {
            Ok(t) => t,
            Err(e) => {
                println!("ERR   {}  ({e})", rec.rel_path);
                continue;
            }
        };
        // The fixed missed-window list (v5 defaults on the app's own tracks).
        let seg = segment::segment_with(&samples, media::SEGMENT_SAMPLE_RATE, &motion, occ3.as_ref(), &p);
        let draft: Vec<Interval> = seg
            .rallies
            .iter()
            .map(|r| Interval { start_ms: r.start_ms, end_ms: r.end_ms })
            .collect();
        let missed: Vec<Interval> = rec
            .gold
            .iter()
            .filter(|g| !draft.iter().any(|d| d.overlaps(g)))
            .copied()
            .collect();
        let low3 = scratch_track(scratch_dir, &rec.rel_path, &rec.abs_path, crate::detect::DETECT_FPS, PRESENCE_FLOOR);
        let low5 = scratch_track(scratch_dir, &rec.rel_path, &rec.abs_path, HEADROOM_FPS, PRESENCE_FLOOR);
        println!("CACHE {}  ({} missed at v5 defaults)", rec.rel_path, missed.len());
        recs.push(PresenceRecording {
            rel_path: rec.rel_path.clone(),
            gold: rec.gold.clone(),
            missed,
            occ3,
            low3,
            low5,
        });
    }
    if recs.is_empty() {
        println!("no gold recordings to measure");
        return;
    }

    // Banding consistency: the shipped-floor band of the low-floor 3 fps scratch
    // track must reproduce the app-path track (same decode, same NMS survivors).
    let count_structure = |occ: &segment::OccupancyTrack| {
        segment::occupancy_kinematics(occ, &p)
            .iter()
            .filter(|s| s.size_structure)
            .count()
    };
    let (mut app_structure, mut banded_structure) = (0usize, 0usize);
    for r in &recs {
        if let (Some(occ3), Some(low3)) = (&r.occ3, &r.low3) {
            app_structure += count_structure(occ3);
            banded_structure += count_structure(&occupancy_at_floor(low3, crate::detect::SCORE_THRESHOLD));
        }
    }
    println!(
        "\nbanding consistency: structured samples on app 3 fps tracks {app_structure} vs scratch@shipped-floor {banded_structure}{}",
        if app_structure == banded_structure { "  (identical)" } else { "  (MISMATCH)" }
    );

    type LowOf = fn(&PresenceRecording) -> Option<&crate::detect::DetectionTrack>;
    let rates: [(&str, LowOf); 2] = [
        ("3 fps", |r| r.low3.as_ref()),
        ("5 fps", |r| r.low5.as_ref()),
    ];

    // Measure every (rate, floor, k) variant once; the tables below are views.
    let mut variants: Vec<Vec<Vec<VariantStats>>> = Vec::new(); // [rate][floor][k]
    for (_, low_of) in rates {
        let mut by_floor = Vec::new();
        for &floor in &PRESENCE_FLOORS {
            let mut by_k = Vec::new();
            for &k in &PRESENCE_KS {
                by_k.push(measure_variant(&recs, low_of, floor, k, &p));
            }
            by_floor.push(by_k);
        }
        variants.push(by_floor);
    }

    let pct = |n: usize, d: usize| 100.0 * n as f64 / d.max(1) as f64;
    let stat_row = |s: &RuleStats| {
        format!(
            "{:>8.1}% {:>7.1}% {:>13} {:>13.1}%",
            pct(s.in_gold_fired, s.in_gold_total),
            pct(s.out_fired, s.out_total),
            format!("{}/{}", s.missed_proposing, s.missed_total),
            pct(s.out_windows_proposing, s.out_windows),
        )
    };

    // 1. Sub-threshold presence by admission floor (k = 0).
    println!("\n=== two-structured-boxes presence by admission floor (plausibility-filtered) ===");
    for (ri, (rate_label, _)) in rates.iter().enumerate() {
        println!(
            "@ {rate_label}: {:<18} {:>9} {:>8} {:>13} {:>14}",
            "floor", "in-gold%", "out%", "missed>=dens", "out win>=dens"
        );
        for (fi, &floor) in PRESENCE_FLOORS.iter().enumerate() {
            let vs = &variants[ri][fi][0];
            println!("         {:<18} {}", floor_label(floor), stat_row(&vs.presence));
        }
    }

    // 2. Continuity-bridged presence at the shipped floor.
    println!("\n=== continuity-bridged presence (shipped floor, k = persisted samples) ===");
    for (ri, (rate_label, _)) in rates.iter().enumerate() {
        println!(
            "@ {rate_label}: {:<18} {:>9} {:>8} {:>13} {:>14}",
            "bridge", "in-gold%", "out%", "missed>=dens", "out win>=dens"
        );
        for (ki, &k) in PRESENCE_KS.iter().enumerate() {
            let vs = &variants[ri][0][ki];
            println!("         {:<18} {}", format!("k = {k}"), stat_row(&vs.presence));
        }
    }

    // 3. Separation: the shipped firing rule on every modified track — what the
    //    windowed-density judge would actually see, both sides of the trade.
    println!("\n=== separation: shipped firing rule on banded + bridged tracks ===");
    println!(
        "{:<8} {:<18} {:<6} {:>9} {:>8} {:>13} {:>14}",
        "rate", "floor", "k", "in-gold%", "out%", "missed>=dens", "out win>=dens"
    );
    for (ri, (rate_label, _)) in rates.iter().enumerate() {
        for (fi, &floor) in PRESENCE_FLOORS.iter().enumerate() {
            for (ki, &k) in PRESENCE_KS.iter().enumerate() {
                let vs = &variants[ri][fi][ki];
                println!(
                    "{:<8} {:<18} {:<6} {}",
                    rate_label,
                    floor_label(floor),
                    k,
                    stat_row(&vs.fired)
                );
            }
        }
    }

    // 4. Every missed window individually. Presence fraction by floor (k = 0),
    //    presence fraction by bridge depth (shipped floor), and the shipped rule's
    //    peak windowed density on the strongest single and combined variants.
    let windows: Vec<(String, Interval)> = recs
        .iter()
        .flat_map(|r| r.missed.iter().map(|m| (r.rel_path.clone(), *m)))
        .collect();
    let cell = |v: f64| format!("{v:.2}");
    println!("\n=== per-missed-window presence fraction by floor (k = 0) ===");
    println!(
        "{:<44} {:>6}   3fps: {:>5} {:>5} {:>5}   5fps: {:>5} {:>5} {:>5}",
        "window", "dur", ".35", ".20", ".10", ".35", ".20", ".10"
    );
    for (wi, (rel, m)) in windows.iter().enumerate() {
        let mut cells: Vec<String> = Vec::new();
        for by_floor in &variants {
            for by_k in by_floor {
                cells.push(cell(by_k[0].window_presence[wi]));
            }
        }
        println!(
            "{:<44} {:>5.1}s   {:>11} {:>5} {:>5}   {:>11} {:>5} {:>5}",
            format!("{rel} {:.1}s–{:.1}s", m.start_ms as f64 / 1000.0, m.end_ms as f64 / 1000.0),
            (m.end_ms - m.start_ms) as f64 / 1000.0,
            cells[0], cells[1], cells[2], cells[3], cells[4], cells[5],
        );
    }
    println!("\n=== per-missed-window presence fraction by bridge depth (shipped floor) ===");
    println!(
        "{:<44} {:>6}   3fps: {:>5} {:>5} {:>5} {:>5}   5fps: {:>5} {:>5} {:>5} {:>5}",
        "window", "dur", "k0", "k1", "k2", "k3", "k0", "k1", "k2", "k3"
    );
    for (wi, (rel, m)) in windows.iter().enumerate() {
        let mut cells: Vec<String> = Vec::new();
        for by_floor in &variants {
            for vs in &by_floor[0] {
                cells.push(cell(vs.window_presence[wi]));
            }
        }
        println!(
            "{:<44} {:>5.1}s   {:>11} {:>5} {:>5} {:>5}   {:>11} {:>5} {:>5} {:>5}",
            format!("{rel} {:.1}s–{:.1}s", m.start_ms as f64 / 1000.0, m.end_ms as f64 / 1000.0),
            (m.end_ms - m.start_ms) as f64 / 1000.0,
            cells[0], cells[1], cells[2], cells[3], cells[4], cells[5], cells[6], cells[7],
        );
    }
    println!("\n=== per-missed-window peak fired density: baseline vs mechanisms vs combined ===");
    println!(
        "{:<44} {:>6}   3fps: {:>5} {:>5} {:>5} {:>5}   5fps: {:>5} {:>5} {:>5} {:>5}",
        "window", "dur", "base", "f.10", "k2", "f+k2", "base", "f.10", "k2", "f+k2"
    );
    // (floor index, k index) per printed column: baseline, floor-only, bridge-only,
    // combined — the k = 2 depth as the representative bridge.
    let picks: [(usize, usize); 4] = [(0, 0), (2, 0), (0, 2), (2, 2)];
    for (wi, (rel, m)) in windows.iter().enumerate() {
        let mut cells: Vec<String> = Vec::new();
        for by_floor in &variants {
            for &(fi, ki) in &picks {
                cells.push(cell(by_floor[fi][ki].window_fired_peak[wi]));
            }
        }
        println!(
            "{:<44} {:>5.1}s   {:>11} {:>5} {:>5} {:>5}   {:>11} {:>5} {:>5} {:>5}",
            format!("{rel} {:.1}s–{:.1}s", m.start_ms as f64 / 1000.0, m.end_ms as f64 / 1000.0),
            (m.end_ms - m.start_ms) as f64 / 1000.0,
            cells[0], cells[1], cells[2], cells[3], cells[4], cells[5], cells[6], cells[7],
        );
    }
}

// ── Span-score separation (issue #93, round 3) ────────────────────────────────
//
// The span-level measurement behind the miss tail: rounds 1–2 refuted every
// per-sample admission mechanism (velocity re-keying, rate, sub-threshold boxes,
// sample persistence) — each hits the same monotone trade before the ≤ 5 bar —
// and left one direction standing: the rally/chatter difference living in
// *temporal structure* over a span rather than in any per-sample predicate. This
// mode measures that premise without building the design: integrated
// continuity+movement(+audio) features are read over three *known* populations
// (all gold rally extents, duration-matched out-of-gold windows, v5's FP spans)
// and hand-constructed, interpretable score functions are swept over a declared
// grid, reporting both sides of every operating point against the declared
// green/red/gray kill criterion. No candidate generator, no draft change —
// features flow through the real seams (`occupancy_kinematics` /
// `occupancy_firing` on bridged tracks, the segmenter's own onset mask) and
// nothing feeds back.

/// Bridge depths span features are read at: 0 = the per-sample status quo, then
/// the shallow depths round 2 measured as the asymmetric regime (~3:1 in-gold at
/// k ≤ 2). Deeper bridging already decayed there and is not re-priced.
const SPAN_KS: [usize; 3] = [0, 1, 2];

/// Index of the representative bridge depth (k = 2) in [`SPAN_KS`] — the
/// best-behaved depth from round 2, which the score functions key on.
const SPAN_K2: usize = 2;

/// Declared threshold grid for the presence-keyed score functions (bridged
/// structured-presence fraction over the span).
const SPAN_PRESENCE_GRID: &[f64] = &[0.50, 0.60, 0.70, 0.80, 0.90];

/// Declared threshold grid for the fired-fraction score functions (shipped rule
/// on the k = 2 bridged track — co-presence *with movement*, integrated).
const SPAN_FIRED_GRID: &[f64] = &[0.20, 0.30, 0.40, 0.50, 0.60];

/// Declared grid for the sustained-run score function, in seconds of continuous
/// firing at k = 2.
const SPAN_SUSTAINED_GRID: &[f64] = &[0.5, 1.0, 1.5, 2.0, 2.5, 3.0];

/// Dropout cap for the gap-capped presence function: the longest unstructured
/// run a span may contain, in seconds. In-rally dropouts measured short in round
/// 2 (bridgeable at k ≤ 2 ≈ 0.7 s at 3 fps); 2 s is comfortably above them and
/// well under a between-rally absence.
const SPAN_MAX_GAP_S: f64 = 2.0;

/// Vision floor inside the audio-rescue clause: audio may only *add* admission
/// to a span vision already half-supports (ADR 0015 / round 3 brief: rescue-only,
/// never sinking or conjuring), so the rescue arm still demands this much bridged
/// structured presence.
const SPAN_RESCUE_PRESENCE: f64 = 0.50;

/// One recording's precomputed per-sample and per-block signal views, read once
/// through the real seams so every span feature is a slice over them.
struct SpanTrackViews {
    fps: f64,
    /// Structured-presence flags per bridge depth ([`SPAN_KS`] order), via
    /// [`segment::occupancy_kinematics`] on the bridged track.
    structure: Vec<Vec<bool>>,
    /// Shipped-rule fired flags per bridge depth, via
    /// [`segment::occupancy_firing`] on the bridged track.
    fired: Vec<Vec<bool>>,
    /// Per-sample nearest-match center speed (frame fractions/second) on the
    /// unbridged track; `0.0` where no step is measurable.
    speed: Vec<f64>,
}

/// Read every view of one occupancy track the span features need.
fn span_track_views(occ: &segment::OccupancyTrack, p: &segment::Params) -> SpanTrackViews {
    let mut structure = Vec::with_capacity(SPAN_KS.len());
    let mut fired = Vec::with_capacity(SPAN_KS.len());
    for &k in &SPAN_KS {
        let bridged = bridge_track(occ, k);
        structure.push(
            segment::occupancy_kinematics(&bridged, p)
                .iter()
                .map(|s| s.size_structure)
                .collect(),
        );
        fired.push(
            segment::occupancy_firing(&bridged, p)
                .iter()
                .map(|s| s.fired)
                .collect(),
        );
    }
    let speed = segment::occupancy_kinematics(occ, p)
        .iter()
        .map(|s| s.max_step.unwrap_or(0.0) * occ.fps)
        .collect();
    SpanTrackViews {
        fps: occ.fps,
        structure,
        fired,
        speed,
    }
}

/// Integrated features of one span (issue #93 round 3) — everything the
/// hand-constructed score functions read.
#[derive(Debug, Clone, PartialEq)]
struct SpanFeatures {
    /// Structured-presence fraction at each bridge depth ([`SPAN_KS`] order).
    presence: Vec<f64>,
    /// Shipped-rule fired fraction at each bridge depth.
    fired: Vec<f64>,
    /// Longest / mean unstructured run inside the span at k = 0, in seconds — the
    /// dropout-run structure. A span holding no detector samples reads as one
    /// full-length gap.
    max_gap_s: f64,
    mean_gap_s: f64,
    /// Nearest-match center speed summed over the span's samples, normalized per
    /// sample — the movement integral, in frame fractions/second.
    mean_speed: f64,
    /// Longest continuous fired run at k = 2, in seconds — sustained co-presence
    /// with movement.
    sustained_s: f64,
    /// Shuttle-hit onsets in the blocks the span overlaps, from the segmenter's
    /// own onset mask, and normalized per second.
    onset_count: usize,
    onset_per_s: f64,
}

/// Lengths of the maximal `false` runs within `[lo, hi)` of `flags` — the
/// dropout runs of a presence track.
fn false_runs(flags: &[bool], lo: usize, hi: usize) -> Vec<usize> {
    let hi = hi.min(flags.len());
    let lo = lo.min(hi);
    let mut runs = Vec::new();
    let mut cur = 0usize;
    for &f in &flags[lo..hi] {
        if f {
            if cur > 0 {
                runs.push(cur);
                cur = 0;
            }
        } else {
            cur += 1;
        }
    }
    if cur > 0 {
        runs.push(cur);
    }
    runs
}

/// Length of the longest `true` run within `[lo, hi)` of `flags`.
fn longest_true_run(flags: &[bool], lo: usize, hi: usize) -> usize {
    let hi = hi.min(flags.len());
    let lo = lo.min(hi);
    let (mut best, mut cur) = (0usize, 0usize);
    for &f in &flags[lo..hi] {
        cur = if f { cur + 1 } else { 0 };
        best = best.max(cur);
    }
    best
}

/// Onsets in the blocks a span overlaps — the same block addressing
/// [`classify_span`] uses, over the segmenter's own onset mask.
fn onsets_in_span(span: Interval, ob: &segment::OnsetBlocks) -> usize {
    let bm = ob.block_ms.max(1);
    let lo = (span.start_ms.max(0) / bm) as usize;
    let hi = (span.end_ms.max(0) as u64).div_ceil(bm as u64) as usize;
    let hi = hi.min(ob.onsets.len());
    let lo = lo.min(hi);
    ob.onsets[lo..hi].iter().sum()
}

/// Read one span's integrated features off the precomputed views. Pure — slice
/// arithmetic over the flag tracks and the onset mask, so it is unit-testable
/// with synthetic views. `views` is `None` when the detector did not run: vision
/// features read as empty (zero presence, one full-length gap), audio still
/// counts.
fn span_features(
    span: Interval,
    views: Option<&SpanTrackViews>,
    onsets: &segment::OnsetBlocks,
) -> SpanFeatures {
    let span_secs = (span.end_ms - span.start_ms).max(0) as f64 / 1000.0;
    let onset_count = onsets_in_span(span, onsets);
    let onset_per_s = if span_secs > 0.0 {
        onset_count as f64 / span_secs
    } else {
        0.0
    };
    let Some(v) = views else {
        return SpanFeatures {
            presence: vec![0.0; SPAN_KS.len()],
            fired: vec![0.0; SPAN_KS.len()],
            max_gap_s: span_secs,
            mean_gap_s: span_secs,
            mean_speed: 0.0,
            sustained_s: 0.0,
            onset_count,
            onset_per_s,
        };
    };
    let (lo, hi) = sample_range(span, v.fps, v.structure[0].len());
    let presence: Vec<f64> = v.structure.iter().map(|s| flag_fraction(s, lo, hi)).collect();
    let fired: Vec<f64> = v.fired.iter().map(|f| flag_fraction(f, lo, hi)).collect();
    let (max_gap_s, mean_gap_s) = if lo == hi {
        (span_secs, span_secs)
    } else {
        let gaps = false_runs(&v.structure[0], lo, hi);
        let max = gaps.iter().copied().max().unwrap_or(0) as f64 / v.fps;
        let mean = if gaps.is_empty() {
            0.0
        } else {
            gaps.iter().sum::<usize>() as f64 / gaps.len() as f64 / v.fps
        };
        (max, mean)
    };
    let mean_speed = if lo == hi {
        0.0
    } else {
        v.speed[lo..hi.min(v.speed.len())].iter().sum::<f64>() / (hi - lo) as f64
    };
    SpanFeatures {
        presence,
        fired,
        max_gap_s,
        mean_gap_s,
        mean_speed,
        sustained_s: longest_true_run(&v.fired[SPAN_K2], lo, hi) as f64 / v.fps,
        onset_count,
        onset_per_s,
    }
}

/// Out-of-gold windows duration-matched to one recording's gold rallies (the
/// stated matching method): gold rallies are walked in start order; each
/// contributes one window of its own duration, placed at the first fit across
/// the recording's out-of-gold gaps starting from a round-robin gap cursor
/// (each gap fills front-to-back, the search wraps across all gaps once). So the
/// duration distribution is matched exactly, windows never overlap gold or each
/// other, and placement is deterministic. Returns the windows and how many
/// rallies fit nowhere (skipped, reported — never silently dropped).
fn matched_out_windows(gold: &[Interval], duration_ms: i64) -> (Vec<Interval>, usize) {
    let mut sorted: Vec<Interval> = gold.to_vec();
    sorted.sort_by_key(|g| g.start_ms);
    let mut gaps: Vec<(i64, i64)> = Vec::new();
    let mut t = 0i64;
    for g in &sorted {
        if g.start_ms > t {
            gaps.push((t, g.start_ms));
        }
        t = t.max(g.end_ms);
    }
    if duration_ms > t {
        gaps.push((t, duration_ms));
    }
    // Next free position per gap; a placed window advances its gap's cursor.
    let mut free: Vec<i64> = gaps.iter().map(|&(s, _)| s).collect();
    let mut out = Vec::new();
    let mut skipped = 0usize;
    let mut start_gap = 0usize;
    for g in &sorted {
        let d = g.end_ms - g.start_ms;
        let mut placed = false;
        for step in 0..gaps.len() {
            let i = (start_gap + step) % gaps.len();
            if free[i] + d <= gaps[i].1 {
                out.push(Interval {
                    start_ms: free[i],
                    end_ms: free[i] + d,
                });
                free[i] += d;
                start_gap = (i + 1) % gaps.len();
                placed = true;
                break;
            }
        }
        if !placed {
            skipped += 1;
        }
    }
    (out, skipped)
}

/// One measured span in a population: its features, whether v5's own density
/// judge would clear it (the same-population reference line), and — gold only —
/// whether it is one of the fixed v5 misses.
struct SpanRow {
    span: Interval,
    rec: usize,
    feats: SpanFeatures,
    v5_clears: bool,
    missed22: bool,
}

/// One hand-constructed score function at one threshold point of its declared
/// grid: an interpretable admit predicate over the span features.
struct ScorePoint {
    label: String,
    admits: Box<dyn Fn(&SpanFeatures) -> bool>,
}

/// The declared score-function grid (issue #93 round 3). Every function is a
/// stated, interpretable formula — the integrity rule: only these can justify a
/// green verdict, since the corpus doubles as tuning data (ADR 0015).
fn span_score_points(p: &segment::Params) -> Vec<ScorePoint> {
    let mut pts: Vec<ScorePoint> = Vec::new();
    for &t in SPAN_PRESENCE_GRID {
        pts.push(ScorePoint {
            label: format!("presence@k2 >= {t:.2}"),
            admits: Box::new(move |f: &SpanFeatures| f.presence[SPAN_K2] >= t),
        });
    }
    for &t in SPAN_FIRED_GRID {
        pts.push(ScorePoint {
            label: format!("fired@k2 >= {t:.2}"),
            admits: Box::new(move |f: &SpanFeatures| f.fired[SPAN_K2] >= t),
        });
    }
    for &s in SPAN_SUSTAINED_GRID {
        pts.push(ScorePoint {
            label: format!("sustained@k2 >= {s:.1}s"),
            admits: Box::new(move |f: &SpanFeatures| f.sustained_s >= s),
        });
    }
    for &t in SPAN_PRESENCE_GRID {
        pts.push(ScorePoint {
            label: format!("presence@k2 >= {t:.2} & max-gap <= {SPAN_MAX_GAP_S:.0}s"),
            admits: Box::new(move |f: &SpanFeatures| {
                f.presence[SPAN_K2] >= t && f.max_gap_s <= SPAN_MAX_GAP_S
            }),
        });
    }
    // Audio rescue (rescue-only): onset density at the v5 confirm bar may admit a
    // span the fired threshold alone rejects, but only over the vision floor —
    // audio never sinks a span vision admits, and never conjures one alone.
    let confirm = p.confirm_onsets_per_sec;
    for &t in SPAN_FIRED_GRID {
        pts.push(ScorePoint {
            label: format!(
                "fired@k2 >= {t:.2} | (presence@k2 >= {SPAN_RESCUE_PRESENCE:.2} & onsets/s >= {confirm:.1})"
            ),
            admits: Box::new(move |f: &SpanFeatures| {
                f.fired[SPAN_K2] >= t
                    || (f.presence[SPAN_K2] >= SPAN_RESCUE_PRESENCE && f.onset_per_s >= confirm)
            }),
        });
    }
    pts
}

/// Median of a feature over a population, as a table cell.
fn med_cell(values: impl Iterator<Item = f64>) -> String {
    let mut ms: Vec<i64> = values.map(|v| (v * 1000.0).round() as i64).collect();
    ms.sort_unstable();
    match median(&ms) {
        Some(m) => format!("{:.2}", m / 1000.0),
        None => "n/a".to_string(),
    }
}

/// One population's feature-table block: every span's row, then the medians.
fn print_span_population(title: &str, rows: &[SpanRow], recs: &[String]) {
    println!("\n=== span features: {title} ({} spans) ===", rows.len());
    if rows.is_empty() {
        return;
    }
    println!(
        "{:<44} {:>6} {:>6} {:>6} {:>6} {:>8} {:>8} {:>8} {:>8} {:>8} {:>7} {:>7}",
        "span", "dur", "prs k0", "k1", "k2", "fired k2", "max-gap", "mean-gap", "sustain", "speed/s", "ons/s", "v5"
    );
    for r in rows {
        println!(
            "{:<44} {:>5.1}s {:>6.2} {:>6.2} {:>6.2} {:>8.2} {:>7.1}s {:>7.1}s {:>7.1}s {:>8.3} {:>7.2} {:>6}{}",
            format!(
                "{} {:.1}s–{:.1}s",
                recs[r.rec],
                r.span.start_ms as f64 / 1000.0,
                r.span.end_ms as f64 / 1000.0
            ),
            (r.span.end_ms - r.span.start_ms) as f64 / 1000.0,
            r.feats.presence[0],
            r.feats.presence[1],
            r.feats.presence[SPAN_K2],
            r.feats.fired[SPAN_K2],
            r.feats.max_gap_s,
            r.feats.mean_gap_s,
            r.feats.sustained_s,
            r.feats.mean_speed,
            r.feats.onset_per_s,
            if r.v5_clears { "yes" } else { "no" },
            if r.missed22 { "  *MISSED*" } else { "" }
        );
    }
    println!(
        "{:<44} {:>6} {:>6} {:>6} {:>6} {:>8} {:>8} {:>8} {:>8} {:>8} {:>7}",
        "medians",
        "",
        med_cell(rows.iter().map(|r| r.feats.presence[0])),
        med_cell(rows.iter().map(|r| r.feats.presence[1])),
        med_cell(rows.iter().map(|r| r.feats.presence[SPAN_K2])),
        med_cell(rows.iter().map(|r| r.feats.fired[SPAN_K2])),
        med_cell(rows.iter().map(|r| r.feats.max_gap_s)),
        med_cell(rows.iter().map(|r| r.feats.mean_gap_s)),
        med_cell(rows.iter().map(|r| r.feats.sustained_s)),
        med_cell(rows.iter().map(|r| r.feats.mean_speed)),
        med_cell(rows.iter().map(|r| r.feats.onset_per_s)),
    );
}

/// The span-score separation report (issue #93 round 3): span features over the
/// three populations, the declared score grid's both-sides table, the audio
/// columns per recording, and the zone verdict against the declared criterion.
fn spans_and_report(recordings: &[CorpusRecording]) {
    let p = segment::Params::default();
    let mut rec_names: Vec<String> = Vec::new();
    let mut gold_rows: Vec<SpanRow> = Vec::new();
    let mut out_rows: Vec<SpanRow> = Vec::new();
    let mut fp_rows: Vec<SpanRow> = Vec::new();
    let mut total_skipped = 0usize;
    for rec in recordings {
        if rec.segment_state != "ready" || !rec.hand_corrected {
            println!("SKIP  {}  (not gold)", rec.rel_path);
            continue;
        }
        let (samples, motion, occupancy) = match extract_tracks(&rec.abs_path) {
            Ok(t) => t,
            Err(e) => {
                println!("ERR   {}  ({e})", rec.rel_path);
                continue;
            }
        };
        let seg = segment::segment_with(&samples, media::SEGMENT_SAMPLE_RATE, &motion, occupancy.as_ref(), &p);
        let draft: Vec<Interval> = seg
            .rallies
            .iter()
            .map(|r| Interval { start_ms: r.start_ms, end_ms: r.end_ms })
            .collect();
        let duration_ms = rec.duration_ms.filter(|&d| d > 0).unwrap_or(
            (samples.len() as f64 / media::SEGMENT_SAMPLE_RATE as f64 * 1000.0) as i64,
        );
        let onsets = segment::onset_blocks(&samples, media::SEGMENT_SAMPLE_RATE, &p);
        let views = occupancy.as_ref().map(|occ| span_track_views(occ, &p));
        let (out_windows, skipped) = matched_out_windows(&rec.gold, duration_ms);
        total_skipped += skipped;
        let ri = rec_names.len();
        rec_names.push(rec.rel_path.clone());

        // The v5 same-population reference: would the shipped density judge
        // propose inside this span (peak fired density at k = 0 over the bar)?
        let v5_clears = |iv: Interval| {
            views.as_ref().is_some_and(|v| {
                let (lo, hi) = sample_range(iv, v.fps, v.fired[0].len());
                peak_density(&v.fired[0], lo, hi, half_window(&p, v.fps)) >= p.occupancy_density
            })
        };
        let row = |iv: Interval, missed22: bool| SpanRow {
            span: iv,
            rec: ri,
            feats: span_features(iv, views.as_ref(), &onsets),
            v5_clears: v5_clears(iv),
            missed22,
        };
        for g in &rec.gold {
            let missed = !draft.iter().any(|d| d.overlaps(g));
            gold_rows.push(row(*g, missed));
        }
        out_rows.extend(out_windows.iter().map(|&w| row(w, false)));
        fp_rows.extend(
            draft
                .iter()
                .filter(|d| !rec.gold.iter().any(|g| d.overlaps(g)))
                .map(|&d| row(d, false)),
        );
        println!(
            "CACHE {}  ({} gold, {} matched-out{}, {} fp spans)",
            rec.rel_path,
            rec.gold.len(),
            out_windows.len(),
            if skipped > 0 { format!(" ({skipped} skipped: no gap fits)") } else { String::new() },
            fp_rows.iter().filter(|r| r.rec == ri).count(),
        );
    }
    if gold_rows.is_empty() {
        println!("no gold recordings to measure");
        return;
    }
    if total_skipped > 0 {
        println!("(duration matching skipped {total_skipped} window(s) corpus-wide — no out-of-gold gap fits)");
    }

    let missed_total = gold_rows.iter().filter(|r| r.missed22).count();
    print_span_population("all gold rallies (missed at v5 defaults flagged *)", &gold_rows, &rec_names);
    print_span_population("duration-matched out-of-gold windows", &out_rows, &rec_names);
    print_span_population("v5 false-positive spans (advisory for #92)", &fp_rows, &rec_names);

    // Audio onset features per recording — history says a minority of recordings
    // poison the onset baseline (neighbour-court bleed, ADR 0015), so the audio
    // column is never pooled across recordings.
    println!("\n=== audio onset density per recording (median onsets/s) ===");
    println!(
        "{:<44} {:>10} {:>12} {:>10}",
        "recording", "gold", "matched-out", "fp spans"
    );
    for (ri, name) in rec_names.iter().enumerate() {
        let of = |rows: &[SpanRow]| {
            med_cell(rows.iter().filter(|r| r.rec == ri).map(|r| r.feats.onset_per_s))
        };
        println!(
            "{:<44} {:>10} {:>12} {:>10}",
            name,
            of(&gold_rows),
            of(&out_rows),
            of(&fp_rows)
        );
    }

    // The both-sides separation table over the declared grid.
    let v5_gold_missed = gold_rows.iter().filter(|r| !r.v5_clears).count();
    let v5_out_clear = out_rows.iter().filter(|r| r.v5_clears).count();
    let v5_fp_clear = fp_rows.iter().filter(|r| r.v5_clears).count();
    let frac = |n: usize, d: usize| 100.0 * n as f64 / d.max(1) as f64;
    let v5_out_frac = frac(v5_out_clear, out_rows.len());
    println!("\n=== span-score separation over the declared grid (both sides) ===");
    println!(
        "{:<64} {:>14} {:>10} {:>12} {:>10}",
        "score point", "gold below", "(of 22)", "out clear%", "fp clear%"
    );
    println!(
        "{:<64} {:>14} {:>10} {:>11.1}% {:>9.1}%   <- same-population v5 reference",
        "v5 density judge (peak fired@k0 >= 0.50)",
        format!("{v5_gold_missed}/{}", gold_rows.len()),
        format!("{}/{missed_total}", gold_rows.iter().filter(|r| r.missed22 && !r.v5_clears).count()),
        v5_out_frac,
        frac(v5_fp_clear, fp_rows.len()),
    );
    struct SepRow {
        label: String,
        gold_below: usize,
        missed_below: usize,
        out_frac: f64,
        fp_frac: f64,
    }
    let mut sep: Vec<SepRow> = Vec::new();
    for pt in span_score_points(&p) {
        let gold_below = gold_rows.iter().filter(|r| !(pt.admits)(&r.feats)).count();
        let missed_below = gold_rows
            .iter()
            .filter(|r| r.missed22 && !(pt.admits)(&r.feats))
            .count();
        let out_clear = out_rows.iter().filter(|r| (pt.admits)(&r.feats)).count();
        let fp_clear = fp_rows.iter().filter(|r| (pt.admits)(&r.feats)).count();
        let row = SepRow {
            label: pt.label,
            gold_below,
            missed_below,
            out_frac: frac(out_clear, out_rows.len()),
            fp_frac: frac(fp_clear, fp_rows.len()),
        };
        println!(
            "{:<64} {:>14} {:>10} {:>11.1}% {:>9.1}%",
            row.label,
            format!("{}/{}", row.gold_below, gold_rows.len()),
            format!("{}/{missed_total}", row.missed_below),
            row.out_frac,
            row.fp_frac,
        );
        sep.push(row);
    }

    // The zone verdict against the criterion declared on the issue before these
    // numbers existed. Green: ≤ 5 of the gold rallies below some point at
    // out-clearance no worse than v5's own (same-population reference). Gray: the
    // frontier moves but doesn't reach. Red: unmoved. A reading, not a decision.
    println!("\n=== zone verdict (criterion declared on issue #93) ===");
    println!(
        "reference: v5 clears {v5_out_frac:.1}% of the matched out-of-gold windows (round-1 sample-centered figure: ~29%)"
    );
    let green: Vec<&SepRow> = sep
        .iter()
        .filter(|r| r.gold_below <= 5 && r.out_frac <= v5_out_frac)
        .collect();
    let gray: Vec<&SepRow> = sep
        .iter()
        .filter(|r| {
            (r.gold_below <= 10 && r.out_frac <= v5_out_frac)
                || (r.gold_below <= 5 && r.out_frac <= 35.0)
        })
        .collect();
    let zone = if !green.is_empty() {
        "GREEN — proceed to the span-scoring design session"
    } else if !gray.is_empty() {
        "GRAY — frontier moved but did not reach; design and spec conversation share the agenda"
    } else {
        "RED — frontier unmoved; the spec-level conversation is the pre-declared fallback"
    };
    println!("zone: {zone}");
    let qualifying = if green.is_empty() { &gray } else { &green };
    for r in qualifying {
        println!(
            "  qualifying point: {}  ({} gold below, {:.1}% out clear)",
            r.label, r.gold_below, r.out_frac
        );
    }
    // The closest operating points either way, for the report's frontier read.
    let mut frontier: Vec<&SepRow> = sep.iter().collect();
    frontier.sort_by(|a, b| a.gold_below.cmp(&b.gold_below).then(a.out_frac.total_cmp(&b.out_frac)));
    println!("\nbest operating points (by gold below, then out clearance):");
    for r in frontier.iter().take(8) {
        println!(
            "  {:<62} {:>3} gold below ({} of 22) | {:>5.1}% out | {:>5.1}% fp",
            r.label, r.gold_below, r.missed_below, r.out_frac, r.fp_frac
        );
    }
}

/// One recording's line block: the headline path, then the three numbers with the
/// counts behind them.
fn print_recording(rel_path: &str, s: &Score) {
    println!("SCORE {rel_path}");
    println!(
        "        gold {} | draft {} | matched {}",
        s.gold_count, s.draft_count, s.matched
    );
    println!("        missed rallies        : {}", s.misses);
    println!(
        "        false positives       : {}  ({:.2} / hour)",
        s.false_positives, s.false_positives_per_hour
    );
    match s.median_boundary_error_secs {
        Some(secs) => println!("        median boundary error : {secs:.2} s"),
        None => println!("        median boundary error : n/a"),
    }
}

/// Parsed command-line options for [`run`].
struct Options {
    db: Option<String>,
    library: Option<String>,
    filter: Option<String>,
    sweep: bool,
    trace: bool,
    fp_trace: bool,
    headroom: bool,
    edges: bool,
    presence: bool,
    spans: bool,
    scratch: Option<String>,
    help: bool,
}

impl Options {
    /// Parse the harness's arguments. Unknown flags and a missing flag value are
    /// hard errors so a typo cannot silently score the wrong thing.
    fn parse(args: &[String]) -> Result<Options, String> {
        let mut opts = Options {
            db: None,
            library: None,
            filter: None,
            sweep: false,
            trace: false,
            fp_trace: false,
            headroom: false,
            edges: false,
            presence: false,
            spans: false,
            scratch: None,
            help: false,
        };
        let mut it = args.iter();
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "-h" | "--help" => opts.help = true,
                "--sweep" => opts.sweep = true,
                "--trace" => opts.trace = true,
                "--fp-trace" => opts.fp_trace = true,
                "--headroom" => opts.headroom = true,
                "--edges" => opts.edges = true,
                "--presence" => opts.presence = true,
                "--spans" => opts.spans = true,
                "--scratch" => {
                    opts.scratch = Some(it.next().ok_or("--scratch needs a directory")?.clone());
                }
                "--db" => {
                    opts.db = Some(it.next().ok_or("--db needs a path")?.clone());
                }
                "--library" => {
                    let k = it.next().ok_or("--library needs local|shared")?.clone();
                    if k != "local" && k != "shared" {
                        return Err(format!("--library must be local or shared, not {k}"));
                    }
                    opts.library = Some(k);
                }
                other if other.starts_with("--") => {
                    return Err(format!("unknown flag {other}"));
                }
                other => {
                    if opts.filter.replace(other.to_string()).is_some() {
                        return Err("give at most one recording filter".to_string());
                    }
                }
            }
        }
        Ok(opts)
    }
}

fn print_usage() {
    println!(
        "eval-harness — referee the segmenter against hand-corrected timelines (ADR 0015)\n\n\
         USAGE:\n    eval-harness [--db PATH] [--library local|shared] [--sweep] [RECORDING]\n\n\
         OPTIONS:\n    \
         --db PATH              metadata DB to read (default: the app's own DB)\n    \
         --library local|shared which library to score (default: the active one)\n    \
         --sweep                re-score the occupancy parameter grid on cached tracks\n    \
         --trace                diagnose every missed gold rally at default params\n    \
         --fp-trace             classify every draft span by signal support (issue #92)\n    \
         --headroom             price the velocity firing rule and 5 fps sampling (issue #93)\n    \
         --edges                split boundary errors by edge provenance (issue #93)\n    \
         --presence             presence headroom: sub-threshold + bridged (issue #93)\n    \
         --spans                span-score separation over the declared grid (issue #93)\n    \
         --scratch DIR          cache dir for scratch detection tracks (default: temp)\n    \
         RECORDING              path substring; score only matching recordings\n    \
         -h, --help             show this help\n\n\
         Recordings without a hand-corrected timeline are skipped, never scored."
    );
}

/// The app's own metadata DB path for this OS — the same `voloph.db` under the
/// platform app-data dir that the Tauri app opens (identifier `com.quantumff.voloph`),
/// so the harness reads the real library with no arguments. Overridable with `--db`.
fn default_db_path() -> Result<PathBuf, String> {
    const IDENTIFIER: &str = "com.quantumff.voloph";
    let data_dir = app_data_root()?;
    Ok(data_dir.join(IDENTIFIER).join("voloph.db"))
}

/// The platform app-data root (Tauri's `app_data_dir` parent), matching what the app
/// resolves: `$XDG_DATA_HOME` or `~/.local/share` on Linux, `Application Support` on
/// macOS, `%APPDATA%` on Windows.
fn app_data_root() -> Result<PathBuf, String> {
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

    root.ok_or_else(|| "cannot resolve the app-data dir; pass --db PATH".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build intervals from `(start, end)` pairs for terse test fixtures.
    fn ivals(pairs: &[(i64, i64)]) -> Vec<Interval> {
        pairs
            .iter()
            .map(|&(start_ms, end_ms)| Interval { start_ms, end_ms })
            .collect()
    }

    /// One hour, so a single false positive reads as exactly `1.0 / hour`.
    const ONE_HOUR_MS: i64 = 3_600_000;

    #[test]
    fn a_perfect_draft_scores_zero_on_every_axis() {
        let gold = ivals(&[(1000, 3000), (5000, 8000)]);
        let s = score(&gold, &gold, ONE_HOUR_MS);
        assert_eq!(s.misses, 0);
        assert_eq!(s.false_positives, 0);
        assert_eq!(s.matched, 2);
        assert_eq!(s.false_positives_per_hour, 0.0);
        // Every boundary lands exactly → all errors are 0 → median 0.
        assert_eq!(s.median_boundary_error_secs, Some(0.0));
    }

    #[test]
    fn a_gold_rally_with_no_overlapping_draft_is_a_miss() {
        let gold = ivals(&[(0, 1000), (5000, 6000)]);
        let draft = ivals(&[(0, 1000)]); // second gold rally has no draft
        let s = score(&draft, &gold, ONE_HOUR_MS);
        assert_eq!(s.misses, 1);
        assert_eq!(s.matched, 1);
        assert_eq!(s.false_positives, 0);
    }

    #[test]
    fn a_draft_rally_overlapping_no_gold_is_a_false_positive() {
        let gold = ivals(&[(0, 1000)]);
        let draft = ivals(&[(0, 1000), (5000, 6000)]); // second draft is spurious
        let s = score(&draft, &gold, ONE_HOUR_MS);
        assert_eq!(s.false_positives, 1);
        assert_eq!(s.misses, 0);
        // One spurious rally over exactly one hour → 1.0 per hour.
        assert_eq!(s.false_positives_per_hour, 1.0);
    }

    #[test]
    fn false_positives_per_hour_scales_with_duration() {
        let gold = ivals(&[(0, 1000)]);
        let draft = ivals(&[(0, 1000), (5000, 6000), (7000, 8000)]); // two spurious
        // Two false positives over half an hour → 4.0 per hour.
        let s = score(&draft, &gold, ONE_HOUR_MS / 2);
        assert_eq!(s.false_positives, 2);
        assert_eq!(s.false_positives_per_hour, 4.0);
    }

    #[test]
    fn boundary_error_is_the_median_of_start_and_end_offsets() {
        // One gold, one draft offset by +200 ms at the start and +100 ms at the end.
        let gold = ivals(&[(1000, 3000)]);
        let draft = ivals(&[(1200, 3100)]);
        let s = score(&draft, &gold, ONE_HOUR_MS);
        // Errors [200, 100] → sorted [100, 200] → median 150 ms → 0.15 s.
        assert_eq!(s.boundary_errors_ms, vec![200, 100]);
        assert_eq!(s.median_boundary_error_secs, Some(0.15));
    }

    #[test]
    fn median_of_an_even_sample_count_averages_the_middle_two() {
        // Two matched pairs → four boundary errors: 0, 100, 200, 400.
        let gold = ivals(&[(1000, 2000), (5000, 6000)]);
        let draft = ivals(&[(1000, 1900), (5200, 6400)]);
        let s = score(&draft, &gold, ONE_HOUR_MS);
        // Pair 1: |1000-1000|=0, |2000-1900|=100. Pair 2: |5000-5200|=200, |6000-6400|=400.
        assert_eq!(s.boundary_errors_ms, vec![0, 100, 200, 400]);
        // Even count → average of the two central values (100, 200) = 150 ms → 0.15 s.
        assert_eq!(s.median_boundary_error_secs, Some(0.15));
    }

    #[test]
    fn a_matched_gold_pairs_with_its_best_overlapping_draft() {
        // Two drafts overlap the gold; the one covering more of it wins the pairing.
        let gold = ivals(&[(0, 1000)]);
        let draft = ivals(&[(0, 300), (200, 1000)]); // overlaps 300 vs 800
        let s = score(&draft, &gold, ONE_HOUR_MS);
        assert_eq!(s.misses, 0);
        assert_eq!(s.false_positives, 0, "both drafts overlap the gold, neither is spurious");
        // Paired with (200,1000): start error |0-200|=200, end error |1000-1000|=0.
        assert_eq!(s.boundary_errors_ms, vec![200, 0]);
    }

    #[test]
    fn touching_edges_do_not_count_as_overlap() {
        // Draft ends exactly where gold begins: no shared span → a miss and an FP.
        let gold = ivals(&[(1000, 2000)]);
        let draft = ivals(&[(0, 1000)]);
        let s = score(&draft, &gold, ONE_HOUR_MS);
        assert_eq!(s.misses, 1);
        assert_eq!(s.false_positives, 1);
        assert_eq!(s.median_boundary_error_secs, None);
    }

    #[test]
    fn one_draft_may_cover_several_gold_rallies_without_becoming_an_fp() {
        // A merged draft spanning two gold rallies: neither gold is missed, and the
        // single draft overlaps gold, so it is not a false positive.
        let gold = ivals(&[(1000, 2000), (3000, 4000)]);
        let draft = ivals(&[(900, 4200)]);
        let s = score(&draft, &gold, ONE_HOUR_MS);
        assert_eq!(s.misses, 0);
        assert_eq!(s.matched, 2);
        assert_eq!(s.false_positives, 0);
    }

    #[test]
    fn empty_draft_misses_everything_and_has_no_boundary_error() {
        let gold = ivals(&[(0, 1000), (2000, 3000)]);
        let s = score(&[], &gold, ONE_HOUR_MS);
        assert_eq!(s.misses, 2);
        assert_eq!(s.matched, 0);
        assert_eq!(s.false_positives, 0);
        assert_eq!(s.median_boundary_error_secs, None);
    }

    #[test]
    fn empty_gold_makes_every_draft_a_false_positive() {
        let draft = ivals(&[(0, 1000), (2000, 3000)]);
        let s = score(&draft, &[], ONE_HOUR_MS);
        assert_eq!(s.misses, 0);
        assert_eq!(s.false_positives, 2);
        assert_eq!(s.median_boundary_error_secs, None);
    }

    // ── FP-trace classification (issue #92) ──────────────────────────────────

    /// A one-second block grid with the given motion / occupancy masks, for terse
    /// provenance fixtures.
    fn blocks(motion: &[bool], occupancy: &[bool]) -> segment::FusionBlocks {
        segment::FusionBlocks {
            block_ms: 1000,
            motion: motion.to_vec(),
            occupancy: occupancy.to_vec(),
        }
    }

    /// A rally-producing gate verdict span, confirmed or not.
    fn verdict(start_ms: i64, end_ms: i64, confirmed: bool) -> segment::SpanVerdict {
        segment::SpanVerdict {
            start_ms,
            end_ms,
            verdict: if confirmed {
                segment::GateVerdict::Kept
            } else {
                segment::GateVerdict::UnconfirmedByAudio
            },
        }
    }

    #[test]
    fn a_span_over_motion_blocks_alone_is_motion_only() {
        let b = blocks(&[false, true, true, false], &[false; 4]);
        let s = classify_span(Interval { start_ms: 1000, end_ms: 3000 }, &b, &[], None, &[]);
        assert_eq!(s.provenance, Provenance::MotionOnly);
        assert_eq!(s.motion_frac, 1.0);
        assert_eq!(s.occupancy_frac, 0.0);
    }

    #[test]
    fn a_span_over_occupancy_blocks_alone_is_occupancy_only() {
        let b = blocks(&[false; 4], &[false, true, true, false]);
        let s = classify_span(Interval { start_ms: 1000, end_ms: 3000 }, &b, &[], None, &[]);
        assert_eq!(s.provenance, Provenance::OccupancyOnly);
    }

    #[test]
    fn a_span_with_both_proposers_is_mixed() {
        let b = blocks(&[false, true, false, false], &[false, false, true, false]);
        let s = classify_span(Interval { start_ms: 1000, end_ms: 3000 }, &b, &[], None, &[]);
        assert_eq!(s.provenance, Provenance::Mixed);
        assert_eq!(s.motion_frac, 0.5);
        assert_eq!(s.occupancy_frac, 0.5);
    }

    #[test]
    fn padding_blocks_dilute_the_fractions_but_not_the_class() {
        // Motion fired only on the middle block; the padded span also covers one
        // inactive block on each side.
        let b = blocks(&[false, false, true, false, false], &[false; 5]);
        let s = classify_span(Interval { start_ms: 1000, end_ms: 4000 }, &b, &[], None, &[]);
        assert_eq!(s.provenance, Provenance::MotionOnly);
        assert!((s.motion_frac - 1.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn a_span_beyond_the_masks_reads_as_the_motion_proposes_fallback() {
        let b = blocks(&[], &[]);
        let s = classify_span(Interval { start_ms: 0, end_ms: 2000 }, &b, &[], None, &[]);
        assert_eq!(s.provenance, Provenance::MotionOnly);
        assert_eq!(s.motion_frac, 0.0);
    }

    #[test]
    fn audio_verdicts_match_the_padded_span_by_overlap() {
        let b = blocks(&[true; 10], &[false; 10]);
        // Raw verdict span 2000–5000 sits inside the padded rally 800–6200.
        let span = Interval { start_ms: 800, end_ms: 6200 };
        let confirmed = classify_span(span, &b, &[verdict(2000, 5000, true)], None, &[]);
        assert!(confirmed.audio_confirmed);
        let unconfirmed = classify_span(span, &b, &[verdict(2000, 5000, false)], None, &[]);
        assert!(!unconfirmed.audio_confirmed);
        // A verdict elsewhere in the recording never confirms this span.
        let elsewhere = classify_span(span, &b, &[verdict(8000, 9000, true)], None, &[]);
        assert!(!elsewhere.audio_confirmed);
    }

    #[test]
    fn a_merged_span_covering_one_confirmed_raw_span_is_confirmed() {
        let b = blocks(&[true; 12], &[false; 12]);
        let span = Interval { start_ms: 0, end_ms: 12000 };
        let vs = [verdict(1000, 4000, false), verdict(7000, 10000, true)];
        assert!(classify_span(span, &b, &vs, None, &[]).audio_confirmed);
    }

    #[test]
    fn non_rally_verdicts_never_confirm_a_span() {
        let b = blocks(&[true; 4], &[false; 4]);
        let vs = [segment::SpanVerdict {
            start_ms: 0,
            end_ms: 4000,
            verdict: segment::GateVerdict::MotionNeverFired,
        }];
        assert!(!classify_span(Interval { start_ms: 0, end_ms: 4000 }, &b, &vs, None, &[]).audio_confirmed);
    }

    #[test]
    fn a_span_overlapping_gold_is_a_tp_and_one_overlapping_none_is_an_fp() {
        let b = blocks(&[true; 4], &[false; 4]);
        let gold = ivals(&[(0, 2000)]);
        assert!(!classify_span(Interval { start_ms: 1000, end_ms: 3000 }, &b, &[], None, &gold).is_fp);
        assert!(classify_span(Interval { start_ms: 2500, end_ms: 3500 }, &b, &[], None, &gold).is_fp);
    }

    #[test]
    fn fired_fraction_counts_the_samples_inside_the_span() {
        // 1 fps: samples at 0s, 1s, 2s, 3s. Span 0–2s holds samples 0 and 1.
        let f = [
            segment::OccupancySample { live: 2, fired: true },
            segment::OccupancySample { live: 2, fired: false },
            segment::OccupancySample { live: 2, fired: true },
            segment::OccupancySample { live: 2, fired: true },
        ];
        assert_eq!(
            fired_fraction(Interval { start_ms: 0, end_ms: 2000 }, &f, 1.0),
            Some(0.5)
        );
        // A span beyond the track's end holds no samples.
        assert_eq!(
            fired_fraction(Interval { start_ms: 9000, end_ms: 12000 }, &f, 1.0),
            None
        );
    }

    #[test]
    fn no_firing_samples_in_the_span_reads_as_zero_not_absent() {
        let f = [
            segment::OccupancySample { live: 0, fired: false },
            segment::OccupancySample { live: 0, fired: false },
        ];
        assert_eq!(
            fired_fraction(Interval { start_ms: 0, end_ms: 2000 }, &f, 1.0),
            Some(0.0)
        );
    }

    /// A classified span fixture for rule pricing.
    fn support(
        span: (i64, i64),
        provenance: Provenance,
        audio_confirmed: bool,
        is_fp: bool,
    ) -> SpanSupport {
        SpanSupport {
            span: Interval { start_ms: span.0, end_ms: span.1 },
            provenance,
            motion_frac: 1.0,
            occupancy_frac: 0.0,
            audio_confirmed,
            fired_fraction: Some(0.0),
            is_fp,
        }
    }

    #[test]
    fn pricing_a_rule_measures_fp_removed_and_gold_lost_exactly() {
        // One gold rally covered *only* by an unsupported motion-only span, one FP
        // in the same class, and one confirmed TP elsewhere.
        let recs = [TracedRecording {
            spans: vec![
                support((0, 1000), Provenance::MotionOnly, false, false), // sole cover of gold #1
                support((5000, 6000), Provenance::MotionOnly, false, true), // FP
                support((8000, 9000), Provenance::Mixed, true, false),    // TP, other class
            ],
            gold: ivals(&[(0, 1000), (8000, 9000)]),
            duration_ms: ONE_HOUR_MS,
        }];
        let (fp_removed, fph_remaining, gold_lost) = price_rule(&recs, |s| {
            s.provenance == Provenance::MotionOnly && !s.audio_confirmed
        });
        assert_eq!(fp_removed, 1);
        assert_eq!(fph_remaining, 0.0);
        assert_eq!(gold_lost, 1, "suppressing the sole covering span loses the gold rally");
    }

    #[test]
    fn a_gold_rally_also_covered_by_a_kept_span_is_not_lost() {
        // The gold rally is covered by both an unsupported span and a confirmed
        // one — suppressing the former costs nothing.
        let recs = [TracedRecording {
            spans: vec![
                support((0, 1000), Provenance::MotionOnly, false, false),
                support((500, 1500), Provenance::Mixed, true, false),
            ],
            gold: ivals(&[(0, 1500)]),
            duration_ms: ONE_HOUR_MS,
        }];
        let (fp_removed, _, gold_lost) = price_rule(&recs, |s| {
            s.provenance == Provenance::MotionOnly && !s.audio_confirmed
        });
        assert_eq!(fp_removed, 0);
        assert_eq!(gold_lost, 0);
    }

    #[test]
    fn every_span_falls_in_exactly_one_support_class() {
        let all = [
            support((0, 1), Provenance::MotionOnly, false, true),
            support((0, 1), Provenance::MotionOnly, true, false),
            support((0, 1), Provenance::Mixed, false, true),
            support((0, 1), Provenance::Mixed, true, false),
            support((0, 1), Provenance::OccupancyOnly, false, true),
            support((0, 1), Provenance::OccupancyOnly, true, false),
        ];
        for s in &all {
            assert_eq!(
                SUPPORT_CLASSES.iter().filter(|&&c| c == s.class()).count(),
                1
            );
        }
    }

    // ── Headroom measurement (issue #93) ──────────────────────────────────────

    #[test]
    fn window_density_judges_only_the_samples_the_window_holds() {
        let fired = [true, true, false, false, true];
        // Center 1, half 1 → samples 0..=2: two of three fire.
        assert!((window_density(&fired, 1, 1) - 2.0 / 3.0).abs() < 1e-9);
        // Center 0 clips at the left edge → samples 0..=1: both fire.
        assert!((window_density(&fired, 0, 1) - 1.0).abs() < 1e-9);
        // Center 4 clips at the right edge → samples 3..=4: one of two.
        assert!((window_density(&fired, 4, 1) - 0.5).abs() < 1e-9);
        assert_eq!(window_density(&[], 0, 1), 0.0);
    }

    #[test]
    fn peak_density_is_the_best_window_in_the_range() {
        let fired = [false, true, true, true, false, false, false, false];
        // half 1: the window centered on 2 holds three firing samples.
        assert!((peak_density(&fired, 0, 8, 1) - 1.0).abs() < 1e-9);
        // Restricted to the quiet tail, nothing fires.
        assert_eq!(peak_density(&fired, 5, 8, 1), 0.0);
        // An empty range has no peak.
        assert_eq!(peak_density(&fired, 3, 3, 1), 0.0);
    }

    #[test]
    fn sample_ranges_and_gold_membership_follow_the_track_clock() {
        // 2 fps: samples at 0, 500, 1000, 1500 ms.
        let gold = ivals(&[(500, 1500)]);
        assert!(!sample_in_gold(0, 2.0, &gold));
        assert!(sample_in_gold(1, 2.0, &gold));
        assert!(sample_in_gold(2, 2.0, &gold));
        assert!(!sample_in_gold(3, 2.0, &gold), "gold end is exclusive");
        assert_eq!(sample_range(Interval { start_ms: 500, end_ms: 1500 }, 2.0, 4), (1, 3));
        // A range beyond the track clamps to its length.
        assert_eq!(sample_range(Interval { start_ms: 0, end_ms: 99_000 }, 2.0, 4), (0, 4));
    }

    /// Two players with size structure whose boxes each step `step` in x per
    /// sample — the controllable fixture for velocity-rule flags. The boxes sit
    /// far apart so each one's nearest match is its own previous position.
    fn stepping_track(fps: f64, secs: f64, step: f64) -> segment::OccupancyTrack {
        let n = (secs * fps) as usize;
        let samples = (0..n)
            .map(|i| {
                let j = (i % 2) as f64 * step;
                let far = segment::DetBox { x: 0.15 + j, y: 0.25, w: 0.14, h: 0.14 }; // area ~0.02
                let near = segment::DetBox { x: 0.75 - j, y: 0.65, w: 0.32, h: 0.32 }; // area ~0.10
                vec![far, near]
            })
            .collect();
        segment::OccupancyTrack { fps, samples }
    }

    #[test]
    fn velocity_flags_fire_at_and_above_the_step_rate() {
        // Steps of 0.06/sample at 3 fps = 0.18/s. The velocity rule must fire at
        // a 0.15/s threshold and stay closed at 0.25/s; the shipped rule fires
        // (0.06 > 0.02) — and all three agree the first sample never fires.
        let occ = stepping_track(3.0, 20.0, 0.06);
        let p = segment::Params::default();
        let slow = rule_flags(&occ, &p, FiringRule::Velocity(0.15));
        let fast = rule_flags(&occ, &p, FiringRule::Velocity(0.25));
        let current = rule_flags(&occ, &p, FiringRule::Current);
        assert!(!slow[0] && !fast[0] && !current[0], "no previous sample yet");
        assert!(slow[1..].iter().all(|&f| f), "0.18/s clears a 0.15/s bar");
        assert!(fast[1..].iter().all(|&f| !f), "0.18/s misses a 0.25/s bar");
        assert!(current[1..].iter().all(|&f| f), "the shipped movement bool fires");
    }

    #[test]
    fn rule_stats_pool_rates_and_probe_missed_windows() {
        // 1 fps, 10 samples; gold covers 0–4 s (samples 0..4), the rest is out.
        // All in-gold samples fire, no out-of-gold sample does.
        let fired = [true, true, true, true, false, false, false, false, false, false];
        let gold = ivals(&[(0, 4000)]);
        let missed = ivals(&[(1000, 3000)]);
        let mut p = segment::Params::default();
        p.occupancy_window_ms = 2000; // half-window 1 sample at 1 fps
        let mut stats = RuleStats::default();
        stats.add(&fired, 1.0, &gold, &missed, &p);
        assert_eq!((stats.in_gold_fired, stats.in_gold_total), (4, 4));
        assert_eq!((stats.out_fired, stats.out_total), (0, 6));
        // The 4 s gold rally is short (≤ 5 s) and saturates its window.
        assert_eq!(stats.short_peaks.len(), 1);
        assert!((stats.short_peaks[0] - 1.0).abs() < 1e-9);
        // The missed window fires densely enough to propose at the default bar.
        assert_eq!((stats.missed_proposing, stats.missed_total), (1, 1));
        // Out-of-gold windows near the rally edge see some firing, but none
        // reaches the 0.5 default: sample 4's window holds samples 3..=5 (1/3).
        assert_eq!(stats.out_windows_proposing, 0);
        assert_eq!(stats.out_windows, 6);
    }

    // ── Boundary-edge provenance (issue #93) ──────────────────────────────────

    #[test]
    fn edge_blocks_are_the_first_and_last_active_blocks_in_the_span() {
        let b = blocks(
            &[false, true, false, false, false, false],
            &[false, false, false, true, true, false],
        );
        // The padded span covers blocks 0..6; the active ones are 1 and 3–4.
        let span = Interval { start_ms: 0, end_ms: 6000 };
        assert_eq!(edge_blocks(span, &b), Some((1, 4)));
        // A span over quiet blocks only has no edge-placing block.
        assert_eq!(edge_blocks(Interval { start_ms: 5000, end_ms: 6000 }, &b), None);
    }

    #[test]
    fn boundary_errors_split_by_the_edge_placing_signal() {
        // One gold rally, one draft: motion placed the start edge (block 1),
        // occupancy the end edge (block 4). Start error 500 ms, end error 1500 ms.
        let b = blocks(
            &[false, true, true, false, false, false],
            &[false, false, false, true, true, false],
        );
        let draft = ivals(&[(500, 5500)]);
        let gold = ivals(&[(1000, 4000)]);
        let mut split = BoundarySplit::default();
        split_boundary_errors(&draft, &gold, &b, &mut split);
        assert_eq!(split.motion_edged_errs, vec![500]);
        assert_eq!(split.occupancy_edged_errs, vec![1500]);
        assert_eq!(split.occupancy_gold_edges, vec![4000]);
    }

    #[test]
    fn an_unmatched_gold_rally_contributes_no_boundaries() {
        let b = blocks(&[true; 4], &[false; 4]);
        let draft = ivals(&[(0, 1000)]);
        let gold = ivals(&[(2000, 3000)]); // no overlap → a miss, not a boundary
        let mut split = BoundarySplit::default();
        split_boundary_errors(&draft, &gold, &b, &mut split);
        assert!(split.motion_edged_errs.is_empty());
        assert!(split.occupancy_edged_errs.is_empty());
    }

    #[test]
    fn motion_near_probes_block_centers_within_the_radius() {
        // Motion only on block 2 (center 2500 ms).
        let b = blocks(&[false, false, true, false], &[false; 4]);
        assert!(motion_near(&b, 2500, 0));
        assert!(motion_near(&b, 3400, 1000));
        assert!(!motion_near(&b, 4000, 1000));
        assert!(motion_near(&b, 4000, 2000));
    }

    // ── Presence measurement (issue #93, round 2) ─────────────────────────────

    /// A normalized box for presence fixtures.
    fn dbox(x: f64, y: f64, side: f64) -> segment::DetBox {
        segment::DetBox { x, y, w: side, h: side }
    }

    /// A scored detector box for banding fixtures.
    fn sbox(x: f32, side: f32, score: f32) -> crate::detect::Box {
        crate::detect::Box { x, y: 0.25, w: side, h: side, score }
    }

    #[test]
    fn det_iou_bounds() {
        let a = dbox(0.1, 0.1, 0.2);
        let far = dbox(0.7, 0.7, 0.2);
        assert!((det_iou(&a, &a) - 1.0).abs() < 1e-9);
        assert_eq!(det_iou(&a, &far), 0.0);
    }

    #[test]
    fn banding_admits_boxes_by_cumulative_floor_and_keeps_geometry() {
        let track = crate::detect::DetectionTrack {
            fps: 3.0,
            samples: vec![vec![
                sbox(0.125, 0.25, 0.5),
                sbox(0.5, 0.25, 0.25),
                sbox(0.75, 0.25, 0.125),
            ]],
        };
        let shipped = occupancy_at_floor(&track, crate::detect::SCORE_THRESHOLD);
        let mid = occupancy_at_floor(&track, 0.20);
        let low = occupancy_at_floor(&track, 0.10);
        assert_eq!(shipped.samples[0].len(), 1);
        assert_eq!(mid.samples[0].len(), 2);
        assert_eq!(low.samples[0].len(), 3);
        // Geometry crosses the seam unchanged (exactly representable values).
        assert_eq!(shipped.samples[0][0], dbox(0.125, 0.25, 0.25));
        assert_eq!(shipped.fps, 3.0);
    }

    #[test]
    fn bridge_depth_zero_returns_the_track_unchanged() {
        let occ = segment::OccupancyTrack {
            fps: 3.0,
            samples: vec![vec![dbox(0.1, 0.1, 0.2)], vec![], vec![dbox(0.5, 0.5, 0.2)]],
        };
        assert_eq!(bridge_track(&occ, 0).samples, occ.samples);
    }

    #[test]
    fn a_vanished_box_persists_for_up_to_k_samples() {
        let a = dbox(0.1, 0.1, 0.2);
        let occ = segment::OccupancyTrack {
            fps: 3.0,
            samples: vec![vec![a], vec![], vec![], vec![]],
        };
        let bridged = bridge_track(&occ, 2);
        assert_eq!(bridged.samples[1], vec![a], "persists one sample after vanishing");
        assert_eq!(bridged.samples[2], vec![a], "persists a second sample");
        assert!(bridged.samples[3].is_empty(), "the chain ends after k samples");
    }

    #[test]
    fn a_redetected_box_ends_its_chain_instead_of_duplicating() {
        let a = dbox(0.10, 0.10, 0.20);
        // Shifted a quarter-side: IoU well above the association bar.
        let a_moved = dbox(0.15, 0.10, 0.20);
        let occ = segment::OccupancyTrack {
            fps: 3.0,
            samples: vec![vec![a], vec![a_moved]],
        };
        let bridged = bridge_track(&occ, 3);
        assert_eq!(bridged.samples[1], vec![a_moved], "no ghost of the re-detected box");
    }

    #[test]
    fn a_disjoint_new_box_does_not_end_another_boxs_chain() {
        let a = dbox(0.1, 0.1, 0.2);
        let b = dbox(0.7, 0.7, 0.2);
        let occ = segment::OccupancyTrack {
            fps: 3.0,
            samples: vec![vec![a], vec![b]],
        };
        let bridged = bridge_track(&occ, 1);
        assert_eq!(bridged.samples[1], vec![b, a], "the real box plus the carried one");
    }

    /// End-to-end through the real pipeline: two players the detector only ever
    /// sees one-at-a-time (alternating samples) show no two-structured-boxes
    /// presence unbridged, and full presence once each box may persist one sample.
    #[test]
    fn bridging_recovers_structure_the_alternating_detector_loses() {
        // Both players drift 0.03 per appearance so the furniture filter (span
        // <= static_frac 0.02) never eats them. Far box area 0.01, near 0.04 —
        // ratio 4 clears the 1.5 structure bar while the near box stays under the
        // area cap even when carried far boxes pull the median area down to 0.01.
        let n = 12;
        let samples: Vec<Vec<segment::DetBox>> = (0..n)
            .map(|i| {
                let drift = 0.03 * (i / 2) as f64;
                if i % 2 == 0 {
                    vec![dbox(0.10 + drift, 0.20, 0.1)]
                } else {
                    vec![dbox(0.60 + drift, 0.60, 0.2)]
                }
            })
            .collect();
        let occ = segment::OccupancyTrack { fps: 3.0, samples };
        let p = segment::Params::default();
        let unbridged: Vec<bool> = segment::occupancy_kinematics(&occ, &p)
            .iter()
            .map(|s| s.size_structure)
            .collect();
        assert!(unbridged.iter().all(|&s| !s), "one visible box never structures");
        let bridged: Vec<bool> = segment::occupancy_kinematics(&bridge_track(&occ, 1), &p)
            .iter()
            .map(|s| s.size_structure)
            .collect();
        assert!(!bridged[0], "nothing to carry into the first sample");
        assert!(bridged[1..].iter().all(|&s| s), "every later sample pairs real + carried");
    }

    #[test]
    fn flag_fraction_counts_within_the_clipped_range() {
        let flags = [true, false, true, true];
        assert!((flag_fraction(&flags, 0, 2) - 0.5).abs() < 1e-9);
        assert!((flag_fraction(&flags, 2, 99) - 1.0).abs() < 1e-9, "clips at the track end");
        assert_eq!(flag_fraction(&flags, 4, 4), 0.0, "an empty range has no presence");
    }

    // ── Span-score separation (issue #93, round 3) ────────────────────────────

    #[test]
    fn matched_out_windows_match_durations_and_stay_out_of_gold() {
        let gold = ivals(&[(10_000, 14_000), (30_000, 33_000)]);
        let (windows, skipped) = matched_out_windows(&gold, 60_000);
        assert_eq!(skipped, 0);
        // One window per gold rally, at that rally's duration.
        let mut durations: Vec<i64> = windows.iter().map(|w| w.end_ms - w.start_ms).collect();
        durations.sort_unstable();
        assert_eq!(durations, vec![3_000, 4_000]);
        // Never overlapping gold, never overlapping each other, inside the recording.
        for (i, w) in windows.iter().enumerate() {
            assert!(w.start_ms >= 0 && w.end_ms <= 60_000, "{w:?}");
            assert!(!gold.iter().any(|g| w.overlaps(g)), "{w:?} overlaps gold");
            for other in &windows[i + 1..] {
                assert!(!w.overlaps(other), "{w:?} overlaps {other:?}");
            }
        }
    }

    #[test]
    fn matched_out_windows_skip_a_rally_no_gap_fits() {
        // Gold covers all but 1 s: a 50 s rally fits in no gap and is skipped,
        // never silently shrunk or dropped from the count.
        let gold = ivals(&[(0, 50_000)]);
        let (windows, skipped) = matched_out_windows(&gold, 51_000);
        assert!(windows.is_empty(), "{windows:?}");
        assert_eq!(skipped, 1);
    }

    #[test]
    fn matched_out_windows_of_an_empty_gold_set_are_empty() {
        let (windows, skipped) = matched_out_windows(&[], 60_000);
        assert!(windows.is_empty());
        assert_eq!(skipped, 0);
    }

    #[test]
    fn false_runs_and_longest_true_run_read_the_range() {
        let flags = [true, false, false, true, false, true, true, true];
        assert_eq!(false_runs(&flags, 0, 8), vec![2, 1]);
        assert_eq!(longest_true_run(&flags, 0, 8), 3);
        // Clipped to a sub-range: only what the span holds counts.
        assert_eq!(false_runs(&flags, 1, 3), vec![2]);
        assert_eq!(longest_true_run(&flags, 1, 5), 1);
        // Beyond the track clamps; an empty range holds nothing.
        assert_eq!(false_runs(&flags, 6, 99), Vec::<usize>::new());
        assert_eq!(longest_true_run(&flags, 8, 8), 0);
    }

    #[test]
    fn onsets_in_span_sum_the_overlapped_blocks() {
        let ob = segment::OnsetBlocks {
            block_ms: 1000,
            onsets: vec![1, 2, 0, 3],
        };
        // 500–2500 ms overlaps blocks 0, 1, 2.
        assert_eq!(onsets_in_span(Interval { start_ms: 500, end_ms: 2500 }, &ob), 3);
        // Beyond the mask clamps to what exists.
        assert_eq!(onsets_in_span(Interval { start_ms: 3000, end_ms: 9000 }, &ob), 3);
        assert_eq!(onsets_in_span(Interval { start_ms: 9000, end_ms: 10_000 }, &ob), 0);
    }

    /// Hand-built views at 2 fps for span-feature fixtures: structure/fired flags
    /// per bridge depth plus per-sample speeds.
    fn views(structure: [&[bool]; 3], fired: [&[bool]; 3], speed: &[f64]) -> SpanTrackViews {
        SpanTrackViews {
            fps: 2.0,
            structure: structure.iter().map(|s| s.to_vec()).collect(),
            fired: fired.iter().map(|f| f.to_vec()).collect(),
            speed: speed.to_vec(),
        }
    }

    #[test]
    fn span_features_slice_the_views_over_the_span() {
        // 2 fps, 8 samples (0–4 s). Span 0–4 s covers all 8.
        let s0 = [true, false, false, true, true, false, true, true];
        let s2 = [true; 8];
        let f2 = [false, true, true, true, false, false, true, false];
        let v = views(
            [&s0, &s2, &s2],
            [&[false; 8], &f2, &f2],
            &[0.1, 0.2, 0.1, 0.0, 0.1, 0.3, 0.2, 0.0],
        );
        let ob = segment::OnsetBlocks { block_ms: 1000, onsets: vec![1, 0, 2, 0] };
        let f = span_features(Interval { start_ms: 0, end_ms: 4000 }, Some(&v), &ob);
        assert!((f.presence[0] - 5.0 / 8.0).abs() < 1e-9);
        assert!((f.presence[SPAN_K2] - 1.0).abs() < 1e-9);
        assert!((f.fired[SPAN_K2] - 0.5).abs() < 1e-9);
        // Dropout runs of s0: [2, 1] samples → max 1.0 s, mean 0.75 s at 2 fps.
        assert!((f.max_gap_s - 1.0).abs() < 1e-9);
        assert!((f.mean_gap_s - 0.75).abs() < 1e-9);
        // Longest fired run at k2: samples 1–3 → 3 samples → 1.5 s.
        assert!((f.sustained_s - 1.5).abs() < 1e-9);
        // Movement integral: mean of the eight speeds.
        assert!((f.mean_speed - 1.0 / 8.0).abs() < 1e-9);
        // Onsets: blocks 0–3 → 3 onsets over 4 s.
        assert_eq!(f.onset_count, 3);
        assert!((f.onset_per_s - 0.75).abs() < 1e-9);
    }

    #[test]
    fn span_features_without_a_detector_read_as_one_full_gap() {
        let ob = segment::OnsetBlocks { block_ms: 1000, onsets: vec![2, 2, 2, 2] };
        let f = span_features(Interval { start_ms: 0, end_ms: 3000 }, None, &ob);
        assert!(f.presence.iter().all(|&p| p == 0.0));
        assert_eq!(f.max_gap_s, 3.0, "no samples → one span-length gap");
        assert_eq!(f.sustained_s, 0.0);
        // Audio still counts without vision.
        assert_eq!(f.onset_count, 6);
        assert!((f.onset_per_s - 2.0).abs() < 1e-9);
    }

    #[test]
    fn span_features_beyond_the_track_read_as_one_full_gap() {
        let s = [true, true];
        let v = views([&s, &s, &s], [&s, &s, &s], &[0.1, 0.1]);
        let ob = segment::OnsetBlocks { block_ms: 1000, onsets: vec![0; 40] };
        // A span past the track's end holds no samples.
        let f = span_features(Interval { start_ms: 30_000, end_ms: 34_000 }, Some(&v), &ob);
        assert_eq!(f.presence[0], 0.0);
        assert_eq!(f.max_gap_s, 4.0);
        assert_eq!(f.mean_speed, 0.0);
    }

    /// A feature fixture for score-point tests: everything zero except the given
    /// k2 presence / k2 fired / onset density.
    fn feats(presence_k2: f64, fired_k2: f64, onset_per_s: f64) -> SpanFeatures {
        SpanFeatures {
            presence: vec![0.0, 0.0, presence_k2],
            fired: vec![0.0, 0.0, fired_k2],
            max_gap_s: 0.0,
            mean_gap_s: 0.0,
            mean_speed: 0.0,
            sustained_s: 0.0,
            onset_count: 0,
            onset_per_s,
        }
    }

    #[test]
    fn audio_rescue_only_ever_adds_admission() {
        let p = segment::Params::default();
        let points = span_score_points(&p);
        let rescue: Vec<_> = points.iter().filter(|pt| pt.label.contains('|')).collect();
        assert!(!rescue.is_empty(), "the grid must include the rescue family");
        for pt in &rescue {
            // A span vision fully admits stays admitted with zero audio (audio
            // never sinks)…
            assert!((pt.admits)(&feats(0.0, 1.0, 0.0)), "{}", pt.label);
            // …audio over the confirm bar rescues a span with enough presence…
            assert!((pt.admits)(&feats(0.6, 0.0, 0.5)), "{}", pt.label);
            // …but never conjures admission without the vision floor.
            assert!(!(pt.admits)(&feats(0.1, 0.0, 5.0)), "{}", pt.label);
        }
    }

    #[test]
    fn the_score_grid_thresholds_behave_monotonically() {
        let p = segment::Params::default();
        let strong = feats(0.95, 0.65, 0.0);
        let weak = feats(0.10, 0.05, 0.0);
        for pt in span_score_points(&p) {
            // Every declared point admits the saturated span; the sustained and
            // gap-capped families read other fields, so only check the weak side
            // for the pure presence/fired families.
            if pt.label.starts_with("presence@k2 >=") || pt.label.starts_with("fired@k2") {
                assert!(!(pt.admits)(&weak), "{}", pt.label);
            }
            if pt.label.starts_with("presence@k2 >=") && !pt.label.contains("max-gap") {
                assert!((pt.admits)(&strong), "{}", pt.label);
            }
        }
    }

    #[test]
    fn unknown_duration_falls_back_to_the_latest_edge() {
        // duration_ms <= 0 → per-hour uses the last edge (6000 ms = 1/600 h), so one
        // false positive reads as 600 / hour rather than dividing by zero.
        let gold = ivals(&[(0, 1000)]);
        let draft = ivals(&[(0, 1000), (5000, 6000)]);
        let s = score(&draft, &gold, 0);
        assert_eq!(s.false_positives, 1);
        assert!((s.false_positives_per_hour - 600.0).abs() < 1e-9, "{s:?}");
    }
}
