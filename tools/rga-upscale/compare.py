#!/usr/bin/env python3
"""Upscaled frames against a reference, with the geometry checked first.

  compare.py REF.nv12 NAME=FILE.nv12 [NAME=FILE.nv12 ...] [--size WxH]
             [--diagnostic-shift N] [--crops DIR]

Every file is tightly packed NV12 at the same size (3840x2160 by default):
writeback captures from wbcap, or buffers downloaded straight out of the
filter chain. Before any quality number is printed, each candidate's
displacement against the reference is measured by block cross-correlation at
a few positions. Two destination pixels or more anywhere and the candidate is
refused: a translated picture loses PSNR and SSIM to the translation, not to
the scaler, and those numbers mean nothing. One pixel is reported and let
through, because VOP2 -- the production scaler, the thing being compared
against -- is itself align-corners and drifts from -0.5 to +0.56 pixel across
a 2x frame, so a corner of it lands one whole pixel away. Integer
correlation cannot see less than that; geom.py measures geometry, this does
not.

--diagnostic-shift N undoes an N-pixel translation (down and right) before
measuring. It exists to ask "what would this scaler score if it were placed
correctly"; the answer is a diagnosis, not a result.

Reported per quadrant of the test card built by README.md (photo, zone plate,
text, testsrc2), on luma, with an 8 pixel border left out:

  PSNR, SSIM         against the reference
  detail             mean gradient magnitude / the reference's -- what was
                     recovered, or invented, in edges and texture
  overshoot          mean amount by which a pixel leaves the reference's own
                     5x5 min..max range, and the share of pixels leaving it by
                     more than 4 codes -- ringing and halo, kept apart from
                     "sharper" on purpose

Needs numpy and scipy; --crops also needs Pillow.
"""
import argparse
import os

import numpy as np
from scipy import ndimage as nd


def luma(path, w, h):
    return np.fromfile(path, np.uint8)[: w * h].reshape(h, w).astype(np.float64)


def nv12_rgb(path, w, h):
    a = np.fromfile(path, np.uint8)
    y = a[: w * h].reshape(h, w).astype(np.float64)
    c = a[w * h : w * h * 3 // 2].reshape(h // 2, w // 2, 2).astype(np.float64)
    u = np.repeat(np.repeat(c[..., 0], 2, 0), 2, 1) - 128
    v = np.repeat(np.repeat(c[..., 1], 2, 0), 2, 1) - 128
    yy = (y - 16) * 255 / 219
    k = 255 / 224
    rgb = np.dstack([yy + 1.5748 * v * k, yy - 0.1873 * u * k - 0.4681 * v * k, yy + 1.8556 * u * k])
    return np.clip(rgb, 0, 255).astype(np.uint8)


def ssim(x, y):
    c1, c2 = (0.01 * 255) ** 2, (0.03 * 255) ** 2
    g = lambda z: nd.gaussian_filter(z, 1.5)
    mx, my = g(x), g(y)
    sx, sy, sxy = g(x * x) - mx * mx, g(y * y) - my * my, g(x * y) - mx * my
    return float(np.mean(((2 * mx * my + c1) * (2 * sxy + c2)) / ((mx * mx + my * my + c1) * (sx + sy + c2))))


def psnr(x, y):
    return float(10 * np.log10(255 ** 2 / np.mean((x - y) ** 2)))


def detail(x):
    return float(np.mean(np.hypot(nd.sobel(x, 0), nd.sobel(x, 1))))


def overshoot(x, ref):
    hi, lo = nd.maximum_filter(ref, 5), nd.minimum_filter(ref, 5)
    e = np.maximum(x - hi, 0) + np.maximum(lo - x, 0)
    return float(e.mean()), float((e > 4).mean() * 100)


def displacement(a, ref, reach=6, block=256):
    """Integer (dy, dx) that best maps ref onto a, at five positions.

    The positions avoid the seams between quadrants and the middle of the
    credits, whose lines repeat vertically and correlate at more than one
    offset."""
    h, w = ref.shape
    found = set()
    for fy, fx in ((0.05, 0.03), (0.05, 0.39), (0.56, 0.03), (0.86, 0.05), (0.86, 0.89)):
        y0, x0 = int(h * fy), int(w * fx)
        r = ref[y0 : y0 + block, x0 : x0 + block]
        r = r - r.mean()
        best = None
        for dy in range(-reach, reach + 1):
            for dx in range(-reach, reach + 1):
                p = a[y0 + dy : y0 + dy + block, x0 + dx : x0 + dx + block]
                p = p - p.mean()
                c = (p * r).sum() / np.sqrt((p * p).sum() * (r * r).sum() + 1e-9)
                if best is None or c > best[0]:
                    best = (c, dy, dx)
        found.add(best[1:])
    return found


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("ref")
    ap.add_argument("candidates", nargs="+", metavar="NAME=FILE")
    ap.add_argument("--size", default="3840x2160")
    ap.add_argument("--diagnostic-shift", type=int, default=0)
    ap.add_argument("--crops")
    o = ap.parse_args()
    w, h = map(int, o.size.split("x"))
    ref = luma(o.ref, w, h)
    m = 8
    quads = {
        "photo": (slice(m, h // 2), slice(m, w // 2)),
        "zoneplate": (slice(m, h // 2), slice(w // 2, w - m)),
        "text": (slice(h // 2, h - m), slice(m, w // 2)),
        "testsrc2": (slice(h // 2, h - m), slice(w // 2, w - m)),
    }
    for spec in o.candidates:
        name, path = spec.split("=", 1)
        a = luma(path, w, h)
        s = o.diagnostic_shift
        if s:
            a = np.roll(np.roll(a, -s, 0), -s, 1)
        moved = displacement(a, ref)
        print(f"{name}: displacement (dy, dx) {sorted(moved)}" + (f" after undoing {s}" if s else ""))
        worst = max(max(abs(dy), abs(dx)) for dy, dx in moved)
        if worst >= 2:
            print("  REFUSED: the picture is displaced; quality numbers would measure the displacement")
            continue
        if worst == 1:
            print("  note: a one-pixel residual somewhere; read geom.py before trusting small differences")
        for q, (ys, xs) in quads.items():
            x, r = a[ys, xs], ref[ys, xs]
            om, op = overshoot(x, r)
            print(f"  {q:9s} PSNR {psnr(x, r):5.2f}  SSIM {ssim(x, r):.4f}  detail {detail(x) / detail(r):.3f}"
                  f"  overshoot {om:.3f} ({op:.2f}% px > 4)")
    if o.crops:
        os.makedirs(o.crops, exist_ok=True)
        from PIL import Image
        imgs = [nv12_rgb(o.ref, w, h)] + [nv12_rgb(c.split("=", 1)[1], w, h) for c in o.candidates]
        for q, (ys, xs) in quads.items():
            y0, x0 = ys.start + 200, xs.start + 200
            row = np.hstack([np.pad(i[y0 : y0 + 360, x0 : x0 + 640], ((0, 0), (0, 8), (0, 0)), constant_values=255) for i in imgs])
            Image.fromarray(row).save(os.path.join(o.crops, f"crop-{q}.png"))
        print(f"crops in {o.crops}: reference | " + " | ".join(c.split("=", 1)[0] for c in o.candidates))


if __name__ == "__main__":
    main()
