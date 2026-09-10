#!/usr/bin/env python3
"""Measure the pixel displacement between two DRM writeback captures.

The horizontal-shift fault has to be reported as a number, not as an
impression of a photograph, so this does the comparison the way image
registration does it: reduce each frame to a projection profile, then find the
integer shift that maximises the normalised cross-correlation between the two.

Only the Y plane of an NV12 capture is used. Luma carries every edge in the
test pattern, chroma is subsampled and would halve the horizontal resolution
of the very measurement being made.

Integer-pixel accuracy is deliberate: the reported fault is a visible shift of
whole pixels, and a subpixel estimate would imply a precision the capture path
does not have.
"""
import argparse
import sys


def read_luma(path, width, height):
    """The Y plane is the first width*height bytes of an NV12 buffer."""
    with open(path, "rb") as fh:
        data = fh.read(width * height)
    if len(data) < width * height:
        raise SystemExit("%s: short read (%d of %d bytes)"
                         % (path, len(data), width * height))
    return data


def column_profile(luma, width, y0, y1):
    """Mean luma per column over a row band, as a list of floats."""
    acc = [0] * width
    for y in range(y0, y1):
        row = luma[y * width:(y + 1) * width]
        for x in range(width):
            acc[x] += row[x]
    n = float(y1 - y0)
    return [v / n for v in acc]


def row_profile(luma, width, height, x0, x1):
    """Mean luma per row over a column band."""
    out = []
    for y in range(height):
        row = luma[y * width:(y + 1) * width]
        out.append(sum(row[x0:x1]) / float(x1 - x0))
    return out


def coherence(luma, width, height, rows):
    """How well rows inside ONE capture agree with each other.

    A writeback that races the composition comes back torn: every row is a
    valid-looking slice of a different instant. Compared row-to-row such a
    capture disagrees with itself, and any displacement measured against it is
    an artefact. The test pattern is identical on every line by construction,
    so for a sound capture this returns ~1.0.
    """
    profiles = [list(luma[y * width:(y + 1) * width]) for y in rows]
    scores = []
    for i in range(1, len(profiles)):
        scores.append(ncc(profiles[0], profiles[i], 0))
    return min(scores) if scores else 1.0


def ncc(a, b, shift):
    """Normalised cross-correlation of b against a, b displaced by `shift`.

    Only the overlapping span is scored, and each side is mean-centred inside
    that span, so a shift is not rewarded merely for aligning brighter regions.
    """
    n = len(a)
    lo = max(0, shift)
    hi = min(n, n + shift)
    if hi - lo < n // 4:
        return 0.0
    xs = a[lo:hi]
    ys = b[lo - shift:hi - shift]
    m = len(xs)
    mx = sum(xs) / m
    my = sum(ys) / m
    num = sxx = syy = 0.0
    for i in range(m):
        dx = xs[i] - mx
        dy = ys[i] - my
        num += dx * dy
        sxx += dx * dx
        syy += dy * dy
    if sxx <= 0 or syy <= 0:
        return 0.0
    return num / (sxx * syy) ** 0.5


def is_flat(profile):
    """True when a projection carries no structure to register against.

    The test pattern is uniform down each column by construction -- the fault
    under investigation is horizontal -- so its row profile is constant and no
    vertical displacement is recoverable from it. Saying so is the honest
    answer; picking the arg-max of an all-zero correlation would report a
    confident-looking number that means nothing.
    """
    lo, hi = min(profile), max(profile)
    return (hi - lo) < 0.5


def best_shift(a, b, limit):
    """Return (best_shift, best_score, runner_up_score_outside_the_peak)."""
    scores = {s: ncc(a, b, s) for s in range(-limit, limit + 1)}
    best = max(scores, key=scores.get)
    # The runner-up is taken from outside a +/-1 neighbourhood of the peak:
    # adjacent samples of a correlation peak are always high and would make
    # every result look ambiguous.
    others = [v for s, v in scores.items() if abs(s - best) > 1]
    return best, scores[best], (max(others) if others else 0.0)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("reference")
    ap.add_argument("test")
    ap.add_argument("--width", type=int, default=3840)
    ap.add_argument("--height", type=int, default=2160)
    ap.add_argument("--band", default="900:1100",
                    help="row band y0:y1 used for the horizontal measurement")
    ap.add_argument("--col-band", default="1800:2200",
                    help="column band x0:x1 used for the vertical measurement")
    ap.add_argument("--limit", type=int, default=64, help="max shift searched")
    args = ap.parse_args()

    y0, y1 = (int(v) for v in args.band.split(":"))
    x0, x1 = (int(v) for v in args.col_band.split(":"))

    ref = read_luma(args.reference, args.width, args.height)
    tst = read_luma(args.test, args.width, args.height)

    probe_rows = [r for r in (200, 600, 1000, 1400, 1800) if r < args.height]
    coh_ref = coherence(ref, args.width, args.height, probe_rows)
    coh_tst = coherence(tst, args.width, args.height, probe_rows)

    print("reference: %s" % args.reference)
    print("test:      %s" % args.test)
    print("geometry:  %dx%d NV12, luma plane only" % (args.width, args.height))
    print("bands:     rows %d:%d for dx, columns %d:%d for dy" % (y0, y1, x0, x1))
    print("coherence: reference %.4f  test %.4f" % (coh_ref, coh_tst))
    if min(coh_ref, coh_tst) < 0.95:
        print("\nCAPTURE INVALID: a capture disagrees with itself across rows,")
        print("so it was torn during readback. No displacement is measurable")
        print("from it -- fix the capture before reading anything into dx.")
        return 2

    if ref == tst:
        print("\nidentical: the two captures are byte-for-byte equal")

    cp_ref = column_profile(ref, args.width, y0, y1)
    cp_tst = column_profile(tst, args.width, y0, y1)
    dx, dx_score, dx_next = best_shift(cp_ref, cp_tst, args.limit)

    rp_ref = row_profile(ref, args.width, args.height, x0, x1)
    rp_tst = row_profile(tst, args.width, args.height, x0, x1)
    dy_flat = is_flat(rp_ref) or is_flat(rp_tst)
    dy, dy_score, dy_next = (0, 0.0, 0.0) if dy_flat \
        else best_shift(rp_ref, rp_tst, args.limit)

    print("\nwriteback_dx_pixels = %d" % dx)
    print("writeback_dy_pixels = %s"
          % ("not measurable (uniform row profile by design)" if dy_flat else dy))
    print("dx_correlation      = %.6f (next best outside peak %.6f)" % (dx_score, dx_next))
    if not dy_flat:
        print("dy_correlation      = %.6f (next best outside peak %.6f)"
              % (dy_score, dy_next))
    # A displacement is only worth reporting if its peak is both strong and
    # clearly better than everything else; otherwise the profiles are too
    # self-similar for the answer to mean anything.
    confident = dx_score > 0.9 and (dx_score - dx_next) > 0.02
    print("confidence          = %s" % ("high" if confident else "low"))
    return 0


if __name__ == "__main__":
    sys.exit(main())
