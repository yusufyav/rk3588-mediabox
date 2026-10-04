#!/usr/bin/env python3
"""Rebuild the OpenCV dnn_superres 2x models in PyTorch and export ONNX.

The weights come from TensorFlow GraphDefs (data only, no pickle); extract.py
turns them into .npz in a TensorFlow venv. The rebuilt graph is checked
against TensorFlow's own output before anything is exported.

The models work on luma only, trained on full-range Y in 0..1. The exported
graphs take NV12 limited-range Y as it comes out of the decoder (0..255) and
give limited-range Y back; the output mapping is folded into the last
convolution, so no full-resolution one-channel tensor is left on the NPU.
Pixel shuffle is left to the CPU, which has to touch the output anyway.

  plain  in 1x1x1080x1920      out 1x4x1080x1920   (2x2 block per input pixel)
  s2d    in 1x4x540x960        out 1x16x540x960    (4x4 block per input 2x2)

s2d is the same network rewritten on 2x2 polyphase components: every stride-1
convolution of the full-resolution graph becomes a 3x3 (or 1x1) convolution
with four times the channels at half resolution, exactly equivalent including
the zero padding at the borders. The NPU is far more efficient on wide
channels than on the one-channel 5x5 first layer.
"""
import argparse
import numpy as np
import torch
import torch.nn as nn
import torch.nn.functional as F


def conv(w, b):
    w = torch.from_numpy(np.ascontiguousarray(w.transpose(3, 2, 0, 1)))
    c = nn.Conv2d(w.shape[1], w.shape[0], w.shape[2], padding=w.shape[2] // 2)
    c.weight.data = w
    c.bias.data = torch.from_numpy(np.broadcast_to(b, (w.shape[0],)).copy())
    return c


def prelu(a):
    p = nn.PReLU(a.shape[0])
    p.weight.data = torch.from_numpy(a.copy())
    return p


class Ref(nn.Module):
    """The published network: full-range Y 0..1 in, 2x Y out."""

    def __init__(self, layers, tanh):
        super().__init__()
        self.body = nn.Sequential(*layers)
        self.tanh = tanh

    def forward(self, x):
        x = F.pixel_shuffle(self.body(x), 2)
        return torch.tanh(x) if self.tanh else x


def reference(name, npz):
    d = np.load(npz)
    if name.startswith('FSRCNN'):
        n = len([k for k in d if k.startswith('f')])
        layers = []
        for i in range(1, n):
            layers += [conv(d[f'f{i}'], d[f'b{i}']), prelu(d[f'alpha{i}'])]
        layers.append(conv(d[f'f{n}'], d[f'b{n}']))
        return Ref(layers, False), d
    layers = [conv(d['f1'], d['b1']), nn.ReLU(), conv(d['f2'], d['b2']), nn.ReLU(), conv(d['f3'], d['b3'])]
    return Ref(layers, True), d


def fold_range(ref):
    """Limited-range Y out: the last conv absorbs 16+219*v (unless a tanh sits in between).

    The input mapping (y-16)/219 stays a separate elementwise op: folding it
    into conv1 would change what conv1 pads its border with, and an explicit
    Pad runs on the CPU in RKNN.
    """
    layers = [m for m in ref.body]
    if not ref.tanh:
        last = layers[-1]
        last.weight.data = last.weight.data * 219.0
        last.bias.data = last.bias.data * 219.0 + 16.0
    return layers


class Plain(nn.Module):
    def __init__(self, layers, tanh):
        super().__init__()
        self.body = nn.Sequential(*layers)
        self.tanh = tanh

    def forward(self, y):
        x = self.body((y - 16.0) * (1.0 / 219.0))
        if self.tanh:
            x = torch.tanh(x) * 219.0 + 16.0
        return torch.clamp(x, 0.0, 255.0)


def polyphase(c):
    """Stride-1 'same' conv on full res -> conv on 2x2 phases at half res.

    Channel order is phase-major: channel = (2*pi + pj) * C + c.
    """
    w = c.weight.data
    co, ci, k, _ = w.shape
    p = k // 2
    taps = [(qi + u - p) for qi in (0, 1) for u in range(k)]
    lo, hi = min(t // 2 for t in taps), max(t // 2 for t in taps)
    K = hi - lo + 1
    assert -lo == hi or K == 1, (lo, hi)
    W = torch.zeros(4 * co, 4 * ci, K, K, dtype=w.dtype)
    for qi in (0, 1):
        for qj in (0, 1):
            for u in range(k):
                for v in range(k):
                    ri, rj = qi + u - p, qj + v - p
                    pi, pj = ri % 2, rj % 2
                    di, dj = ri // 2 - lo, rj // 2 - lo
                    W[(2 * qi + qj) * co:(2 * qi + qj + 1) * co,
                      (2 * pi + pj) * ci:(2 * pi + pj + 1) * ci, di, dj] += w[:, :, u, v]
    n = nn.Conv2d(4 * ci, 4 * co, K, padding=K // 2)
    n.weight.data = W
    n.bias.data = c.bias.data.repeat(4)
    return n


def s2d_layers(layers):
    out = []
    for m in layers:
        if isinstance(m, nn.Conv2d):
            out.append(polyphase(m))
        elif isinstance(m, nn.PReLU):
            p = nn.PReLU(4 * m.weight.numel())
            p.weight.data = m.weight.data.repeat(4)
            out.append(p)
        else:
            out.append(m)
    # last conv: phase-major (2qi+qj)*4 + (2a+b) -> pixel-shuffle(4) order
    # (2qi+a)*4 + (2qj+b)
    last = out[-1]
    perm = [0] * 16
    for qi in (0, 1):
        for qj in (0, 1):
            for a in (0, 1):
                for b in (0, 1):
                    perm[(2 * qi + a) * 4 + (2 * qj + b)] = (2 * qi + qj) * 4 + (2 * a + b)
    last.weight.data = last.weight.data[perm]
    last.bias.data = last.bias.data[perm]
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('name')
    ap.add_argument('npz')
    ap.add_argument('onnx')
    ap.add_argument('--variant', choices=['plain', 's2d'], default='plain')
    ap.add_argument('--hw', default='1080x1920')
    a = ap.parse_args()

    ref, d = reference(a.name, a.npz)
    with torch.no_grad():
        got = ref(torch.from_numpy(d['x'].transpose(0, 3, 1, 2))).numpy()
    err = np.abs(got - d['y']).max()
    print(f'{a.name}: max |torch - tensorflow| = {err:.2e}')
    assert err < 1e-4

    params = sum(p.numel() for p in ref.parameters())
    macs = sum(c.weight.numel() for c in ref.modules() if isinstance(c, nn.Conv2d))

    layers = fold_range(reference(a.name, a.npz)[0])
    plain = Plain(layers, ref.tanh)
    model = Plain(s2d_layers(fold_range(reference(a.name, a.npz)[0])), ref.tanh) if a.variant == 's2d' else plain

    # equivalence on a natural-ish limited-range Y: published net vs exported graph
    rng = np.random.default_rng(1)
    y = torch.from_numpy((16 + 219 * np.clip(rng.random((1, 1, 64, 96)) * 0.4
                          + np.linspace(0, 0.6, 96), 0, 1)).astype(np.float32))
    with torch.no_grad():
        want = torch.clamp(ref((y - 16) / 219) * 219 + 16, 0, 255)
        if a.variant == 's2d':
            got = F.pixel_shuffle(model(F.pixel_unshuffle(y, 2)), 4)
        else:
            got = F.pixel_shuffle(model(y), 2)
    err = (got - want).abs().max().item()
    print(f'{a.name}/{a.variant}: max |exported - published| = {err:.2e} (Y codes, whole frame)')
    assert err < 1e-2

    h, w = map(int, a.hw.split('x'))
    dmacs = sum(c.weight.numel() for c in model.modules() if isinstance(c, nn.Conv2d))
    px = h * w if a.variant == 'plain' else h * w // 4
    print(f'{a.name}: params={params} MAC/input px={macs} GMAC/frame({w}x{h})={macs * h * w / 1e9:.2f}'
          f' deployed GMAC/frame={dmacs * px / 1e9:.2f}')
    x = torch.zeros(1, 1, h, w) if a.variant == 'plain' else torch.zeros(1, 4, h // 2, w // 2)
    torch.onnx.export(model, x, a.onnx, input_names=['y'], output_names=['y2'], opset_version=13)


if __name__ == '__main__':
    main()
