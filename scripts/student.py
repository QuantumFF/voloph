#!/usr/bin/env python3
"""The student segmenter (issue #100 → #105): a small model on the cheap signals v6
already computes in the app (motion energy, audio energy + hit onsets, person-detector
boxes; student-signals.py), trained on the teacher's silver timelines plus the dev
gold files. Same shape as the teacher (silver-ctx.py): windowed context features →
gradient-boosted trees → smoothing → edge refiner (also boosted trees), all saved as
plain JSON the app can evaluate.

    student.py dev     --signals DIR --silver DIR [--gold eval/gold] --holdout FILE
    student.py train   --signals DIR --silver DIR [--gold eval/gold] --holdout FILE --model M.json
    student.py predict --signals DIR --root RECORDINGS --model M.json --out DIR

`dev` is leave-one-gold-recording-out over the dev gold files, scored with the gate-v2
measures; the --holdout test recordings never reach training or development (#100).

Student v1 (eval/models/student-v1.json, threshold 0.5), declared before scoring: dev
0.980 / 0.905 / 79 %; gate v2 test (three recordings, 146 rallies): GREEN — span precision
0.991 (1.7 invented/h), recall >= 3 s 0.962, edges <= 1 s 81.5 % (teacher v3: 0.992 / 0.971
/ 87 %; v6: 0.757 / 0.990 / 32 %).
"""

import argparse
import hashlib
import importlib.util
import json
import struct
from pathlib import Path

import numpy as np

_spec = importlib.util.spec_from_file_location("sc", Path(__file__).with_name("silver-ctx.py"))
sc = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(sc)

FPS = sc.FPS  # the student's frame grid, so the teacher's edge refiner applies as is
STEP = sc.STEP
WINDOWS_S = [(-8, -4), (-4, -2), (-2, 0), (0, 2), (2, 4)]
PERSON_MIN = 0.5  # detector score for a counted person
MOVE_STILL = 0.3  # body-heights/s
ASPECT = 16 / 9  # source-normalized x → y units for distances (landscape footage)
POST = {"smooth": 5, "merge_ms": 3000, "min_ms": 1500}
SIGNALS = ["motion", "audio_level", "onset", "people", "fast1", "fast2", "still"]
VIDEO_EXT = sc.VIDEO_EXT


def signal_index(d: Path) -> dict:
    return {p.name.split(".")[-3]: p for p in d.glob("*.sig.npz")}


def player_signals(boxes: np.ndarray, fps: float):
    """Per detector sample: people counted, fastest / second-fastest box-centre speed
    (body-heights/s, nearest-centre matching to the previous sample), someone still."""
    D, K, _ = boxes.shape
    ok = np.nan_to_num(boxes[..., 4], nan=0) >= PERSON_MIN
    cx = (boxes[..., 0] + boxes[..., 2] / 2) * ASPECT
    cy = boxes[..., 1] + boxes[..., 3] / 2
    bh = np.maximum(np.nan_to_num(boxes[..., 3], nan=0), 1e-3)
    speed = np.full((D, K), -1.0)
    for f in range(1, D):
        prev = np.where(ok[f - 1])[0]
        if not len(prev):
            continue
        for k in np.where(ok[f])[0]:
            dist = np.hypot(cx[f - 1, prev] - cx[f, k], cy[f - 1, prev] - cy[f, k])
            j = prev[np.argmin(dist)]
            speed[f, k] = dist.min() / ((bh[f, k] + bh[f - 1, j]) / 2) * fps
    srt = -np.sort(-speed, axis=1)
    fast1 = np.maximum(srt[:, 0], 0)
    fast2 = np.maximum(srt[:, 1], 0) if K > 1 else np.zeros(D)
    slow = np.where(ok & (speed >= 0), speed, np.inf).min(1)
    return ok.sum(1).astype(float), fast1, fast2, (slow < MOVE_STILL).astype(float)


def load_signals(npz: Path) -> np.ndarray:
    """(n, len(SIGNALS)) per frame at FPS, each signal held from its own sample rate."""
    d = np.load(npz)
    n = int(d["duration_ms"]) * FPS // 1000
    t = np.arange(n) / FPS  # seconds
    m, mf = d["motion"].astype(float), float(d["motion_fps"])
    mi = np.clip(np.round(t * mf).astype(int) - 1, 0, len(m) - 1)
    motion = np.log((m[mi] + 1e-6) / (np.median(m) + 1e-6))
    e = np.log(d["audio_energy"].astype(float) + 1e-12)
    ai = np.clip((t * 1000 / float(d["audio_frame_ms"])).astype(int), 0, len(e) - 1)
    level = e[ai] - np.median(e)
    onset = d["audio_onset"].astype(float)[ai]
    df = float(d["detect_fps"])
    people, f1, f2, still = player_signals(d["boxes"], df)
    di = np.clip((t * df).astype(int), 0, len(people) - 1)
    return np.stack([np.clip(motion, -5, 5), np.clip(level, -10, 10), onset, people[di],
                     np.minimum(f1[di], 5), np.minimum(f2[di], 5), still[di]], 1)


MAX_COLS = [0, 1, 4, 5]  # motion, audio level, fast1, fast2
STD_COLS = [0, 1]  # motion, audio level
QUIET_S = 2  # a "quiet" stretch: this long with below-median motion and no hit onset
QUIET_CAP_S = 30


def _windows(sig: np.ndarray, frames: np.ndarray, cols: list, fn) -> np.ndarray:
    pad = 8 * FPS
    s = np.vstack([np.zeros((pad, len(cols))), sig[:, cols], np.zeros((pad, len(cols)))])
    out = []
    for a, b in WINDOWS_S:
        V = np.lib.stride_tricks.sliding_window_view(s, (b - a) * FPS, axis=0)
        out.append(fn(V[np.clip(frames + pad + a * FPS, 0, len(V) - 1)], axis=2))
    return np.hstack(out)


def quiet_distance(sig: np.ndarray, frames: np.ndarray) -> np.ndarray:
    """Seconds back to / forward to the nearest quiet stretch, capped — the student's
    version of the teacher's "time since the shuttle vanished"."""
    q = (sig[:, 0] < 0) & (sig[:, 2] == 0)
    n, L = len(q), QUIET_S * FPS
    c = np.concatenate([[0], np.cumsum(q)])
    full = np.zeros(n, bool)
    full[: max(0, n - L + 1)] = (c[L:] - c[:-L]) == L
    idx = np.where(full)[0]
    back = np.full(len(frames), float(QUIET_CAP_S))
    fwd = back.copy()
    if len(idx):
        k = np.searchsorted(idx, frames, side="right") - 1
        back = np.where(k >= 0, np.minimum((frames - idx[np.maximum(k, 0)]) / FPS, QUIET_CAP_S), QUIET_CAP_S)
        k2 = np.searchsorted(idx, frames, side="left")
        fwd = np.where(k2 < len(idx), np.minimum((idx[np.minimum(k2, len(idx) - 1)] - frames) / FPS, QUIET_CAP_S),
                       QUIET_CAP_S)
    return np.stack([back, fwd], 1)


def context_features(sig: np.ndarray, frames: np.ndarray) -> np.ndarray:
    """Per step: window means of every signal, window max / spread of the motion, audio
    and player-speed signals, and the distance to the nearest quiet stretch."""
    pad = 8 * FPS
    s = np.vstack([np.zeros((pad, sig.shape[1])), sig, np.zeros((pad, sig.shape[1]))])
    c = np.vstack([np.zeros((1, s.shape[1])), np.cumsum(s, 0)])
    cols = []
    for a, b in WINDOWS_S:
        lo = np.clip(frames + pad + a * FPS, 0, len(s))
        hi = np.clip(frames + pad + b * FPS, 0, len(s))
        cols.append((c[hi] - c[lo]) / np.maximum(hi - lo, 1)[:, None])
    return np.hstack(cols + [_windows(sig, frames, MAX_COLS, np.max), _windows(sig, frames, STD_COLS, np.std),
                             quiet_distance(sig, frames)])


# Both models are gradient-boosted trees (exported as plain JSON, silver-ctx.export_trees):
# on these weaker signals a linear model plateaus (dev span precision ~0.92) and cannot use
# the silver; boosting reaches ~0.98 and gains from it.
HGB = dict(learning_rate=0.05, max_depth=4, max_iter=300, l2_regularization=1.0, min_samples_leaf=40, random_state=0)


def fit_trees(X: np.ndarray, Y: np.ndarray) -> dict:
    from sklearn.ensemble import HistGradientBoostingClassifier
    return sc.export_trees(HistGradientBoostingClassifier(**HGB).fit(X, Y))


def labels_for(n: int, rallies) -> np.ndarray:
    steps = np.arange(0, n, STEP)
    y = np.zeros(len(steps), bool)
    for s0, e0 in rallies:
        y[(steps >= s0 * FPS // 1000) & (steps < e0 * FPS // 1000)] = True
    return y


def timeline_index(d: Path, suffix: str) -> dict:
    out = {}
    for f in d.glob(f"*{suffix}"):
        t = json.loads(f.read_text())
        out[t["quick_hash"][:12]] = t["rallies"]
    return out


def dataset(a, exclude: set):
    """Per recording: (signals, step labels, label source) — gold where it exists,
    teacher silver elsewhere, nothing from `exclude`."""
    sigs = signal_index(a.signals)
    gold = timeline_index(a.gold, ".gold.json") if a.gold else {}
    silver = timeline_index(a.silver, ".silver.json")
    data = {}
    for h, p in sigs.items():
        if h in exclude:
            continue
        src = "gold" if h in gold else "silver" if h in silver else None
        if src is None:
            continue
        sig = load_signals(p)
        R = gold[h] if src == "gold" else silver[h]
        data[h] = (sig, labels_for(len(sig), R), src, R)
    return data


def edge_model(data: dict, keys):
    X, Y = [], []
    for h in keys:
        sig, _, _, R = data[h]
        n = len(sig)
        RR = [(s0 * FPS // 1000, e0 * FPS // 1000) for s0, e0 in R]
        fr = np.array(sorted({f for s0, e0 in RR for t in (s0, e0) for f in range(t - 90, t + 91, 3) if 0 <= f < n}))
        X.append(sc.edge_features(sig, fr))
        Y += [any(s0 <= f < e0 for s0, e0 in RR) for f in fr]
    m = fit_trees(np.vstack(X), np.array(Y))
    m["n"] = len(Y)
    return m


def fit_model(data: dict, keys, edge_keys, threshold: float) -> dict:
    X = np.vstack([context_features(data[h][0], np.arange(0, len(data[h][0]), STEP)) for h in keys])
    Y = np.concatenate([data[h][1] for h in keys])
    m = fit_trees(X, Y)
    m.update({"threshold": threshold, "post": POST, "signals": SIGNALS, "windows_s": WINDOWS_S,
              "n": int(len(Y)), "n_rally": int(Y.sum())})
    if edge_keys:
        m["edge"] = edge_model(data, edge_keys)
    return m


def predict(m: dict, sig: np.ndarray) -> list[list[int]]:
    n = len(sig)
    steps = np.arange(0, n, STEP)
    pr = sc.prob(m, context_features(sig, steps))
    post = m["post"]
    if post.get("smooth", 1) > 1:
        pr = np.convolve(pr, np.ones(post["smooth"]) / post["smooth"], "same")
    R = sc.tidy(sc.spans(pr >= m["threshold"], n), post["merge_ms"], post["min_ms"])
    if m.get("edge"):
        R = [r for r in sc.refine_edges(m["edge"], sig, R) if r[1] - r[0] >= post["min_ms"]]
    return R, pr, steps


def gate_measures(pairs) -> str:
    spans = inv = l_n = l_f = s_n = s_f = 0
    edges = []
    for R, G in pairs:
        spans += len(R)
        inv += sum(not any(a < ge and b > gs for gs, ge in G) for a, b in R)
        for gs, ge in G:
            ov = [(min(b, ge) - max(a, gs), a, b) for a, b in R if a < ge and b > gs]
            if ge - gs >= 3000:
                l_n += 1
                l_f += bool(ov)
            else:
                s_n += 1
                s_f += bool(ov)
            if ov:
                _, a, b = max(ov)
                edges += [abs(a - gs) <= 1000, abs(b - ge) <= 1000]
    return (f"G1 span precision {1 - inv / max(1, spans):.3f} ({inv}/{spans} invented)  G2 recall ≥3 s {l_f / max(1, l_n):.3f} "
            f"({l_f}/{l_n})  G3 edges ≤1 s {np.mean(edges):.0%}  short found {s_f}/{s_n}")


def cmd_dev(a) -> None:
    data = dataset(a, sc.holdout_hashes(a.holdout))
    golds = [h for h in data if data[h][2] == "gold"]
    print(f"{len(data)} recordings ({len(golds)} gold, {len(data) - len(golds)} silver), "
          f"{sum(len(v[1]) for v in data.values())} steps")
    for thr in a.thresholds:
        pairs, pairs_raw = [], []
        for g in golds:
            keys = [h for h in data if h != g and (a.with_silver or data[h][2] == "gold")]
            ekeys = [h for h in golds if h != g] + ([h for h in data if data[h][2] == "silver"] if a.silver_edges else [])
            m = fit_model(data, keys, ekeys if a.edges else [], thr)
            R, _, _ = predict(m, data[g][0])
            pairs.append((R, data[g][3]))
        print(f"thr {thr:.2f}: {gate_measures(pairs)}")


def cmd_train(a) -> None:
    data = dataset(a, sc.holdout_hashes(a.holdout))
    golds = [h for h in data if data[h][2] == "gold"]
    m = fit_model(data, list(data), golds + ([h for h in data if data[h][2] == "silver"] if a.silver_edges else []),
                  a.threshold)
    m["held_out"] = str(a.holdout)
    a.model.parent.mkdir(parents=True, exist_ok=True)
    a.model.write_text(json.dumps(m, indent=1) + "\n")
    print(f"trained on {m['n']} steps ({m['n_rally']} rally) from {len(data)} recordings "
          f"({len(golds)} gold) + edge refiner → {a.model}")


def quick_hash(path: Path, size: int) -> str:
    h = hashlib.sha256()
    h.update(struct.pack("<Q", size))
    with open(path, "rb") as f:
        h.update(f.read(64 * 1024))
    return h.hexdigest()


def cmd_predict(a) -> None:
    m = json.loads(a.model.read_text())
    ident = {}
    for v in a.root.rglob("*"):
        if v.is_file() and v.suffix.lower() in VIDEO_EXT:
            size = v.stat().st_size
            qh = quick_hash(v, size)
            ident[qh[:12]] = (str(v.relative_to(a.root)), size, qh)
    a.out.mkdir(parents=True, exist_ok=True)
    for h, p in sorted(signal_index(a.signals).items()):
        if h not in ident:
            continue
        rel, size, qh = ident[h]
        sig = load_signals(p)
        R, pr, steps = predict(m, sig)
        t_ms = steps * 1000 / FPS
        thr = m["threshold"]
        conf = []
        for s0, e0 in R:
            sel = pr[(t_ms >= s0) & (t_ms < e0)]
            v = float(sel.mean()) if len(sel) else thr
            conf.append(round(0.5 + 0.5 * (v - thr) / (1 - thr) if v >= thr else 0.5 * v / thr, 3))
        doc = {"format": 1, "recording": rel, "file_size": size, "quick_hash": qh,
               "duration_ms": len(sig) * 1000 // FPS, "verified_unix": 0, "rallies": R, "confidence": conf,
               "source": {"student": str(a.model), "threshold": thr, "post": m["post"]}}
        out = a.out / f"{Path(rel).name}.{qh[:12]}.student.json"
        out.write_text(json.dumps(doc, indent=1) + "\n")
        print(f"STUDENT {rel}  {len(R)} spans → {out}")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    for name in ("dev", "train"):
        s = sub.add_parser(name)
        s.add_argument("--signals", type=Path, required=True)
        s.add_argument("--silver", type=Path, required=True)
        s.add_argument("--gold", type=Path, default=Path("eval/gold"))
        s.add_argument("--holdout", type=Path, required=True)
        s.add_argument("--no-silver-edges", dest="silver_edges", action="store_false",
                       help="train the edge refiner on gold edges only")
        if name == "dev":
            s.add_argument("--thresholds", type=float, nargs="+", default=[0.4, 0.5, 0.6])
            s.add_argument("--no-silver", dest="with_silver", action="store_false", help="gold only (diagnostic)")
            s.add_argument("--no-edges", dest="edges", action="store_false")
        else:
            s.add_argument("--threshold", type=float, required=True)
            s.add_argument("--model", type=Path, required=True)
    s = sub.add_parser("predict")
    s.add_argument("--signals", type=Path, required=True)
    s.add_argument("--root", type=Path, required=True)
    s.add_argument("--model", type=Path, required=True)
    s.add_argument("--out", type=Path, required=True)
    a = ap.parse_args()
    {"dev": cmd_dev, "train": cmd_train, "predict": cmd_predict}[a.cmd](a)


if __name__ == "__main__":
    main()
