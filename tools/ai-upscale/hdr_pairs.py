#!/usr/bin/env python3
"""Matched frames from a film's 1080p and 2160p HDR10 releases, as SDR NV12 test pairs.

  hdr_pairs.py URL1080 URL2160 OUTDIR SECONDS [SECONDS ...] [--fps1080 24 --fps2160 23.976]

The two releases carry the same frames, timed differently (a 1080p release
flagged 24 fps against the UHD's 24000/1001). Frame n is found at n/fps in
each; the 2160p frame is then chosen among n-3..n+3 as the one whose 2x box
downscale matches the 1080p frame best, so the pair is the same picture.

Both are tone-mapped to SDR BT.709 with the mapping aisr-demo uses (PQ ->
nits, 203-nit white, soft shoulder, BT.1886, chroma scaled with luma) and
centred in 1920x1080 / 3840x2160 canvases:

  OUTDIR/fNNNNNN-src1080.nv12   what VOP2 and MBSR are given
  OUTDIR/fNNNNNN-ref4k.nv12     the 2160p release: the answer
"""
import argparse
import os
import subprocess

import numpy as np

M1, M2, C1, C2, C3 = 0.1593017578125, 78.84375, 0.8359375, 18.8515625, 18.6875


def luts():
    v = np.arange(1024)
    e = np.clip((v - 64) / 876.0, 0, 1)
    p = e ** (1 / M2)
    L = 10000 * (np.maximum(p - C1, 0) / (C2 - C3 * p)) ** (1 / M1)
    ls = L / 203.0
    o = np.where(ls <= 0.8, ls, 0.8 + 0.2 * (1 - np.exp(-(ls - 0.8) / 0.2)))
    V = o ** (1 / 2.4)
    ty = np.rint(16 + 219 * V).astype(np.uint8)
    ts = np.where(e > 0.02, np.minimum(V / np.maximum(e, 1e-9), 2.5), 1.0).astype(np.float32)
    return ty, ts


TY, TS = luts()


def decode(url, t, w, h, n=1):
    cmd = ['ffmpeg', '-v', 'error', '-ss', f'{t:.6f}', '-i', url, '-map', '0:v:0', '-frames:v', str(n),
           '-f', 'rawvideo', '-pix_fmt', 'yuv420p10le', '-']
    a = np.frombuffer(subprocess.run(cmd, capture_output=True, check=True).stdout, '<u2')
    fs = w * h * 3 // 2
    return [a[i * fs:(i + 1) * fs] for i in range(len(a) // fs)]


def planes(f, w, h):
    y = f[:w * h].reshape(h, w) & 1023
    u = f[w * h:w * h * 5 // 4].reshape(h // 2, w // 2) & 1023
    v = f[w * h * 5 // 4:].reshape(h // 2, w // 2) & 1023
    return y, u, v


def to_nv12(f, w, h, W, H):
    """Tone map (as aisr-demo's tm_rows) and centre in a WxH NV12 canvas."""
    y, u, v = planes(f, w, h)
    Y = TY[y]
    s = TS[y[0::2, 0::2]] * 0.25
    U = np.clip(np.rint(128 + (u.astype(np.float32) - 512) * s), 16, 240).astype(np.uint8)
    V = np.clip(np.rint(128 + (v.astype(np.float32) - 512) * s), 16, 240).astype(np.uint8)
    ox, oy = (W - w) // 2 & ~1, (H - h) // 2 & ~1
    cy = np.full((H, W), 16, np.uint8)
    cy[oy:oy + h, ox:ox + w] = Y
    cuv = np.full((H // 2, W // 2, 2), 128, np.uint8)
    cuv[oy // 2:(oy + h) // 2, ox // 2:(ox + w) // 2] = np.stack([U, V], -1)
    return cy.tobytes() + cuv.tobytes()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('url1080')
    ap.add_argument('url2160')
    ap.add_argument('out')
    ap.add_argument('seconds', nargs='+', type=float)
    ap.add_argument('--fps1080', type=float, default=24.0)
    ap.add_argument('--fps2160', type=float, default=24000 / 1001)
    ap.add_argument('--size1080', default='1792x1080')
    a = ap.parse_args()
    u1, u2 = open(a.url1080).read().strip(), open(a.url2160).read().strip()
    w, h = map(int, a.size1080.split('x'))
    os.makedirs(a.out, exist_ok=True)
    for s in a.seconds:
        n = int(round(s * a.fps1080))
        lo = decode(u1, n / a.fps1080, w, h)[0]
        his = decode(u2, (n - 3) / a.fps2160, 2 * w, 2 * h, 7)
        ly = planes(lo, w, h)[0].astype(np.float32)
        best = None
        for k, hf in enumerate(his):
            hy = planes(hf, 2 * w, 2 * h)[0].astype(np.float32)
            d = hy.reshape(h, 2, w, 2).mean(axis=(1, 3))
            err = np.mean((d - ly) ** 2)
            if best is None or err < best[0]:
                best = (err, k, hf)
        err, k, hf = best
        psnr10 = 10 * np.log10(1023 ** 2 / max(err, 1e-9))
        tag = f'{a.out}/f{n:06d}'
        open(f'{tag}-src1080.nv12', 'wb').write(to_nv12(lo, w, h, 1920, 1080))
        open(f'{tag}-ref4k.nv12', 'wb').write(to_nv12(hf, 2 * w, 2 * h, 3840, 2160))
        print(f'{s:7.1f} s  frame {n}  2160p match at offset {k - 3:+d}  '
              f'(2x box downscale vs 1080p release: {psnr10:.1f} dB, 10-bit)', flush=True)


if __name__ == '__main__':
    main()
