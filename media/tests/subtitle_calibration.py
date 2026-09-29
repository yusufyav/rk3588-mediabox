"""The calibration run behind the sync engine's thresholds.

    python3 -m media.tests.subtitle_calibration [seeds]

For each seed, one synthetic film and five subtitles for it -- shifted,
frame-rate converted, cut once, cut twice, and one for another film -- each
put through the staged listening plan the worker uses. It prints, per kind,
how many corrections were applied and right, and how many were applied and
wrong. The second number is the one that has to stay at zero.
"""

from __future__ import annotations

import statistics
import sys
import time

from .subtitle_synth import cut, film, invert, listen
from media.subtitles.sync import align, next_windows

NTSC = 24000 / 1001


def synchronise(sub, f):
    spans, result = [], None
    while True:
        more = next_windows(result, f.duration, spans, seekable=True)
        if not more:
            return result, spans
        spans = sorted(spans + more)
        result = align(sub, listen(f.heard, spans), duration=f.duration)


def cases(seed):
    f = film(seed)
    offset = [2.5, -7.3, 41.0, 0.0, -1.2][seed % 5]
    scale = 25 / NTSC if seed % 2 else NTSC / 25
    drift_offset = [1.0, -3.0, 12.0][seed % 3]
    edits = {
        "cut": [(1800.0 + seed * 37 % 600, 45.0)],
        "cuts": [(1500.0, -30.0), (3600.0, 20.0)],
    }

    def pairs_for(edit):
        return [
            (v + sum(s for a, s in edit if v >= a), v)
            for v, _ in f.reference
            if not any(s < 0 and a <= v < a - s for a, s in edit)
        ]

    sub = invert(f.reference, lambda v: v - offset)
    yield "offset", f, sub, [(s, s + offset) for s, _ in sub]
    sub = invert(f.reference, lambda v: (v - drift_offset) / scale)
    yield "linear", f, sub, [(s, scale * s + drift_offset) for s, _ in sub]
    for kind, edit in edits.items():
        yield kind, f, cut(f.reference, edit), pairs_for(edit)
    yield "unrelated", f, list(film(seed + 1000).reference), None


def run(seeds):
    table: dict[str, list] = {}
    for seed in seeds:
        for kind, f, sub, pairs in cases(seed):
            started = time.perf_counter()
            result, spans = synchronise(sub, f)
            elapsed = time.perf_counter() - started
            wrong = right = False
            if result.decision == "apply":
                mapping = result.mapping
                errors = sorted(abs(mapping(s) - v) for s, v in pairs) if pairs else None
                if errors and errors[int(len(errors) * 0.9)] < 0.5:
                    right = True
                else:
                    wrong = True
            table.setdefault(kind, []).append((right, wrong, result, len(spans), elapsed))
    return table


def main(argv):
    count = int(argv[1]) if len(argv) > 1 else 40
    table = run(range(count))
    for kind, rows in table.items():
        right = sum(1 for r, *_ in rows if r)
        wrong = sum(1 for _, w, *_ in rows if w)
        windows = statistics.mean(n for *_, n, _ in rows)
        seconds = statistics.mean(t for *_, t in rows)
        print(
            f"{kind:10s} applied-right={right:3d}/{len(rows)} applied-WRONG={wrong} "
            f"mean-windows={windows:5.1f} mean-cpu={seconds:5.2f}s"
        )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
