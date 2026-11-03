#!/usr/bin/env python3
"""The student segmenter's inputs (issue #105): the cheap signals v6 already computes in
the app — motion energy, audio energy + hit onsets, person-detector boxes — dumped by
the `signals-dump` dev binary through the app's own extractors, one file per recording:

    OUT/<name>.<hash12>.sig.npz
        motion_fps, motion      (M,)       mean abs frame difference, sample i at (i+1)/fps s
        audio_frame_ms, audio_energy (A,)  mean-square energy per frame
        audio_onset             (A,)       hit-onset flag per frame
        detect_fps, boxes       (D, K, 5)  x, y, w, h, score (source-normalized), NaN = none
        duration_ms

    student-signals.py --root DIR --out DIR [--filter SUBSTR] [--bin PATH]

Needs `cargo build --features dev-tools --bin signals-dump` in src-tauri first.
Resumable per recording (an existing npz is skipped).
"""

import argparse
import hashlib
import json
import os
import struct
import subprocess
import time
from pathlib import Path

import numpy as np

VIDEO_EXT = {".mov", ".mp4", ".mkv", ".m4v"}
K = 6  # boxes kept per detector sample (highest score first)
REPO = Path(__file__).resolve().parent.parent


def quick_hash(path: Path, size: int) -> str:
    h = hashlib.sha256()
    h.update(struct.pack("<Q", size))
    with open(path, "rb") as f:
        h.update(f.read(64 * 1024))
    return h.hexdigest()


def convert(src: Path, out: Path) -> None:
    d = json.loads(src.read_text())
    det = d["detect"]
    boxes = np.full((len(det["samples"]), K, 5), np.nan, np.float32)
    for i, frame in enumerate(det["samples"]):
        for k, b in enumerate(sorted(frame, key=lambda b: -b["score"])[:K]):
            boxes[i, k] = (b["x"], b["y"], b["w"], b["h"], b["score"])
    tmp = out.with_name(out.name + ".tmp.npz")
    np.savez_compressed(tmp, motion_fps=d["motion"]["fps"], motion=np.array(d["motion"]["energy"], np.float32),
                        audio_frame_ms=d["audio"]["frame_ms"],
                        audio_energy=np.array(d["audio"]["energy"], np.float32),
                        audio_onset=np.array(d["audio"]["onset"], bool),
                        detect_fps=det["fps"], boxes=boxes, duration_ms=d["duration_ms"])
    os.replace(tmp, out)


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--root", type=Path, required=True)
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--filter", default=None)
    ap.add_argument("--bin", type=Path, default=REPO / "src-tauri/target/debug/signals-dump")
    a = ap.parse_args()
    a.out.mkdir(parents=True, exist_ok=True)
    videos = sorted(p for p in a.root.rglob("*") if p.is_file() and p.suffix.lower() in VIDEO_EXT
                    and (a.filter is None or a.filter in str(p)))
    print(f"{len(videos)} recording(s) under {a.root}", flush=True)
    for v in videos:
        out = a.out / f"{v.name}.{quick_hash(v, v.stat().st_size)[:12]}.sig.npz"
        if out.exists():
            print(f"skip  {v.relative_to(a.root)} (done)", flush=True)
            continue
        js = out.with_suffix(".json")
        t0 = time.time()
        r = subprocess.run([str(a.bin), "--file", str(v), "--out", str(js)], capture_output=True, text=True)
        if r.returncode != 0:
            print(f"FAIL  {v.relative_to(a.root)}: {r.stderr.strip()}", flush=True)
            continue
        convert(js, out)
        js.unlink()
        print(f"done  {v.relative_to(a.root)}  {time.time() - t0:.0f}s  {r.stdout.strip()}", flush=True)


if __name__ == "__main__":
    main()
