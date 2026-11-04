#!/usr/bin/env python3
"""Run a person/pose detector (a second *teacher*) over a folder of recordings.

The shuttle track alone can't tell a rally from the shuttle being knocked back
between points (silver v1/v2, issue #100). Players can: between points they walk
and the server stands still; in a rally both move fast. This samples each
recording at POSE_FPS, runs torchvision's Keypoint R-CNN (BSD-3; COCO 17
keypoints), and stores the top people per frame:

    OUT/<name>.<hash12>.pose.npz
        t_ms   (F,)          frame time
        boxes  (F, K, 4)     x1,y1,x2,y2 at W x H, NaN where no person
        scores (F, K)        person score, 0 where none
        kps    (F, K, 17, 3) keypoint x, y, score

Resumable per recording (an existing npz is skipped). Recordings are identified by
file size + quick hash, like the app and teacher-tracknet.py.

    teacher-pose.py --root DIR --out DIR [--filter SUBSTR]   (run with the teacher venv)
"""

import argparse
import hashlib
import os
import queue
import struct
import subprocess
import threading
import time
from pathlib import Path

import numpy as np
import torch
from torchvision.models.detection import KeypointRCNN_ResNet50_FPN_Weights, keypointrcnn_resnet50_fpn

VIDEO_EXT = {".mov", ".mp4", ".mkv", ".m4v"}
POSE_FPS = 5
W, H = 640, 360
K = 4  # people kept per frame (2 players + slack for spectators/other courts)
MIN_SCORE = 0.3
BATCH = 8


def quick_hash(path: Path, size: int) -> str:
    h = hashlib.sha256()
    h.update(struct.pack("<Q", size))
    with open(path, "rb") as f:
        h.update(f.read(64 * 1024))
    return h.hexdigest()


def frames(video: Path, q: queue.Queue) -> None:
    """Decode in a thread so the GPU never waits on ffmpeg. None marks the end."""
    p = subprocess.Popen(
        ["ffmpeg", "-v", "error", "-i", str(video), "-vf", f"fps={POSE_FPS},scale={W}:{H}",
         "-an", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"],
        stdout=subprocess.PIPE)
    size = W * H * 3
    while True:
        buf = p.stdout.read(size)
        if len(buf) < size:
            break
        q.put(np.frombuffer(buf, np.uint8).reshape(H, W, 3).copy())
    q.put(None)
    if p.wait() != 0:
        q.put(RuntimeError(f"ffmpeg failed on {video}"))


def run(model, video: Path, out: Path) -> int:
    q: queue.Queue = queue.Queue(maxsize=64)
    th = threading.Thread(target=frames, args=(video, q), daemon=True)
    th.start()
    boxes, scores, kps = [], [], []
    done = False
    while not done:
        batch = []
        while len(batch) < BATCH:
            f = q.get()
            if f is None:
                done = True
                break
            batch.append(f)
        if not batch:
            break
        with torch.no_grad():
            ims = [torch.from_numpy(f).cuda().permute(2, 0, 1).float() / 255 for f in batch]
            res = model(ims)
        for r in res:
            keep = (r["scores"] >= MIN_SCORE).nonzero().flatten()[:K]
            b = np.full((K, 4), np.nan, np.float32)
            s = np.zeros(K, np.float32)
            k = np.zeros((K, 17, 3), np.float32)
            n = len(keep)
            b[:n] = r["boxes"][keep].cpu().numpy()
            s[:n] = r["scores"][keep].cpu().numpy()
            k[:n] = r["keypoints"][keep].cpu().numpy()
            k[:n, :, 2] = r["keypoints_scores"][keep].cpu().numpy()
            boxes.append(b)
            scores.append(s)
            kps.append(k)
    th.join()
    if not q.empty() and isinstance(err := q.get(), RuntimeError):
        raise err
    n = len(boxes)
    tmp = out.with_name(out.name + ".tmp.npz")
    np.savez_compressed(tmp, t_ms=np.round(np.arange(n) * 1000 / POSE_FPS).astype(np.int64),
                        boxes=np.array(boxes), scores=np.array(scores), kps=np.array(kps),
                        fps=POSE_FPS, size=np.array([W, H]))
    os.replace(tmp, out)
    return n


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--root", type=Path, required=True)
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--filter", default=None)
    a = ap.parse_args()
    a.out.mkdir(parents=True, exist_ok=True)
    model = keypointrcnn_resnet50_fpn(weights=KeypointRCNN_ResNet50_FPN_Weights.DEFAULT,
                                      min_size=H, max_size=W).eval().cuda()
    videos = sorted(p for p in a.root.rglob("*") if p.is_file() and p.suffix.lower() in VIDEO_EXT
                    and (a.filter is None or a.filter in str(p)))
    print(f"{len(videos)} recording(s) under {a.root}", flush=True)
    for v in videos:
        size = v.stat().st_size
        out = a.out / f"{v.name}.{quick_hash(v, size)[:12]}.pose.npz"
        if out.exists():
            print(f"skip  {v.relative_to(a.root)} (done)", flush=True)
            continue
        t0 = time.time()
        n = run(model, v, out)
        print(f"done  {v.relative_to(a.root)}  {n} frames  {time.time() - t0:.0f}s → {out.name}", flush=True)


if __name__ == "__main__":
    main()
