# The segmenter eval harness

A **dev tool, not app UI** (ADR 0015): it referees the rally segmenter against
recordings whose timelines a human has corrected into truth (**gold**). Given a
gold corpus, it re-runs the *current* segmenter on each recording and prints the
three acceptance-bar numbers ADR 0015 set, so a change to the segmenter is judged
by evidence instead of by eye.

This is the referee for the eval-gated replacement: **Stage 1's "re-measure" and
Stage 2's "built only if Stage 1 misses the bar" both read off this tool.** It is
the companion to [tuning the segmenter](tuning-segmentation.md) — tuning changes
the numbers, the harness tells you whether they got better.

## The acceptance bar (ADR 0015)

The three numbers, in the order the ADR ranks their cost:

| Metric | Definition | Target |
| --- | --- | --- |
| **Missed rallies** | Gold rallies with **no** overlapping draft rally | **≈ 0 per session** (the hard constraint) |
| **False positives / hour** | Draft rallies overlapping **no** gold rally, per hour of footage | ≤ 5–10 / hour (a bounded one-keystroke cost) |
| **Median boundary error** | Median of the start/end offsets between each matched gold rally and its best-overlapping draft, in seconds | within ~1 s (bounded) |

The errors are **not symmetric**: a missed rally hides play with no marker on the
timeline (findable only by scrubbing raw footage), while a false positive or a
loose edge is a one-keystroke correction. So recall is the constraint and
everything else is optimization — a candidate that misses 3% of rallies is worse
than one that over-includes 20% downtime. Read the missed-rally count first.

## Building the gold corpus

A recording is **gold** when its timeline has been hand-corrected. There is no
separate "verified" flag: an inline correction (adjust, split, merge, add) stamps
the [confidence sentinel](../src-tauri/src/db/timeline.rs) on the rally it
touches, and the harness treats any recording carrying that sentinel as gold. A
recording with no hand-corrected rally is **skipped and reported**, never scored
against an unverified draft.

So the corpus is just your reviewed footage:

1. In the app, open each recording you want in the corpus and correct its whole
   timeline — fix every boundary, add the missed rallies, delete the false
   positives (the five inline operations of issue #7).
2. That's it. The corrected timeline is the gold; the harness finds it by the
   sentinel.

> Per ADR 0015 the current ~5 hours of footage is corrected fully — **including
> the noisy recordings** — as the corpus. The corpus doubles as tuning data, so
> its numbers are *evidence, not proof*: newly imported footage the segmenter has
> never been tuned against stays the real test.

## Running it

The harness is a second binary in the `src-tauri` crate. From the repo root:

```bash
cd src-tauri
cargo run --bin eval-harness            # score the whole active library
cargo run --bin eval-harness -- gold.mp4   # score only recordings whose path contains "gold.mp4"
```

With no arguments it reads the app's own database and scores every recording in
the active library.

```
USAGE:
    eval-harness [--db PATH] [--library local|shared] [RECORDING]

    --db PATH              metadata DB to read (default: the app's own DB)
    --library local|shared which library to score (default: the active one)
    RECORDING              path substring; score only matching recordings
    -h, --help             show this help
```

> **ffmpeg/ffprobe must be resolvable.** The harness re-extracts each recording's
> audio and motion, so it needs the bundled sidecars — which it looks for **beside
> its own binary**, exactly as the app does. After a `bun run tauri dev` (or a
> build) has populated `src-tauri/target/debug/` with `ffmpeg`/`ffprobe`, running
> the harness from that same `target` just works. Otherwise put `ffmpeg` and
> `ffprobe` on `PATH` next to the binary.

> **The DB default** is the app's own metadata database for this OS — on Linux
> `~/.local/share/com.quantumff.voloph/voloph.db`. Point `--db` at another copy to
> score a corpus without touching your working library.

## Reading the output

One block per recording, then a corpus aggregate:

```
eval harness — segmenter v1 against 'local' library (/home/you/badminton)

SCORE 2026-05-01/court-a.mp4
        gold 42 | draft 40 | matched 39
        missed rallies        : 3
        false positives       : 1  (0.34 / hour)
        median boundary error : 0.42 s
SKIP  2026-05-01/court-b.mp4  (no hand-corrected timeline — not gold)
SKIP  2026-05-02/warmup.mp4   (not segmented: unknown)
ERR   2026-05-02/broken.mp4   (recording has no decodable audio track)

=== aggregate over 1 scored recording(s) — 2 skipped, 1 error(s) ===
footage scored        : 1.00 h
missed rallies        : 3   (ADR 0015 target ≈ 0)
false positives       : 1  (0.34 / hour)
median boundary error : 0.42 s
```

- **SCORE** — a gold recording that was scored. `matched` is the gold rallies that
  found an overlapping draft (`gold − missed`).
- **SKIP** — not scored, with the reason: no hand-corrected timeline (not gold), or
  not segmented yet. Skips never contribute to the numbers.
- **ERR** — the segmenter could not re-extract the recording (missing file,
  undecodable audio). Also excluded from the numbers.
- The **aggregate** pools every scored recording: missed and false-positive counts
  sum, the per-hour rate is over the total footage scored, and the median boundary
  error is taken over *all* recordings' edge errors at once (not an average of
  per-recording medians).

## The eval-gate loop

The harness re-runs the segmenter from source, so it measures whatever the code
currently does — that is the point. To evaluate a change:

1. Establish the baseline: `cargo run --bin eval-harness` on the gold corpus,
   note the three numbers.
2. Change the segmenter — a threshold in `Default for Params`, or the algorithm
   itself (`src-tauri/src/segment.rs`).
3. **Rebuild and re-run** the harness. Because it re-extracts and re-segments from
   the code, no app re-analysis is needed — and a gold recording's hand-corrected
   timeline is never disturbed.
4. Compare. Accept the change only if missed rallies did not rise; then weigh the
   false-positive and boundary numbers.

> **Why re-run instead of reading the published Analysis?** A gold recording's
> `.vanalysis` file (ADR 0013) is frozen at the machine draft produced *before* the
> human corrected the timeline, and it never regenerates for a hand-touched
> recording. Reading it would only ever score segmenter v1. Re-running from source
> is what lets the harness measure a *changed* segmenter — the whole purpose of the
> gate.

## What lives where

- **Scoring** — the pure `score(draft, gold, duration) → metrics` function in
  [`src-tauri/src/eval.rs`](../src-tauri/src/eval.rs). No DB, no ffmpeg; unit-tested
  with synthetic interval sets. This is the part that must be correct.
- **The shell** — the same file's `run()` plus the thin
  [`src-tauri/src/bin/eval-harness.rs`](../src-tauri/src/bin/eval-harness.rs) entry
  point: gather gold from the DB, re-run the segmenter, print. Deliberately thin
  and untested.
