#!/usr/bin/env python3
"""Score timelines against the rally-level gate v2 (issue #100, approved 2026-11-04).

    gate2.py --gold eval/gold --pred DIR [--baseline DIR] [--holdout eval/test/gate2-holdout.json]

Scores every recording that has both a gold file in --gold and a timeline in --pred
(any *.json with file_size/quick_hash/rallies; matched by quick hash), restricted to
the --holdout test set when given. --baseline (v6 drafts) is scored the same way for G4.

A predicted span *finds* a gold rally if they overlap; a predicted span overlapping no
gold rally is *invented*.

    G1 span precision (share of predicted spans that are real)  green >= 0.95, red < 0.90
    G2 recall on gold rallies >= 3 s                             green >= 0.90, red < 0.80
    G3 edges within 1 s (starts + ends, found rallies)           green >= 75 %, red < 60 %
    G4 span precision above the baseline                         required

Green = all green; red = any red; gray otherwise. Rallies under 3 s are reported, not
gated. Wilson 95 % intervals are printed for the proportions.
"""

import argparse
import json
import math
from pathlib import Path

LONG_MS = 3000
EDGE_MS = 1000


def wilson(k: int, n: int) -> str:
    if n == 0:
        return "n/a"
    z = 1.96
    p = k / n
    d = 1 + z * z / n
    c = (p + z * z / (2 * n)) / d
    h = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / d
    return f"{c - h:.3f}–{c + h:.3f}"


def timelines(d: Path) -> dict:
    out = {}
    for f in d.glob("*.json"):
        t = json.loads(f.read_text())
        if "quick_hash" in t and "rallies" in t:
            out[t["quick_hash"]] = t
    return out


def score(gold: dict, pred: dict, keys: list) -> dict:
    s = dict(spans=0, invented=0, hours=0.0, long=0, long_found=0, short=0, short_found=0, edges=0, edges_ok=0)
    for k in keys:
        G, R = gold[k]["rallies"], pred[k]["rallies"]
        s["hours"] += gold[k]["duration_ms"] / 3.6e6
        s["spans"] += len(R)
        s["invented"] += sum(not any(a < ge and b > gs for gs, ge in G) for a, b in R)
        for gs, ge in G:
            ov = [(min(b, ge) - max(a, gs), a, b) for a, b in R if a < ge and b > gs]
            key = "long" if ge - gs >= LONG_MS else "short"
            s[key] += 1
            s[key + "_found"] += bool(ov)
            if ov:
                _, a, b = max(ov)
                s["edges"] += 2
                s["edges_ok"] += (abs(a - gs) <= EDGE_MS) + (abs(b - ge) <= EDGE_MS)
    return s


def band(v: float, green: float, red: float) -> str:
    return "green" if v >= green else "red" if v < red else "gray"


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--gold", type=Path, required=True)
    ap.add_argument("--pred", type=Path, required=True)
    ap.add_argument("--baseline", type=Path)
    ap.add_argument("--holdout", type=Path)
    a = ap.parse_args()
    gold = {g["quick_hash"]: g for g in (json.loads(f.read_text()) for f in a.gold.glob("*.gold.json"))}
    pred = timelines(a.pred)
    keys = sorted(k for k in gold if k in pred)
    if a.holdout:
        test = {r["quick_hash"] for r in json.loads(a.holdout.read_text())["recordings"]}
        missing = sorted(gold[k]["recording"] if k in gold else k[:12] for k in test if k not in gold or k not in pred)
        if missing:
            raise SystemExit(f"test set incomplete — no gold or no prediction for: {', '.join(missing)}")
        keys = sorted(test)
    if not keys:
        raise SystemExit("no recording has both a gold file and a prediction")
    print("recordings: " + ", ".join(gold[k]["recording"] for k in keys))
    s = score(gold, pred, keys)
    p1 = 1 - s["invented"] / s["spans"]
    r2 = s["long_found"] / s["long"]
    e3 = s["edges_ok"] / s["edges"]
    verdicts = [band(p1, 0.95, 0.90), band(r2, 0.90, 0.80), band(e3, 0.75, 0.60)]
    print(f"G1 span precision  {p1:.3f}  ({s['spans'] - s['invented']}/{s['spans']}, 95% CI "
          f"{wilson(s['spans'] - s['invented'], s['spans'])})  {s['invented'] / s['hours']:.1f} invented/h  → {verdicts[0]}")
    print(f"G2 recall ≥ 3 s    {r2:.3f}  ({s['long_found']}/{s['long']}, 95% CI {wilson(s['long_found'], s['long'])})  → {verdicts[1]}")
    print(f"G3 edges ≤ 1 s     {e3:.3f}  ({s['edges_ok']}/{s['edges']})  → {verdicts[2]}")
    print(f"   short rallies (< 3 s) found {s['short_found']}/{s['short']} (not gated)")
    if a.baseline:
        base = timelines(a.baseline)
        lacking = [gold[k]["recording"] for k in keys if k not in base]
        if lacking:
            raise SystemExit(f"baseline has no timeline for: {', '.join(lacking)}")
        b = score(gold, base, keys)
        bp = 1 - b["invented"] / b["spans"]
        g4 = p1 > bp
        verdicts.append("green" if g4 else "red")
        print(f"G4 vs baseline     span precision {p1:.3f} vs {bp:.3f}  → {'green' if g4 else 'red'}   "
              f"(baseline: recall ≥ 3 s {b['long_found'] / b['long']:.3f}, edges ≤ 1 s {b['edges_ok'] / b['edges']:.3f}, "
              f"{b['invented'] / b['hours']:.1f} invented/h)")
    else:
        print("G4 not scored (no --baseline)")
    verdict = "RED" if "red" in verdicts else "GREEN" if all(v == "green" for v in verdicts) and a.baseline else "GRAY"
    print(f"VERDICT: {verdict}")


if __name__ == "__main__":
    main()
