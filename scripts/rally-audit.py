#!/usr/bin/env python3
"""Rally audit — a cheap, teacher-independent truth sample for the eval loop.

Instead of hand-correcting whole timelines, a human labels a few hundred random
*moments*: "is a rally in progress at this instant?" Because the moments are drawn
uniformly over the footage, not from any model's output, the same labels can score
every teacher and student. They give per-moment precision/recall with error bars;
rally-level numbers (misses, FP/h, boundary error) still need gold files
(`eval-harness --export-gold`).

Recordings are identified the way the app and gold files do it, by
(file size, quick hash) where quick hash = sha256(size as u64 LE + first 64 KiB), so
labels survive moving the recordings.

    rally-audit.py sample --root DIR [--n 150] [--seed 1]   # draw moments
    rally-audit.py label  --root DIR                        # label them (mpv)
    rally-audit.py score  --root DIR --pred DIR             # score a timeline set
    rally-audit.py status                                   # progress
    rally-audit.py fix N  --root DIR                        # re-answer moment #N
    rally-audit.py list                                     # every answer, numbered
    rally-audit.py reset                                    # clear labels, keep moments

Labels live in eval/audit/moments.json (override with --file). `label` saves after
every answer and resumes where it left off.
"""

import argparse
import hashlib
import json
import math
import os
import random
import struct
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
DEFAULT_FILE = REPO / "eval" / "audit" / "moments.json"
VIDEO_EXT = {".mov", ".mp4", ".mkv", ".m4v"}
LEAD_IN_S = 4.0  # seconds of context played before the judged instant
EDGE_MARGIN_S = 5.0  # never sample this close to either end of a recording

KEYS = """\
y quit 10
n quit 11
u quit 12
r quit 13
q quit 14
ESC quit 14
b quit 15
BS quit 15
"""
EXIT_LABEL = {10: "rally", 11: "no_rally", 12: "unsure"}
EXIT_REPLAY, EXIT_STOP, EXIT_BACK = 13, 14, 15
SHOW = {"rally": "RALLY (y)", "no_rally": "NO RALLY (n)", "unsure": "UNSURE (u)"}
HELP = "y rally · n no · u unsure · r replay · b back · q stop"


def quick_hash(path: Path, size: int) -> str:
    h = hashlib.sha256()
    h.update(struct.pack("<Q", size))
    with open(path, "rb") as f:
        h.update(f.read(64 * 1024))
    return h.hexdigest()


def duration_s(path: Path) -> float:
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", str(path)],
        capture_output=True, text=True, check=True,
    ).stdout.strip()
    return float(out)


def scan(root: Path) -> list[dict]:
    """Every video under root with its identity and duration, sorted by path."""
    recs = []
    for p in sorted(root.rglob("*")):
        if p.is_file() and p.suffix.lower() in VIDEO_EXT:
            size = p.stat().st_size
            recs.append({
                "recording": str(p.relative_to(root)),
                "file_size": size,
                "quick_hash": quick_hash(p, size),
                "duration_s": duration_s(p),
            })
    return recs


def locate(root: Path) -> dict:
    """(file_size, quick_hash) -> absolute path, for every video under root."""
    out = {}
    for p in root.rglob("*"):
        if p.is_file() and p.suffix.lower() in VIDEO_EXT:
            size = p.stat().st_size
            out[(size, quick_hash(p, size))] = p
    return out


def load(file: Path) -> dict:
    return json.loads(file.read_text())


def save(file: Path, data: dict) -> None:
    file.parent.mkdir(parents=True, exist_ok=True)
    tmp = file.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(data, indent=1) + "\n")
    os.replace(tmp, file)


def cmd_sample(a) -> None:
    if a.file.exists() and not a.force:
        sys.exit(f"{a.file} exists — labels would be lost. Pass --force to redraw.")
    recs = scan(a.root)
    if not recs:
        sys.exit(f"no videos under {a.root}")
    usable = [max(0.0, r["duration_s"] - 2 * EDGE_MARGIN_S) for r in recs]
    total = sum(usable)
    rng = random.Random(a.seed)
    moments = []
    for _ in range(a.n):
        # Uniform over the concatenated usable footage: longer recordings get
        # proportionally more moments.
        x = rng.uniform(0, total)
        for r, u in zip(recs, usable):
            if x < u:
                t = EDGE_MARGIN_S + x
                moments.append({
                    "recording": r["recording"],
                    "file_size": r["file_size"],
                    "quick_hash": r["quick_hash"],
                    "t_ms": int(t * 1000),
                    "label": None,
                })
                break
            x -= u
    moments.sort(key=lambda m: (m["recording"], m["t_ms"]))
    # Label in shuffled order so fatigue/drift doesn't correlate with recording.
    order = list(range(len(moments)))
    rng.shuffle(order)
    save(a.file, {
        "format": 1,
        "question": "Is a rally (serve to point decided) in progress at the final frame? "
                    "Warm-up/knock-up hitting is NOT a rally.",
        "seed": a.seed,
        "footage_hours": round(sum(r["duration_s"] for r in recs) / 3600, 3),
        "label_order": order,
        "moments": moments,
    })
    print(f"drew {len(moments)} moments over {len(recs)} recordings "
          f"({total / 3600:.2f} h usable) → {a.file}")


def gui_env() -> dict:
    """The environment for mpv, with a display attached. Some terminals (e.g. ones
    an IDE or agent host spawns) don't inherit WAYLAND_DISPLAY/DISPLAY even though a
    desktop session is running; then fall back to the session's Wayland socket."""
    env = dict(os.environ)
    if env.get("WAYLAND_DISPLAY") or env.get("DISPLAY"):
        return env
    runtime = Path(env.get("XDG_RUNTIME_DIR") or f"/run/user/{os.getuid()}")
    sockets = sorted(p.name for p in runtime.glob("wayland-*") if p.is_socket())
    if sockets:
        env["WAYLAND_DISPLAY"] = sockets[0]
        env.setdefault("XDG_RUNTIME_DIR", str(runtime))
        print(f"(no display in this terminal — using Wayland socket {sockets[0]})")
        return env
    sys.exit("no display found: set WAYLAND_DISPLAY or DISPLAY, or run this from a desktop terminal")


def play(path: Path, t: float, top: str, keyfile: str, env: dict) -> int:
    """Play the LEAD_IN_S before t, freeze on t, return mpv's exit code (the key)."""
    while True:
        code = subprocess.run([
            "mpv", "--no-config", f"--input-conf={keyfile}",
            f"--start={max(0.0, t - LEAD_IN_S):.3f}", f"--end={t:.3f}",
            "--hr-seek=yes", "--keep-open=always", "--hwdec=auto-safe",
            "--osd-level=1", f"--osd-playing-msg={top}", "--osd-duration=600000",
            "--osd-font-size=30", "--osd-align-y=top", "--osd-border-size=3",
            f"--title=rally audit — {top.splitlines()[0]}",
            "--msg-level=all=error", "--term-status-msg=", str(path),
        ], env=env).returncode
        if code != EXIT_REPLAY:
            return code


def cmd_label(a) -> None:
    """Label unlabelled moments in label_order. Every moment has a fixed display
    number (#1..#N, its place in label_order). `b` steps back through earlier
    answers (most recent first) to re-answer them; `fix N` re-asks just #N."""
    data = load(a.file)
    env = gui_env()
    where = locate(a.root)
    moments = data["moments"]
    order = data["label_order"]
    num = {i: k + 1 for k, i in enumerate(order)}
    # `seq` records the order answers were first given, so `b` walks back in time.
    # Labels from before seq existed were given in label_order, so number them so.
    for i in order:
        if moments[i]["label"] is not None and "seq" not in moments[i]:
            moments[i]["seq"] = num[i]
    seq = max((m.get("seq", 0) for m in moments), default=0)

    def answered_before(limit):
        c = [i for i in order if moments[i]["label"] is not None
             and (limit is None or moments[i]["seq"] < limit)]
        return max(c, key=lambda i: moments[i]["seq"]) if c else None

    fix_only = getattr(a, "fix", None)
    if fix_only is not None:
        if not 1 <= fix_only <= len(order):
            sys.exit(f"#{fix_only} is out of range 1..{len(order)}")
        revisit = order[fix_only - 1]
    else:
        revisit = None
    print("Judge only the FROZEN LAST FRAME: is a rally (serve contact → point decided) "
          "in progress? Warm-up is not a rally.")
    print(f"Keys (click the video window first):  {HELP}\n")
    with tempfile.NamedTemporaryFile("w", suffix=".conf", delete=False) as kf:
        kf.write(KEYS)
        keyfile = kf.name
    try:
        while True:
            done = sum(m["label"] is not None for m in moments)
            if revisit is not None:
                i = revisit
            else:
                todo = [i for i in order if moments[i]["label"] is None]
                if not todo:
                    print(f"all {done} moments labelled → {a.file}")
                    return
                i = todo[0]
            m = moments[i]
            path = where.get((m["file_size"], m["quick_hash"]))
            if path is None:
                sys.exit(f"{m['recording']} not found under {a.root}")
            top = f"#{num[i]}   ({done}/{len(moments)} done)   rally at the LAST frame?\n{HELP}"
            if revisit is not None:
                top += f"\n>>> FIXING #{num[i]} — you answered {SHOW[m['label']] if m['label'] else 'nothing'}"
            else:
                prev = answered_before(None)
                if prev is not None:
                    top += f"\nprevious #{num[prev]}: {SHOW[moments[prev]['label']]}   (b = go back & fix)"
            code = play(path, m["t_ms"] / 1000, top, keyfile, env)

            if code == EXIT_BACK:
                if fix_only is not None:
                    continue  # fixing one moment: b just replays it
                limit = m.get("seq") if revisit is not None and m["label"] is not None else None
                target = answered_before(limit)
                if target is None:
                    print("  (nothing earlier to go back to)")
                else:
                    revisit = target
                continue
            if code not in EXIT_LABEL:
                if code != EXIT_STOP:
                    # Closing the window counts as stop; anything else is mpv failing
                    # (no display, bad file) and must not look like a clean stop.
                    print(f"mpv exited with code {code} without an answer "
                          "(window closed, or mpv failed — see any error above)")
                print(f"stopped — {done}/{len(moments)} labelled, saved to {a.file}")
                return
            old = m["label"]
            m["label"] = EXIT_LABEL[code]
            if "seq" not in m:
                seq += 1
                m["seq"] = seq
            save(a.file, data)
            change = f"   (was {old})" if old is not None and old != m["label"] else ""
            print(f"  #{num[i]:<4} {m['recording']} @ {m['t_ms'] / 1000:7.1f}s  → {m['label']}{change}")
            if fix_only is not None:
                return
            revisit = None
    finally:
        os.unlink(keyfile)


def cmd_fix(a) -> None:
    cmd_label(a)


def cmd_list(a) -> None:
    """Every answered moment, numbered, in the order answered — to spot slips."""
    data = load(a.file)
    num = {i: k + 1 for k, i in enumerate(data["label_order"])}
    ms = [(i, m) for i, m in enumerate(data["moments"]) if m["label"] is not None]
    ms.sort(key=lambda im: im[1].get("seq", num[im[0]]))
    for i, m in ms:
        print(f"#{num[i]:<4} {m['label']:9s} {m['recording']} @ {m['t_ms'] / 1000:.1f}s")


def load_pred(pred_dir: Path) -> dict:
    """Timelines in gold-file format (*.json with file_size, quick_hash, rallies)."""
    out = {}
    for p in pred_dir.glob("*.json"):
        d = json.loads(p.read_text())
        out[(d["file_size"], d["quick_hash"])] = [tuple(r) for r in d["rallies"]]
    return out


def wilson(k: int, n: int, z: float = 1.96) -> tuple[float, float]:
    if n == 0:
        return (0.0, 1.0)
    p = k / n
    d = 1 + z * z / n
    c = (p + z * z / (2 * n)) / d
    h = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / d
    return (max(0.0, c - h), min(1.0, c + h))


def cmd_score(a) -> None:
    data = load(a.file)
    pred = load_pred(a.pred)
    tp = fp = fn = tn = uncovered = 0
    for m in data["moments"]:
        if m["label"] not in ("rally", "no_rally"):
            continue
        rallies = pred.get((m["file_size"], m["quick_hash"]))
        if rallies is None:
            uncovered += 1
            continue
        said = any(s <= m["t_ms"] < e for s, e in rallies)
        truth = m["label"] == "rally"
        tp += said and truth
        fp += said and not truth
        fn += (not said) and truth
        tn += (not said) and not truth
    n = tp + fp + fn + tn
    print(f"scored {n} labelled moments ({uncovered} in recordings with no prediction)")
    if n == 0:
        return
    for name, k, d in (("precision", tp, tp + fp), ("recall", tp, tp + fn),
                       ("accuracy", tp + tn, n)):
        lo, hi = wilson(k, d)
        val = f"{k / d:.3f}" if d else "n/a"
        print(f"  {name:9s} {val}  ({k}/{d}, 95% CI {lo:.3f}–{hi:.3f})")
    print(f"  confusion  tp={tp} fp={fp} fn={fn} tn={tn}")


def cmd_reset(a) -> None:
    """Clear every label but keep the drawn moments (same sample, fresh answers)."""
    data = load(a.file)
    n = sum(m["label"] is not None for m in data["moments"])
    for m in data["moments"]:
        m["label"] = None
        m.pop("seq", None)
    save(a.file, data)
    print(f"cleared {n} label(s); {len(data['moments'])} moments kept → {a.file}")


def cmd_status(a) -> None:
    data = load(a.file)
    ms = data["moments"]
    counts = {}
    for m in ms:
        counts[m["label"]] = counts.get(m["label"], 0) + 1
    print(f"{len(ms)} moments over {data['footage_hours']} h: "
          + ", ".join(f"{k or 'unlabelled'}={v}" for k, v in sorted(counts.items(), key=str)))


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--file", type=Path, default=DEFAULT_FILE, help="moments file")
    sub = ap.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("sample")
    s.add_argument("--root", type=Path, required=True)
    s.add_argument("--n", type=int, default=150)
    s.add_argument("--seed", type=int, default=1)
    s.add_argument("--force", action="store_true")
    s = sub.add_parser("label")
    s.add_argument("--root", type=Path, required=True)
    s = sub.add_parser("score")
    s.add_argument("--root", type=Path, required=False)
    s.add_argument("--pred", type=Path, required=True)
    s = sub.add_parser("fix", help="re-answer one moment by its #number")
    s.add_argument("fix", type=int, metavar="NUMBER")
    s.add_argument("--root", type=Path, required=True)
    sub.add_parser("list", help="show every answer, numbered, in the order given")
    sub.add_parser("status")
    sub.add_parser("reset", help="clear all labels, keep the moments")
    a = ap.parse_args()
    {"sample": cmd_sample, "label": cmd_label, "fix": cmd_fix, "list": cmd_list,
     "score": cmd_score, "status": cmd_status, "reset": cmd_reset}[a.cmd](a)


if __name__ == "__main__":
    main()
