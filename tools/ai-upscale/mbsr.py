#!/usr/bin/env python3
"""MBSR: a 2x super-resolution network shaped for the RK3588 NPU (NPU_SHAPE.md).

NV12 in, NV12 out, everything at 540x960 for a 1080p frame:

  in   6 ch : Y of each 2x2 block (2i+j), U, V            -- codes / 255
  body conv3x3(6->C) LeakyReLU(0.1), (D-2) x conv3x3(C->C) LeakyReLU, conv3x3(C->24)
  + anchor conv3x3(6->24), a learnt linear upscaler the body corrects
  out  24 ch: Y of the 4x4 output block (4a+b), U (2p+q), V (2p+q)

Training uses structural re-parameterisation computed on the fly: each conv is
a 3x3 + 1x1 + expanded (1x1 -> 3x3, no bias in between) + identity branch,
composed into one 3x3 kernel before every forward, so the deployed graph is
the trained function exactly. LeakyReLU: with Clip(0,1)
and with ReLU whole layers died and the body went silent behind the anchor;
training prints the body's share of the output so that cannot pass unseen. --qat fine-tunes with fake INT8 (per-channel
symmetric weights, per-tensor asymmetric activations) the way RKNN runs it.

  mbsr.py prep  HRDIR LRDIR                    LR 1080p NV12 for every 4K NV12 in HRDIR (ffmpeg)
  mbsr.py train HRDIR LRDIR CKPT [--iters N] [--qat --init CKPT]
  mbsr.py run   CKPT SRC.nv12 OUT.nv12         FP32 on 1080p NV12 frames
  mbsr.py onnx  CKPT OUT.onnx                  deploy graph, 0..1 in, codes out, clamped
                                               (convert with --std 255: NV12 codes in)
"""
import argparse
import glob
import math
import os
import subprocess
import sys
import time

import numpy as np
import torch
import torch.nn as nn
import torch.nn.functional as F

SW, SH, DW, DH = 1920, 1080, 3840, 2160
C, D = 32, 9


def fq_tensor(x, lo, hi):
    """Fake per-tensor asymmetric 8-bit quantisation with a straight-through gradient."""
    scale = max((hi - lo) / 255.0, 1e-8)
    zp = int(round(-lo / scale)) - 128
    zp = max(-128, min(127, zp))
    return torch.fake_quantize_per_tensor_affine(x, scale, zp, -128, 127)


def fq_weight(w):
    scale = (w.detach().abs().amax(dim=(1, 2, 3)) / 127.0).clamp_min(1e-8)
    return torch.fake_quantize_per_channel_affine(w, scale, torch.zeros_like(scale, dtype=torch.int32), 0, -127, 127)


class RepConv(nn.Module):
    def __init__(self, cin, cout, exp=2):
        super().__init__()
        # near-identity start (QuickSRNet's idea): with the identity branch on top,
        # a full-gain 3x3 made activations grow ~4x per layer (0.09 -> 454 over
        # seven layers) and killed the last ReLU layer outright
        self.w3 = nn.Parameter(torch.randn(cout, cin, 3, 3) * math.sqrt(2.0 / (9 * cin)) * (0.5 if cin != cout else 0.05))
        self.w1 = nn.Parameter(torch.zeros(cout, cin, 1, 1))
        self.e1 = nn.Parameter(torch.randn(cin * exp, cin, 1, 1) * math.sqrt(1.0 / cin))
        self.e3 = nn.Parameter(torch.zeros(cout, cin * exp, 3, 3))
        self.b = nn.Parameter(torch.zeros(cout))
        self.ident = cin == cout

    def kernel(self):
        k = self.w3 + F.pad(self.w1, (1, 1, 1, 1))
        k = k + torch.einsum('omhw,mi->oihw', self.e3, self.e1[:, :, 0, 0])
        if self.ident:
            k = k + F.pad(torch.eye(k.shape[0], device=k.device).view(k.shape[0], k.shape[0], 1, 1), (1, 1, 1, 1))
        return k


def exact_bilinear():
    """Fixed anchor: centre-aligned bilinear 2x in the NV12 phase layout, with the
    taps 9/16, 3/16, 1/16 replaced by 127/225, 42/225, 14/225. Those sum to
    exactly 1 per output and sit on the per-channel INT8 grid (scale = largest
    tap / 127), so RKNN computes the anchor exactly. A learnt anchor came out
    about one code wrong per pixel in INT8; the exact nearest anchor left the
    body the whole interpolation to learn."""
    w = torch.zeros(24, 6, 3, 3)
    tap = {3: 42 / 225, 1: 14 / 225, 9: 127 / 225}

    def rows(ph):                       # 1-D: [(offset, weight16)], ph = output phase of 4 (luma)
        p = ph / 2 - 0.25               # source position relative to row 2Y
        lo = math.floor(p)
        f = p - lo                      # 0.25 or 0.75
        near, far = (3, 1) if f == 0.25 else (1, 3)   # 1-D weights in quarters
        return [(lo, near), (lo + 1, far)]

    for a in range(4):
        for b in range(4):
            for (dy, wy) in rows(a):
                for (dx, wx) in rows(b):
                    # 1080p row 2Y+dy -> half-res offset dy//2, phase dy%2
                    w[4 * a + b, 2 * (dy % 2) + dx % 2, 1 + dy // 2, 1 + dx // 2] += tap[wy * wx]
    for k in range(4):
        p, q = k // 2, k % 2
        ry = [(-1, 1), (0, 3)] if p == 0 else [(0, 3), (1, 1)]
        rx = [(-1, 1), (0, 3)] if q == 0 else [(0, 3), (1, 1)]
        for (dy, wy) in ry:
            for (dx, wx) in rx:
                w[16 + k, 4, 1 + dy, 1 + dx] = tap[wy * wx]
                w[20 + k, 5, 1 + dy, 1 + dx] = tap[wy * wx]
    return w


class MBSR(nn.Module):
    def __init__(self, c=C, d=D):
        super().__init__()
        self.convs = nn.ModuleList([RepConv(6, c)] + [RepConv(c, c) for _ in range(d - 2)] + [RepConv(c, 24)])
        with torch.no_grad():
            self.convs[-1].w3.mul_(0.1)
        self.anchor = nn.Conv2d(6, 24, 3, padding=1)
        with torch.no_grad():
            # start as nearest: every output phase copies its source sample
            self.anchor.weight.zero_()
            self.anchor.bias.zero_()
            for a in range(4):
                for b in range(4):
                    self.anchor.weight[4 * a + b, 2 * (a // 2) + b // 2, 1, 1] = 1.0
            for k in range(4):
                self.anchor.weight[16 + k, 4, 1, 1] = 1.0
                self.anchor.weight[20 + k, 5, 1, 1] = 1.0
        self.fixed_anchor = False
        self.qat = False
        self.register_buffer('ranges', torch.zeros(len(self.convs) + 2, 2))
        self.register_buffer('seen', torch.zeros(()))

    def _obs(self, i, x):
        if self.training:
            lo, hi = x.detach().amin(), x.detach().amax()
            if self.seen < 1:
                self.ranges[i] = torch.stack([lo, hi])
            else:
                self.ranges[i] = 0.99 * self.ranges[i] + 0.01 * torch.stack([lo, hi])
        lo, hi = self.ranges[i].tolist()
        return fq_tensor(x, min(lo, 0.0), max(hi, 1e-3))

    def forward(self, x):
        h = x
        n = len(self.convs)
        for i, c in enumerate(self.convs):
            k = c.kernel()
            if self.qat:
                k = fq_weight(k)
            h = F.conv2d(h, k, c.b, padding=1)
            if i < n - 1:
                h = F.leaky_relu(h, 0.1)
                if self.qat:
                    h = self._obs(i, h)
        aw = fq_weight(self.anchor.weight) if self.qat else self.anchor.weight
        self.body_abs = h.detach().abs().mean()
        out = h + F.conv2d(x, aw, self.anchor.bias, padding=1)
        if self.qat:
            out = self._obs(n, out)
            if self.training:
                self.seen += 1
        return out


class Deploy(nn.Module):
    """Plain convs, what RKNN gets: x in 0..1 (RKNN's input std 255 makes that the
    NV12 codes), codes out, clamped.

    INT8 needs the anchor to be the fixed nearest copy (train --fixed-anchor):
    its weights are exactly 1 (255 here), which per-channel INT8 represents
    exactly. A learnt anchor's 1/4-3/4 taps share a scale with that centre tap
    and come out about one code wrong on every pixel: 5-7 dB on smooth film
    frames, which RKNN's accuracy_analysis pinned on the anchor conv and a
    hybrid build with only the last convs in FP16 removed completely.
    """

    def __init__(self, m):
        super().__init__()
        ks = [(c.kernel().detach().clone(), c.b.detach().clone()) for c in m.convs]
        self.convs = nn.ModuleList()
        for i, (k, b) in enumerate(ks):
            cv = nn.Conv2d(k.shape[1], k.shape[0], 3, padding=1)
            if i == len(ks) - 1:
                k, b = k * 255.0, b * 255.0
            cv.weight.data, cv.bias.data = k, b
            self.convs.append(cv)
        self.anchor = nn.Conv2d(6, 24, 3, padding=1)
        self.anchor.weight.data = m.anchor.weight.detach().clone() * 255.0
        self.anchor.bias.data = m.anchor.bias.detach().clone() * 255.0

    def forward(self, x):
        h = x
        for i, c in enumerate(self.convs):
            h = c(h)
            if i < len(self.convs) - 1:
                h = F.leaky_relu(h, 0.1)
        return torch.clamp(h + self.anchor(x), 0.0, 255.0)


# ---------- NV12 <-> tensors ----------

def nv12_in(f, w, h):
    """1x6xH/2xW/2 from one NV12 frame, codes."""
    y = f[:w * h].reshape(h // 2, 2, w // 2, 2).transpose(1, 3, 0, 2).reshape(4, h // 2, w // 2)
    c = f[w * h:].reshape(h // 2, w // 2, 2).transpose(2, 0, 1)
    return np.concatenate([y, c]).astype(np.float32)[None]


def nv12_out(o):
    """1x24xH/2xW/2 codes -> NV12 bytes at 4x the tensor's size."""
    o = np.clip(np.rint(o[0]), 0, 255).astype(np.uint8)
    h, w = o.shape[1:]
    Y = o[:16].reshape(4, 4, h, w).transpose(2, 0, 3, 1).reshape(4 * h, 4 * w)
    U = o[16:20].reshape(2, 2, h, w).transpose(2, 0, 3, 1).reshape(2 * h, 2 * w)
    V = o[20:24].reshape(2, 2, h, w).transpose(2, 0, 3, 1).reshape(2 * h, 2 * w)
    return Y.tobytes() + np.stack([U, V], -1).tobytes()


# ---------- data ----------

def prep(hrdir, lrdir):
    os.makedirs(lrdir, exist_ok=True)
    for f in sorted(glob.glob(f'{hrdir}/*.nv12')):
        b = os.path.basename(f)[:-5]
        for kind, flags in (('lanczos', 'lanczos'), ('bicubic', 'bicubic')):
            out = f'{lrdir}/{b}-{kind}.nv12'
            if not os.path.exists(out):
                subprocess.run(['ffmpeg', '-v', 'error', '-y', '-f', 'rawvideo', '-pix_fmt', 'nv12', '-s', f'{DW}x{DH}',
                                '-i', f, '-vf', f'scale={SW}:{SH}:flags={flags}+accurate_rnd+full_chroma_int,format=nv12',
                                '-f', 'rawvideo', out], check=True)


class Patches(torch.utils.data.IterableDataset):
    def __init__(self, hrdir, lrdir, p=64):
        self.pairs = []
        for f in sorted(glob.glob(f'{hrdir}/*.nv12')):
            b = os.path.basename(f)[:-5]
            for kind in ('lanczos', 'bicubic'):
                lr = f'{lrdir}/{b}-{kind}.nv12'
                if os.path.exists(lr):
                    self.pairs.append((f, lr))
        self.p = p

    def __iter__(self):
        rng = np.random.default_rng((os.getpid() * 7919 + int(time.time() * 1000)) % 2**32)
        maps = {}
        p = self.p
        while True:
            hr, lr = self.pairs[rng.integers(len(self.pairs))]
            if hr not in maps:
                maps[hr] = np.memmap(hr, np.uint8, 'r')
            if lr not in maps:
                maps[lr] = np.memmap(lr, np.uint8, 'r')
            H, L = maps[hr], maps[lr]
            # half-res tensor coordinates; skip the scope letterbox rows mostly
            r = rng.integers(0, SH // 2 - p)
            c = rng.integers(0, SW // 2 - p)
            ly = np.asarray(L[:SW * SH]).reshape(SH, SW)[2 * r:2 * r + 2 * p, 2 * c:2 * c + 2 * p]
            if ly.max() - ly.min() < 8:
                continue
            luv = np.asarray(L[SW * SH:]).reshape(SH // 2, SW // 2, 2)[r:r + p, c:c + p]
            hy = np.asarray(H[:DW * DH]).reshape(DH, DW)[4 * r:4 * r + 4 * p, 4 * c:4 * c + 4 * p]
            huv = np.asarray(H[DW * DH:]).reshape(DH // 2, DW // 2, 2)[2 * r:2 * r + 2 * p, 2 * c:2 * c + 2 * p]
            t = rng.integers(8)
            ly, luv, hy, huv = (aug(z, t) for z in (ly, luv, hy, huv))
            x = np.concatenate([ly.reshape(p, 2, p, 2).transpose(1, 3, 0, 2).reshape(4, p, p),
                                luv.transpose(2, 0, 1)])
            yt = np.concatenate([hy.reshape(p, 4, p, 4).transpose(1, 3, 0, 2).reshape(16, p, p),
                                 huv[..., 0].reshape(p, 2, p, 2).transpose(1, 3, 0, 2).reshape(4, p, p),
                                 huv[..., 1].reshape(p, 2, p, 2).transpose(1, 3, 0, 2).reshape(4, p, p)])
            yield (torch.from_numpy(x.astype(np.float32) / 255.0), torch.from_numpy(yt.astype(np.float32) / 255.0))


def aug(z, t):
    if t & 1:
        z = z[:, ::-1]
    if t & 2:
        z = z[::-1]
    if t & 4:
        z = z.transpose(1, 0, 2) if z.ndim == 3 else z.T
    return np.ascontiguousarray(z)


def train(a):
    dev = 'cuda'
    m = MBSR().to(dev)
    if a.init:
        m.load_state_dict(torch.load(a.init, map_location=dev, weights_only=True))
    m.qat = a.qat
    if a.fixed_anchor:
        with torch.no_grad():
            m.anchor.weight.copy_(exact_bilinear())
            m.anchor.bias.zero_()
        m.anchor.requires_grad_(False)
    dl = torch.utils.data.DataLoader(Patches(a.hr, a.lrdir), batch_size=a.batch, num_workers=10, prefetch_factor=4)
    opt = torch.optim.Adam([p for p in m.parameters() if p.requires_grad], lr=a.rate)
    sched = torch.optim.lr_scheduler.CosineAnnealingLR(opt, a.iters, eta_min=a.rate * 0.01)
    m.train()
    t0, run = time.time(), 0.0
    for it, (x, y) in enumerate(dl):
        if it >= a.iters:
            break
        x, y = x.to(dev, non_blocking=True), y.to(dev, non_blocking=True)
        loss = F.l1_loss(m(x), y)
        opt.zero_grad(set_to_none=True)
        loss.backward()
        torch.nn.utils.clip_grad_norm_(m.parameters(), 1.0)
        opt.step()
        sched.step()
        run = 0.99 * run + 0.01 * loss.item() if it else loss.item()
        if it % 2000 == 0 or it == a.iters - 1:
            print(f'it {it} loss {run * 255:.3f} codes  body |r| {m.body_abs.item() * 255:.2f} codes'
                  f'  lr {sched.get_last_lr()[0]:.2e}  {time.time() - t0:.0f}s', flush=True)
            torch.save(m.state_dict(), a.ckpt)
    torch.save(m.state_dict(), a.ckpt)


def load(ckpt):
    m = MBSR()
    m.load_state_dict(torch.load(ckpt, map_location='cpu', weights_only=True))
    return m.eval()


def run(a):
    dev = 'cuda' if torch.cuda.is_available() else 'cpu'
    d = Deploy(load(a.ckpt)).to(dev).eval()
    src = np.fromfile(a.src, np.uint8)
    n = len(src) // (SW * SH * 3 // 2)
    with open(a.out, 'wb') as o, torch.no_grad():
        for k in range(n):
            f = src[k * SW * SH * 3 // 2:(k + 1) * SW * SH * 3 // 2]
            o.write(nv12_out(d(torch.from_numpy(nv12_in(f, SW, SH) / 255.0).to(dev)).cpu().numpy()))


def onnx(a):
    m = load(a.ckpt)
    d = Deploy(m).eval()
    x = torch.rand(1, 6, 32, 48)
    with torch.no_grad():
        err = (d(x) - torch.clamp(m(x) * 255, 0, 255)).abs().max().item()
    convs = [c for c in d.modules() if isinstance(c, nn.Conv2d)]
    print(f'deploy vs training graph: max |diff| {err:.2e} codes; {len(convs)} convs, '
          f'{sum(c.weight.numel() for c in convs) * (SH // 2) * (SW // 2) / 1e9:.1f} GMAC/frame')
    torch.onnx.export(d, torch.zeros(1, 6, SH // 2, SW // 2), a.out, input_names=['x'], output_names=['y'],
                      opset_version=13)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest='cmd', required=True)
    p = sub.add_parser('prep'); p.add_argument('hr'); p.add_argument('lr')
    p = sub.add_parser('train'); p.add_argument('hr'); p.add_argument('lrdir'); p.add_argument('ckpt')
    p.add_argument('--iters', type=int, default=100000); p.add_argument('--batch', type=int, default=32)
    p.add_argument('--rate', type=float, default=1e-3); p.add_argument('--qat', action='store_true')
    p.add_argument('--init'); p.add_argument('--fixed-anchor', action='store_true')
    p = sub.add_parser('run'); p.add_argument('ckpt'); p.add_argument('src'); p.add_argument('out')
    p = sub.add_parser('onnx'); p.add_argument('ckpt'); p.add_argument('out')
    a = ap.parse_args()
    {'prep': lambda: prep(a.hr, a.lr), 'train': lambda: train(a), 'run': lambda: run(a), 'onnx': lambda: onnx(a)}[a.cmd]()


if __name__ == '__main__':
    main()
