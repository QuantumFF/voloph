#!/usr/bin/env python3
"""Compare shuttle tracks from the app's TrackNetV3 port (`tracknet-dump`) with the
offline run (`teacher-tracknet.py`), per recording (#105).

    tracknet-parity.py --ref DIR --got DIR

Both directories hold `<recording>.<hash12>.ball.csv` files (`frame,t_ms,visible,x,y`,
x/y at 512x288); files are paired by name. Per pair it prints the frame counts, how
often the visibility flags agree, and — over frames visible in both — the median and
p95 pixel distance and how many frames sit more than 4 px apart. Frames past the
shorter track are ignored by the per-frame columns.
"""

import argparse
import csv
import math
from pathlib import Path


def load(path: Path) -> list[tuple[int, int, int]]:
    with open(path) as f:
        return [(int(r["visible"]), int(r["x"]), int(r["y"])) for r in csv.DictReader(f)]


def pct(xs: list[float], q: float) -> float:
    if not xs:
        return float("nan")
    xs = sorted(xs)
    k = (len(xs) - 1) * q
    lo, hi = math.floor(k), math.ceil(k)
    return xs[lo] + (xs[hi] - xs[lo]) * (k - lo)


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--ref", type=Path, required=True, help="offline CSVs (teacher-tracknet.py)")
    ap.add_argument("--got", type=Path, required=True, help="app CSVs (tracknet-dump)")
    a = ap.parse_args()

    print("| recording | frames ref | frames app | vis agree % | both vis | median px | p95 px | > 4 px |")
    print("|---|---:|---:|---:|---:|---:|---:|---:|")
    tot = {"n": 0, "agree": 0, "both": 0, "far": 0}
    all_d: list[float] = []
    for ref in sorted(a.ref.glob("*.ball.csv")):
        got = a.got / ref.name
        if not got.exists():
            continue
        r, g = load(ref), load(got)
        n = min(len(r), len(g))
        agree = sum(r[i][0] == g[i][0] for i in range(n))
        d = [math.hypot(r[i][1] - g[i][1], r[i][2] - g[i][2]) for i in range(n) if r[i][0] and g[i][0]]
        far = sum(x > 4 for x in d)
        name = ref.name.split(".")[0]
        print(f"| {name} | {len(r)} | {len(g)} | {100 * agree / max(n, 1):.2f} | {len(d)} | "
              f"{pct(d, 0.5):.2f} | {pct(d, 0.95):.2f} | {far} ({100 * far / max(len(d), 1):.2f} %) |")
        tot["n"] += n
        tot["agree"] += agree
        tot["both"] += len(d)
        tot["far"] += far
        all_d += d
    if tot["n"]:
        print(f"| **all** | | {tot['n']} | {100 * tot['agree'] / tot['n']:.2f} | {tot['both']} | "
              f"{pct(all_d, 0.5):.2f} | {pct(all_d, 0.95):.2f} | {tot['far']} "
              f"({100 * tot['far'] / max(tot['both'], 1):.2f} %) |")


if __name__ == "__main__":
    main()
