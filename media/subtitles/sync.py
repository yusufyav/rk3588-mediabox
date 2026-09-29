"""Where a subtitle's lines belong in the film, worked out from the sound.

The question is `video_time = f(subtitle_time)`, and the answer is one of:

    offset     f(t) = t + b                         one release, shifted
    linear     f(t) = a * t + b                     a frame-rate conversion
    piecewise  f(t) = a * t + b_k on range k        a different cut
    rejected   nothing fits well enough to be believed

Nothing here listens for words. The evidence is when somebody is speaking --
an interval list per sampled stretch of the film, produced by `audio.py` --
and when the subtitle has a line on screen. A subtitle made for this film has
its lines where the speech is, give or take a lead-in; one made for another
film does not, at any shift.

The same idea as ffsubsync and alass (MIT and GPL-3.0 respectively; neither is
used or copied): score the agreement of two on/off signals at every shift and
take the best. What is different is the size of the evidence. They decode the
whole soundtrack; this box plays 4K films off a network and cannot afford to,
so the evidence is a handful of sampled windows and the scoring is done per
window, which is also what makes a change of offset in the middle of a film
visible at all.

Per window, the score at shift d is the Pearson correlation of the speech
indicator and the shifted cue indicator over the window. Both are unions of
intervals, so their overlap as a function of d is a sum of trapezoids; it is
accumulated exactly on a grid from the trapezoids' corners in O(pairs + grid),
with no per-sample arrays and no numpy (the worker has none).

Every result carries a confidence and a decision. A wrong correction is worse
than none, so the thresholds that turn a confidence into "apply" were set from
measurement (`media/tests/test_subtitle_sync.py` holds the cases and the
calibration note in `docs/subtitles.md` the numbers), not chosen by eye.
"""

from __future__ import annotations

import bisect
import itertools
import math
from dataclasses import dataclass, field
from typing import Any, Iterable, Sequence


#: Changes whenever a change here could change an answer. Part of every cache
#: key, so a better algorithm is never served a worse one's conclusions.
#: mbsync-2: a subtitle that ends early is refused, and 5.1 is heard from its
#: centre channel -- answers of mbsync-1 for those films are not these.
ALGORITHM = "mbsync-2"

#: Subtitle timing bases that differ from the video's by a whole frame-rate
#: conversion. Each is tried as the scale before the offset is searched, which
#: is what turns a four per cent drift -- minutes by the end of a film -- into
#: a constant offset the search can find.
FRAME_RATE_RATIOS: tuple[float, ...] = (
    1.0,
    25 / (24000 / 1001),
    (24000 / 1001) / 25,
    24 / (24000 / 1001),
    (24000 / 1001) / 24,
    25 / 24,
    24 / 25,
)

Interval = tuple[float, float]


@dataclass(frozen=True, slots=True)
class Window:
    """One sampled stretch of the film and where speech was heard in it.

    Times are the film's own, in seconds. `speech` lies inside the window.
    """

    start: float
    end: float
    speech: tuple[Interval, ...]

    @property
    def length(self) -> float:
        return self.end - self.start

    @property
    def center(self) -> float:
        return (self.start + self.end) / 2

    @property
    def speech_fraction(self) -> float:
        if self.length <= 0:
            return 0.0
        return sum(end - start for start, end in self.speech) / self.length


@dataclass(frozen=True, slots=True)
class Params:
    #: How far a line may be from its speech, either way, in seconds.
    search_range: float = 150.0
    grid: float = 0.05
    fine_grid: float = 0.01
    #: Two windows agree when their best shifts are this close.
    inlier_tolerance: float = 0.35
    #: A window whose speech fraction is outside this says nothing: silence
    #: correlates with nothing and wall-to-wall "speech" is music.
    speech_fraction_range: tuple[float, float] = (0.04, 0.92)
    #: At least this many cues within reach of a window for it to count.
    min_cues_in_window: int = 3
    #: ... and this many separate stretches of speech in it. One or two
    #: utterances fit some line perfectly at some shift; measured, a window
    #: with a single two-second utterance scored 0.99 at 146 s from the truth.
    min_speech_segments: int = 3
    #: A window's peak is believed when it is at least this high ...
    min_peak: float = 0.18
    #: ... and this far above the best shift more than `rival_gap` from it.
    min_prominence: float = 0.04
    rival_gap: float = 1.5
    #: Fewer agreeing windows than this and nothing is applied.
    min_inliers: int = 3
    #: Below this many windows with a believable peak, a poor result asks for
    #: more listening instead of rejecting the subtitle. A right subtitle in a
    #: film with little dialogue looks, after a few windows, just like a
    #: wrong one; after a dozen believable windows it no longer does.
    min_windows_to_reject: int = 12
    #: The drift left after the frame-rate ratio, as a slope. 5e-4 is three
    #: and a half seconds over two hours: a rounding in somebody's tool, not
    #: a conversion.
    max_residual_slope: float = 5e-4
    #: A window this far above its rivals is one the model has to explain.
    strong_prominence: float = 0.10
    #: Significance (-log10 chance) that reads as full confidence, and the
    #: least any one region of a piecewise answer must have on its own.
    full_significance: float = 8.0
    #: How close an agreeing window has to be to vouch for the model near an
    #: edge window that disagrees with it.
    edge_reach: float = 150.0
    #: How close to either end of the film windows are ever placed.
    edge_margin: float = 0.03
    #: The longest stretch at either end of the film, as a fraction of it,
    #: that a model may cover without a single window agreeing there.
    max_unsupported: float = 0.20
    min_segment_significance: float = 4.0
    #: Confidence at or above which a result is applied, and below which it
    #: is rejected outright; between the two, more evidence is asked for.
    #: Measured: see `docs/subtitles.md`, "Calibration" -- over 160 synthetic
    #: films no wrong correction was applied at 0.6, 0.7 or 0.8; 0.7 keeps a
    #: margin below the lowest value that was safe.
    apply_threshold: float = 0.70
    reject_threshold: float = 0.45
    #: How many windows the frame-rate ratio is chosen on.
    scale_windows: int = 16
    #: Another rate has to score this much better than the film's own.
    rate_preference: float = 0.9
    #: The largest number of timing regions a piecewise answer may have.
    max_segments: int = 5
    #: The smallest residual drift worth calling linear rather than offset:
    #: the change of offset across the evidence, in seconds.
    min_drift: float = 0.25
    #: A subtitle whose last line comes before this share of the film is not
    #: the film's. OpenSubtitles files a trailer's subtitle under the film's
    #: id: for *Avengers: Endgame* (3 h 01 min) two of the three Turkish ones
    #: offered ended at 2 min 19 s.
    min_coverage: float = 0.25


@dataclass(frozen=True, slots=True)
class Segment:
    """One region of the subtitle's own timeline and where it goes."""

    #: The region, in subtitle time. The first starts at -inf, the last ends
    #: at +inf: a mapping covers the whole file.
    source_start: float
    source_end: float
    scale: float
    offset: float

    def apply(self, t: float) -> float:
        return self.scale * t + self.offset

    def as_dict(self) -> dict[str, float | None]:
        return {
            "sourceStart": None if math.isinf(self.source_start) else round(self.source_start, 3),
            "sourceEnd": None if math.isinf(self.source_end) else round(self.source_end, 3),
            "scale": round(self.scale, 7),
            "offset": round(self.offset, 3),
        }


@dataclass(frozen=True, slots=True)
class Mapping:
    segments: tuple[Segment, ...]

    def segment_for(self, t: float) -> Segment:
        for segment in self.segments:
            if t < segment.source_end:
                return segment
        return self.segments[-1]

    def __call__(self, t: float) -> float:
        return self.segment_for(t).apply(t)

    def cue_mapper(self, cue_starts: Sequence[float]):
        """A `Document.remap` function for this mapping.

        Where the video lost a scene the subtitle still has, the lines of that
        scene map on top of the lines after it; they are the ones dropped.
        Where the video gained one, the mapping leaves a gap, which is right.
        """
        # The first mapped start of each later segment: nothing of an earlier
        # segment may land at or after it.
        firsts: list[float] = []
        for segment in self.segments[1:]:
            inside = [t for t in cue_starts if segment.source_start <= t < segment.source_end]
            firsts.append(segment.apply(min(inside)) if inside else segment.apply(segment.source_start))

        def mapper(start: float, end: float) -> tuple[float, float] | None:
            index = next(
                (i for i, segment in enumerate(self.segments) if start < segment.source_end),
                len(self.segments) - 1,
            )
            segment = self.segments[index]
            new_start, new_end = segment.apply(start), segment.apply(end)
            for later in firsts[index:]:
                if new_start >= later - 0.05:
                    return None
                new_end = min(new_end, later - 0.05)
            return new_start, new_end

        return mapper


@dataclass(slots=True)
class SyncResult:
    model: str
    decision: str
    confidence: float
    offset: float = 0.0
    scale: float = 1.0
    segments: tuple[Segment, ...] = ()
    #: Windows that had anything to say, and how many of those agree.
    windows: int = 0
    inliers: int = 0
    reason: str = ""
    #: Stretches of the film, in video time, where a boundary between two
    #: timing regions lies and has not been pinned down. More windows there
    #: would place it better.
    uncertain: tuple[Interval, ...] = ()
    #: Stretches just beyond an edge window the model does not explain, where
    #: listening would say whether that window was noise or a region.
    contested: tuple[Interval, ...] = ()
    #: One entry per window with evidence: where it is, its best shift, and
    #: how strongly it said so. For the log and for the tests, never the UI.
    evidence: list[dict[str, float]] = field(default_factory=list)

    @property
    def mapping(self) -> Mapping:
        if self.segments:
            return Mapping(self.segments)
        return Mapping((Segment(-math.inf, math.inf, self.scale, self.offset),))

    def as_dict(self) -> dict[str, object]:
        return {
            "algorithm": ALGORITHM,
            "model": self.model,
            "decision": self.decision,
            "confidence": round(self.confidence, 3),
            "offset": round(self.offset, 3),
            "scale": round(self.scale, 7),
            "segments": [segment.as_dict() for segment in self.segments],
            "windows": self.windows,
            "inliers": self.inliers,
            "reason": self.reason,
            "uncertain": [[round(a, 1), round(b, 1)] for a, b in self.uncertain],
            "contested": [[round(a, 1), round(b, 1)] for a, b in self.contested],
        }

    @classmethod
    def from_dict(cls, answer: dict[str, Any]) -> "SyncResult":
        def bound(value: Any, default: float) -> float:
            return default if value is None else float(value)

        return cls(
            model=str(answer["model"]),
            decision=str(answer["decision"]),
            confidence=float(answer["confidence"]),
            offset=float(answer.get("offset", 0.0)),
            scale=float(answer.get("scale", 1.0)),
            segments=tuple(
                Segment(
                    bound(s.get("sourceStart"), -math.inf),
                    bound(s.get("sourceEnd"), math.inf),
                    float(s["scale"]),
                    float(s["offset"]),
                )
                for s in answer.get("segments", [])
            ),
            windows=int(answer.get("windows", 0)),
            inliers=int(answer.get("inliers", 0)),
            reason=str(answer.get("reason", "")),
            uncertain=tuple((float(a), float(b)) for a, b in answer.get("uncertain", [])),
            contested=tuple((float(a), float(b)) for a, b in answer.get("contested", [])),
            evidence=list(answer.get("evidence", [])),
        )


# ------------------------------------------------------------- intervals

def merge(intervals: Iterable[Interval], gap: float = 0.0) -> list[Interval]:
    """Sorted, disjoint union of `intervals`; neighbours closer than `gap` join."""
    merged: list[list[float]] = []
    for start, end in sorted(intervals):
        if end <= start:
            continue
        if merged and start <= merged[-1][1] + gap:
            merged[-1][1] = max(merged[-1][1], end)
        else:
            merged.append([start, end])
    return [(start, end) for start, end in merged]


# -------------------------------------------------------------- scoring

class _Grid:
    """Values of a piecewise-linear function on a uniform grid, built from the
    corners of the pieces: f(x) = sum c * max(0, x - t)."""

    __slots__ = ("x0", "h", "n", "a", "b")

    def __init__(self, x0: float, h: float, n: int) -> None:
        self.x0, self.h, self.n = x0, h, n
        self.a = [0.0] * n
        self.b = [0.0] * n

    def ramp(self, t: float, c: float) -> None:
        k = math.ceil((t - self.x0) / self.h - 1e-9)
        if k >= self.n:
            return
        if k < 0:
            k = 0
        self.a[k] += c
        self.b[k] += c * (self.x0 - t)

    def trapezoid(self, fixed: Interval, moving: Interval) -> None:
        """Add |fixed ∩ (moving + d)| as a function of d."""
        p0, p1 = fixed
        q0, q1 = moving
        m = min(p1 - p0, q1 - q0)
        t1 = p0 - q1
        t4 = p1 - q0
        self.ramp(t1, 1.0)
        self.ramp(t1 + m, -1.0)
        self.ramp(t4 - m, -1.0)
        self.ramp(t4, 1.0)

    def values(self) -> list[float]:
        h = self.h
        return [
            slope * k * h + base
            for k, (slope, base) in enumerate(
                zip(itertools.accumulate(self.a), itertools.accumulate(self.b))
            )
        ]


def _window_curve(
    window: Window, cues: Sequence[Interval], starts: Sequence[float], x0: float, h: float, n: int, reach: float
) -> list[float] | None:
    """Pearson correlation of speech and shifted cues over one window, per shift.

    `cues` are merged and already scaled; `starts` are their start times, for
    finding the ones within reach of this window.
    """
    lo = bisect.bisect_left(starts, window.start - reach - 60.0)
    near = [
        cue for cue in itertools.takewhile(lambda cue: cue[0] <= window.end + reach, cues[lo:])
        if cue[1] >= window.start - reach
    ]
    if not near:
        return None
    overlap = _Grid(x0, h, n)
    coverage = _Grid(x0, h, n)
    frame = (window.start, window.end)
    for cue in near:
        coverage.trapezoid(frame, cue)
        for speech in window.speech:
            overlap.trapezoid(speech, cue)
    length = window.length
    px = sum(end - start for start, end in window.speech) / length
    vx = px * (1.0 - px)
    if vx <= 1e-9:
        return None
    curve: list[float] = []
    for o, c in zip(overlap.values(), coverage.values()):
        py = c / length
        vy = py * (1.0 - py)
        if vy <= 1e-6:
            curve.append(0.0)
            continue
        curve.append((o / length - px * py) / math.sqrt(vx * vy))
    return curve


def _peak(curve: Sequence[float], x0: float, h: float, rival_gap: float) -> tuple[float, float, float]:
    """Best shift, its score, and the best score more than `rival_gap` away."""
    best = max(range(len(curve)), key=curve.__getitem__)
    gap = int(round(rival_gap / h))
    rival = max(
        (value for index, value in enumerate(curve) if abs(index - best) > gap),
        default=-1.0,
    )
    return x0 + best * h, curve[best], rival


def _robust_z(curve: Sequence[float], peak_index: int, exclude: int) -> float:
    rest = sorted(
        value for index, value in enumerate(curve) if abs(index - peak_index) > exclude
    )
    if len(rest) < 10:
        return 0.0
    median = rest[len(rest) // 2]
    mad = sorted(abs(value - median) for value in rest)[len(rest) // 2] * 1.4826
    if mad <= 1e-9:
        return 0.0
    return (curve[peak_index] - median) / mad


@dataclass(slots=True)
class _Evidence:
    window: Window
    curve: list[float]
    peak: float
    score: float
    rival: float

    @property
    def prominence(self) -> float:
        return self.score - self.rival


def _evidence(
    windows: Sequence[Window], cues: Sequence[Interval], scale: float, params: Params
) -> list[_Evidence]:
    scaled = merge((scale * start, scale * end) for start, end in cues)
    starts = [start for start, _ in scaled]
    reach = params.search_range
    h = params.grid
    n = int(round(2 * reach / h)) + 1
    x0 = -reach
    found: list[_Evidence] = []
    low, high = params.speech_fraction_range
    for window in windows:
        if window.length <= 0 or not (low <= window.speech_fraction <= high):
            continue
        if sum(1 for a, b in window.speech if b - a >= 0.3) < params.min_speech_segments:
            continue
        in_reach = bisect.bisect_right(starts, window.end + reach) - bisect.bisect_left(
            starts, window.start - reach
        )
        if in_reach < params.min_cues_in_window:
            continue
        curve = _window_curve(window, scaled, starts, x0, h, n, reach)
        if curve is None:
            continue
        peak, score, rival = _peak(curve, x0, h, params.rival_gap)
        found.append(_Evidence(window, curve, peak, score, rival))
    return found


def _refine(evidence: _Evidence, cues: Sequence[Interval], scale: float, around: float, params: Params) -> float:
    """The best shift near `around`, on the fine grid."""
    scaled = merge((scale * start, scale * end) for start, end in cues)
    starts = [start for start, _ in scaled]
    span = params.grid * 3
    h = params.fine_grid
    n = int(round(2 * span / h)) + 1
    curve = _window_curve(evidence.window, scaled, starts, around - span, h, n, abs(around) + span + 1.0)
    if curve is None:
        return around
    best = max(range(len(curve)), key=curve.__getitem__)
    return around - span + best * h


def _good(evidence: _Evidence, params: Params) -> bool:
    return evidence.score >= params.min_peak and evidence.prominence >= params.min_prominence


# -------------------------------------------------------------- fitting

def _median(values: Sequence[float]) -> float:
    ordered = sorted(values)
    middle = len(ordered) // 2
    return ordered[middle] if len(ordered) % 2 else (ordered[middle - 1] + ordered[middle]) / 2


def _significance(agreeing: int, windows: int, band: float, reach: float, freedom: float = 1.0) -> float:
    """How unlikely it is that `agreeing` of `windows` agree this well by chance.

    -log10 of the chance, under the null that each window's best shift is
    anywhere in the ±`reach` search with equal probability: choose which
    windows agree, and then every one after the first has to land within
    `band` of the first. `freedom` counts the other lines the model could
    have drawn (a slope, a choice of scale) and makes chance that much likelier.

    This is what keeps an unrelated subtitle from being "corrected". Its
    windows each find some best shift -- the maximum of a long noisy curve is
    always somewhere -- but those shifts are scattered, and two or three of
    them agreeing is what chance does and ten is not.
    """
    if agreeing < 2 or windows < agreeing:
        return 0.0
    chance = (
        math.log10(math.comb(windows, agreeing))
        + (agreeing - 1) * math.log10(min(1.0, band / (2 * reach)))
        + math.log10(max(1.0, freedom))
    )
    return max(0.0, -chance)


def _cluster(points: Sequence[tuple[float, float]], tolerance: float) -> tuple[float, list[int]]:
    """The constant shift most windows agree on, and which windows those are."""
    best: tuple[int, float, float, list[int]] | None = None
    for _, anchor in points:
        members = [p for _, p in points if abs(p - anchor) <= tolerance]
        center = _median(members)
        inliers = [i for i, (_, p) in enumerate(points) if abs(p - center) <= tolerance]
        spread = sum(abs(points[i][1] - center) for i in inliers)
        if best is None or (len(inliers), -spread) > (best[0], -best[2]):
            best = (len(inliers), center, spread, inliers)
    assert best is not None
    return best[1], best[3]


def _fit_line(
    points: Sequence[tuple[float, float]], tolerance: float, max_slope: float
) -> tuple[float, float, list[int]]:
    """Slope, intercept and inliers of the line most shifts agree with.

    The slope is what is left of a drift after the frame-rate ratio has been
    taken out, so it is kept small; a free slope finds a line through any
    three scattered points, which is the fault the significance guards
    against but is better not invited.
    """
    best: tuple[int, float, float, float] | None = None
    for (ti, pi), (tj, pj) in itertools.combinations(points, 2):
        if abs(tj - ti) < 1.0:
            continue
        slope = (pj - pi) / (tj - ti)
        if abs(slope) > max_slope:
            continue
        intercept = pi - slope * ti
        residuals = [abs(p - (slope * t + intercept)) for t, p in points]
        count = sum(1 for r in residuals if r <= tolerance)
        error = sum(min(r, tolerance) for r in residuals)
        if best is None or (count, -error) > (best[0], -best[3]):
            best = (count, slope, intercept, error)
    if best is None:
        return 0.0, _median([p for _, p in points]), []
    _, slope, intercept, _ = best
    inliers = [i for i, (t, p) in enumerate(points) if abs(p - (slope * t + intercept)) <= tolerance]
    if len(inliers) >= 2:
        xs = [points[i][0] for i in inliers]
        ys = [points[i][1] for i in inliers]
        mx, my = sum(xs) / len(xs), sum(ys) / len(ys)
        sxx = sum((x - mx) ** 2 for x in xs)
        if sxx > 1e-6:
            fitted = sum((x - mx) * (y - my) for x, y in zip(xs, ys)) / sxx
            if abs(fitted) <= max_slope:
                slope, intercept = fitted, my - fitted * mx
        inliers = [
            i for i, (t, p) in enumerate(points) if abs(p - (slope * t + intercept)) <= tolerance
        ]
    return slope, intercept, inliers


def _segment(
    points: Sequence[tuple[float, float]], tolerance: float, max_segments: int
) -> list[tuple[int, int, float]]:
    """Split time-ordered shifts into runs of constant offset.

    Dynamic programming over the split points: each run costs the squared
    residuals from its median, capped so one wild window cannot force a split,
    and every run after the first costs a penalty worth two disagreeing
    windows. A run needs two windows: one window alone is not a region, it is
    an outlier.
    """
    n = len(points)
    cap = (4 * tolerance) ** 2
    # Less than two outliers' worth: two windows that agree with each other
    # and not with their neighbours are a region, not two accidents.
    penalty = 1.5 * cap

    def cost(i: int, j: int) -> tuple[float, float]:
        values = [p for _, p in points[i:j]]
        center = _median(values)
        return sum(min((v - center) ** 2, cap) for v in values), center

    best: list[tuple[float, list[tuple[int, int, float]]] | None] = [None] * (n + 1)
    best[0] = (0.0, [])
    for j in range(2, n + 1):
        for i in range(0, j - 1):
            previous = best[i]
            if previous is None or len(previous[1]) >= max_segments:
                continue
            run_cost, center = cost(i, j)
            total = previous[0] + run_cost + (penalty if previous[1] else 0.0)
            if best[j] is None or total < best[j][0]:
                best[j] = (total, previous[1] + [(i, j, center)])
    answer = best[n]
    return answer[1] if answer else [(0, n, _median([p for _, p in points]))]


# -------------------------------------------------------------- the answer

def _trim(cues: Sequence[Interval]) -> list[Interval]:
    """Cues as they relate to speech.

    A line stays on screen after it has been said -- reading time, measured
    in the synthetic cases as a systematic 0.2 s pull of the best shift
    towards early. The tail is what is trimmed; the onset is where the
    subtitle author put the line and is left alone.
    """
    return [
        (start, max(start + 0.5 * (end - start), end - 0.4)) for start, end in cues if end > start
    ]


def covers(cues: Sequence[Interval], duration: float, params: Params | None = None) -> bool:
    """Whether the subtitle reaches far enough into the film to be the film's.

    Asked before anything is listened to: a trailer's two minutes of lines
    are more than ±`search_range` from every window past the first few
    minutes, so each one is heard, none says anything, and the answer after
    all of that listening is "no evidence".
    """
    params = params or Params()
    last = max((end for _, end in cues), default=0.0)
    return last >= params.min_coverage * duration


def align(
    cues: Sequence[Interval],
    windows: Sequence[Window],
    params: Params | None = None,
    duration: float | None = None,
) -> SyncResult:
    """How the subtitle's timeline maps onto the film, and how sure that is.

    `duration` is the film's, when known; without it the subtitle's last line
    stands in for the end of the film.
    """
    params = params or Params()
    original = [(start, end) for start, end in cues if end > start]
    if len(original) < 2 * params.min_cues_in_window:
        return SyncResult("rejected", "reject", 0.0, reason="too-few-cues")
    if duration and not covers(original, duration, params):
        return SyncResult("rejected", "reject", 0.0, reason="does-not-cover-film")
    cues = _trim(original)

    # Which timing base: the scale under which the windows agree best. Asked
    # of an even spread of at most `scale_windows` windows -- the answer is
    # the same and it is seven times the work on every one of them.
    ordered = sorted(windows, key=lambda w: w.start)
    step = max(1, math.ceil(len(ordered) / params.scale_windows))
    sample = ordered[::step]
    values: dict[float, float] = {}
    for scale in FRAME_RATE_RATIOS:
        evidence = _evidence(sample, cues, scale, params)
        if not evidence:
            continue
        total = [sum(curve) for curve in zip(*(e.curve for e in evidence))]
        values[scale] = max(total) / len(evidence)
    if not values:
        return SyncResult("rejected", "gather", 0.0, reason="no-evidence")
    scale = max(values, key=values.__getitem__)
    # The film's own rate unless another is clearly better: a subtitle cut
    # for another release spreads its agreement over several offsets, and a
    # slightly wrong rate can gather a little more of it by drifting between
    # them. Measured on the synthetic cuts: without this, one in six.
    evidence = _evidence(ordered, cues, scale, params)
    if scale != 1.0 and len(sample) < len(ordered):
        # Settled on every window, not the sample, before leaving the film's
        # own rate: the sample picked 24/23.976 for a cut film once where
        # the full set did not.
        native = _evidence(ordered, cues, 1.0, params)

        def agreement(found: list[_Evidence]) -> float:
            if not found:
                return -1.0
            return max(sum(curve) for curve in zip(*(e.curve for e in found))) / len(found)

        if agreement(native) >= params.rate_preference * agreement(evidence):
            scale, evidence = 1.0, native
    elif scale != 1.0 and values.get(1.0, -1.0) >= params.rate_preference * values[scale]:
        scale, evidence = 1.0, _evidence(ordered, cues, 1.0, params)

    log = [
        {
            "at": round(e.window.center, 1),
            "shift": round(e.peak, 3),
            "score": round(e.score, 3),
            "prominence": round(e.prominence, 3),
        }
        for e in evidence
    ]
    good = [e for e in evidence if _good(e, params)]
    enough = len(good) >= params.min_windows_to_reject
    if len(good) < params.min_inliers:
        return SyncResult(
            "rejected",
            "reject" if enough else "gather",
            0.0,
            windows=len(evidence),
            reason="weak-evidence",
            evidence=log,
        )

    for e in good:
        e.peak = _refine(e, cues, scale, e.peak, params)
    points = sorted((e.window.center, e.peak) for e in good)
    n = len(points)
    tolerance = params.inlier_tolerance
    band = 2 * tolerance
    reach = params.search_range
    scales = len(FRAME_RATE_RATIOS)
    span = points[-1][0] - points[0][0]

    # Three explanations, each scored by how unlikely its agreement is.
    center, offset_inliers = _cluster(points, tolerance)
    offset_sig = _significance(len(offset_inliers), n, band, reach, scales)

    slope, intercept, line_inliers = _fit_line(points, tolerance, params.max_residual_slope)
    freedom = scales * max(1.0, params.max_residual_slope * span / tolerance)
    line_sig = _significance(len(line_inliers), n, band, reach, freedom)

    runs = _segment(points, tolerance, params.max_segments) if n >= 4 else []
    piece_sig, piece_inliers, weakest = 0.0, [], 0.0
    if len(runs) > 1:
        sigs = []
        for i, j, run_center in runs:
            members = [k for k in range(i, j) if abs(points[k][1] - run_center) <= tolerance]
            piece_inliers.extend(members)
            sigs.append(_significance(len(members), j - i, band, reach))
        weakest = min(sigs)
        piece_sig = max(
            0.0, sum(sigs) - math.log10(math.comb(n - 1, len(runs) - 1)) - math.log10(scales)
        )

    single_sig = max(offset_sig, line_sig)
    use_piecewise = (
        len(runs) > 1
        and weakest >= params.min_segment_significance
        and len(piece_inliers) >= len(max(offset_inliers, line_inliers, key=len)) + 2
        and piece_sig > single_sig
    )
    # A residual slope has to earn its place with windows, not just with a
    # better fit to the same ones: the detector's jitter alone fits a slope
    # of about 1e-4 through a constant offset.
    use_line = (
        not use_piecewise
        and line_sig > offset_sig + 1.0
        and len(line_inliers) >= len(offset_inliers) + 2
    )
    if use_piecewise:
        inliers, significance = sorted(piece_inliers), piece_sig
    elif use_line:
        inliers, significance = line_inliers, line_sig
    else:
        inliers, significance = offset_inliers, offset_sig

    # Confidence: the significance, scaled so that the evidence of about
    # six agreeing windows reads as certain, and discounted by strong windows
    # the model leaves unexplained -- those are what a wrong model looks like.
    by_center = {e.window.center: e for e in good}
    strong = [e for e in good if e.prominence >= params.strong_prominence]
    explained = {points[i][0] for i in inliers}
    unexplained = [e for e in strong if e.window.center not in explained]
    dominance = len(inliers) / (len(inliers) + len(unexplained))
    confidence = min(1.0, significance / params.full_significance) * dominance
    # Two windows the model does not explain, agreeing with each other with
    # nothing the model does explain between them: a region with a timing of
    # its own that this model does not have.
    # Measured on the synthetic cuts, applying past one of these corrected
    # a third of the film to the wrong place.
    inside = set(inliers)
    outliers = [k for k in range(n) if k not in inside]
    structure = any(
        abs(points[a][1] - points[b][1]) <= 2 * tolerance
        and not any(k in inside for k in range(a + 1, b))
        for a, b in itertools.combinations(outliers, 2)
    )
    # A strong window at the edge of what the model explains. Between two
    # agreeing windows an outlier is an accident; before the first or after
    # the last it may be the only window heard in a region with a timing of
    # its own, and the model would be extrapolated over that region. Measured:
    # every wrong correction left in the synthetic cuts was this, including
    # one where an agreeing window 75 s on the near side made the outlier look
    # like noise -- the cut was between the two. So it is settled only by
    # listening on its far side, and only when there is room to: past the
    # last few per cent of the film nothing is sampled, and a model carried
    # over that little is not worth refusing for.
    length = _median([e.window.length for e in evidence])
    end = duration if duration else max(b for _, b in original)
    low, high = end * params.edge_margin, end * (1.0 - params.edge_margin)
    contested: list[Interval] = []
    for k in outliers:
        if by_center[points[k][0]].prominence < params.strong_prominence:
            continue
        if any(i < k for i in inside) and any(i > k for i in inside):
            continue
        t = points[k][0]
        if any(i > k for i in inside):
            region = (max(low, t - length / 2 - params.edge_reach), t - length / 2)
        else:
            region = (t + length / 2, min(high, t + length / 2 + params.edge_reach))
        if region[1] - region[0] >= length:
            contested.append(region)
    # And a long stretch at either end with no agreeing window at all: the
    # model would be a guess there, however sure it is elsewhere. Measured:
    # a cut at 31 % of a film whose first third was mostly quiet, corrected
    # by the second third's offset alone.
    supported = [points[i][0] for i in inside]
    if supported:
        if supported[0] - length / 2 - low > params.max_unsupported * end:
            contested.append((low, supported[0] - length / 2))
        if high - supported[-1] - length / 2 > params.max_unsupported * end:
            contested.append((supported[-1] + length / 2, high))
        # The same inside the film: a long stretch between two agreeing
        # windows where the windows that did hear something all disagree.
        # Measured: one accidental agreement at 20 % of a film anchored an
        # offset over a first third that had a timing of its own.
        for before, after in zip(supported, supported[1:]):
            if after - before <= params.max_unsupported * end:
                continue
            between = [points[k][0] for k in outliers if before < points[k][0] < after]
            if len(between) >= 2:
                contested.append((before + length / 2, after - length / 2))

    result = SyncResult(
        "rejected",
        "reject",
        confidence,
        windows=len(evidence),
        inliers=len(inliers),
        evidence=log,
    )

    if use_piecewise:
        result.model = "piecewise"
        result.segments, result.uncertain = _piecewise_segments(points, runs, inside, scale, original)
        first = result.segments[0]
        result.offset, result.scale = first.offset, first.scale
    elif use_line and abs(slope) * span >= params.min_drift:
        # d(v) = slope * v + intercept and v = scale * s + d(v), so
        # v = (scale * s + intercept) / (1 - slope).
        result.offset = intercept / (1.0 - slope)
        result.scale = scale / (1.0 - slope)
        result.model = "linear"
    else:
        result.offset, result.scale = (_median([points[i][1] for i in inliers]), scale)
        result.model = "offset" if abs(scale - 1.0) < 1e-9 else "linear"

    result.contested = tuple(contested)
    if structure and not enough:
        result.decision, result.reason = "gather", "unexplained-region"
    elif structure:
        result.decision, result.model, result.reason = "reject", "rejected", "unexplained-region"
    elif confidence < params.reject_threshold:
        if enough:
            result.decision, result.model, result.reason = "reject", "rejected", "low-confidence"
        else:
            result.decision, result.reason = "gather", "needs-more-evidence"
    elif contested:
        result.decision, result.reason = "gather", "unsupported-edge"
    elif len(inliers) >= params.min_inliers and confidence >= params.apply_threshold:
        result.decision, result.reason = "apply", "confident"
    else:
        result.decision, result.reason = "gather", "needs-more-evidence"
    return result


def _piecewise_segments(
    points: Sequence[tuple[float, float]],
    runs: Sequence[tuple[int, int, float]],
    inliers: set[int],
    scale: float,
    cues: Sequence[Interval],
) -> tuple[tuple[Segment, ...], tuple[Interval, ...]]:
    """Runs of shifts in video time as regions of the subtitle's own time.

    Between the last window of one run and the first of the next, the change
    happened somewhere nobody listened. The boundary is put in the widest
    pause between lines inside that stretch -- a cut is a change of scene, and
    scenes change between lines -- and the stretch is reported as uncertain so
    that more listening can narrow it.
    """
    starts = sorted(start for start, _ in cues)
    segments: list[Segment] = []
    uncertain: list[Interval] = []
    source_start = -math.inf
    for index, (i, j, center) in enumerate(runs):
        if index == len(runs) - 1:
            segments.append(Segment(source_start, math.inf, scale, center))
            break
        next_start, next_end, next_center = runs[index + 1]
        # Between the windows that agree, not the outliers the runs carry.
        mine = [k for k in range(i, j) if k in inliers] or [j - 1]
        theirs = [k for k in range(next_start, next_end) if k in inliers] or [next_start]
        last_video = points[mine[-1]][0]
        first_video = points[theirs[0]][0]
        uncertain.append((last_video, first_video))
        # Subtitle time: before `lo` is certainly this run's, after `hi` the
        # next one's.
        lo = (last_video - center) / scale
        hi = (first_video - next_center) / scale
        a, b = min(lo, hi), max(lo, hi)
        inside = [t for t in starts if a <= t <= b]
        boundary = (a + b) / 2
        if len(inside) >= 2:
            gaps = [(inside[k + 1] - inside[k], k) for k in range(len(inside) - 1)]
            _, k = max(gaps)
            boundary = (inside[k] + inside[k + 1]) / 2
        segments.append(Segment(source_start, boundary, scale, center))
        source_start = boundary
    return tuple(segments), tuple(uncertain)


# ------------------------------------------------------------- listening

def plan_windows(
    duration: float,
    *,
    count: int,
    length: float,
    taken: Sequence[Interval] = (),
    seekable: bool = True,
    available_until: float | None = None,
    margin: float = 0.03,
) -> list[Interval]:
    """Where to listen next.

    Seekable sources are sampled across the whole film, evenly, in the gaps of
    what has been heard already. A torrent is not seekable in that sense:
    reading far ahead makes the engine fetch distant pieces and steals the
    bandwidth the film is playing from, so only what the film has already
    played through (`available_until`) is sampled, newest stretches first.
    """
    if duration <= 0 or count <= 0 or length <= 0:
        return []
    begin = duration * margin
    finish = duration * (1.0 - margin)
    if not seekable:
        finish = min(finish, available_until if available_until is not None else 0.0)
    if finish - begin < length:
        return []
    taken = merge(taken)

    def free(start: float) -> bool:
        return all(start + length <= a or start >= b for a, b in taken)

    chosen: list[Interval] = []
    if seekable:
        # Midpoints of the widest free stretches, repeatedly: an even spread
        # that fills in between earlier windows when asked for more.
        for _ in range(count):
            spans = []
            blocked = merge(list(taken) + chosen)
            cursor = begin
            for a, b in blocked:
                if a > cursor:
                    spans.append((cursor, min(a, finish)))
                cursor = max(cursor, b)
            if cursor < finish:
                spans.append((cursor, finish))
            spans = [(a, b) for a, b in spans if b - a >= length]
            if not spans:
                break
            a, b = max(spans, key=lambda span: span[1] - span[0])
            start = (a + b) / 2 - length / 2
            chosen.append((start, start + length))
        return sorted(chosen)

    # Not seekable: the newest played stretch first, spaced apart.
    start = finish - length
    spacing = max(length * 3, 60.0)
    while start >= begin and len(chosen) < count:
        if free(start):
            chosen.append((start, start + length))
            start -= spacing
        else:
            start -= length
    return sorted(chosen)


def refine_windows(uncertain: Sequence[Interval], length: float, taken: Sequence[Interval]) -> list[Interval]:
    """One more window inside each boundary stretch that is still wide.

    Bisection, with a fallback: the middle first, and when the middle has
    been listened to and said nothing (a quiet scene, music), the quarters,
    then the eighths.
    """
    chosen: list[Interval] = []
    taken = merge(taken)
    for a, b in uncertain:
        if b - a < 3 * length:
            continue
        for fraction in (0.5, 0.25, 0.75, 0.125, 0.375, 0.625, 0.875):
            start = a + (b - a) * fraction - length / 2
            if all(start + length <= x or start >= y for x, y in taken + chosen):
                chosen.append((start, start + length))
                break
    return chosen


def contest_windows(regions: Sequence[Interval], length: float, taken: Sequence[Interval]) -> list[Interval]:
    """The first free window in each contested stretch, nearest the edge
    window it is meant to settle."""
    chosen: list[Interval] = []
    blocked = merge(taken)

    def free(start: float) -> bool:
        return all(start + length <= x or start >= y for x, y in blocked + chosen)

    for a, b in regions:
        # Nearest the edge window first; in a long stretch, its middle too.
        for anchor in (a, (a + b) / 2 - length / 2) if b - a >= 4 * length else (a,):
            start = anchor
            while start + length <= b + 1e-6:
                if free(start):
                    chosen.append((start, start + length))
                    break
                start += length / 2
    return chosen


def next_windows(
    result: "SyncResult | None",
    duration: float,
    taken: Sequence[Interval],
    *,
    seekable: bool,
    available_until: float | None = None,
    length: float = 30.0,
    first: int = 16,
    more: int = 8,
    most: int = 40,
) -> list[Interval]:
    """What to listen to after `result`, or nothing when listening is done.

    Staged, cheapest first: a first spread, then more of the same while the
    answer is short of confident, then bisection of any boundary a piecewise
    answer could not place. Never past `most` windows: the film is what the
    viewer came for, and the evidence is fetched over the same connection.
    """
    budget = most - len(taken)
    if budget <= 0:
        return []
    if result is None:
        return plan_windows(
            duration, count=min(first, budget), length=length, taken=taken,
            seekable=seekable, available_until=available_until,
        )
    if result.decision == "reject":
        return []
    if result.contested and result.decision != "apply":
        beyond = contest_windows(result.contested, length, taken)
        if not seekable:
            limit = available_until if available_until is not None else 0.0
            beyond = [span for span in beyond if span[1] <= limit]
        if beyond:
            return beyond[:budget]
    if result.uncertain:
        refined = refine_windows(result.uncertain, length, taken)
        if not seekable:
            limit = available_until if available_until is not None else 0.0
            refined = [span for span in refined if span[1] <= limit]
        refined = refined[:budget]
        if refined or result.decision == "apply":
            return refined
    if result.decision == "apply":
        return []
    return plan_windows(
        duration, count=min(more, budget), length=length, taken=taken,
        seekable=seekable, available_until=available_until,
    )
