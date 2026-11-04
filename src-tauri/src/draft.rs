//! Drafting a recording's timeline (ADR 0019): extract the signals the segmenter
//! reads, run the library's chosen segmenter — falling back teacher → student → v6
//! when one cannot run — and say which one actually drew the draft. No DB and no
//! events: the media worker persists the [`Draft`] and turns [`Progress`] into ticks.

use std::time::Instant;

use crate::segment::{self, MotionTrack, Rally, Segmenter};
use crate::{detect, media, student};

/// A drawn draft timeline and what an Analysis carries beside it (ADR 0013).
#[derive(Debug)]
pub struct Draft {
    pub rallies: Vec<Rally>,
    /// The segmenter that actually drew it, not the chosen one when that fell back,
    /// so the next scan offers the recording to the chosen one again (ADR 0019).
    pub made_by: Segmenter,
    /// The timeline strip's waveform (issue #6).
    pub waveform: Vec<f32>,
    /// From the decoded audio — the authoritative length; the container probe can be
    /// slightly off.
    pub duration_ms: i64,
}

impl Draft {
    fn new(rallies: Vec<Rally>, made_by: Segmenter, samples: &[f32]) -> Draft {
        Draft {
            rallies,
            made_by,
            waveform: segment::waveform(samples),
            duration_ms: (samples.len() as f64 / media::SEGMENT_SAMPLE_RATE as f64 * 1000.0) as i64,
        }
    }
}

/// Where a draft reports how far it has got (issue #81).
pub trait Progress {
    /// A timed pass over the footage starts; ticks after it measure that pass alone,
    /// so the frontend's processed ÷ elapsed speed is not skewed by work before it.
    fn pass_started(&mut self);
    /// The current pass has processed `footage_ms` of the recording.
    fn processed(&mut self, footage_ms: i64);
}

/// Draft the recording at `path` with `chosen`, falling back when it cannot run.
/// Errors only when the signals every segmenter needs (audio, motion) cannot be
/// extracted — the recording then has no draft.
pub fn draft(path: &str, chosen: Segmenter, progress: &mut impl Progress) -> Result<Draft, String> {
    draft_with(&mut Footage { path }, chosen, progress, path)
}

/// The expensive steps of drafting, behind one seam so the fallback chain is testable
/// without footage or models. [`Footage`] is the real one.
trait Passes {
    /// The decoded audio at [`media::SEGMENT_SAMPLE_RATE`].
    fn audio(&mut self) -> Result<Vec<f32>, String>;
    /// The teacher's whole run: its shuttle and pose passes on one footage scale.
    fn teacher(&mut self, on_progress: &mut dyn FnMut(i64)) -> Result<Vec<Rally>, String>;
    /// Motion energy at [`media::MOTION_FPS`].
    fn motion(&mut self, on_progress: &mut dyn FnMut(i64)) -> Result<Vec<f64>, String>;
    /// The person-detection track, or `None` on any failure (ADR 0015).
    fn detections(&mut self) -> Option<detect::DetectionTrack>;
    fn student_model(&mut self) -> Result<&'static student::StudentModel, String>;
}

fn draft_with(
    passes: &mut impl Passes,
    chosen: Segmenter,
    progress: &mut impl Progress,
    path: &str,
) -> Result<Draft, String> {
    let samples = passes
        .audio()
        .map_err(|e| format!("audio extraction failed: {e}"))?;
    let mut chosen = chosen;
    if chosen == Segmenter::Teacher {
        // The teacher reads neither motion nor the detection track (it runs its own
        // shuttle and pose passes), so they are extracted only if it fails.
        progress.pass_started();
        match passes.teacher(&mut |ms| progress.processed(ms)) {
            Ok(rallies) => return Ok(Draft::new(rallies, Segmenter::Teacher, &samples)),
            Err(e) => {
                log::warn!("media worker: teacher failed for {path}, using the student: {e}");
                chosen = Segmenter::Student;
            }
        }
    }
    // Motion extraction is the long pole of analysis. Its pass starts the clock, after
    // the (untracked) audio extraction that ran first.
    progress.pass_started();
    let motion = MotionTrack {
        fps: f64::from(media::MOTION_FPS),
        energy: passes
            .motion(&mut |ms| progress.processed(ms))
            .map_err(|e| format!("motion extraction failed: {e}"))?,
    };
    // Occupancy detection (ADR 0015 Stage 2, issue #84): v6 turns it into occupancy that
    // *proposes* candidate play spans, the student reads it with scores. Any failure
    // yields `None`; v6 then falls back to motion-proposes, so a failed detector never
    // turns into deleted play.
    let detections = passes.detections();
    let (rallies, made_by) =
        run_segmenter(passes, chosen, &samples, &motion, detections.as_ref(), path);
    Ok(Draft::new(rallies, made_by, &samples))
}

/// Run the student or v6 and say which one drew the draft.
///
/// The student needs the person detections (with scores) and its bundled model; if
/// either is missing it falls back to v6, which degrades to motion-proposes without
/// occupancy (ADR 0015) — so a failed detector never costs the recording its draft.
fn run_segmenter(
    passes: &mut impl Passes,
    chosen: Segmenter,
    samples: &[f32],
    motion: &MotionTrack,
    detections: Option<&detect::DetectionTrack>,
    path: &str,
) -> (Vec<Rally>, Segmenter) {
    if chosen == Segmenter::Student {
        match (passes.student_model(), detections) {
            (Ok(model), Some(detect)) => {
                let started = Instant::now();
                let signals = student::AudioSignals::of(samples);
                let rallies =
                    student::segment(model, &signals.inputs(motion.fps, &motion.energy, detect));
                log::info!(
                    "media worker: student segmented {path} in {} ms",
                    started.elapsed().as_millis()
                );
                return (rallies, Segmenter::Student);
            }
            (Err(e), _) => {
                log::warn!("media worker: student model unavailable, using v6 for {path}: {e}")
            }
            (_, None) => log::warn!(
                "media worker: no person detections for {path}; the student needs them, using v6"
            ),
        }
    }
    let occupancy = detections.map(detect::DetectionTrack::to_occupancy_track);
    let segmentation = segment::segment(
        samples,
        media::SEGMENT_SAMPLE_RATE,
        motion,
        occupancy.as_ref(),
    );
    // Per-span gate verdicts (ADR 0015 Stage 0): one line per candidate span the
    // segmenter weighed, so running a bad recording shows which gate ate a rally.
    // Diagnostic only — the draft is unaffected.
    for v in &segmentation.verdicts {
        log::info!(
            "media worker: gate verdict {} for span {}-{} ms of {path}",
            v.verdict.label(),
            v.start_ms,
            v.end_ms
        );
    }
    (segmentation.rallies, Segmenter::V6)
}

/// The real passes over a recording file.
struct Footage<'a> {
    path: &'a str,
}

impl Passes for Footage<'_> {
    fn audio(&mut self) -> Result<Vec<f32>, String> {
        media::extract_pcm(self.path)
    }

    fn teacher(&mut self, on_progress: &mut dyn FnMut(i64)) -> Result<Vec<Rally>, String> {
        crate::teacher::segment_recording(self.path, on_progress)
    }

    fn motion(&mut self, on_progress: &mut dyn FnMut(i64)) -> Result<Vec<f64>, String> {
        media::extract_motion(self.path, on_progress)
    }

    /// Loads the vendored nano detector (probing a GPU, falling back to CPU
    /// silently), runs it over the recording, and logs a one-line summary of what it
    /// saw. `None` on **every** failure path — the shared degradation policy in
    /// [`detect::detections_or_none`].
    fn detections(&mut self) -> Option<detect::DetectionTrack> {
        let path = self.path;
        let started = Instant::now();
        let track = detect::detections_or_none(path, |why| {
            log::warn!("media worker: occupancy disabled for {path}: {why}");
        })?;
        let total: usize = track.samples.iter().map(Vec::len).sum();
        let peak = track.samples.iter().map(Vec::len).max().unwrap_or(0);
        log::info!(
            "media worker: occupancy track for {path} — {} samples @ {} fps, \
             {total} person boxes, peak {peak} simultaneous ({} ms)",
            track.samples.len(),
            track.fps,
            started.elapsed().as_millis(),
        );
        Some(track)
    }

    fn student_model(&mut self) -> Result<&'static student::StudentModel, String> {
        student::StudentModel::bundled()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic synthetic signals for a 120 s recording with play in 20–50 s and
    /// 70–100 s: hit clicks in the audio, high motion, two players moving.
    fn synthetic() -> (Vec<f32>, Vec<f64>, detect::DetectionTrack) {
        let in_play = |t: f64| (20.0..50.0).contains(&t) || (70.0..100.0).contains(&t);
        let sr = media::SEGMENT_SAMPLE_RATE as usize;
        let mut seed: u32 = 12345;
        let mut noise = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1 << 24) as f32 - 0.5
        };
        let samples: Vec<f32> = (0..120 * sr)
            .map(|i| {
                let t = i as f64 / sr as f64;
                let since_hit = t % 0.9;
                let click = if in_play(t) && since_hit < 0.03 {
                    0.6 * (1.0 - since_hit / 0.03) as f32
                } else {
                    0.0
                };
                0.01 * noise() + click * noise().signum()
            })
            .collect();
        let motion = (0..600)
            .map(|i| {
                let t = (i + 1) as f64 / 5.0;
                if in_play(t) {
                    6.0 + (t * 3.0).sin()
                } else {
                    1.0 + 0.1 * (t * 0.7).sin()
                }
            })
            .collect();
        let person = |x: f64| detect::Box {
            x: x as f32,
            y: 0.4,
            w: 0.1,
            h: 0.3,
            score: 0.9,
        };
        let detect = detect::DetectionTrack {
            fps: 3.0,
            samples: (0..360)
                .map(|i| {
                    let t = i as f64 / 3.0;
                    if in_play(t) {
                        vec![
                            person(0.3 + 0.1 * (t * 2.0).sin()),
                            person(0.6 + 0.1 * (t * 1.7).cos()),
                        ]
                    } else {
                        vec![person(0.45)]
                    }
                })
                .collect(),
        };
        (samples, motion, detect)
    }

    /// The teacher's canned draft — no other segmenter draws a 1–2 s rally here.
    fn teacher_rallies() -> Vec<Rally> {
        vec![Rally {
            start_ms: 1000,
            end_ms: 2000,
            confidence: 0.75,
        }]
    }

    /// Fake passes over [`synthetic`] signals; each can be made to fail, and every
    /// pass that ran is logged.
    struct Fake {
        audio_fails: bool,
        teacher_fails: bool,
        motion_fails: bool,
        no_detections: bool,
        student_model_fails: bool,
        ran: Vec<&'static str>,
    }

    impl Fake {
        fn new() -> Fake {
            Fake {
                audio_fails: false,
                teacher_fails: false,
                motion_fails: false,
                no_detections: false,
                student_model_fails: false,
                ran: Vec::new(),
            }
        }
    }

    impl Passes for Fake {
        fn audio(&mut self) -> Result<Vec<f32>, String> {
            self.ran.push("audio");
            if self.audio_fails {
                return Err("no audio stream".into());
            }
            Ok(synthetic().0)
        }

        fn teacher(&mut self, on_progress: &mut dyn FnMut(i64)) -> Result<Vec<Rally>, String> {
            self.ran.push("teacher");
            on_progress(5000);
            if self.teacher_fails {
                return Err("no shuttle model".into());
            }
            Ok(teacher_rallies())
        }

        fn motion(&mut self, on_progress: &mut dyn FnMut(i64)) -> Result<Vec<f64>, String> {
            self.ran.push("motion");
            on_progress(7000);
            if self.motion_fails {
                return Err("decode failed".into());
            }
            Ok(synthetic().1)
        }

        fn detections(&mut self) -> Option<detect::DetectionTrack> {
            self.ran.push("detections");
            (!self.no_detections).then(|| synthetic().2)
        }

        fn student_model(&mut self) -> Result<&'static student::StudentModel, String> {
            if self.student_model_fails {
                return Err("bad model file".into());
            }
            student::StudentModel::bundled()
        }
    }

    #[derive(Default)]
    struct Recorded(Vec<String>);

    impl Progress for Recorded {
        fn pass_started(&mut self) {
            self.0.push("pass".into());
        }
        fn processed(&mut self, footage_ms: i64) {
            self.0.push(footage_ms.to_string());
        }
    }

    fn pin(rallies: &[Rally]) -> Vec<(i64, i64, u64)> {
        rallies
            .iter()
            .map(|r| (r.start_ms, r.end_ms, r.confidence.to_bits()))
            .collect()
    }

    // Pinned before the drafting code moved out of the media worker; confidences as
    // bits: 0.604, 0.502 (student), 1.0 (v6).
    const STUDENT: [(i64, i64, u64); 2] = [
        (19466, 26266, 4603615567895139975),
        (69866, 75766, 4602696833571156394),
    ];
    const V6: [(i64, i64, u64); 2] = [
        (19968, 49664, 4607182418800017408),
        (70144, 99840, 4607182418800017408),
    ];

    /// The student's and v6's drafts on fixed synthetic signals, bit for bit: moving
    /// how the student's inputs are assembled must not move a single edge (ADR 0019
    /// parity with `scripts/student.py`).
    #[test]
    fn drafts_on_synthetic_signals_are_pinned() {
        for (chosen, want) in [(Segmenter::Student, STUDENT), (Segmenter::V6, V6)] {
            let d = draft_with(&mut Fake::new(), chosen, &mut Recorded::default(), "x").unwrap();
            assert_eq!(d.made_by, chosen);
            assert_eq!(pin(&d.rallies), want, "{chosen:?}");
            assert_eq!(d.duration_ms, 120_000);
            assert_eq!(d.waveform, segment::waveform(&synthetic().0));
        }
        let signals = student::AudioSignals::of(&synthetic().0);
        assert_eq!(signals.duration_ms, 120_000);
    }

    /// The fallback chain (ADR 0019): a segmenter that cannot run hands over to the
    /// next, the draft is stamped with the one that drew it, and only a missing signal
    /// every segmenter needs fails the recording.
    #[test]
    fn the_fallback_chain_stamps_the_segmenter_that_drew_the_draft() {
        type Setup = fn(&mut Fake);
        /// Name, chosen segmenter, what fails, who draws (`None`: no draft), passes run.
        type Case = (
            &'static str,
            Segmenter,
            Setup,
            Option<Segmenter>,
            &'static [&'static str],
        );
        let ok: Setup = |_| {};
        let cases: [Case; 11] = [
            (
                "teacher draws",
                Segmenter::Teacher,
                ok,
                Some(Segmenter::Teacher),
                &["audio", "teacher"],
            ),
            (
                "teacher fails → student",
                Segmenter::Teacher,
                |f| f.teacher_fails = true,
                Some(Segmenter::Student),
                &["audio", "teacher", "motion", "detections"],
            ),
            (
                "teacher and student model fail → v6",
                Segmenter::Teacher,
                |f| (f.teacher_fails, f.student_model_fails) = (true, true),
                Some(Segmenter::V6),
                &["audio", "teacher", "motion", "detections"],
            ),
            (
                "teacher fails, no detections → v6",
                Segmenter::Teacher,
                |f| (f.teacher_fails, f.no_detections) = (true, true),
                Some(Segmenter::V6),
                &["audio", "teacher", "motion", "detections"],
            ),
            (
                "student draws",
                Segmenter::Student,
                ok,
                Some(Segmenter::Student),
                &["audio", "motion", "detections"],
            ),
            (
                "student model fails → v6",
                Segmenter::Student,
                |f| f.student_model_fails = true,
                Some(Segmenter::V6),
                &["audio", "motion", "detections"],
            ),
            (
                "no detections → v6",
                Segmenter::Student,
                |f| f.no_detections = true,
                Some(Segmenter::V6),
                &["audio", "motion", "detections"],
            ),
            (
                "v6 draws",
                Segmenter::V6,
                ok,
                Some(Segmenter::V6),
                &["audio", "motion", "detections"],
            ),
            (
                "v6 without detections still draws",
                Segmenter::V6,
                |f| f.no_detections = true,
                Some(Segmenter::V6),
                &["audio", "motion", "detections"],
            ),
            (
                "no audio fails the recording",
                Segmenter::Teacher,
                |f| f.audio_fails = true,
                None,
                &["audio"],
            ),
            (
                "no motion fails the recording",
                Segmenter::Teacher,
                |f| (f.teacher_fails, f.motion_fails) = (true, true),
                None,
                &["audio", "teacher", "motion"],
            ),
        ];
        for (name, chosen, setup, want, ran) in cases {
            let mut fake = Fake::new();
            setup(&mut fake);
            let got = draft_with(&mut fake, chosen, &mut Recorded::default(), "x");
            assert_eq!(fake.ran, ran, "{name}");
            let Some(want) = want else {
                assert!(got.is_err(), "{name}");
                continue;
            };
            let d = got.unwrap();
            assert_eq!(d.made_by, want, "{name}");
            // The rallies are the stamped segmenter's own.
            let drawn = match want {
                Segmenter::Teacher => pin(&teacher_rallies()),
                Segmenter::Student => STUDENT.to_vec(),
                Segmenter::V6 if fake.no_detections => {
                    let (samples, energy, _) = synthetic();
                    let motion = MotionTrack {
                        fps: f64::from(media::MOTION_FPS),
                        energy,
                    };
                    pin(
                        &segment::segment(&samples, media::SEGMENT_SAMPLE_RATE, &motion, None)
                            .rallies,
                    )
                }
                Segmenter::V6 => V6.to_vec(),
            };
            assert_eq!(pin(&d.rallies), drawn, "{name}");
        }
    }

    /// Each timed pass restarts the clock before its ticks; the teacher's ticks and
    /// the motion pass's after it are two passes.
    #[test]
    fn each_pass_restarts_the_progress_clock() {
        let mut fake = Fake::new();
        fake.teacher_fails = true;
        let mut progress = Recorded::default();
        draft_with(&mut fake, Segmenter::Teacher, &mut progress, "x").unwrap();
        assert_eq!(progress.0, ["pass", "5000", "pass", "7000"]);

        let mut progress = Recorded::default();
        draft_with(&mut Fake::new(), Segmenter::Teacher, &mut progress, "x").unwrap();
        assert_eq!(progress.0, ["pass", "5000"]);
    }
}
