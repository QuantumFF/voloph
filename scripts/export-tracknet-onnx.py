#!/usr/bin/env python3
"""Export TrackNetV3's two networks to ONNX for the app's shuttle tracker (#105).

    export-tracknet-onnx.py [--tracknet ~/.cache/voloph-teacher/TrackNetV3] [--out src-tauri/models]

Reads the checkout's pretrained checkpoints and writes

    tracknet_v3.onnx    TrackNet   x (N, 27, 288, 512) float32 -> y (N, 8, 288, 512)  sigmoid heatmaps
    inpaintnet_v3.onnx  InpaintNet x (N, 16, 2), m (N, 16, 1) float32 -> y (N, 16, 2)  sigmoid coords

with a dynamic batch dimension (opset 17), then checks each file with ONNX Runtime
(CPU) against torch on random input at two batch sizes, and prints the SHA-256s for
src-tauri/models/README.md. The I/O contract is consumed by src-tauri/src/tracknet.rs.

Run with a python that has torch, onnx and onnxruntime (the teacher venv:
~/.cache/voloph-teacher/.venv/bin/python). TrackNetV3 is MIT (github.com/qaz812345/TrackNetV3),
checkpoints included; the checkout is only read.
"""

import argparse
import hashlib
import os
import sys
from pathlib import Path

import numpy as np
import torch


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def check(path: Path, model, make_inputs, names, tol: float) -> None:
    import onnxruntime as ort

    sess = ort.InferenceSession(str(path), providers=["CPUExecutionProvider"])
    for n in (1, 3):
        inputs = make_inputs(n)
        with torch.no_grad():
            ref = model(*inputs).numpy()
        got = sess.run(None, {k: v.numpy() for k, v in zip(names, inputs)})[0]
        err = float(np.abs(ref - got).max())
        print(f"  batch {n}: output {tuple(got.shape)}  max |onnx - torch| = {err:.2e}")
        if got.shape != ref.shape or not err < tol:
            sys.exit(f"{path.name}: ONNX output does not match torch (tolerance {tol})")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--tracknet", type=Path, default=Path.home() / ".cache" / "voloph-teacher" / "TrackNetV3")
    ap.add_argument("--out", type=Path, default=Path(__file__).resolve().parent.parent / "src-tauri" / "models")
    a = ap.parse_args()

    sys.path.insert(0, str(a.tracknet))
    os.chdir(a.tracknet)  # their modules import siblings relative to the checkout
    from utils.general import HEIGHT, WIDTH, get_model  # noqa: E402

    torch.manual_seed(0)
    ck = torch.load(a.tracknet / "ckpts" / "TrackNet_best.pt", map_location="cpu", weights_only=False)
    seq_len, bg_mode = ck["param_dict"]["seq_len"], ck["param_dict"]["bg_mode"]
    assert (seq_len, bg_mode) == (8, "concat"), f"unexpected TrackNet config {seq_len}, {bg_mode}"
    tracknet = get_model("TrackNet", seq_len, bg_mode)
    tracknet.load_state_dict(ck["model"])
    tracknet.eval()
    in_ch = (seq_len + 1) * 3

    ik = torch.load(a.tracknet / "ckpts" / "InpaintNet_best.pt", map_location="cpu", weights_only=False)
    inpaint_len = ik["param_dict"]["seq_len"]
    assert inpaint_len == 16, f"unexpected InpaintNet seq_len {inpaint_len}"
    inpaintnet = get_model("InpaintNet")
    inpaintnet.load_state_dict(ik["model"])
    inpaintnet.eval()

    a.out.mkdir(parents=True, exist_ok=True)
    tn_path = a.out / "tracknet_v3.onnx"
    ip_path = a.out / "inpaintnet_v3.onnx"

    print(f"TrackNet (seq_len {seq_len}, bg_mode {bg_mode}) -> {tn_path}")
    torch.onnx.export(
        tracknet, (torch.rand(1, in_ch, HEIGHT, WIDTH),), str(tn_path),
        input_names=["x"], output_names=["y"], opset_version=17,
        dynamic_axes={"x": {0: "n"}, "y": {0: "n"}}, dynamo=False,
    )
    check(tn_path, tracknet, lambda n: (torch.rand(n, in_ch, HEIGHT, WIDTH),), ["x"], 1e-4)

    print(f"InpaintNet (seq_len {inpaint_len}) -> {ip_path}")
    torch.onnx.export(
        inpaintnet, (torch.rand(1, inpaint_len, 2), torch.ones(1, inpaint_len, 1)), str(ip_path),
        input_names=["x", "m"], output_names=["y"], opset_version=17,
        dynamic_axes={"x": {0: "n"}, "m": {0: "n"}, "y": {0: "n"}}, dynamo=False,
    )
    check(ip_path, inpaintnet,
          lambda n: (torch.rand(n, inpaint_len, 2), (torch.rand(n, inpaint_len, 1) < 0.5).float()),
          ["x", "m"], 1e-5)

    for p in (tn_path, ip_path):
        print(f"{p.name}  {p.stat().st_size / 1e6:.1f} MB  sha256 {sha256(p)}")


if __name__ == "__main__":
    main()
