#!/usr/bin/env python3
"""Where does a source pixel land after an upscale?

  geom.py pattern luma|chroma OUT.nv12 W H [EDGE]
  geom.py measure luma|chroma SRC.nv12 SW SH DST.nv12 DW DH
  geom.py pattern vlines|hlines|corners OUT.nv12 W H
  geom.py measure vlines|hlines|corners SRC.nv12 SW SH DST.nv12 DW DH

`pattern` writes isolated single-pixel impulses on a flat field, every 24
pixels, plus one row and column at distance EDGE (0, 1, 2, ...) from each
border. Edge rows are kept apart from each other on purpose: impulses one pixel
apart have overlapping responses and their centroids would pull together. The
chroma pattern puts the impulses in the Cb plane over a flat luma.

`measure` finds every impulse in SRC and the centroid of its response in DST,
and reports the displacement against the centre-aligned mapping, where source
pixel x lands at (x + 0.5) * DW / SW - 0.5. For interior impulses it fits

    x  ->  j = x / f + c

with f the factor the RGA3 kernel driver programs for an upscale
(rga3_reg_info.c: FACTOR_MAX * (sw - 1) / (dw - 1), minus one when exact) and
prints c in destination and in source pixels. A correct scaler has an offset
of 0 everywhere; the RK3588's RGA3 shows c = +1.000 source pixel at every
ratio measured, see README.md.

`vlines`/`hlines` are one-pixel lines every 48 pixels plus the first and
last column/row; each is located by the centroid of the destination's mean
profile across it. `corners` puts a different one-pixel L in each corner (arm
lengths 6x3, 6x5, 4x5, 4x7), so a mirrored, clipped or displaced corner shows
up as a moved centroid.

Needs numpy.
"""
import sys

import numpy as np

BG, PEAK, STEP = 64, 224, 24


def load(path, w, h):
    a = np.fromfile(path, np.uint8)
    y = a[: w * h].reshape(h, w).astype(np.float64)
    c = a[w * h : w * h * 3 // 2].reshape(h // 2, w // 2, 2).astype(np.float64)
    return y, c


def coords(n, edge):
    return sorted({edge, n - 1 - edge} | set(range(STEP, n - STEP, STEP)))


def pattern(kind, out, w, h, edge=0):
    y = np.full((h, w), BG if kind == "luma" else 128, np.uint8)
    c = np.full((h // 2, w // 2, 2), 128, np.uint8)
    plane = y if kind == "luma" else c[..., 0]
    for yy in coords(plane.shape[0], edge):
        for xx in coords(plane.shape[1], edge):
            plane[yy, xx] = PEAK
    with open(out, "wb") as f:
        f.write(y.tobytes())
        f.write(c.tobytes())


CORNERS = {  # name: (rows, cols) of the L, relative to the corner
    "top-left": ([0] * 6 + [1, 2], list(range(6)) + [0, 0]),
    "top-right": ([0] * 6 + [1, 2, 3, 4], [-1 - i for i in range(6)] + [-1] * 4),
    "bottom-left": ([-1] * 4 + [-2, -3, -4, -5], list(range(4)) + [0] * 4),
    "bottom-right": ([-1] * 4 + [-2 - i for i in range(6)], [-1 - i for i in range(4)] + [-1] * 6),
}


def pattern_shapes(kind, out, w, h):
    y = np.full((h, w), BG, np.uint8)
    c = np.full((h // 2, w // 2, 2), 128, np.uint8)
    if kind == "vlines":
        y[:, sorted({0, w - 1} | set(range(48, w - 1, 48)))] = PEAK
    elif kind == "hlines":
        y[sorted({0, h - 1} | set(range(48, h - 1, 48))), :] = PEAK
    else:
        for rows, cols in CORNERS.values():
            y[np.array(rows) % h, np.array(cols) % w] = PEAK
    with open(out, "wb") as f:
        f.write(y.tobytes())
        f.write(c.tobytes())


def measure_shapes(kind, src, sw, sh, dst, dw, dh):
    s, _ = load(src, sw, sh)
    d, _ = load(dst, dw, dh)
    kx, ky = dw / sw, dh / sh
    print(f"{kind} {sw}x{sh} -> {dw}x{dh}")
    if kind in ("vlines", "hlines"):
        vertical = kind == "vlines"
        k = kx if vertical else ky
        n, m = (sw, dw) if vertical else (sh, dh)
        prof = np.clip((d if vertical else d.T)[int(0.1 * (dh if vertical else dw)) : int(0.9 * (dh if vertical else dw))].mean(0) - BG, 0, None)
        lines = np.nonzero((s if vertical else s.T)[sh // 2 if vertical else sw // 2] > (BG + PEAK) / 2)[0]
        f = rga3_factor(n, m) if k > 1 else 1.0
        offs, cs = [], []
        reach = int(2 * k) + 4
        for x in lines:
            e = (x + 0.5) * k - 0.5
            a, b = max(int(e) - reach, 0), min(int(e) + reach + 1, m)
            win = prof[a:b]
            j = (win * np.arange(a, b)).sum() / win.sum()
            offs.append((x, j - e))
            if 6 <= x <= n - 7:
                cs.append(j - x / f)
        inner = [o for x, o in offs if 6 <= x <= n - 7]
        fit = np.polyfit([x for x, _ in offs if 6 <= x <= n - 7], inner, 1)
        print(f"  offset vs centre-aligned {np.polyval(fit, 0):+.3f} at 0, {np.polyval(fit, n - 1):+.3f} at {n - 1}"
              f" | c = {np.mean(cs):+.3f} dst px = {np.mean(cs) * f:+.3f} src px")
        for x, o in offs:
            if x < 6 or x > n - 7:
                print(f"    edge line {x}: {o:+.3f}")
        return
    for name, (rows, cols) in CORNERS.items():
        r, cl = np.array(rows) % sh, np.array(cols) % sw
        sy_, sx_ = r.mean(), cl.mean()
        ey, ex = (sy_ + 0.5) * ky - 0.5, (sx_ + 0.5) * kx - 0.5
        y0, x0 = (0 if rows[0] >= 0 else dh - 24), (0 if cols[0] >= 0 else dw - 24)
        win = np.clip(d[y0 : y0 + 24, x0 : x0 + 24] - BG, 0, None)
        if win.sum() <= 0:
            print(f"  {name:12s} marker lost")
            continue
        gy, gx = np.mgrid[y0 : y0 + 24, x0 : x0 + 24]
        cy, cx = (win * gy).sum() / win.sum(), (win * gx).sum() / win.sum()
        print(f"  {name:12s} centroid offset dx {cx - ex:+.3f} dy {cy - ey:+.3f} dst px")


def rga3_factor(sw, dw):
    """The upscale factor rga3_reg_info.c writes, as a fraction."""
    p = 65536 * (sw - 1) // (dw - 1)
    if (65536 * (sw - 1)) % (dw - 1) == 0:
        p -= 1
    return p / 65536


def measure(kind, src, sw, sh, dst, dw, dh):
    sy, sc = load(src, sw, sh)
    dy, dc = load(dst, dw, dh)
    s = sy if kind == "luma" else sc[..., 0]
    d = dy if kind == "luma" else dc[..., 0]
    bg = BG if kind == "luma" else 128
    kx, ky = d.shape[1] / s.shape[1], d.shape[0] / s.shape[0]
    reach = int(2 * max(kx, ky)) + 4
    rows = []
    for yy, xx in zip(*np.nonzero(s > (bg + PEAK) / 2)):
        ex, ey = (xx + 0.5) * kx - 0.5, (yy + 0.5) * ky - 0.5
        x0, x1 = max(int(ex) - reach, 0), min(int(ex) + reach + 1, d.shape[1])
        y0, y1 = max(int(ey) - reach, 0), min(int(ey) + reach + 1, d.shape[0])
        win = np.clip(d[y0:y1, x0:x1] - bg, 0, None)
        if win.sum() <= 0:
            continue
        gy, gx = np.mgrid[y0:y1, x0:x1]
        cx = (win * gx).sum() / win.sum()
        cy = (win * gy).sum() / win.sum()
        rows.append((yy, xx, cx, cy, cx - ex, cy - ey))
    r = np.array(rows)
    h, w = s.shape
    inner = (r[:, 0] >= 6) & (r[:, 0] <= h - 7) & (r[:, 1] >= 6) & (r[:, 1] <= w - 7)
    q = r[inner]
    print(f"{kind} {sw}x{sh} -> {dw}x{dh}: {len(r)} impulses, {inner.sum()} interior")
    for axis, src_col, pos_col, off_col, n, k in (("x", 1, 2, 4, w, kx), ("y", 0, 3, 5, h, ky)):
        fit = np.polyfit(q[:, src_col], q[:, off_col], 1)
        # One factor register, programmed from the luma sizes: the chroma
        # plane is walked with the same step.
        luma = n * (2 if kind == "chroma" else 1)
        f = rga3_factor(luma, int(round(luma * k))) if k > 1 else 1.0
        c = q[:, pos_col] - q[:, src_col] / f
        print(f"  {axis}: offset vs centre-aligned {np.polyval(fit, 0):+.3f} at {axis}=0,"
              f" {np.polyval(fit, n - 1):+.3f} at {axis}={n - 1} (dst px)"
              f" | j = {axis}/f + c with f={f:.6f}: c = {c.mean():+.3f} dst px"
              f" (sd {c.std():.3f}) = {c.mean() * f:+.3f} src px")
    for name, col, n in (("x", 1, w), ("y", 0, h)):
        for v in sorted({int(e) for e in r[:, col] if e < 6 or e > n - 7}):
            sel = r[r[:, col] == v]
            print(f"    edge {name}={v:4d}: offset {np.mean(sel[:, 4 if name == 'x' else 5]):+.3f}")


def main(argv):
    if len(argv) >= 5 and argv[0] == "pattern" and argv[1] in ("vlines", "hlines", "corners"):
        pattern_shapes(argv[1], argv[2], int(argv[3]), int(argv[4]))
    elif len(argv) == 8 and argv[0] == "measure" and argv[1] in ("vlines", "hlines", "corners"):
        measure_shapes(argv[1], argv[2], int(argv[3]), int(argv[4]), argv[5], int(argv[6]), int(argv[7]))
    elif len(argv) >= 5 and argv[0] == "pattern":
        pattern(argv[1], argv[2], int(argv[3]), int(argv[4]), int(argv[5]) if len(argv) > 5 else 0)
    elif len(argv) == 8 and argv[0] == "measure":
        measure(argv[1], argv[2], int(argv[3]), int(argv[4]), argv[5], int(argv[6]), int(argv[7]))
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main(sys.argv[1:])
