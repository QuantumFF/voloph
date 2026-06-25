# Local, audio-first heuristic segmentation

> **Revised by [ADR 0006](0006-hybrid-audio-visual-motion-segmentation.md):** audio alone proved insufficient for rally boundaries in practice. Visual motion is now the primary boundary signal and audio is confirmation. Everything else below — local, GPU-free, heuristic-not-learned, replaceable behind the timeline, per-region confidence, and the inclusion bias — still holds.

Rally/gap segmentation runs **fully locally** and is driven primarily by **audio** (the rhythm and onset density of shuttle hits vs the quiet of a gap), starting as a **tunable heuristic** rather than a trained model.

- **Local**, because a session is hours of video — uploading it is slow, costs money per run, needs a network, and ships footage off-device. The files are already on disk; inference should be too.
- **Audio-first**, because badminton has a clean audio signature and audio analysis is cheap and GPU-free. Visual motion / deep action-recognition models are far heavier and were judged unnecessary for v1.
- **Heuristic, not a learned model**, because it reaches most of the value at a fraction of the effort, and the hand-correctable timeline (see ADR 0001) makes occasional errors cheap to fix.

Known limitation accepted, not designed around: a neighbouring court within earshot can bleed into the audio and cause false positives. This is an outlier; the user corrects it in the timeline.

The segmenter emits **per-region confidence**, not just a play/gap decision. Low-confidence spans surface as "uncertain regions" on the timeline during review, turning correction into "visit the few spots the machine doubts." This catches uncertain errors but not confident-but-wrong ones (e.g. a loud neighbouring court that looks exactly like play), so it is an aid, not a safety net.

**Tuning bias: err toward inclusion.** The product's North Star is watching a session with no gaps, so the two error types are not symmetric — dropping or shortening a real rally loses play the user wanted (lossy, bad), while keeping some downtime only costs a few seconds of dead air (annoying, recoverable). When unsure, the segmenter keeps the span as play. Extra downtime is cheaper than lost play.

The segmenter is a **replaceable component behind the timeline** — it emits a draft timeline and nothing else depends on *how* it was produced. Swapping the heuristic for visual confirmation or a learned model later is a local change, which is what makes this choice low-risk.
