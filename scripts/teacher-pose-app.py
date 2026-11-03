#!/usr/bin/env python3
"""Pose teacher from the stack the app ships (issue #105): YOLOX-Nano + RTMPose-t,
run by the `pose-dump` dev binary, instead of teacher-pose.py's torchvision
Keypoint R-CNN. The rally teacher shipped in the app can only use what the app can
compute, so its pose features must be trained on this.

Writes the same layout as teacher-pose.py so silver-ctx.py reads either:

    OUT/<name>.<hash12>.pose.npz
        t_ms   (F,)          frame time
        boxes  (F, K, 4)     x1,y1,x2,y2 at W x H, NaN where no person
        scores (F, K)        person score (YOLOX obj*cls), 0 where none
        kps    (F, K, 17, 3) keypoint x, y, score

Coordinates are scaled to W x H = 640 x 360 like teacher-pose.py (the app's output
is source-normalized), so body-height-relative features are unchanged.

    teacher-pose-app.py --root DIR --out DIR [--filter SUBSTR] [--bin PATH]
    teacher-pose-app.py --convert JSON --out NPZ        (one existing dump)

Needs `cargo build --features dev-tools --bin pose-dump` in src-tauri first.
Resumable per recording (an existing npz is skipped).
"""

import argparse
import hashlib
import json
import os
import struct
import subprocess
import sys
import time
from pathlib import Path

import numpy as np

VIDEO_EXT = {".mov", ".mp4", ".mkv", ".m4v"}
POSE_FPS = 5
W, H = 640, 360
K = 4
REPO = Path(__file__).resolve().parent.parent


def quick_hash(path: Path, size: int) -> str:
    h = hashlib.sha256()
    h.update(struct.pack("<Q", size))
    with open(path, "rb") as f:
        h.update(f.read(64 * 1024))
    return h.hexdigest()


def convert(src: Path, out: Path) -> int:
    win = json.loads(src.read_text())
    frames = win["frames"]
    F = len(frames)
    boxes = np.full((F, K, 4), np.nan, np.float32)
    scores = np.zeros((F, K), np.float32)
    kps = np.zeros((F, K, 17, 3), np.float32)
    t_ms = np.zeros(F, np.int64)
    for i, fr in enumerate(frames):
        t_ms[i] = fr["t_ms"]
        for k, p in enumerate(fr["players"][:K]):
            b = p["bbox"]
            boxes[i, k] = (b["x"] * W, b["y"] * H, (b["x"] + b["w"]) * W, (b["y"] + b["h"]) * H)
            scores[i, k] = b["score"]
            kps[i, k] = [(q["x"] * W, q["y"] * H, q["score"]) for q in p["keypoints"]]
    tmp = out.with_name(out.name + ".tmp.npz")
    np.savez_compressed(tmp, t_ms=t_ms, boxes=boxes, scores=scores, kps=kps,
                        fps=int(round(win["fps"])), size=np.array([W, H]))
    os.replace(tmp, out)
    return F


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--root", type=Path)
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--filter", default=None)
    ap.add_argument("--bin", type=Path, default=REPO / "src-tauri/target/debug/pose-dump")
    ap.add_argument("--convert", type=Path, help="convert one pose-dump JSON to --out NPZ")
    a = ap.parse_args()
    if a.convert:
        print(f"{convert(a.convert, a.out)} frames → {a.out}")
        return
    if a.root is None:
        sys.exit("--root is required (or --convert)")
    a.out.mkdir(parents=True, exist_ok=True)
    videos = sorted(p for p in a.root.rglob("*") if p.is_file() and p.suffix.lower() in VIDEO_EXT
                    and (a.filter is None or a.filter in str(p)))
    print(f"{len(videos)} recording(s) under {a.root}", flush=True)
    for v in videos:
        out = a.out / f"{v.name}.{quick_hash(v, v.stat().st_size)[:12]}.pose.npz"
        if out.exists():
            print(f"skip  {v.relative_to(a.root)} (done)", flush=True)
            continue
        js = out.with_suffix(".json")
        t0 = time.time()
        r = subprocess.run([str(a.bin), "--file", str(v), "--out", str(js),
                            "--fps", str(POSE_FPS), "--players", str(K)], capture_output=True, text=True)
        if r.returncode != 0:
            print(f"FAIL  {v.relative_to(a.root)}: {r.stderr.strip()}", flush=True)
            continue
        n = convert(js, out)
        js.unlink()
        print(f"done  {v.relative_to(a.root)}  {n} frames  {time.time() - t0:.0f}s → {out.name}", flush=True)


if __name__ == "__main__":
    main()
