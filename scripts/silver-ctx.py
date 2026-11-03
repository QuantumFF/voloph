#!/usr/bin/env python3
"""Silver v2: classify rally vs not from the *context* of the teacher's shuttle track.

Silver v1 (silver-from-shuttle.py) marks a rally wherever a shuttle is airborne. Its
audit precision was 0.39 (issue #100), mostly because players hit the shuttle back to
the server between points, which looks like flight. Single span features (speed,
direction changes, detection density) don't separate the two. The *context* around
a moment does: how play was going over the last 8 s, the next 2 s, and how long since
or until the shuttle disappeared from the track.

v2 is a logistic regression over those context features, trained on the human moment
audit. Training is the only step that needs scikit-learn; the model is saved as plain
JSON (scaler + coefficients), and `predict` is pure numpy.

    silver-ctx.py cv      --audit eval/audit/moments.json --tracks DIR
    silver-ctx.py train   --audit eval/audit/moments.json --tracks DIR --model M.json
    silver-ctx.py predict --tracks DIR --root RECORDINGS --model M.json --out DIR

Integrity: the audit a model is trained on can never score it. The seed-1 audit is the
dev set; the declared model is judged once, on the seed-2 audit, labelled blind.
"""

import argparse
import hashlib
import json
import struct
from collections import Counter
from pathlib import Path

import numpy as np

FPS = 30
STEP = 15  # predict every 0.5 s
HOTSPOT_FRAMES = 300  # 10 s in one 8 px cell = static background (as silver v1)
CELL = 8
WINDOWS_S = [(-8, -4), (-4, -2), (-2, 0), (0, 2)]
GAP_RUN = 45  # 1.5 s with no shuttle = "the shuttle is gone"
GAP_CAP = 900  # look at most 30 s for a gap
PAD = 8 * FPS  # covers the widest window and the gap search horizon per side
C_REG = 0.3
# Dev result (seed-1 audit, leave-one-recording-out, 2026-11-03): AUC 0.95, but the best
# threshold only reaches precision = recall = 0.72, and the learning curve is flat from
# 56 to 141 training moments; adding v6's output as a feature moved it to 0.75. The
# shuttle track alone caps out below the #100 gate (0.80 floor).
#
# With player features from teacher-pose.py (--pose) the same CV reaches precision
# 0.871 / recall 0.844 at threshold 0.40 (player features alone: 0.53).
# DECLARED 2026-11-03, before any seed-2 label existed: eval/models/silver-ctx-pose-v1.json,
# shuttle + pose, trained on all of seed 1, threshold 0.40 (max min(P, R) in CV). It is
# judged once, on the seed-2 audit; changing it afterwards needs a fresh audit.
VIDEO_EXT = {".mov", ".mp4", ".mkv", ".m4v"}


def quick_hash(path: Path, size: int) -> str:
    h = hashlib.sha256()
    h.update(struct.pack("<Q", size))
    with open(path, "rb") as f:
        h.update(f.read(64 * 1024))
    return h.hexdigest()


def load_track(csv_path: Path) -> dict:
    a = np.loadtxt(csv_path, delimiter=",", skiprows=1, ndmin=2)
    vis = a[:, 2] > 0
    x = a[:, 3].astype(int)
    y = a[:, 4].astype(int)
    cx, cy = x // CELL, y // CELL
    cells = Counter(zip(cx[vis].tolist(), cy[vis].tolist()))
    hot = {c for c, n in cells.items() if n >= HOTSPOT_FRAMES}
    near = {(i + dx, j + dy) for i, j in hot for dx in (-1, 0, 1) for dy in (-1, 0, 1)}
    det = vis & np.array([(i, j) not in near for i, j in zip(cx.tolist(), cy.tolist())], bool)
    ok = det[1:] & det[:-1]
    sp = np.zeros(len(a))
    sp[1:][ok] = np.hypot(np.diff(x), np.diff(y))[ok]
    vy = np.zeros(len(a))
    vy[1:][ok] = np.diff(y)[ok]
    return {"det": det, "x": x.astype(float), "y": y.astype(float), "sp": sp, "vy": vy}


def _flips(vy: np.ndarray) -> np.ndarray:
    """1 where the vertical direction reverses versus the last clear move (|vy| > 2)."""
    sg = np.where(np.abs(vy) > 2, np.sign(vy), 0.0)
    idx = np.where(sg != 0, np.arange(len(sg)), -1)
    last = np.maximum.accumulate(idx)
    prev = np.full(len(sg), -1)
    prev[1:] = last[:-1]
    prev_sg = np.where(prev >= 0, sg[np.maximum(prev, 0)], 0.0)
    return ((sg != 0) & (prev_sg != 0) & (sg != prev_sg)).astype(float)


def _gap_dist(det: np.ndarray, frames: np.ndarray) -> tuple[np.ndarray, np.ndarray]:
    """Frames back to / forward to the nearest GAP_RUN-long run with no shuttle,
    capped at GAP_CAP (the recording's edge counts as a gap)."""
    n = len(det)
    nd = (~det).astype(int)
    c = np.concatenate([[0], np.cumsum(nd)])
    starts = np.arange(max(0, n - GAP_RUN + 1))
    full = (c[starts + GAP_RUN] - c[starts]) == GAP_RUN  # det[j:j+GAP_RUN] all empty
    # backward: largest start j with j + GAP_RUN - 1 <= f
    lastj = np.maximum.accumulate(np.where(full, starts, -1)) if len(starts) else np.array([-1])
    q = np.clip(frames - GAP_RUN + 1, -1, len(starts) - 1)
    jb = np.where(q >= 0, lastj[np.maximum(q, 0)], -1)
    back = np.where(jb >= 0, frames - jb, frames + 1)
    # forward: smallest start j >= f, gap end j + GAP_RUN - 1
    nxt = np.full(len(starts) + 1, n, dtype=int)
    for_idx = np.where(full, starts, n)
    nxt[:-1] = np.minimum.accumulate(for_idx[::-1])[::-1] if len(starts) else n
    jf = nxt[np.clip(frames, 0, len(starts))]
    fwd = np.where(jf < n, jf + GAP_RUN - 1 - frames, n - frames)
    return np.minimum(back, GAP_CAP).astype(float), np.minimum(fwd, GAP_CAP).astype(float)


def features(tr: dict, frames: np.ndarray) -> np.ndarray:
    n = len(tr["det"])
    det = np.concatenate([np.zeros(PAD, bool), tr["det"], np.zeros(PAD, bool)])
    sp = np.concatenate([np.zeros(PAD), tr["sp"], np.zeros(PAD)])
    yv = np.where(det, np.concatenate([np.zeros(PAD), tr["y"], np.zeros(PAD)]), np.nan)
    xv = np.where(det, np.concatenate([np.zeros(PAD), tr["x"], np.zeros(PAD)]), np.nan)
    flip = np.concatenate([np.zeros(PAD), _flips(tr["vy"]), np.zeros(PAD)])
    f = np.clip(frames, 0, n - 1) + PAD
    cols = []
    sw = np.lib.stride_tricks.sliding_window_view
    with np.errstate(all="ignore"):
        import warnings
        warnings.simplefilter("ignore", RuntimeWarning)
        for a, b in WINDOWS_S:
            L = (b - a) * FPS
            s = f + a * FPS
            d = sw(det, L)[s]
            q = sw(sp, L)[s]
            yy = sw(yv, L)[s]
            xx = sw(xv, L)[s]
            cols += [
                d.mean(1), (q > 2).mean(1), (q > 8).mean(1), np.percentile(q, 90, axis=1),
                np.nan_to_num(np.nanmin(yy, 1)), np.nan_to_num(np.nanmean(yy, 1)),
                np.nan_to_num(np.nanmax(yy, 1)), np.nan_to_num(np.nanmax(xx, 1) - np.nanmin(xx, 1)),
                sw(flip, L)[s].sum(1),
            ]
    back, fwd = _gap_dist(tr["det"], np.clip(frames, 0, n - 1))
    cols += [back, fwd]
    return np.stack(cols, 1)


PERSON_MIN = 0.7  # pose teacher: people counted
MOVE_STILL = 0.3  # body-heights/s below which a player is "standing still"
# COCO keypoints: 5/6 shoulders, 9/10 wrists.
L_SH, R_SH, L_WR, R_WR = 5, 6, 9, 10


def load_pose(npz: Path) -> dict:
    """Per pose frame: speeds of each matched person (body-heights/s), arm-raised
    flag, people count. People are matched frame to frame by nearest box centre."""
    d = np.load(npz)
    fps = int(d["fps"])
    boxes, scores, kps = d["boxes"], d["scores"], d["kps"]
    F, K = scores.shape
    ok = scores >= PERSON_MIN
    cx = (boxes[..., 0] + boxes[..., 2]) / 2
    cy = (boxes[..., 1] + boxes[..., 3]) / 2
    bh = np.maximum(boxes[..., 3] - boxes[..., 1], 1)
    speed = np.full((F, K), np.nan)
    for f in range(1, F):
        prev = np.where(ok[f - 1])[0]
        if not len(prev):
            continue
        for k in np.where(ok[f])[0]:
            dist = np.hypot(cx[f - 1, prev] - cx[f, k], cy[f - 1, prev] - cy[f, k])
            j = prev[np.argmin(dist)]
            speed[f, k] = dist.min() / ((bh[f, k] + bh[f - 1, j]) / 2) * fps
    wr_y = np.minimum(kps[..., L_WR, 1], kps[..., R_WR, 1])
    sh_y = np.minimum(kps[..., L_SH, 1], kps[..., R_SH, 1])
    raised = ok & (wr_y < sh_y)
    with np.errstate(all="ignore"):
        import warnings
        warnings.simplefilter("ignore", RuntimeWarning)
        srt = -np.sort(-np.nan_to_num(speed, nan=-1), axis=1)  # fastest first
        fast1 = np.where(srt[:, 0] >= 0, srt[:, 0], 0)
        fast2 = np.where(srt[:, 1] >= 0, srt[:, 1], 0)
        slow = np.nanmin(np.where(ok, speed, np.nan), axis=1)
    return {"fps": fps, "n": ok.sum(1).astype(float), "fast1": fast1, "fast2": fast2,
            "still": np.nan_to_num(slow, nan=1.0) < MOVE_STILL, "raised": raised.any(1).astype(float)}


def pose_features(po: dict, t_ms: np.ndarray) -> np.ndarray:
    fps = po["fps"]
    pad = 8 * fps
    sw = np.lib.stride_tricks.sliding_window_view
    P = {k: np.concatenate([np.zeros(pad), po[k].astype(float), np.zeros(pad)])
         for k in ("n", "fast1", "fast2", "still", "raised")}
    f = np.clip(t_ms * fps // 1000, 0, len(po["n"]) - 1) + pad
    cols = []
    for a, b in WINDOWS_S:
        L = (b - a) * fps
        s = f + a * fps
        for k in ("n", "fast1", "fast2", "still", "raised"):
            cols.append(sw(P[k], L)[s].mean(1))
        cols.append(sw(P["fast1"], L)[s].max(1))
    return np.stack(cols, 1)


def pose_index(pose: Path | None) -> dict:
    return {p.name.split(".")[-3]: p for p in pose.glob("*.pose.npz")} if pose else {}


def all_features(tr: dict, frames: np.ndarray, pose_npz: Path | None, shuttle: bool = True) -> np.ndarray:
    parts = [features(tr, frames)] if shuttle else []
    if pose_npz is not None:
        parts.append(pose_features(load_pose(pose_npz), frames * 1000 // FPS))
    return np.hstack(parts)


def track_index(tracks: Path) -> dict:
    return {p.name.split(".")[-3]: p for p in tracks.glob("*.ball.csv")}


GOLD_STEP_MS = 4000  # gold is sampled every 4 s: dense neighbours add little


def audit_xy(audits, tracks: Path, pose: Path | None = None, shuttle: bool = True,
             gold: Path | None = None):
    """Labelled points from moment audits (one or several files) and, optionally,
    gold files sampled every GOLD_STEP_MS. A recording with gold contributes only
    its gold samples (the audit moments in it are the same truth, coarser)."""
    if isinstance(audits, Path):
        audits = [audits]
    idx = track_index(tracks)
    pidx = pose_index(pose)
    by_rec: dict[str, list] = {}
    gold_recs = set()
    for g in sorted(gold.glob("*.gold.json")) if gold else []:
        d = json.loads(g.read_text())
        h = d["quick_hash"][:12]
        gold_recs.add(h)
        R = d["rallies"]
        for t in range(GOLD_STEP_MS // 2, d["duration_ms"], GOLD_STEP_MS):
            lab = "rally" if any(s0 <= t < e0 for s0, e0 in R) else "no_rally"
            by_rec.setdefault(h, []).append({"t_ms": t, "label": lab, "recording": d["recording"]})
    for audit in audits:
        for m in json.loads(Path(audit).read_text())["moments"]:
            h = m["quick_hash"][:12]
            if m.get("label") in ("rally", "no_rally") and h not in gold_recs:
                by_rec.setdefault(h, []).append(m)
    X, Y, G = [], [], []
    for h, ms in by_rec.items():
        tr = load_track(idx[h])
        fr = np.array([m["t_ms"] * FPS // 1000 for m in ms])
        if pose is not None and h not in pidx:
            raise SystemExit(f"no pose file for {ms[0]['recording']} in {pose}")
        X.append(all_features(tr, fr, pidx.get(h), shuttle))
        Y += [m["label"] == "rally" for m in ms]
        G += [h] * len(ms)
    return np.vstack(X), np.array(Y), np.array(G)


def fit(X, Y):
    from sklearn.linear_model import LogisticRegression
    mu, sd = X.mean(0), X.std(0)
    sd[sd == 0] = 1
    clf = LogisticRegression(C=C_REG, max_iter=5000).fit((X - mu) / sd, Y)
    return {"mu": mu.tolist(), "sd": sd.tolist(), "coef": clf.coef_[0].tolist(),
            "intercept": float(clf.intercept_[0])}


def prob(model: dict, X: np.ndarray) -> np.ndarray:
    z = ((X - np.array(model["mu"])) / np.array(model["sd"])) @ np.array(model["coef"]) + model["intercept"]
    return 1 / (1 + np.exp(-z))


def cmd_cv(a) -> None:
    X, Y, G = audit_xy(a.audit, a.tracks, a.pose, not a.no_shuttle, a.gold)
    p = np.zeros(len(Y))
    for g in sorted(set(G)):
        tr = G != g
        p[~tr] = prob(fit(X[tr], Y[tr]), X[~tr])
    print(f"{len(Y)} moments, {Y.sum()} rally, {len(set(G))} recordings — leave-one-recording-out")
    best = None
    for thr in np.arange(0.20, 0.81, 0.05):
        pr = p >= thr
        tp = (pr & Y).sum()
        prec, rec = tp / max(1, pr.sum()), tp / Y.sum()
        print(f"  thr {thr:.2f}  precision {prec:.3f}  recall {rec:.3f}  ({pr.sum()} predicted)")
        if best is None or min(prec, rec) > best[1]:
            best = (round(float(thr), 2), min(prec, rec))
    print(f"max min(precision, recall): thr {best[0]} → {best[1]:.3f}")


def cmd_train(a) -> None:
    X, Y, G = audit_xy(a.audit, a.tracks, a.pose, gold=a.gold)
    m = fit(X, Y)
    m.update({"pose": a.pose is not None, "threshold": a.threshold,
              "trained_on": [str(x) for x in a.audit] + ([str(a.gold)] if a.gold else []), "n": int(len(Y)),
              "n_rally": int(Y.sum()), "step_frames": STEP})
    a.model.parent.mkdir(parents=True, exist_ok=True)
    a.model.write_text(json.dumps(m, indent=1) + "\n")
    print(f"trained on {len(Y)} moments ({Y.sum()} rally) → {a.model}  threshold {a.threshold}")


def spans(on: np.ndarray, n_frames: int) -> list[list[int]]:
    """Per-step decisions → [[start_ms, end_ms]]. Each step covers ±STEP/2 frames;
    a single-step hole inside play is filled."""
    on = on.copy()
    hole = ~on[1:-1] & on[:-2] & on[2:]
    on[1:-1] |= hole
    out = []
    k = 0
    while k < len(on):
        if not on[k]:
            k += 1
            continue
        j = k
        while j + 1 < len(on) and on[j + 1]:
            j += 1
        s = max(0, k * STEP - STEP // 2)
        e = min(n_frames, j * STEP + STEP - STEP // 2)
        out.append([round(s * 1000 / FPS), round(e * 1000 / FPS)])
        k = j + 1
    return out


def cmd_predict(a) -> None:
    model = json.loads(a.model.read_text())
    ident = {}
    for v in a.root.rglob("*"):
        if v.is_file() and v.suffix.lower() in VIDEO_EXT:
            size = v.stat().st_size
            qh = quick_hash(v, size)
            ident[qh[:12]] = (str(v.relative_to(a.root)), size, qh)
    pidx = pose_index(a.pose)
    if model.get("pose") and a.pose is None:
        raise SystemExit("this model uses pose features: pass --pose DIR")
    a.out.mkdir(parents=True, exist_ok=True)
    for h, csv_path in sorted(track_index(a.tracks).items()):
        if h not in ident:
            print(f"skip  {csv_path.name} (no matching recording under {a.root})")
            continue
        rel, size, qh = ident[h]
        tr = load_track(csv_path)
        n = len(tr["det"])
        steps = np.arange(0, n, STEP)
        if model.get("pose") and h not in pidx:
            print(f"skip  {rel} (no pose file)")
            continue
        X = all_features(tr, steps, pidx[h] if model.get("pose") else None)
        pr = prob(model, X)
        rallies = spans(pr >= model["threshold"], n)
        # Per-rally confidence for the app's review (amber below 0.5 = "check this"):
        # the mean model probability over the rally, rescaled so the threshold maps
        # to 0.5 — a rally the model only just called reads as uncertain.
        t_ms = steps * 1000 / FPS
        thr = model["threshold"]
        conf = []
        for s0, e0 in rallies:
            m = float(pr[(t_ms >= s0) & (t_ms < e0)].mean())
            conf.append(round(0.5 + 0.5 * (m - thr) / (1 - thr) if m >= thr else 0.5 * m / thr, 3))
        doc = {"format": 1, "recording": rel, "file_size": size, "quick_hash": qh,
               "duration_ms": round(n * 1000 / FPS), "verified_unix": 0, "rallies": rallies,
               "confidence": conf,
               "source": {"teacher": "TrackNetV3", "rule": "silver-ctx", "model": str(a.model),
                          "threshold": model["threshold"]}}
        out = a.out / f"{Path(rel).name}.{qh[:12]}.silver.json"
        out.write_text(json.dumps(doc, indent=1) + "\n")
        print(f"SILVER {rel}  {len(rallies)} spans → {out}")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("cv")
    s.add_argument("--audit", type=Path, nargs="+", required=True)
    s.add_argument("--gold", type=Path, help="gold-file dir: add its recordings, sampled every 4 s")
    s.add_argument("--tracks", type=Path, required=True)
    s.add_argument("--pose", type=Path, help="teacher-pose.py output: add player features")
    s.add_argument("--no-shuttle", action="store_true", help="player features only (diagnostic)")
    s = sub.add_parser("train")
    s.add_argument("--audit", type=Path, nargs="+", required=True)
    s.add_argument("--gold", type=Path)
    s.add_argument("--tracks", type=Path, required=True)
    s.add_argument("--pose", type=Path)
    s.add_argument("--model", type=Path, required=True)
    s.add_argument("--threshold", type=float, required=True)
    s = sub.add_parser("predict")
    s.add_argument("--tracks", type=Path, required=True)
    s.add_argument("--root", type=Path, required=True)
    s.add_argument("--model", type=Path, required=True)
    s.add_argument("--pose", type=Path)
    s.add_argument("--out", type=Path, required=True)
    a = ap.parse_args()
    {"cv": cmd_cv, "train": cmd_train, "predict": cmd_predict}[a.cmd](a)


if __name__ == "__main__":
    main()
