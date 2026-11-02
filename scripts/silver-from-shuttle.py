#!/usr/bin/env python3
"""Turn teacher shuttle tracks (teacher-tracknet.py CSVs) into *silver* timelines.

Rule (parameters declared before any audit numbers were seen, 2026-11-03):

1. **Static hotspots out.** TrackNet parks on shuttle-like static things (a ceiling
   light in IMG_5930) when no shuttle is airborne. A real shuttle crosses any one
   spot briefly, so an 8 px cell holding >= HOTSPOT_S seconds of detections over the
   whole recording is background; detections in it or its 8 neighbours are dropped.
2. **Sparse noise out.** A frame counts as shuttle-in-play only if >= DENSE_K of the
   DENSE_WIN frames centred on it are surviving detections.
3. **Spans.** In-play frames closer than GAP_S merge into one span (the shuttle leaves
   the top of frame on clears); spans shorter than MIN_SPAN_S are dropped; edges are
   padded by PAD_S each side, clamped to the recording and to neighbours.

Output is the gold-file schema with suffix .silver.json, so eval tooling (and
rally-audit.py score) reads it like any other timeline; the gold loader never does.

    silver-from-shuttle.py --tracks DIR --root RECORDINGS --out DIR [--param k=v ...]
"""

import argparse
import csv
import hashlib
import json
import struct
from collections import Counter
from pathlib import Path

FPS = 30
PARAMS = {
    "HOTSPOT_S": 10.0,
    "CELL_PX": 8,
    "DENSE_WIN": 10,
    "DENSE_K": 3,
    "GAP_S": 2.5,
    "MIN_SPAN_S": 3.0,
    "PAD_S": 0.5,
}
VIDEO_EXT = {".mov", ".mp4", ".mkv", ".m4v"}


def quick_hash(path: Path, size: int) -> str:
    h = hashlib.sha256()
    h.update(struct.pack("<Q", size))
    with open(path, "rb") as f:
        h.update(f.read(64 * 1024))
    return h.hexdigest()


def silver(rows: list[tuple[int, int, int]], n_frames: int, p: dict) -> list[list[int]]:
    """rows: (visible, x, y) per frame. Returns [[start_ms, end_ms], ...]."""
    cp = p["CELL_PX"]
    cells = Counter((x // cp, y // cp) for v, x, y in rows if v)
    hot = {c for c, n in cells.items() if n >= p["HOTSPOT_S"] * FPS}
    near_hot = {(cx + dx, cy + dy) for cx, cy in hot for dx in (-1, 0, 1) for dy in (-1, 0, 1)}
    det = [1 if v and (x // cp, y // cp) not in near_hot else 0 for v, x, y in rows]

    # Dense in-play flag via a sliding window count.
    half = p["DENSE_WIN"] // 2
    prefix = [0]
    for d in det:
        prefix.append(prefix[-1] + d)
    play = []
    for i in range(n_frames):
        lo, hi = max(0, i - half), min(n_frames, i + half)
        play.append(det[i] and (prefix[hi] - prefix[lo]) >= p["DENSE_K"])

    spans = []
    gap = p["GAP_S"] * FPS
    start = last = None
    for i, on in enumerate(play):
        if not on:
            continue
        if start is None:
            start = last = i
        elif i - last <= gap:
            last = i
        else:
            spans.append((start, last))
            start = last = i
    if start is not None:
        spans.append((start, last))

    out = []
    pad = p["PAD_S"] * 1000
    end_ms = n_frames * 1000 / FPS
    for s, e in spans:
        if (e - s + 1) / FPS < p["MIN_SPAN_S"]:
            continue
        a = max(0.0, s * 1000 / FPS - pad)
        b = min(end_ms, (e + 1) * 1000 / FPS + pad)
        if out and a < out[-1][1]:
            a = out[-1][1]  # padding never overlaps the previous span
        if b > a:
            out.append([int(a), int(b)])
    return out


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--tracks", type=Path, required=True)
    ap.add_argument("--root", type=Path, required=True, help="recordings, to resolve full identity")
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--param", action="append", default=[], help="override, e.g. GAP_S=3")
    a = ap.parse_args()
    p = dict(PARAMS)
    for kv in a.param:
        k, v = kv.split("=", 1)
        if k not in p:
            raise SystemExit(f"unknown param {k}")
        p[k] = type(p[k])(v)

    ident = {}
    for v in a.root.rglob("*"):
        if v.is_file() and v.suffix.lower() in VIDEO_EXT:
            size = v.stat().st_size
            qh = quick_hash(v, size)
            ident[f"{v.name}.{qh[:12]}"] = (str(v.relative_to(a.root)), size, qh)

    a.out.mkdir(parents=True, exist_ok=True)
    for t in sorted(a.tracks.glob("*.ball.csv")):
        key = t.name[: -len(".ball.csv")]
        if key not in ident:
            print(f"skip  {t.name} (no matching recording under {a.root})")
            continue
        rel, size, qh = ident[key]
        rows = [(int(r["visible"]), int(r["x"]), int(r["y"])) for r in csv.DictReader(open(t))]
        rallies = silver(rows, len(rows), p)
        doc = {
            "format": 1, "recording": rel, "file_size": size, "quick_hash": qh,
            "duration_ms": round(len(rows) * 1000 / FPS), "verified_unix": 0,
            "rallies": rallies, "source": {"teacher": "TrackNetV3", "params": p},
        }
        out = a.out / f"{Path(rel).name}.{qh[:12]}.silver.json"
        out.write_text(json.dumps(doc, indent=1) + "\n")
        print(f"SILVER {rel}  {len(rallies)} spans → {out}")


if __name__ == "__main__":
    main()
