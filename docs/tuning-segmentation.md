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

## How the signals combine

**Occupancy and motion both *propose* rally spans; motion *places their edges*;
audio only *steers confidence*** (ADR 0015). Occupancy (when a detector runs)
proposes a span where two players are rallying with near/far size structure
(ADR 0016); motion proposes
a span wherever the court moves. The two are unioned — either can add a span,
neither can delete one — and motion's rising/falling envelope sets the block-level
edges. Audio then decides only how sure we are a kept span is real play (issue #79):
under the zero-miss bar no single signal may drop a rally on its own, so the worst a
hit-less moving span suffers is being marked uncertain (amber), not removed. So tune
motion first to get the spans and boundaries right, add the occupancy knobs to
suppress non-play (bystanders, milling), then use the audio knobs only to move spans
between "confident" and "uncertain".

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

### Occupancy — the proposer (ADR 0016, issue #91)

Once a person detector runs (issue #83), **occupancy proposes candidate play
spans**. A detector sample *fires* when it shows two plausible people with
**near/far size structure** — two boxes whose area ratio reaches
`occupancy_ratio`; box area is the depth proxy, and depth is **never** read from
frame height (on real footage both players sit in the same vertical band, #85) —
and real inter-sample movement. A block is proposed when the *firing density* in
a sliding window centered on it reaches `occupancy_density`. Motion still places
the edges of its own spans, and audio still only modulates confidence.

**Occupancy and motion are unioned, never intersected.** A block is a candidate
rally block when motion fired there *or* occupancy proposed it. So occupancy can
only *add* spans motion missed; it can never delete a rally motion found. Under the
zero-miss bar (ADR 0015) no single signal drops a rally on its own — which is also
why a **failed or absent detector is safe**: with no occupancy track the fusion is
exactly the motion-proposes path (see "Degradation" below).

| Symptom | Knob (default) | Direction |
| --- | --- | --- |
| A net-side bystander (spectator, coach) turns downtime into play | `occupancy_static_frac` (0.02) | ↑ raise — a box whose center stays within this fraction of the frame (both axes) over the recording is dropped as furniture; also the per-sample movement floor of the firing rule |
| A real, lightly-moving player gets dropped as "furniture" (or slow rallies never fire) | `occupancy_static_frac` | ↓ lower toward 0 to trust almost every detected box |
| Two people milling at the same depth (no near/far structure) proposed as play | `occupancy_ratio` (1.5) | ↑ raise — demands a starker large-box/small-box area ratio (in-rally median on the gold corpus is 4.4×) |
| Real rallies not proposed because the boxes are too similar in size | `occupancy_ratio` | ↓ lower toward 1.0 (which disables the size test entirely) |
| A near-camera passer-by (huge box) turns downtime into play | `occupancy_area_cap_k` (8.0) | ↓ lower — a box bigger than this multiple of the recording's *median* box area is discarded as implausible |
| A legitimately huge near-player box gets discarded | `occupancy_area_cap_k` | ↑ raise |
| Occupancy spans bleed into the downtime around rallies (edges drift, neighbours glue) | `occupancy_density` (0.5) | ↑ raise — the firing density a block's window must reach; between-rally chatter fires ~32% of samples on the gold corpus, so keep this well above ~0.35 |
| Short rallies motion missed are still missed by occupancy | `occupancy_density` | ↓ lower — **warning**: below ~0.4 the corpus collapses toward glued mega-spans (median edge error blows past 5 s); the sweep found no point that rescues them cleanly |
| Edges of occupancy-only spans feel smeared | `occupancy_window_ms` (3000) | ↓ lower (sharper edges, less gap rejection); at 3 fps keep it ≥ ~1500 so a window holds enough samples for density to mean anything |
| Brief gap chatter proposed as play | `occupancy_window_ms` | ↑ raise (more smoothing) |

One occupancy rule is a fixed constant, not a knob: **`LINK_RADIUS` (0.2)** is how
far apart two detections may be and still be treated as the same player across
frames (used only by the staticness clustering — a player's between-sample step is
far larger than a fixture's jitter). It lives in
[`src-tauri/src/segment.rs`](../src-tauri/src/segment.rs) beside the code that uses
it.

To tune these against the gold corpus, run the harness sweep — it caches each
recording's extracted tracks once and re-scores the pure `segment_with` seam over
the whole occupancy grid:

```bash
cargo run --release --bin eval-harness -- --sweep 21-06-2026
```

Known hard edge (issue #91's sweep): missed rallies and edge quality trade
monotonically. In-rally firing (~40–70% of samples) sits close to between-rally
firing (~32%), so pushing density/window down to rescue the residual missed
rallies (short, 3–5 s) glues neighbouring rallies into mega-spans long before it
gets them all. The chosen defaults take misses 49 → 22 at 58 FP/h and a 1.5 s
median edge; going below ~10 misses on the current corpus costs a ≥ 4 s median.

> **Degradation — a failed detector never loses a rally.** The detector can fail to
> load or run (missing ONNX model, `ort` init failure, ffmpeg error). When it does,
> the occupancy track is absent and the segmenter falls back to **motion-proposes**,
> i.e. exactly the pre-#84 behavior — analysis still completes with a full draft
> timeline. This is enforced at the seam: `segment(…, occupancy: Option<&OccupancyTrack>)`
> accepts `None` (or an empty track) and unions it with motion, so a missing signal
> can only cost precision (a few false positives that occupancy would have suppressed),
> never recall. The `media_worker` and the eval harness both swallow detector failures
> to `None`.

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
