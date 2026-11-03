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
# Span post-processing saved into each trained model: moving average over `smooth`
# steps, then threshold, then join spans less than `merge_ms` apart and drop spans
# shorter than `min_ms`. Chosen on the four gold recordings (dense, leave-one-out).
POST = {"smooth": 5, "merge_ms": 3000, "min_ms": 1500}
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
# Seed-2 result: precision 0.773 / recall 0.723 — RED (v6: 0.38 precision).
#
# v2 (2026-11-04): pose from the app's own YOLOX + RTMPose-t (teacher-pose-app.py), plus
# the four hand-verified gold files. Dev, leave-one-recording-out (audits on the 14
# non-gold recordings / gold samples on the 4 gold ones): shuttle only 0.700 / 0.644;
# + R-CNN pose 0.771 / 0.714 with gold; + app pose 0.771 / 0.727, with gold 0.819 /
# 0.741. Gold helps once it spans the four setups, but saturates (1→4 gold recordings:
# 0.800→0.819); regularization, gradient boosting and wider windows are all within
# noise. 69% of gold-sample errors sit within 1.5 s of a rally edge. Smoothing the
# per-step probability (POST below) cuts spurious spans on gold from 31 to 8 at
# rally-time precision 0.79 / recall 0.73 (0.86 / 0.84 ignoring ±1 s of each edge);
# v6 on the same gold: 0.33 / 0.91, 34 spurious.
# DECLARED 2026-11-04, before any seed-3 label existed: eval/models/silver-ctx-v2.json.
# Seed-3 result (150 moments, 14 non-gold recordings): precision 0.838 (31/37) / recall
# 0.738 (31/42) — RED on recall (v6: 0.368 / 0.929; accuracy 0.883 vs 0.517). Of the 11
# missed moments, 6 are within 1 s of a predicted edge and 4 are whole missed rallies;
# 5 of the 6 false ones are within 1.2 s of an edge. Padding spans only trades
# precision for recall on gold (min(P, R) never rises above the unpadded 0.735).
#
# Gate v2 (rally-level, #100, approved 2026-11-04 after seed 3): span precision, recall on
# rallies >= 3 s, edges within 1 s — scored by scripts/gate2.py on the four held-out
# recordings in eval/test/gate2-holdout.json, verified into gold from v6 drafts.
# v3 = v2 + the edge refiner (--edges), trained on seed-1/2/3 audits and the four dev gold
# files with every test recording excluded (--holdout). Dev (gold, leave-one-out): span
# precision 0.963, recall >= 3 s 0.905, edges <= 1 s 80 % (v6: 0.755 / 1.00 / 36 %).
# DECLARED 2026-11-04, before any test gold existed: eval/models/silver-ctx-v3.json; its
# test-set predictions are frozen in eval/test/gate2/v3/.
# Gate v2 result (three test recordings, 146 rallies; Pixel dropped before scoring): GREEN —
# span precision 0.992 (1.7 invented/h), recall >= 3 s 0.971, edges <= 1 s 87 %, above v6
# (0.757, 56.2/h). Short rallies found 20/42. Test gold was verified from v3's drafts.
# In-app cost check (#105): TrackNet's non-overlapping mode (teacher-tracknet.py --eval-mode
# nonoverlap, 8x cheaper) gives near-identical tracks; the v3 recipe retrained on them scores
# 0.968 / 0.952 / 86 % on the same test set (frozen v3 on them: 0.974 / 0.913 / 87 %) — still
# green on every measure (comparison only, the test set was already seen).
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


def holdout_hashes(path: Path | None) -> set[str]:
    """12-char quick hashes of a test set's recordings (eval/test/*-holdout.json),
    which training must never see."""
    if path is None:
        return set()
    return {r["quick_hash"][:12] for r in json.loads(path.read_text())["recordings"]}


def audit_xy(audits, tracks: Path, pose: Path | None = None, shuttle: bool = True,
             gold: Path | None = None, exclude: set[str] = frozenset()):
    """Labelled points from moment audits (one or several files) and, optionally,
    gold files sampled every GOLD_STEP_MS. A recording with gold contributes only
    its gold samples (the audit moments in it are the same truth, coarser).
    Recordings in `exclude` (a held-out test set) contribute nothing."""
    if isinstance(audits, Path):
        audits = [audits]
    idx = track_index(tracks)
    pidx = pose_index(pose)
    by_rec: dict[str, list] = {}
    gold_recs = set(exclude)
    for g in sorted(gold.glob("*.gold.json")) if gold else []:
        d = json.loads(g.read_text())
        h = d["quick_hash"][:12]
        if h in exclude:
            continue
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


# Edge refiner (v3). The context model above works on 0.5 s steps smoothed over 2.5 s,
# so its edges are ~0.9 s off on average — a lot when gold rallies average 4.4 s. A
# second, fine-grained model says "in a rally?" every 0.1 s from local shuttle and
# player signals (EDGE_OFFS bins of EDGE_BIN frames around the frame), trained on
# frames within 3 s of gold edges; each predicted edge then moves to the change point
# within EDGE_WIN frames that best fits those per-frame probabilities.
EDGE_BIN = 6  # 0.2 s
EDGE_OFFS = range(-8, 8)  # -1.6 .. +1.6 s
EDGE_PAD = 60
EDGE_WIN = 60  # search ±2 s around each predicted edge
EDGE_C = 1.0


def edge_signals(tr: dict, po: dict) -> np.ndarray:
    """(n, 8) per-frame signals at 30 fps: shuttle seen, moving, speed, height, hit;
    fastest and second-fastest player, someone standing still (pose upsampled)."""
    n = len(tr["det"])

    def up(v):
        x = np.repeat(np.asarray(v, float), FPS // po["fps"])
        return np.pad(x, (0, max(0, n - len(x))))[:n]

    return np.stack([tr["det"].astype(float), (tr["sp"] > 2).astype(float), np.minimum(tr["sp"], 30) / 30,
                     np.where(tr["det"], tr["y"] / 288, 0), _flips(tr["vy"]),
                     np.minimum(up(po["fast1"]), 5) / 5, np.minimum(up(po["fast2"]), 5) / 5, up(po["still"])], 1)


def edge_features(sig: np.ndarray, frames: np.ndarray) -> np.ndarray:
    s = np.vstack([np.zeros((EDGE_PAD, sig.shape[1])), sig, np.zeros((EDGE_PAD, sig.shape[1]))])
    c = np.vstack([np.zeros((1, s.shape[1])), np.cumsum(s, 0)])
    cols = []
    for o in EDGE_OFFS:
        lo = np.clip(frames + EDGE_PAD + o * EDGE_BIN, 0, len(s))
        hi = np.clip(lo + EDGE_BIN, 0, len(s))
        cols.append((c[hi] - c[lo]) / EDGE_BIN)
    return np.hstack(cols)


def edge_xy(gold: Path, tracks: Path, pose: Path, exclude: set[str]):
    idx, pidx = track_index(tracks), pose_index(pose)
    X, Y = [], []
    for g in sorted(gold.glob("*.gold.json")):
        d = json.loads(g.read_text())
        h = d["quick_hash"][:12]
        if h in exclude:
            continue
        tr = load_track(idx[h])
        n = len(tr["det"])
        sig = edge_signals(tr, load_pose(pidx[h]))
        R = [(s0 * FPS // 1000, e0 * FPS // 1000) for s0, e0 in d["rallies"]]
        fr = np.array(sorted({f for s0, e0 in R for t in (s0, e0) for f in range(t - 90, t + 91, 3) if 0 <= f < n}))
        X.append(edge_features(sig, fr))
        Y += [any(s0 <= f < e0 for s0, e0 in R) for f in fr]
    return np.vstack(X), np.array(Y)


def refine_edges(edge: dict, sig: np.ndarray, rallies: list[list[int]]) -> list[list[int]]:
    n = len(sig)
    out = []
    for s0, e0 in rallies:
        new = []
        for t, is_start in ((s0, True), (e0, False)):
            f0 = t * FPS // 1000
            fr = np.arange(max(0, f0 - EDGE_WIN - 45), min(n, f0 + EDGE_WIN + 45), 3)
            q = np.clip(prob(edge, edge_features(sig, fr)), 1e-3, 1 - 1e-3)
            li, lo = np.log(q), np.log(1 - q)
            best = None
            for k in range(len(fr)):
                if abs(fr[k] - f0) > EDGE_WIN:
                    continue
                ll = lo[:k].sum() + li[k:].sum() if is_start else li[:k].sum() + lo[k:].sum()
                if best is None or ll > best[0]:
                    best = (ll, fr[k])
            new.append(int(best[1] * 1000 / FPS) if best else t)
        if new[1] - new[0] >= 500:
            out.append(new)
    return out


def cmd_train(a) -> None:
    exclude = holdout_hashes(a.holdout)
    X, Y, G = audit_xy(a.audit, a.tracks, a.pose, gold=a.gold, exclude=exclude)
    m = fit(X, Y)
    m.update({"pose": a.pose is not None, "threshold": a.threshold, "post": POST,
              "trained_on": [str(x) for x in a.audit] + ([str(a.gold)] if a.gold else []), "n": int(len(Y)),
              "n_rally": int(Y.sum()), "step_frames": STEP,
              "held_out": str(a.holdout) if a.holdout else None})
    if a.edges:
        if not (a.gold and a.pose):
            raise SystemExit("--edges needs --gold and --pose")
        from sklearn.linear_model import LogisticRegression
        Xe, Ye = edge_xy(a.gold, a.tracks, a.pose, exclude)
        mu, sd = Xe.mean(0), Xe.std(0) + 1e-6
        clf = LogisticRegression(C=EDGE_C, max_iter=5000).fit((Xe - mu) / sd, Ye)
        m["edge"] = {"mu": mu.tolist(), "sd": sd.tolist(), "coef": clf.coef_[0].tolist(),
                     "intercept": float(clf.intercept_[0]), "n": int(len(Ye))}
    a.model.parent.mkdir(parents=True, exist_ok=True)
    a.model.write_text(json.dumps(m, indent=1) + "\n")
    print(f"trained on {len(Y)} moments ({Y.sum()} rally){' + edge refiner' if a.edges else ''}"
          f"{f', {len(exclude)} recordings held out' if exclude else ''} → {a.model}  threshold {a.threshold}")


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


def tidy(rallies: list[list[int]], merge_ms: int, min_ms: int) -> list[list[int]]:
    """Join spans less than `merge_ms` apart, then drop those shorter than `min_ms`."""
    out: list[list[int]] = []
    for s0, e0 in rallies:
        if out and s0 - out[-1][1] < merge_ms:
            out[-1][1] = e0
        else:
            out.append([s0, e0])
    return [r for r in out if r[1] - r[0] >= min_ms]


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
        post = model.get("post", {})
        if post.get("smooth", 1) > 1:
            pr = np.convolve(pr, np.ones(post["smooth"]) / post["smooth"], "same")
        rallies = tidy(spans(pr >= model["threshold"], n), post.get("merge_ms", 0), post.get("min_ms", 0))
        if model.get("edge"):
            sig = edge_signals(tr, load_pose(pidx[h]))
            rallies = [r for r in refine_edges(model["edge"], sig, rallies)
                       if r[1] - r[0] >= post.get("min_ms", 0)]
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
                          "threshold": model["threshold"], "post": model.get("post", {})}}
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
    s.add_argument("--holdout", type=Path, help="test-set file: its recordings never reach training")
    s.add_argument("--edges", action="store_true", help="also train the edge refiner (needs --gold, --pose)")
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
