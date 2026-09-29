"""Synthetic films for the sync engine: speech, the subtitle made for it, and
what a voice-activity detector would have heard.

Deterministic by seed. The numbers are chosen to be worse than a clean
recording, not better: the detector misses lines, hears music and effects as
speech, and places every edge a little wrong, and the subtitle has lead-ins,
merged lines, untranslated lines and sign captions that nobody says.
"""

from __future__ import annotations

import random
from dataclasses import dataclass
from typing import Callable, Sequence

from media.subtitles.sync import Interval, Window, merge


@dataclass(frozen=True)
class Film:
    duration: float
    #: When somebody actually speaks, in video time.
    speech: tuple[Interval, ...]
    #: What a detector heard: `speech`, damaged.
    heard: tuple[Interval, ...]
    #: The subtitle as it would be for this exact video.
    reference: tuple[Interval, ...]


def film(
    seed: int,
    duration: float = 5400.0,
    *,
    mean_gap: float = 3.5,
    quiet_scenes: int = 12,
    miss_rate: float = 0.15,
    false_rate: float = 1 / 12.0,
    edge_jitter: float = 0.2,
) -> Film:
    rng = random.Random(seed)
    # Quiet stretches: action, music, titles. Nobody speaks in them.
    quiet: list[Interval] = []
    for _ in range(quiet_scenes):
        start = rng.uniform(0, duration)
        quiet.append((start, start + rng.uniform(40, 150)))
    quiet = merge(quiet)

    speech: list[Interval] = []
    t = rng.uniform(20, 60)
    while t < duration:
        length = rng.uniform(0.7, 4.5)
        if not any(a <= t <= b for a, b in quiet):
            speech.append((t, min(duration, t + length)))
        t += length + rng.expovariate(1.0 / mean_gap)

    heard: list[Interval] = []
    for start, end in speech:
        if rng.random() < miss_rate:
            continue
        heard.append((start + rng.gauss(0, edge_jitter), end + rng.gauss(0, edge_jitter)))
    t = 0.0
    while t < duration:
        t += rng.expovariate(false_rate)
        heard.append((t, t + rng.uniform(0.3, 3.0)))
    heard = merge(((max(0.0, a), min(duration, b)) for a, b in heard), gap=0.15)

    reference: list[Interval] = []
    index = 0
    while index < len(speech):
        start, end = speech[index]
        # Two short lines one after the other are often one cue.
        if index + 1 < len(speech) and speech[index + 1][0] - end < 0.6 and rng.random() < 0.3:
            end = speech[index + 1][1]
            index += 1
        index += 1
        if rng.random() < 0.06:
            continue
        reference.append((start - rng.uniform(0.0, 0.25), end + rng.uniform(0.2, 0.9)))
    # Sign captions and song lyrics: on screen with nobody talking.
    for _ in range(int(duration / 400)):
        start = rng.uniform(0, duration)
        reference.append((start, start + rng.uniform(1.5, 4.0)))
    reference.sort()
    return Film(duration, tuple(speech), tuple(heard), tuple(reference))


def listen(heard: Sequence[Interval], spans: Sequence[Interval]) -> list[Window]:
    """What the detector reports for each sampled stretch."""
    windows = []
    for a, b in spans:
        inside = tuple(
            (max(a, start), min(b, end)) for start, end in heard if end > a and start < b
        )
        windows.append(Window(a, b, inside))
    return windows


def invert(reference: Sequence[Interval], video_to_subtitle: Callable[[float], float | None]) -> list[Interval]:
    """The subtitle file's own times, given where each line belongs in the video."""
    out = []
    for start, end in reference:
        a, b = video_to_subtitle(start), video_to_subtitle(end)
        if a is None or b is None or b <= a:
            continue
        out.append((a, b))
    return out


def cut(reference: Sequence[Interval], edits: Sequence[tuple[float, float]]) -> list[Interval]:
    """A subtitle made for a different cut.

    `edits` are (video time, seconds): at that point the subtitle's release
    has `seconds` more material than this video (positive: a scene this video
    lacks, so the subtitle's later lines are late by that much) or less
    (negative: this video has a scene the subtitle's release lacks). Returns
    the subtitle's own times.
    """
    edits = sorted(edits)
    out = []
    for start, end in reference:
        # Lines inside a scene only this video has are in no subtitle of the
        # other release.
        if any(seconds < 0 and at <= start < at - seconds for at, seconds in edits):
            continue
        shift = sum(seconds for at, seconds in edits if start >= at)
        out.append((start + shift, end + shift))
    # A scene this video lacks has lines in the subtitle too: invent them.
    rng = random.Random(len(edits))
    for at, seconds in edits:
        if seconds <= 0:
            continue
        base = at + sum(s for a, s in edits if a < at)
        t = base + 1.0
        while t < base + seconds - 3:
            out.append((t, t + rng.uniform(1.0, 3.0)))
            t += rng.uniform(3.0, 6.0)
    return sorted(out)
