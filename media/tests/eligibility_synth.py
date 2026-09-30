"""Synthetic films for the subtitle pre-filter.

One film's dialogue -- when somebody speaks -- is written twice, as two
languages would: lines split, merged and dropped differently, each edge a
few frames off. One rendition is the embedded track, the other the external
subtitle, and the external one is then put on another timeline (a timebase,
an offset, a cut, a truncation). Nothing lines up cue for cue; only the
activity does, which is all the pre-filter may use.
"""

from __future__ import annotations

import random

Line = tuple[float, float]


def dialogue(seed: int, duration: float = 6700.0, credits: float = 240.0) -> list[Line]:
    """Scenes of talk and scenes of none, up to the credits."""
    rng = random.Random(seed)
    lines: list[Line] = []
    t = rng.uniform(20.0, 40.0)
    end = duration - credits
    while t < end:
        scene = rng.uniform(60.0, 300.0)
        stop = min(end, t + scene)
        while t < stop:
            length = rng.uniform(1.0, 4.5)
            lines.append((t, min(stop, t + length)))
            t += length + rng.uniform(0.2, 2.5)
        t = stop + rng.uniform(10.0, 90.0)
    return [(a, b) for a, b in lines if b - a > 0.3]


def rendition(
    lines: list[Line],
    seed: int,
    *,
    split: float = 0.20,
    merge: float = 0.15,
    drop: float = 0.05,
    jitter: float = 0.06,
    lag: float = 0.0,
    scene_lag: float = 0.0,
) -> list[Line]:
    """The same dialogue as one language's subtitle author wrote it.

    `lag` is a per-line offset spread (seconds), `scene_lag` one per scene
    (lines less than 8 s apart): authoring that is looser in one language,
    and that loosens and tightens over a film -- what makes measured window
    offsets scatter 0.1-0.3 s about their line."""
    rng = random.Random(seed)
    out: list[Line] = []
    i = 0
    scene_shift = 0.0
    previous_end = -1e9
    while i < len(lines):
        if lines[i][0] - previous_end > 8.0:
            scene_shift = rng.gauss(0.0, scene_lag) if scene_lag else 0.0
        previous_end = lines[i][1]
        a, b = lines[i]
        roll = rng.random()
        if roll < drop:
            i += 1
            continue
        if roll < drop + merge and i + 1 < len(lines) and lines[i + 1][0] - b < 1.5:
            b = lines[i + 1][1]
            i += 1
        shift = (rng.gauss(0.0, lag) if lag else 0.0) + scene_shift
        a, b = a + rng.gauss(0.0, jitter) + shift, b + rng.gauss(0.0, jitter) + shift
        if rng.random() < split and b - a > 2.0:
            middle = (a + b) / 2 + rng.uniform(-0.3, 0.3)
            out.append((a, middle - 0.05))
            out.append((middle + 0.05, b))
        elif b > a:
            out.append((a, b))
        i += 1
    return sorted(out)


def picture_events(lines: list[Line]) -> tuple[float, ...]:
    """A PGS track's packets: one when a line appears, one when it clears."""
    return tuple(sorted([a for a, _ in lines] + [b for _, b in lines]))


def text_events(lines: list[Line]) -> tuple[float, ...]:
    """A text track's blocks: one per line, at its start."""
    return tuple(sorted(a for a, _ in lines))


def on_timeline(lines: list[Line], ratio: float, offset: float) -> list[Line]:
    """The subtitle whose timeline maps onto the video as
    t_video = ratio * t_subtitle + offset."""
    return [((a - offset) / ratio, (b - offset) / ratio) for a, b in lines]


def with_step(lines: list[Line], at: float, jump: float) -> list[Line]:
    """A different cut: everything after `at` (video time) sits `jump`
    seconds away from the rest."""
    return [(a + jump, b + jump) if a >= at else (a, b) for a, b in lines]
