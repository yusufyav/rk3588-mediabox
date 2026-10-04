#!/usr/bin/env python3
"""RT4KSR x2 (Zamfir et al., CVPRW 2023), rebuilt from its state dict.

The published checkpoint is a pickle; extract it once with the restricted
loader, which never executes code from the file:

  python -c "import torch, numpy as np; sd = torch.load('rt4ksr_x2.pth', map_location='cpu',
      weights_only=True)['state_dict']; np.savez('rt4ksr_x2.npz',
      **{k.replace('module.', ''): v.float().numpy() for k, v in sd.items()})"

Configuration is the repository's default for rt4ksr_x2: 24 features, 4
blocks, GELU, LayerNorm, forget=False (the high-frequency branch is computed
by the training graph but never used), no residual. RGB 0..1 in and out.

  rt4ksr.py NPZ SRC.nv12 OUT.nv12       FP32 on 1080p NV12 frames -> 2160p NV12
  rt4ksr.py NPZ --onnx OUT.onnx         deployable graph for RKNN (see below)

Deployed graph: the three-conv training blocks reparameterised into one 3x3
conv each (checked against the training graph), input already pixel-unshuffled
(1x12x540x960, RGB 0..1), output before the pixel shuffle (1x48x540x960).
"""
import argparse
import sys

import numpy as np
import torch
import torch.nn as nn
import torch.nn.functional as F

SW, SH, DW, DH = 1920, 1080, 3840, 2160


def chan_norm(x, w, b, eps=1e-6):
    mu = x.mean(1, keepdim=True)
    d = x - mu
    var = (d * d).mean(1, keepdim=True)
    return d / torch.sqrt(var + eps) * w.view(1, -1, 1, 1) + b.view(1, -1, 1, 1)


class RT4KSR(nn.Module):
    """Training-form forward pass, straight from the state dict."""

    def __init__(self, sd):
        super().__init__()
        self.sd = {k: torch.from_numpy(v) for k, v in sd.items()}

    def resblock(self, x, p):
        s = self.sd
        out = F.conv2d(x, s[p + 'expand_conv.weight'], s[p + 'expand_conv.bias'])
        ident = out
        b0 = s[p + 'expand_conv.bias'].view(1, -1, 1, 1)
        out = F.pad(out, (1, 1, 1, 1))
        out[:, :, 0:1, :] = b0
        out[:, :, -1:, :] = b0
        out[:, :, :, 0:1] = b0
        out[:, :, :, -1:] = b0
        out = F.conv2d(out, s[p + 'fea_conv.weight'], s[p + 'fea_conv.bias']) + ident
        out = F.conv2d(out, s[p + 'reduce_conv.weight'], s[p + 'reduce_conv.bias'])
        return out + x

    def forward(self, x):
        s = self.sd
        x = F.conv2d(F.pixel_unshuffle(x, 2), s['head.0.weight'], s['head.0.bias'], padding=1)
        for i in range(4):
            p = f'body.{i}.'
            x = F.gelu(self.resblock(chan_norm(x, s[p + 'norm.weight'], s[p + 'norm.bias']), p + 'conv1.'))
        x = self.resblock(chan_norm(x, s['tail.0.weight'], s['tail.0.bias']), 'tail.1.')
        x = F.conv2d(x, s['upsample.0.weight'], s['upsample.0.bias'], padding=1)
        return F.pixel_shuffle(x, 4)

    def to(self, dev):
        self.sd = {k: v.to(dev) for k, v in self.sd.items()}
        return self


def rep(sd, p):
    """expand 1x1 -> (bias-padded) 3x3 + identity -> reduce 1x1, + identity: one 3x3."""
    k0, b0 = torch.from_numpy(sd[p + 'expand_conv.weight']), torch.from_numpy(sd[p + 'expand_conv.bias'])
    k1, b1 = torch.from_numpy(sd[p + 'fea_conv.weight']).clone(), torch.from_numpy(sd[p + 'fea_conv.bias'])
    k2, b2 = torch.from_numpy(sd[p + 'reduce_conv.weight']), torch.from_numpy(sd[p + 'reduce_conv.bias'])
    mid, n = k0.shape[:2]
    for i in range(mid):
        k1[i, i, 1, 1] += 1.0
    k01 = F.conv2d(k1, k0.permute(1, 0, 2, 3))
    b01 = F.conv2d(b0.view(1, -1, 1, 1) * torch.ones(1, mid, 3, 3), k1, b1)
    k = F.conv2d(k01.permute(1, 0, 2, 3), k2).permute(1, 0, 2, 3).clone()
    b = F.conv2d(b01, k2, b2).view(-1)
    for i in range(n):
        k[i, i, 1, 1] += 1.0
    c = nn.Conv2d(n, n, 3, padding=1)
    c.weight.data, c.bias.data = k, b
    return c


class NormDecomp(nn.Module):
    """The same channel LayerNorm out of ops RKNN spreads over all NPU cores:
    centring and the channel mean are 1x1 convs (no channel broadcast)."""

    def __init__(self, w, b, n=24):
        super().__init__()
        self.center = nn.Conv2d(n, n, 1, bias=False)
        self.center.weight.data = (torch.eye(n) - 1.0 / n).view(n, n, 1, 1)
        self.mean = nn.Conv2d(n, n, 1, bias=False)
        self.mean.weight.data = torch.full((n, n, 1, 1), 1.0 / n)
        self.w = nn.Parameter(torch.from_numpy(w).view(1, -1, 1, 1))
        self.b = nn.Parameter(torch.from_numpy(b).view(1, -1, 1, 1))

    def forward(self, x):
        d = self.center(x)
        return d / torch.sqrt(self.mean(d * d) + 1e-6) * self.w + self.b


class SigmoidGELU(nn.Module):
    """x * sigmoid(1.702 x): GELU to within 0.02."""

    def forward(self, x):
        return x * torch.sigmoid(1.702 * x)


class Norm(nn.Module):
    def __init__(self, w, b):
        super().__init__()
        self.w = nn.Parameter(torch.from_numpy(w).view(1, -1, 1, 1))
        self.b = nn.Parameter(torch.from_numpy(b).view(1, -1, 1, 1))

    def forward(self, x):
        mu = x.mean(1, keepdim=True)
        d = x - mu
        return d / torch.sqrt((d * d).mean(1, keepdim=True) + 1e-6) * self.w + self.b


# BT.709: RGB from normalised y, u, v and back
KR, KB = 0.2126, 0.0722
KG = 1 - KR - KB


def yuv2rgb_matrix():
    """12x6: unshuffled RGB (c*4 + phase) from [y00, y01, y10, y11, u, v], chroma nearest."""
    A = torch.zeros(12, 6)
    for ph in range(4):
        A[0 * 4 + ph, ph], A[0 * 4 + ph, 5] = 1, 2 * (1 - KR)
        A[1 * 4 + ph, ph], A[1 * 4 + ph, 4], A[1 * 4 + ph, 5] = 1, -2 * (1 - KB) * KB / KG, -2 * (1 - KR) * KR / KG
        A[2 * 4 + ph, ph], A[2 * 4 + ph, 4] = 1, 2 * (1 - KB)
    return A


def rgb2nv12_matrix():
    """24x48 and offsets: [Y 16 phases (4a+b), U 2x2, V 2x2] codes from shuffled RGB (c*16 + 4a+b)."""
    M, o = torch.zeros(24, 48), torch.zeros(24)
    for a in range(4):
        for b in range(4):
            ph = 4 * a + b
            yw = (KR, KG, KB)
            for c in range(3):
                M[ph, c * 16 + ph] = 219 * yw[c]
            o[ph] = 16
            cq = 16 + (a // 2) * 2 + b // 2
            for c in range(3):
                # u = (B - Y) / (2(1-KB)), v = (R - Y) / (2(1-KR)); 2x2 average
                M[cq, c * 16 + ph] += 0.25 * 224 * ((c == 2) - yw[c]) / (2 * (1 - KB))
                M[cq + 4, c * 16 + ph] += 0.25 * 224 * ((c == 0) - yw[c]) / (2 * (1 - KR))
    o[16:] = 128
    return M, o


class Deploy(nn.Module):
    """NV12 in, NV12 out, all colour maths folded into the first and last conv.

    in   1x6x540x960: Y phases (2i+j) of each 2x2 block, then U, V -- the NV12
         planes as they are, in codes; chroma is used nearest (one UV per block)
    out  1x24x540x960: Y of the 4x4 output block (4a+b), U and V of its 2x2
         chroma samples (2p+q), in codes, clamped 0..255
    """

    def __init__(self, sd, norm='layer', gelu='exact'):
        super().__init__()
        t = lambda k: torch.from_numpy(sd[k])
        N = NormDecomp if norm == 'decomp' else Norm
        act = SigmoidGELU if gelu == 'sigmoid' else nn.GELU
        self.register_buffer('mean', torch.tensor([16.] * 4 + [128., 128.]).view(1, 6, 1, 1))
        self.register_buffer('scale', torch.tensor([1 / 219.] * 4 + [1 / 224.] * 2).view(1, 6, 1, 1))
        A = yuv2rgb_matrix()
        self.head = nn.Conv2d(6, 24, 3, padding=1)
        self.head.weight.data = torch.einsum('ochw,ck->okhw', t('head.0.weight'), A)
        self.head.bias.data = t('head.0.bias')
        layers = []
        for i in range(4):
            p = f'body.{i}.'
            layers += [N(sd[p + 'norm.weight'], sd[p + 'norm.bias']), rep(sd, p + 'conv1.'), act()]
        layers += [N(sd['tail.0.weight'], sd['tail.0.bias']), rep(sd, 'tail.1.')]
        self.body = nn.Sequential(*layers)
        M, o = rgb2nv12_matrix()
        self.up = nn.Conv2d(24, 24, 3, padding=1)
        self.up.weight.data = torch.einsum('kc,cihw->kihw', M, t('upsample.0.weight'))
        self.up.bias.data = M @ t('upsample.0.bias') + o

    def forward(self, x):
        x = (x - self.mean) * self.scale
        return torch.clamp(self.up(self.body(self.head(x))), 0, 255)


def nv12_planes(f, w, h):
    """1x6xH/2xW/2 deploy input from one NV12 frame."""
    y = f[:w * h].reshape(h // 2, 2, w // 2, 2).transpose(1, 3, 0, 2).reshape(4, h // 2, w // 2)
    c = f[w * h:].reshape(h // 2, w // 2, 2).transpose(2, 0, 1)
    return np.concatenate([y, c]).astype(np.float32)[None]


def deploy_to_nv12(o):
    """1x24xH/2xW/2 deploy output -> NV12 bytes at 4x."""
    o = np.clip(np.rint(o[0]), 0, 255).astype(np.uint8)
    h, w = o.shape[1:]
    Y = o[:16].reshape(4, 4, h, w).transpose(2, 0, 3, 1).reshape(4 * h, 4 * w)
    U = o[16:20].reshape(2, 2, h, w).transpose(2, 0, 3, 1).reshape(2 * h, 2 * w)
    V = o[20:24].reshape(2, 2, h, w).transpose(2, 0, 3, 1).reshape(2 * h, 2 * w)
    return Y.tobytes() + np.stack([U, V], -1).tobytes()


# ---- BT.709 limited-range NV12 <-> RGB 0..1 ----

def uv2x(c):
    from reference import uv2x as f
    return f(c)


def nv12_to_rgb(f, w, h):
    y = f[:w * h].reshape(h, w).astype(np.float32)
    c = uv2x(f[w * h:].reshape(h // 2, w // 2, 2)).astype(np.float32)
    yy = (y - 16) / 219
    u, v = (c[..., 0] - 128) / 224, (c[..., 1] - 128) / 224
    r = yy + 1.5748 * v
    g = yy - 0.1873 * u - 0.4681 * v
    b = yy + 1.8556 * u
    return np.clip(np.stack([r, g, b]), 0, 1)


def rgb_to_nv12(rgb):
    r, g, b = rgb
    y = 0.2126 * r + 0.7152 * g + 0.0722 * b
    u = (b - y) / 1.8556
    v = (r - y) / 1.5748
    h, w = y.shape
    sub = lambda p: p.reshape(h // 2, 2, w // 2, 2).mean(axis=(1, 3))
    Y = np.clip(np.rint(16 + 219 * y), 0, 255).astype(np.uint8)
    C = np.stack([sub(u), sub(v)], -1)
    C = np.clip(np.rint(128 + 224 * C), 0, 255).astype(np.uint8)
    return Y.tobytes() + C.tobytes()


def check(sd):
    """Deploy graph == training graph on the same (nearest-chroma, unclipped) RGB."""
    rng = np.random.default_rng(0)
    h, w = 64, 96
    f = np.concatenate([rng.integers(30, 220, h * w), rng.integers(90, 170, h * w // 2)]).astype(np.uint8)
    with torch.no_grad():
        out = Deploy(sd)(torch.from_numpy(nv12_planes(f, w, h))).numpy()
        x6 = (torch.from_numpy(nv12_planes(f, w, h)) - torch.tensor([16.] * 4 + [128.] * 2).view(1, 6, 1, 1)) \
            * torch.tensor([1 / 219.] * 4 + [1 / 224.] * 2).view(1, 6, 1, 1)
        rgb = F.pixel_shuffle(torch.einsum('bkhw,ck->bchw', x6, yuv2rgb_matrix()), 2)
        want = RT4KSR(sd)(rgb)
        M, o = rgb2nv12_matrix()
        want = torch.clamp(torch.einsum('bchw,kc->bkhw', F.pixel_unshuffle(want, 4), M) + o.view(1, -1, 1, 1), 0, 255)
    print(f'NV12 deploy graph vs training graph: max |diff| {np.abs(out - want.numpy()).max():.2e} codes')


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('npz')
    ap.add_argument('src', nargs='?')
    ap.add_argument('out', nargs='?')
    ap.add_argument('--onnx')
    ap.add_argument('--deploy', action='store_true', help='run the NV12 deploy graph instead')
    ap.add_argument('--norm', choices=['layer', 'decomp'], default='layer')
    ap.add_argument('--gelu', choices=['exact', 'sigmoid'], default='exact')
    a = ap.parse_args()
    sd = dict(np.load(a.npz))
    if a.onnx:
        check(sd)
        dep = Deploy(sd, a.norm, a.gelu)
        if a.norm != 'layer' or a.gelu != 'exact':
            x = torch.from_numpy(nv12_planes(np.random.default_rng(2).integers(16, 236, 64 * 96 * 3 // 2).astype(np.uint8), 96, 64))
            with torch.no_grad():
                print(f'{a.norm}/{a.gelu} vs layer/exact: max |diff| {(dep(x) - Deploy(sd)(x)).abs().max().item():.3f} codes')
        print(f'params {sum(p.numel() for p in dep.parameters())}, '
              f'MAC/1080p frame {sum(m.weight.numel() for m in dep.modules() if isinstance(m, nn.Conv2d)) * 540 * 960 / 1e9:.2f} G')
        torch.onnx.export(dep, torch.zeros(1, 6, SH // 2, SW // 2), a.onnx, input_names=['x'],
                          output_names=['y'], opset_version=13)
        return
    dev = 'cuda' if torch.cuda.is_available() else 'cpu'
    if a.deploy:
        dep = Deploy(sd, a.norm, a.gelu).to(dev).eval()
        src = np.fromfile(a.src, np.uint8)
        n = len(src) // (SW * SH * 3 // 2)
        with open(a.out, 'wb') as o, torch.no_grad():
            for k in range(n):
                f = src[k * SW * SH * 3 // 2:(k + 1) * SW * SH * 3 // 2]
                o.write(deploy_to_nv12(dep(torch.from_numpy(nv12_planes(f, SW, SH)).to(dev)).cpu().numpy()))
        return
    model = RT4KSR(sd).to(dev)
    src = np.fromfile(a.src, np.uint8)
    n = len(src) // (SW * SH * 3 // 2)
    with open(a.out, 'wb') as o, torch.no_grad():
        for k in range(n):
            f = src[k * SW * SH * 3 // 2:(k + 1) * SW * SH * 3 // 2]
            x = torch.from_numpy(nv12_to_rgb(f, SW, SH))[None].to(dev)
            y = model(x).clamp(0, 1)[0].cpu().numpy()
            o.write(rgb_to_nv12(y))
    print(f'{n} frames -> {a.out}', file=sys.stderr)


if __name__ == '__main__':
    main()
