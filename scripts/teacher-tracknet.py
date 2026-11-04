#!/usr/bin/env python3
"""Run the TrackNetV3 shuttle tracker (the *teacher*) over a folder of recordings.

Each recording is cut into fixed-length 30 fps proxies at TrackNet's input size
(512x288) so memory stays bounded (TrackNet's in-memory mode holds every frame of
its input). TrackNet runs per chunk, and the per-frame shuttle tracks are stitched
into one CSV per recording:

    OUT/<name>.<hash12>.ball.csv   columns: frame,t_ms,visible,x,y   (x,y at 512x288)

Resumable: a recording whose CSV exists is skipped, and finished chunks are kept
until the recording is stitched. Recordings are identified like the app does it
(file size + quick hash), so outputs survive moving the recordings.

TrackNetV3 (MIT, github.com/qaz812345/TrackNetV3) lives outside the repo; point
--tracknet at its checkout with ckpts/ and a venv python that has torch.

    teacher-tracknet.py --root DIR --out DIR --tracknet ~/.cache/voloph-teacher/TrackNetV3 \
        --python ~/.cache/voloph-teacher/.venv/bin/python [--work DIR] [--filter SUBSTR]
"""

import argparse
import csv
import hashlib
import os
import shutil
import struct
import subprocess
import sys
import time
from pathlib import Path

VIDEO_EXT = {".mov", ".mp4", ".mkv", ".m4v"}
FPS = 30
CHUNK_S = 120
W, H = 512, 288


def quick_hash(path: Path, size: int) -> str:
    h = hashlib.sha256()
    h.update(struct.pack("<Q", size))
    with open(path, "rb") as f:
        h.update(f.read(64 * 1024))
    return h.hexdigest()


def make_chunks(video: Path, work: Path) -> list[Path]:
    """30 fps 512x288 proxies, CHUNK_S long, with a keyframe forced at every cut so
    chunk k starts exactly at frame k * CHUNK_S * FPS."""
    done = work / "chunks.done"
    if not done.exists():
        for p in work.glob("chunk_*.mp4"):
            p.unlink()
        subprocess.run([
            "ffmpeg", "-v", "error", "-y", "-i", str(video),
            "-vf", f"fps={FPS},scale={W}:{H}", "-an",
            "-c:v", "libx264", "-preset", "veryfast", "-crf", "18",
            "-force_key_frames", f"expr:gte(t,n_forced*{CHUNK_S})",
            "-f", "segment", "-segment_time", str(CHUNK_S), "-reset_timestamps", "1",
            str(work / "chunk_%04d.mp4"),
        ], check=True)
        done.touch()
    return sorted(work.glob("chunk_*.mp4"))


def run_tracknet(chunk: Path, a) -> Path:
    csv_path = chunk.with_name(chunk.stem + "_ball.csv")
    if csv_path.exists():
        return csv_path
    tn = a.tracknet
    env = dict(os.environ, TRACKNET_WORKERS=str(a.workers))
    r = subprocess.run([
        str(a.python), "predict.py",
        "--video_file", str(chunk),
        "--tracknet_file", "ckpts/TrackNet_best.pt",
        "--inpaintnet_file", "ckpts/InpaintNet_best.pt",
        "--save_dir", str(chunk.parent),
        "--eval_mode", a.eval_mode,
    ], cwd=tn, env=env, capture_output=True, text=True)
    if r.returncode != 0 or not csv_path.exists():
        sys.exit(f"TrackNet failed on {chunk}:\n{r.stderr[-2000:]}")
    return csv_path


def stitch(csvs: list[Path], out: Path, duration_s: float) -> int:
    """Concatenate chunk tracks. The segment muxer can only cut on a keyframe and
    sometimes cuts late (IMG_5930's first chunk ran 3850 frames, not 3600), so each
    chunk is offset by the *actual* frame count before it — the chunks are one
    contiguous re-encode, so that is exact. The total is checked against the
    recording's duration so a dropped or duplicated chunk can never mis-time it."""
    tmp = out.with_suffix(".csv.tmp")
    expected = duration_s * FPS
    base = written = 0
    with open(tmp, "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["frame", "t_ms", "visible", "x", "y"])
        for c in csvs:
            rows = list(csv.DictReader(open(c)))
            for r in rows:
                fr = base + int(r["Frame"])
                if fr >= expected:
                    continue  # tail padding past the recording's end (see below)
                w.writerow([fr, round(fr * 1000 / FPS), r["Visibility"], r["X"], r["Y"]])
                written += 1
            base += len(rows)
    # Variable-frame-rate sources (IMG_0046: 12494 real frames, avg 29.86 fps) come
    # out of the fps filter a little long — the excess is padding at the END
    # (verified frame-exact against the source at 3/100/200/300/410 s), so it is
    # trimmed above. Short output, or a long tail, means something real is wrong.
    if base < expected - FPS or base > expected + 3 * FPS:
        tmp.unlink()
        sys.exit(f"{out.name}: stitched {base} frames, expected ~{expected:.0f} from duration")
    os.replace(tmp, out)
    return written


def duration_s(path: Path) -> float:
    return float(subprocess.run(
        ["ffprobe", "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", str(path)],
        capture_output=True, text=True, check=True).stdout.strip())


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--root", type=Path, required=True)
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--tracknet", type=Path, required=True)
    ap.add_argument("--python", type=Path, required=True)
    ap.add_argument("--work", type=Path, default=Path.home() / ".cache" / "voloph-teacher" / "work")
    ap.add_argument("--workers", type=int, default=2)
    ap.add_argument("--eval-mode", default="weight", choices=["weight", "average", "nonoverlap"],
                    help="TrackNet's temporal ensemble: weight/average run one 8-frame window per "
                         "frame; nonoverlap one per 8 frames (8x cheaper — what an app could afford)")
    ap.add_argument("--filter", default=None)
    a = ap.parse_args()
    a.out.mkdir(parents=True, exist_ok=True)

    videos = sorted(p for p in a.root.rglob("*") if p.is_file() and p.suffix.lower() in VIDEO_EXT
                    and (a.filter is None or a.filter in str(p)))
    print(f"{len(videos)} recording(s) under {a.root}", flush=True)
    for v in videos:
        size = v.stat().st_size
        qh = quick_hash(v, size)
        out = a.out / f"{v.name}.{qh[:12]}.ball.csv"
        if out.exists():
            print(f"skip  {v.relative_to(a.root)} (done)", flush=True)
            continue
        work = a.work / f"{v.name}.{qh[:12]}"
        work.mkdir(parents=True, exist_ok=True)
        t0 = time.time()
        chunks = make_chunks(v, work)
        csvs = []
        for i, c in enumerate(chunks):
            csvs.append(run_tracknet(c, a))
            print(f"  {v.name} chunk {i + 1}/{len(chunks)}  {time.time() - t0:6.0f}s", flush=True)
        n = stitch(csvs, out, duration_s(v))
        shutil.rmtree(work)  # our own proxies/chunk CSVs; the stitched CSV is kept
        print(f"done  {v.relative_to(a.root)}  {n} frames  {time.time() - t0:.0f}s → {out}", flush=True)


if __name__ == "__main__":
    main()
