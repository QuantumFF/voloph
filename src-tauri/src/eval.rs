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
            help: false,
        };
        let mut it = args.iter();
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "-h" | "--help" => opts.help = true,
                "--sweep" => opts.sweep = true,
                "--trace" => opts.trace = true,
                "--fp-trace" => opts.fp_trace = true,
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
