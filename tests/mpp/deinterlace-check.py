#!/usr/bin/env python3
"""Each picture MPP's deinterlacer put out shows the time it says it is.

deinterlace-check.py <frames.yuv> <pts.txt> <width> <height> <px> <first-field> [<key-after-hole>]

frames.yuv is the decoder's output, one picture after another, luma first
(NV12 or NV16: the size of one is the file's over the count); pts.txt their
timestamps in nanoseconds, one a line. The streams (tests/mpp) show a white
bar <px> pixels wide at x = px * (n + 1) in their n-th field (read back with
FFmpeg, undeinterlaced), every field its own: a picture's content time is
where its bar is. Each starts at an open GOP's key frame, field
<first-field>, with a B picture before it in time that needs what came
before the cut.

Passes when the first picture is the key frame's first field, every picture
shows the field its timestamp names, and no field from there on is missing.

With <key-after-hole>, the stream has lost 480 ms from the middle of a
picture on: every field up to the loss as above, then nothing until the
first key frame after it, field <key-after-hole>, and from its first field
on every field again, at one time (tsdemux maps the clock anew after a loss,
the same for every stream of the programme: the offset is constant, the
same for the sound).
"""
import sys

frames, pts_file = sys.argv[1], sys.argv[2]
w, h, px, first = (int(a) for a in sys.argv[3:7])
key = int(sys.argv[7]) if len(sys.argv) > 7 else None
FIELD_NS = 20_000_000
data = open(frames, "rb").read()
pts = [int(l) for l in open(pts_file) if l.strip()]
size = len(data) // len(pts) if pts else 0
if not pts or size * len(pts) != len(data) or size < w * h:
    sys.exit(f"FAIL: {len(data)} bytes for {len(pts)} timestamps of {w}x{h}")
n = len(pts)
problems = []


def bar(i):
    # Mean luma of every column over a band of rows; the bar is the
    # brightest run. Its centre, in fields.
    base = i * size
    cols = [0] * w
    for y in range(h // 4, h // 4 + 32, 4):
        row = data[base + y * w: base + (y + 1) * w]
        for x in range(w):
            cols[x] += row[x]
    top = max(cols)
    xs = [x for x in range(w) if cols[x] > top * 0.6]
    if not xs or top == 0:
        return -1.0
    return (sum(xs) / len(xs) - (px - 1) / 2) / px - 1


shown = [bar(i) for i in range(n)]
said = [first + (p - pts[0]) / FIELD_NS for p in pts]
if abs(shown[0] - first) > 0.25:
    problems.append(f"first picture shows field {shown[0]:.2f}, not the key frame's first ({first})")
steps = [said[i + 1] - said[i] for i in range(n - 1)]
holes = [i for i, s in enumerate(steps) if abs(s - 1) > 0.01]
if key is None:
    if holes:
        problems.append(f"timestamp steps {sorted({round(s * 20, 1) for s in steps})} ms, not 20")
    parts = [(0, n, 0.0)]
else:
    if len(holes) != 1:
        problems.append(f"{len(holes)} breaks in the timestamps, not the one of the loss")
    cut = holes[0] + 1 if holes else n
    offset = shown[cut] - said[cut] if cut < n else 0.0
    if cut < n and abs(shown[cut] - key) > 0.25:
        problems.append(f"after the loss the first picture shows field {shown[cut]:.2f}, not the key frame's first ({key})")
    parts = [(0, cut, 0.0), (cut, n, offset)]
for lo, hi, offset in parts:
    for i in range(lo, hi):
        if abs(shown[i] - said[i] - offset) > 0.25:
            problems.append(f"picture {i}: says field {said[i]:.2f}{offset:+.0f}, shows {shown[i]:.2f}")
print(f"{n} pictures, fields {shown[0]:.2f}..{shown[-1]:.2f}" + (f", after the loss from {shown[holes[0] + 1]:.2f}" if key is not None and holes else ""))
for p in problems[:12]:
    print("  " + p)
print("PASS" if not problems else f"FAIL ({len(problems)})")
sys.exit(1 if problems else 0)
