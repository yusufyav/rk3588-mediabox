#!/usr/bin/env python3
"""Upscaled 2160p frames against the 4K reference they were made from.

  quality.py REF.nv12 NAME=FILE.nv12 [...] [--crops DIR --at Y,X[,Y,X...]]
  quality.py --temporal REF.nv12 NAME=FILE.nv12 [...]

All files are tightly packed 3840x2160 NV12; a file may hold several frames.
Measured on luma inside the picture (the scope letterbox and an 8 pixel
border left out), after the geometry check of ../rga-upscale/compare.py
(integer) and a sub-pixel phase correlation:

  PSNR, SSIM   against the reference
  detail       mean gradient magnitude / the reference's
  hf           energy above the 1080p source's Nyquist (|f| > 0.25 cy/px)
               / the reference's: < 1 is blur, > 1 is invented or aliased
               high frequency
  overshoot    ringing and halo: mean excursion outside the reference's own
               5x5 min..max, and the share of pixels leaving it by > 4 codes

--temporal compares consecutive frames: how much of the frame-to-frame change
of a candidate is not in the reference's frame-to-frame change (mean
|d_cand - d_ref|, whole picture and its 5% worst 64x64 blocks). A scaler
that is a fixed linear filter can only pass on the source's own change; a
per-frame network can add shimmer, which shows up here.
"""
import argparse
import os
import sys

import numpy as np

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'rga-upscale'))
from compare import detail, nv12_rgb, overshoot, psnr, ssim  # noqa: E402

W, H = 3840, 2160
FRAME = W * H * 3 // 2


def frames(path):
    a = np.fromfile(path, np.uint8)
    return [a[i * FRAME:(i + 1) * FRAME] for i in range(len(a) // FRAME)]


def luma(f):
    return f[:W * H].reshape(H, W).astype(np.float64)


COLS = (8, W - 8)


def active(ref):
    """Rows that carry picture: the letterbox is flat Y=16. Columns: --cols."""
    rows = np.where(ref.max(axis=1) > 20)[0]
    return slice(max(rows[0], 0) + 8, rows[-1] - 7), slice(*COLS)


def hf_energy(x):
    f = np.abs(np.fft.rfft2(x - x.mean())) ** 2
    fy = np.abs(np.fft.fftfreq(x.shape[0]))[:, None]
    fx = np.fft.rfftfreq(x.shape[1])[None, :]
    return float(f[np.maximum(fy, fx) > 0.25].sum())


def displacement(a, r, ys, reach=6, block=192):
    """Integer (dy, dx) mapping r onto a, at the five most textured blocks.

    compare.py's fixed positions were chosen for the RGA test card; on film
    frames they can land on letterbox black or repeating credit lines, which
    correlate at any offset."""
    cand = []
    for y0 in range(ys.start + reach, ys.stop - block - reach, block):
        for x0 in range(reach + 8, W - block - reach - 8, block):
            cand.append((r[y0:y0 + block, x0:x0 + block].std(), y0, x0))
    found = set()
    for _, y0, x0 in sorted(cand)[-5:]:
        rb = r[y0:y0 + block, x0:x0 + block]
        rb = rb - rb.mean()
        best = None
        for dy in range(-reach, reach + 1):
            for dx in range(-reach, reach + 1):
                p = a[y0 + dy:y0 + dy + block, x0 + dx:x0 + dx + block]
                p = p - p.mean()
                c = (p * rb).sum() / np.sqrt((p * p).sum() * (rb * rb).sum() + 1e-9)
                if best is None or c > best[0]:
                    best = (c, dy, dx)
        found.add(best[1:])
    return found


def subpixel(a, r):
    from skimage.registration import phase_cross_correlation
    c = (slice(H // 2 - 512, H // 2 + 512), slice(W // 2 - 512, W // 2 + 512))
    s, _, _ = phase_cross_correlation(r[c], a[c], upsample_factor=20)
    return s


def still(o):
    ref = frames(o.ref)
    cands = [(c.split('=', 1)[0], frames(c.split('=', 1)[1])) for c in o.candidates]
    for k, rf in enumerate(ref):
        r = luma(rf)
        ys, xs = active(r)
        rr = r[ys, xs]
        hf_r = hf_energy(rr)
        print(f'frame {k}: rows {ys.start}..{ys.stop}')
        for name, fs in cands:
            a = luma(fs[k])
            moved = displacement(a, r, ys)
            worst = max(max(abs(dy), abs(dx)) for dy, dx in moved)
            sp = subpixel(a, r)
            if worst >= 2:
                print(f'  {name:14s} REFUSED: displaced {sorted(moved)}')
                continue
            x = a[ys, xs]
            om, op = overshoot(x, rr)
            print(f'  {name:14s} PSNR {psnr(x, rr):6.2f}  SSIM {ssim(x, rr):.4f}  detail {detail(x) / detail(rr):.3f}'
                  f'  hf {hf_energy(x) / hf_r:.3f}  overshoot {om:.3f} ({op:.2f}% >4)'
                  f'  shift dy,dx {sp[0]:+.2f},{sp[1]:+.2f}')
        if o.crops:
            from PIL import Image
            os.makedirs(o.crops, exist_ok=True)
            imgs = [nv12_rgb_frame(rf)] + [nv12_rgb_frame(fs[k]) for _, fs in cands]
            pts = [tuple(map(int, p.split(','))) for p in o.at[k].split(';')] if o.at else []
            for j, (y0, x0) in enumerate(pts):
                row = np.hstack([np.pad(i[y0:y0 + 270, x0:x0 + 320], ((0, 0), (0, 6), (0, 0)), constant_values=255)
                                 for i in imgs])
                Image.fromarray(row).resize((row.shape[1] * 2, row.shape[0] * 2), Image.NEAREST).save(
                    os.path.join(o.crops, f'{o.tag}{k}-{j}.png'))
    if o.crops:
        print(f'crops: reference | ' + ' | '.join(n for n, _ in cands))


def nv12_rgb_frame(f):
    tmp = '/dev/shm/quality-frame.nv12'
    f.tofile(tmp)
    return nv12_rgb(tmp, W, H)


def temporal(o):
    ref = [luma(f) for f in frames(o.ref)]
    ys, xs = active(ref[0])
    dref = [ref[k + 1][ys, xs] - ref[k][ys, xs] for k in range(len(ref) - 1)]
    print(f'{len(ref)} frames, reference mean |frame diff| {np.mean([np.abs(d).mean() for d in dref]):.3f}')
    for c in o.candidates:
        name, path = c.split('=', 1)
        a = [luma(f)[ys, xs] for f in frames(path)]
        errs, worst = [], []
        for k in range(len(a) - 1):
            e = np.abs((a[k + 1] - a[k]) - dref[k])
            errs.append(e.mean())
            hb, wb = e.shape[0] // 64, e.shape[1] // 64
            blocks = e[:hb * 64, :wb * 64].reshape(hb, 64, wb, 64).mean(axis=(1, 3)).ravel()
            worst.append(np.sort(blocks)[-max(1, len(blocks) // 20):].mean())
        print(f'  {name:14s} temporal error mean {np.mean(errs):.3f} max {np.max(errs):.3f}'
              f'  worst-5% blocks {np.mean(worst):.3f}')


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('ref')
    ap.add_argument('candidates', nargs='+', metavar='NAME=FILE')
    ap.add_argument('--temporal', action='store_true')
    ap.add_argument('--crops')
    ap.add_argument('--tag', default='f')
    ap.add_argument('--at', nargs='*', help='per frame: "y,x;y,x" crop origins')
    ap.add_argument('--cols', help='A:B, columns to measure (pillarboxed pictures)')
    o = ap.parse_args()
    if o.cols:
        global COLS
        COLS = tuple(map(int, o.cols.split(':')))
    (temporal if o.temporal else still)(o)


if __name__ == '__main__':
    main()
