# Voloph

A desktop app for reviewing badminton games recorded by the player. Its purpose is to make starting a review frictionless: skip the downtime between points and jump straight between meaningful moments. Reviewing is non-destructive — nothing the user does alters a recording; all structure lives as metadata layered over the recordings, which are reviewed in place exactly as they came off the camera.

## Language

**Session**:
The top-level grouping and the unit the user reviews by — one outing, identified by its day. Holds one or more recordings. The day is the recording's embedded capture date (the camera's creation date), falling back to file mtime only when no usable embedded date exists (see ADR 0007); recordings are re-homed to the matching day in the background after import.
_Avoid_: match, day, event

**Recording**:
A single raw video file as it came off the camera, attached to a session. May contain one or several games of play. The app does not model games or matches as entities — they are not navigated by.
_Avoid_: VOD, video, clip, game, match

**Rally**:
A continuous segment of play, roughly serve to point decided. The atomic unit of review — you navigate, annotate, and skip at the granularity of rallies.
_Avoid_: point, clip, segment (when you mean a rally specifically)

**Gap**:
Downtime between rallies — walking to position, retrieving the shuttle, towel/water breaks. Automatically skipped during review. **Derived, never stored**: a gap is simply any span of a recording not inside a rally.
_Avoid_: dead time, downtime, break

**Timeline**:
The set of rally intervals over a single recording (gaps are the derived complement). The central editable artifact: the ML produces a first draft, and the user hand-corrects it inline during review via five operations — adjust boundary, split, merge, add, delete. Playback and export both read from it.
_Avoid_: cut list, EDL, edit

**Uncertain region**:
A span the segmenter marks as low-confidence — where it is unsure whether play is happening or where a rally boundary sits. Surfaced on the timeline during review as "check this," so correcting the draft becomes "visit the few spots the machine doubts" rather than scanning everything. Machine-produced and segmentation-related — distinct from a **Flag** (user-produced, about a rally's review value). Catches *uncertain* errors only, not confident-but-wrong segmentation.
_Avoid_: flag, low-confidence marker, suspect

**Suppressed span**:
A draft span the segmenter judged to be non-play — off the default timeline, so review and export skip it, but never destroyed: revealed on demand during review and restorable to a rally with one operation. The third tier between "rally on the timeline" and "nothing there": it lets the segmenter fight false positives aggressively while no span of play is ever unrecoverable. Distinct from an **Uncertain region**, which stays visible ("check this"); a suppressed span is hidden ("judged non-play — recoverable if the machine was wrong").
_Avoid_: deleted rally, hidden span, rejected span, trash, filtered rally

**Analysis**:
The machine-produced output of segmenting one recording, captured the moment segmentation completes — before any human correction: the draft timeline with per-rally confidence (uncertain regions included), the waveform, the duration, and the capture day. Impersonal — a pure function of the recording and the segmenter, carrying no review state and no attribution, which is why another user may adopt it silently. Hand corrections never enter an Analysis; they travel only inside a session bundle.
_Avoid_: analyzed segments, segmentation results, analysis cache, draft (alone)

**Annotation**:
A single observation pinned to a precise timestamp within a recording (a moment), not to a whole rally — what matters is the specific shot or moment. Its core is a one-keystroke **verdict**, optionally enriched with an **aspect** and a free-text **note**. The rally it belongs to is implied by its timestamp falling within that rally's range. A moment with mixed verdicts (good decision, poor execution) is recorded as more than one annotation at the same timestamp.
_Avoid_: comment, label, marker, tag

**Verdict**:
The classification on an annotation: `good`, `bad`, or `mistake`. The fast thing captured in the moment during review (one keystroke). A **mistake** is an outright, unforced error that ended the point (into the net, out, missed). **bad** is suboptimal but not point-ending (a weak lift, a loose return, poor selection you got away with). **good** is well done. Shot type (smash, drop, serve…) is **not** part of the verdict and is **not** a structured field — it lives in the note.
_Avoid_: tag, rating, quality, label

**Aspect**:
The dimension an annotation's verdict is judging — a structured, filterable field set after the quick verdict. Lets a single moment carry split verdicts across dimensions (good `selection`, mistake on `execution`) and lets reviews be filtered by dimension ("show my selection mistakes"). Seeded with `selection`, `execution`, `deception`, `footwork`, `positioning`; the user can add more from settings. A user-editable vocabulary, not a fixed code enum.
_Avoid_: category, dimension, type

**Flag**:
A one-keystroke mark meaning "this rally matters" — orthogonal to its annotations. Flagged rallies are the source material for an export reel.
_Avoid_: highlight, star, favourite, bookmark

**Rally length**:
Every rally is classified **long** or **short** by its duration against a threshold. Purely objective and automatic — derived from the timeline, carrying no judgment of quality. Used to filter and surface rallies (e.g. "show me the long rallies"). Quality (good/bad) is never inferred from length; it is only ever set manually.
_Avoid_: good/bad, quality, rating

**Library**:
A folder that is the app's whole world of recordings while active — every recording lives under the active library; a video outside it does not exist to the app. There are at most two: one **shared library** on common storage (NAS, cloud-drive mount) that other people and devices reach, and one **local library**, private to this device. A switcher makes one active at a time; the session list, filters, and review all see only the active library, and a session never spans libraries. The user puts files in a library; the app only references them (reference-in-place, ADR 0003, is unchanged). Identity inside a library is by location relative to its folder. For the shared library, each device declares where it is mounted and whether that mount is local or network — per-device knowledge, never shared. Session bundles exist only in the shared library.
_Avoid_: library root, managed library, sync folder, cloud folder, central storage, scanned folder, network library (how a device reaches the shared library is per-device)

**Session bundle**:
A shareable snapshot of one session's review state — timeline, annotations, flags — carrying no video. Its recordings are referenced by their place within the shared library, so a bundle is useful only to a recipient who reaches the same shared storage. **Share** produces a bundle; **receive** applies one: the state becomes the recipient's own review, with no attribution or provenance (a note can carry a signature). Scanning the shared library **discovers** foreign bundles and offers each by session + sharer label; the offer comes before analysis, so accepting it registers the covered recordings straight from the bundle with no probe, segmentation, or staging. Your own bundle is never offered back; a declined bundle stops nagging until the sharer re-shares it (then it returns as an update). Where the recipient has already hand-touched a recording's review, they choose keep-mine-or-take-theirs per recording; nothing merges. A snapshot handed over, not a collaboration channel. Distinct from Export, which renders a watchable video.
_Avoid_: export/import (for metadata), sync, backup, snapshot (alone)

**Review**:
Watching back a recorded game to study one's own play. The primary activity the app exists to support. Non-destructive: navigating and annotating over the original file, never altering it.

**Export**:
A downstream operation that renders one new video file from a **selection of rallies** — stitch the selected rallies together, drop everything else. One engine, varied by selection: all rallies → the condensed session (only gaps removed); flagged rallies → a highlight reel; a filter (e.g. rallies containing a `mistake`) → a targeted study reel. The headline case is the condensed *session* — all rallies across all its recordings, gaps removed, one portable file to watch on any device (requires concatenating cuts across files, i.e. a re-encode). v1 exports clean footage with no burned-in annotations. Distinct from review — review never produces a file.
_Avoid_: render, cut, save (when you mean export); reel and condensed game are not separate features
