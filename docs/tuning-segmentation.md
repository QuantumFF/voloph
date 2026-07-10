# Tuning the rally segmenter

The draft timeline is produced by a **local, hybrid heuristic** — visual motion
as the primary boundary signal, audio hit-density as confirmation (ADR 0006) —
not a learned model. Its quality on real badminton footage depends on a handful
of thresholds that must be **tuned by a human**; an agent can't judge whether a
boundary is right. This is the human-in-the-loop step of issue #3.

> **Assumes a roughly static camera** (tripod/propped). Frame-differencing
> measures motion under a fixed frame; if your camera pans or is handheld the
> motion signal breaks and these knobs won't save it — that needs a different
> approach (ADR 0006).

All thresholds live in one place: the `Default for Params` block in
[`src-tauri/src/segment.rs`](../src-tauri/src/segment.rs). The algorithm reads
them; you only change the numbers.

## The iteration loop

1. **Edit** a threshold in `Default for Params` (`src-tauri/src/segment.rs`).
2. **Run** the app: `bun run tauri dev`.
3. **Open a recording** and watch the timeline strip under the player:
   - Rally blocks are laid out over the full recording; gaps are the space
     between them. **Amber** blocks are uncertain (low confidence).
   - The vertical **playhead** marker shows where you are — play through and
     check blocks line up with actual rallies.
   - Hover any block for its `start–end · confidence`.
   - The Rust log prints `media worker: segmented <path> into N rallies`.
4. **Re-analyze** to apply the new params to that recording: click the
   **Re-analyze** button by the strip. It re-runs segmentation in place — no
   re-import, no re-transcode — and the strip refreshes when it finishes.
5. Repeat until the boundaries look right on several real recordings.

> Re-analyze only re-segments the **open** recording. To reset everything, clear
> all timelines at the DB level:
> ```bash
> sqlite3 ~/.local/share/com.quantumff.voloph/voloph.db \
>   "DELETE FROM rallies; UPDATE recordings SET segment_state='unknown';"
> ```
> then restart the app — the worker re-segments on launch.

## How the two signals combine

**Motion decides where rallies are, where their edges fall, and *whether* a span
is kept at all.** **Audio only steers confidence** — how sure we are a kept span
is real play (ADR 0015, issue #79). Audio never deletes a span: under the
zero-miss bar no single signal may drop a rally on its own, so the worst a
hit-less moving span suffers is being marked uncertain (amber), not removed. So
tune motion first to get the spans and boundaries right, then use the audio knobs
only to move spans between "confident" and "uncertain".

### Motion — the play/gap decision and boundaries

| Symptom | Knob (default) | Direction |
| --- | --- | --- |
| Misses rallies; edges start late / end early | `motion_active_ratio` (1.1) | ↓ lower — but keep it **above 1.0**: at ≤ 1.0 a flat, no-play recording reads as one giant rally (the threshold falls to the clip's own mean) |
| Gaps (players milling, walking to net) detected as play | `motion_active_ratio` | ↑ raise |
| Boundaries feel coarse (≈0.5 s steps) | `block_ms` (500) | ↓ lower (costs CPU) |

If motion fails to separate play from gaps at *any* `motion_active_ratio`, the
frames themselves likely lack contrast — check the static-camera assumption, or
that the court fills enough of the frame for player movement to register.

### Audio — the confidence modulator

These knobs steer **confidence, not inclusion** (issue #79). A moving span is
always kept; audio decides whether it reads as confident or uncertain (amber).

| Symptom | Knob (default) | Direction |
| --- | --- | --- |
| Real rallies wrongly flagged uncertain ("not enough hits") | `confirm_onsets_per_sec` (0.2) | ↓ lower |
| Non-rally movement reads as confident play | `confirm_onsets_per_sec` | ↑ raise |
| Unconfirmed spans not amber enough / too amber | `unconfirmed_confidence` (0.3) | ↓ lower = more doubtful |
| Hits not registering (so real rallies read uncertain) | `onset_ratio` (2.2) | ↓ lower |
| Spurious hits in near-silence | `onset_floor_ratio` (0.75) | ↑ raise |

A span whose onset density reaches `confirm_onsets_per_sec` keeps its
motion-derived confidence; one below it is kept but capped at
`unconfirmed_confidence`, which sits below the review UI's uncertain threshold
(0.5) so it reliably surfaces as amber. `frame` (1024 ≈ 64 ms) and `baseline_ms`
(1000) set the grain of hit detection and rarely need touching. Audio can't tell
a knock-up apart from a real rally (both have hits) — that is a review-time call,
which is exactly why audio now only nudges confidence and never deletes.

### Structure — merging and trimming (applies to the motion spans)

| Symptom | Knob (default) | Direction |
| --- | --- | --- |
| One rally shattered into fragments | `bridge_gap_ms` (2900) | ↑ raise |
| A rally and the downtime after it fused into one | `bridge_gap_ms` | ↓ lower |
| Serve or final shot clipped off | `pad_ms` (1200) | ↑ raise |
| Brief movements become "rallies" | `min_rally_ms` (1500) | ↑ raise — effective in whole `block_ms`-grid steps (~512 ms): values that truncate to a single block let any one flickering block through |

The "uncertain" amber styling is controlled separately by `UNCERTAIN_CONFIDENCE`
(0.5) in
[`src/components/recording-player.tsx`](../src/components/recording-player.tsx) —
a review-UX dial (how much gets flagged "check this"), not a detection threshold.

## Guiding principle: err toward inclusion

Per ADR 0002, the two error types are **not symmetric**. Dropping a real rally
loses footage the user wanted (lossy, bad); keeping a little downtime costs a few
seconds of dead air (annoying, recoverable). So when a pass is borderline, bias
toward over-inclusion:

- keep `motion_active_ratio` and `confirm_onsets_per_sec` on the **low** side, and
- keep `bridge_gap_ms` / `pad_ms` on the **generous** side.

A timeline that occasionally keeps a bit of downtime is a better default than one
that clips play. The user hand-corrects the draft during review either way.
