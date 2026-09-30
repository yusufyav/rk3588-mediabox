"""Whether an external subtitle belongs to this video at all, before timing it.

AutoSync corrects one thing: a constant offset between a subtitle made for
this video's timeline and the video. It does not rescue a subtitle made for
another timeline. Every external subtitle is therefore asked, in this order:

    1. PARTIAL          does it cover the film, or only part of it (CD1,
                        a trailer's lines filed under the film)?
    2. WRONG RELEASE    is its timeline the video's up to one offset, or does
                        it jump, spill outside the film, carry broken timings?
    3. TIMEBASE         with the video's own frame rate fixed, which canonical
                        timebase equivalent does its timeline follow?
    4. ACCEPT           ratio 1, strong, stable: AutoSync may time it.

and anything that cannot be told apart reliably is INCONCLUSIVE, which is
treated like a rejection: a wrong correction is worse than none.

The model everywhere is

    t_video = ratio * t_subtitle + offset,     ratio = F_sub_equivalent / F_video

where F_video is the video's measured rate and F_sub_equivalent is one of
`CANONICAL_RATES` near it. A subtitle file has no frame rate of its own;
"25/24" only says its timeline follows the 25-from-24 conversion.

The evidence here is an embedded subtitle track of the same video, in any
language: its events (a text track's lines, a picture track's show and clear
packets) mark when somebody speaks in *this* file's timeline. Nothing is
matched cue to cue -- another language splits and merges lines differently --
only the two activity patterns are compared, globally and per window.

The numbers in `Thresholds` come from four films and eight subtitles measured
on the board; they are starting values, not calibrated constants. Each one
leans towards INCONCLUSIVE.
"""

from __future__ import annotations

import bisect
import math
import re
from dataclasses import dataclass, field, replace
from enum import Enum
from typing import Any, Sequence

#: Part of every cache key an answer of this module goes into. mbelig-2: an
#: accepted subtitle is timed by the reference's own offset.
VERSION = "mbelig-2"


class Eligibility(str, Enum):
    """The pre-filter's answer. The strings are the API's and the log's."""

    ACCEPT_TIMELINE_COMPATIBLE = "ACCEPT_TIMELINE_COMPATIBLE"
    REJECT_PARTIAL = "REJECT_PARTIAL"
    REJECT_TIMEBASE_MISMATCH = "REJECT_TIMEBASE_MISMATCH"
    REJECT_WRONG_RELEASE = "REJECT_WRONG_RELEASE"
    REJECT_DUPLICATE = "REJECT_DUPLICATE"
    INCONCLUSIVE = "INCONCLUSIVE"

    @property
    def accepted(self) -> bool:
        return self is Eligibility.ACCEPT_TIMELINE_COMPATIBLE


#: The rates a subtitle's timeline may have been made for.
CANONICAL_RATES: tuple[tuple[str, float], ...] = (
    ("23.976", 24000 / 1001),
    ("24", 24.0),
    ("25", 25.0),
    ("29.97", 30000 / 1001),
    ("30", 30.0),
)


@dataclass(frozen=True, slots=True)
class Thresholds:
    """Every decision boundary of the pre-filter, in one place.

    CALIBRATION NEEDED: measured on 4 films / 8 external subtitles only
    (docs/subtitles.md, "Eligibility"). Do not read them as product constants.
    """

    # ---------------------------------------------------------- the rates
    #: How close a measured rate has to be to a canonical one to be it
    #: (23.976 and 24 are 0.1 % apart).
    rate_snap: float = 2e-4
    #: Canonical rates further than this from the video's are not tried:
    #: 30/23.976 is not a conversion anybody makes of a film.
    family_span: float = 0.10

    # ------------------------------------------------------------ matching
    #: Resolution of the activity match, and how close two events have to be
    #: to count as the same moment (a few frames of authoring either way).
    grid: float = 0.02
    tolerance: float = 0.10
    #: How far the offset is looked for, for the whole film and per window.
    global_reach: float = 150.0
    window_reach: float = 15.0

    # ------------------------------------------------------------ partial
    #: The existing trailer rule: a subtitle whose last line comes before
    #: this share of the film is not the film's.
    min_coverage: float = 0.25
    #: Reference events left after the subtitle's last line (under the most
    #: stretching canonical ratio, plus `partial_slack`), as a share of all.
    partial_uncovered: float = 0.25
    partial_slack: float = 60.0
    #: The subtitle's own density over its last `partial_tail` seconds, as a
    #: share of its mean: at or above it the subtitle stops, it does not fade.
    partial_tail: float = 300.0
    partial_abrupt: float = 0.5
    #: The reference's density just after that end, as a share of its mean:
    #: at or above it, people are still talking.
    partial_continues: float = 0.5
    #: With a CD1/Part 1 name, this little uncovered reference is enough ...
    partial_named_uncovered: float = 0.15
    #: ... and without a reference, ending before this share of the film is.
    partial_named_coverage: float = 0.75

    # --------------------------------------------------------- the ratio
    #: A ratio's global match has to stand this far above its own noise ...
    min_global_z: float = 8.0
    #: ... and this far above the next best ratio's.
    min_z_margin: float = 3.0

    # ----------------------------------------------------------- windows
    windows: int = 12
    min_window_points: int = 15
    #: A window's own match has to stand this far above its noise to count.
    window_min_z: float = 5.0
    min_reliable_windows: int = 5

    # -------------------------------------------------------- structure
    #: Mapped cues further than `outside_margin` before the video's start or
    #: after its end; this many is a timeline that is not this video's.
    outside_margin: float = 2.0
    max_outside_cues: int = 3
    #: Timed lines that end before they start.
    max_reversed_cues: int = 2
    #: A step: the window offsets are two plateaus this far apart ...
    step_min: float = 1.5
    #: ... with at least this many windows on each side ...
    step_min_side: int = 2
    #: ... that fit two constants at most this share of a line's error.
    step_fit_advantage: float = 0.5

    # ------------------------------------------------------------- drift
    #: What the chosen ratio leaves of a drift, over the windows' span. More
    #: and no canonical timebase explains the timeline.
    max_residual_drift: float = 1.0
    #: Scatter of the window offsets about their line. Cross-language
    #: matching measured 0.17-0.32 s.
    max_window_rms: float = 0.6

    # ------------------------------------------------------ the reference
    #: Events an embedded track needs to be a reference at all ...
    min_reference_events: int = 200
    #: ... and to be checked against the first one.
    min_second_reference_events: int = 60
    #: How close the audio engine's linear scale has to be to a canonical
    #: ratio to be that timebase.
    linear_ratio_tolerance: float = 3e-4


DEFAULT = Thresholds()

#: CD1, CD 1, Disc1, Part 1, Pt.1, 1of2, 1/2 -- in a label or a file name.
_FIRST_PART = re.compile(
    r"(?<![0-9a-z])(?:cd|disc|disk|part|pt)[ ._-]*0?1(?![0-9])|(?<![0-9])1[ ._-]*(?:of|/)[ ._-]*[2-9](?![0-9])",
    re.IGNORECASE,
)


# ----------------------------------------------------------------- inputs

@dataclass(frozen=True, slots=True)
class Reference:
    """An embedded track's activity: when its events are, in video time."""

    #: "picture" (PGS, VobSub: a show and a clear packet per line) or "text"
    #: (one block per line, at its start).
    kind: str
    times: tuple[float, ...]
    track: int | None = None
    language: str | None = None
    codec: str | None = None
    #: "confirmed" (a second track agrees), "single", or "ambiguous".
    quality: str = "single"

    @property
    def label(self) -> str:
        return f"embedded-{self.kind}"

    def as_dict(self) -> dict[str, Any]:
        return {
            "kind": self.kind,
            "track": self.track,
            "language": self.language,
            "codec": self.codec,
            "events": len(self.times),
            "quality": self.quality,
        }


@dataclass(slots=True)
class Verdict:
    result: Eligibility
    reason: str
    #: What the timeline was compared with: "embedded-picture",
    #: "embedded-text", "audio" or "none".
    reference: str = "none"
    ratio: float | None = None
    ratio_label: str | None = None
    offset: float | None = None
    video_rate: float | None = None
    #: Numbers for the log and the tests; never for the television.
    metrics: dict[str, Any] = field(default_factory=dict)

    def as_dict(self) -> dict[str, Any]:
        return {
            "result": self.result.value,
            "reason": self.reason,
            "reference": self.reference,
            "ratio": None if self.ratio is None else round(self.ratio, 7),
            "ratioLabel": self.ratio_label,
            "offset": None if self.offset is None else round(self.offset, 3),
            "videoRate": None if self.video_rate is None else round(self.video_rate, 5),
            "version": VERSION,
            "metrics": self.metrics,
        }

    @classmethod
    def from_dict(cls, answer: dict[str, Any]) -> "Verdict":
        return cls(
            result=Eligibility(answer["result"]),
            reason=str(answer.get("reason", "")),
            reference=str(answer.get("reference", "none")),
            ratio=answer.get("ratio"),
            ratio_label=answer.get("ratioLabel"),
            offset=answer.get("offset"),
            video_rate=answer.get("videoRate"),
            metrics=dict(answer.get("metrics") or {}),
        )


# ------------------------------------------------------------------ rates

def snap_rate(fps: float | None, th: Thresholds = DEFAULT) -> float | None:
    """The canonical rate a measured one is, or None."""
    if not fps or fps <= 0 or math.isnan(fps):
        return None
    for _, rate in CANONICAL_RATES:
        if abs(fps / rate - 1.0) <= th.rate_snap:
            return rate
    return None


def rate_label(rate: float) -> str:
    return next((label for label, value in CANONICAL_RATES if value == rate), f"{rate:.3f}")


def canonical_ratios(video_rate: float | None, th: Thresholds = DEFAULT) -> tuple[tuple[str, float], ...]:
    """The timebase hypotheses that mean something for this video, ratio 1
    first: F_sub_equivalent / F_video for each canonical rate near it."""
    if video_rate is None:
        return (("1", 1.0),)
    found = [("1", 1.0)]
    for label, rate in CANONICAL_RATES:
        if rate == video_rate or abs(rate / video_rate - 1.0) > th.family_span:
            continue
        found.append((f"{label}/{rate_label(video_rate)}", rate / video_rate))
    return tuple(found)


# ------------------------------------------------------------- matching

@dataclass(frozen=True, slots=True)
class _Peak:
    offset: float
    z: float
    count: int


def _peak(ref: Sequence[float], points: Sequence[float], ratio: float, center: float, reach: float, th: Thresholds) -> _Peak:
    """Where `points`, mapped by `ratio`, meet `ref`: the offset at which the
    most of them have an event within `tolerance`, and how far that stands
    above the rest of the search."""
    bins = int(round(2 * reach / th.grid)) + 1
    counts = [0] * bins
    for t in points:
        mapped = ratio * t + center
        lo = bisect.bisect_left(ref, mapped - reach)
        hi = bisect.bisect_right(ref, mapped + reach)
        for p in ref[lo:hi]:
            counts[int(round((p - mapped + reach) / th.grid))] += 1
    half = max(1, int(round(th.tolerance / th.grid)))
    prefix = [0]
    for c in counts:
        prefix.append(prefix[-1] + c)
    smooth = [prefix[min(bins, i + half + 1)] - prefix[max(0, i - half)] for i in range(bins)]
    best = max(range(bins), key=smooth.__getitem__)
    mean = sum(smooth) / bins
    spread = math.sqrt(sum((v - mean) ** 2 for v in smooth) / bins) or 1.0
    return _Peak(center + best * th.grid - reach, (smooth[best] - mean) / spread, smooth[best])


def _line(xs: Sequence[float], ys: Sequence[float]) -> tuple[float, float]:
    n = len(xs)
    mx, my = sum(xs) / n, sum(ys) / n
    sxx = sum((x - mx) ** 2 for x in xs) or 1.0
    slope = sum((x - mx) * (y - my) for x, y in zip(xs, ys)) / sxx
    return slope, my - slope * mx


def _median(values: Sequence[float]) -> float:
    ordered = sorted(values)
    middle = len(ordered) // 2
    return ordered[middle] if len(ordered) % 2 else (ordered[middle - 1] + ordered[middle]) / 2


def _rms(values: Sequence[float]) -> float:
    return math.sqrt(sum(v * v for v in values) / len(values)) if values else 0.0


def _step(times: Sequence[float], offsets: Sequence[float], th: Thresholds) -> dict[str, float] | None:
    """Two plateaus, if the window offsets are that rather than one line."""
    n = len(offsets)
    if n < 2 * th.step_min_side:
        return None
    slope, intercept = _line(times, offsets)
    line_rms = _rms([o - (slope * t + intercept) for t, o in zip(times, offsets)])
    best: dict[str, float] | None = None
    for k in range(th.step_min_side, n - th.step_min_side + 1):
        left, right = offsets[:k], offsets[k:]
        ml, mr = _median(left), _median(right)
        rms = _rms([o - ml for o in left] + [o - mr for o in right])
        jump = mr - ml
        if abs(jump) < th.step_min:
            continue
        if line_rms > 0 and rms > th.step_fit_advantage * line_rms:
            continue
        if best is None or rms < best["rms"]:
            best = {"at": round((times[k - 1] + times[k]) / 2, 1), "jump": round(jump, 3), "rms": round(rms, 3),
                    "lineRms": round(line_rms, 3)}
    return best


def _windows(
    ref: Sequence[float],
    cues: Sequence[tuple[float, float]],
    points: Sequence[float],
    ratio: float,
    offset: float,
    th: Thresholds,
) -> list[tuple[float, float, float]]:
    """(centre, offset, z) of each window of the subtitle's own time that has
    enough in it and lies where the reference has events, matched on its own
    around the global offset."""
    starts = [a for a, _ in cues]
    lo, hi = starts[0], starts[-1]
    step = (hi - lo) / th.windows if hi > lo else 0.0
    ref_lo, ref_hi = ref[0] - th.window_reach, ref[-1] + th.window_reach
    found: list[tuple[float, float, float]] = []
    for k in range(th.windows):
        a = lo + k * step
        b = lo + (k + 1) * step if k < th.windows - 1 else hi + 1e-6
        inside = [t for t in points if a <= t < b]
        if len(inside) < th.min_window_points:
            continue
        centre = sum(inside) / len(inside)
        if not ref_lo <= ratio * centre + offset <= ref_hi:
            continue
        local = _peak(ref, inside, ratio, offset, th.window_reach, th)
        found.append((centre, local.offset, local.z))
    return found


# ------------------------------------------------------------------ gates

def first_part_named(*names: str | None) -> bool:
    return any(name and _FIRST_PART.search(name) for name in names)


def partial(
    cues: Sequence[tuple[float, float]],
    duration: float | None,
    reference: Reference | None,
    ratios: Sequence[tuple[str, float]],
    *,
    names: Sequence[str | None] = (),
    th: Thresholds = DEFAULT,
) -> tuple[str, dict[str, Any]] | None:
    """Why this subtitle covers only part of the film, or None.

    Never one number alone: an early last line is normal where the credits
    roll. It takes the subtitle stopping (not fading) while the reference's
    dialogue carries on, or a first-part name with the coverage to match.
    """
    if not cues:
        return "no-cues", {}
    last = max(end for _, end in cues)
    first = min(start for start, _ in cues)
    named = first_part_named(*names)
    metrics: dict[str, Any] = {"lastCue": round(last, 1), "named": named}
    if duration:
        metrics["coverage"] = round(last / duration, 3)
        if last < th.min_coverage * duration:
            return "does-not-cover-film", metrics
    if reference is None or not reference.times:
        if named and duration and last < th.partial_named_coverage * duration:
            return "first-part-name", metrics
        return None
    ref = reference.times
    stretch = max(ratio for _, ratio in ratios)
    end = stretch * last + th.partial_slack
    uncovered = len(ref) - bisect.bisect_right(ref, end)
    share = uncovered / len(ref)
    span = max(1.0, last - first)
    tail = sum(1 for start, _ in cues if start >= last - th.partial_tail)
    own_density = len(cues) / span
    tail_density = tail / min(th.partial_tail, span)
    ref_span = max(1.0, ref[-1] - ref[0])
    after = bisect.bisect_left(ref, end + th.partial_tail) - bisect.bisect_left(ref, end)
    ref_density = len(ref) / ref_span
    metrics.update(
        uncoveredReference=round(share, 3),
        tailDensity=round(tail_density / own_density, 3) if own_density else 0.0,
        referenceAfter=round(after / th.partial_tail / ref_density, 3) if ref_density else 0.0,
    )
    abrupt = own_density > 0 and tail_density >= th.partial_abrupt * own_density
    continues = ref_density > 0 and after / th.partial_tail >= th.partial_continues * ref_density
    if share >= th.partial_uncovered and abrupt and continues:
        return "reference-continues", metrics
    if named and share >= th.partial_named_uncovered:
        return "first-part-name", metrics
    return None


def evaluate(
    cues: Sequence[tuple[float, float]],
    reference: Reference,
    video_rate: float | None,
    duration: float | None,
    *,
    reversed_cues: int = 0,
    names: Sequence[str | None] = (),
    th: Thresholds = DEFAULT,
) -> Verdict:
    """The pre-filter against an embedded reference."""
    ratios = canonical_ratios(video_rate, th)
    verdict = Verdict(Eligibility.INCONCLUSIVE, "", reference=reference.label, video_rate=video_rate)
    good = sorted((a, b) for a, b in cues if b > a)
    if len(good) < 2 * th.min_window_points:
        verdict.reason = "too-few-cues"
        return verdict

    # 1. partial
    found = partial(good, duration, reference, ratios, names=names, th=th)
    if found:
        verdict.result, verdict.reason = Eligibility.REJECT_PARTIAL, found[0]
        verdict.metrics = found[1]
        return verdict

    # The points compared: a picture track has an event where a line starts
    # and where it ends; a text track only where it starts.
    points = sorted([a for a, _ in good] + ([b for _, b in good] if reference.kind == "picture" else []))
    ref = list(reference.times)

    scored = []
    for label, ratio in ratios:
        peak = _peak(ref, points, ratio, 0.0, th.global_reach, th)
        scored.append((peak.z, label, ratio, peak))
    scored.sort(key=lambda item: item[0], reverse=True)
    z, label, ratio, peak = scored[0]
    runner_up = scored[1][0] if len(scored) > 1 else 0.0
    verdict.metrics["ratios"] = {item[1]: round(item[0], 2) for item in scored}
    verdict.metrics["globalZ"] = round(z, 2)
    verdict.metrics["margin"] = round(z - runner_up, 2)

    # 2. structure, before any ratio is believed: a cut film fits a rate by
    # drifting between its two offsets, and a wrong release is that whatever
    # its rate. Asked of every timeline that matches strongly at all.
    if reversed_cues >= th.max_reversed_cues:
        verdict.result, verdict.reason = Eligibility.REJECT_WRONG_RELEASE, "reversed-cues"
        verdict.metrics["reversedCues"] = reversed_cues
        return verdict
    if z < th.min_global_z:
        verdict.reason = "no-timeline-match"
        return verdict
    strong = [item for item in scored if item[0] >= th.min_global_z]
    measured: dict[str, list[tuple[float, float, float]]] = {}
    for _, candidate_label, candidate_ratio, candidate_peak in strong:
        if duration:
            outside = sum(
                1
                for a, b in good
                if candidate_ratio * a + candidate_peak.offset < -th.outside_margin
                or candidate_ratio * b + candidate_peak.offset > duration + th.outside_margin
            )
            verdict.metrics.setdefault("outsideCues", {})[candidate_label] = outside
            if outside >= th.max_outside_cues:
                verdict.result, verdict.reason = Eligibility.REJECT_WRONG_RELEASE, "cues-outside-video"
                verdict.ratio, verdict.ratio_label, verdict.offset = candidate_ratio, candidate_label, candidate_peak.offset
                return verdict
        windows = _windows(ref, good, points, candidate_ratio, candidate_peak.offset, th)
        measured[candidate_label] = windows
        reliable = [(t, o) for t, o, wz in windows if wz >= th.window_min_z]
        found_step = _step([t for t, _ in reliable], [o for _, o in reliable], th)
        if found_step:
            verdict.result, verdict.reason = Eligibility.REJECT_WRONG_RELEASE, "offset-step"
            verdict.ratio, verdict.ratio_label = candidate_ratio, candidate_label
            verdict.metrics["step"] = found_step
            return verdict

    if z - runner_up < th.min_z_margin:
        verdict.reason = "ambiguous-ratio"
        return verdict

    # 3. the timebase: the best ratio's windows, one line through them.
    windows = measured[label]
    reliable = [(t, o) for t, o, wz in windows if wz >= th.window_min_z]
    verdict.metrics["windows"] = [[round(t, 1), round(o, 3), round(wz, 1)] for t, o, wz in windows]
    verdict.metrics["reliableWindows"] = len(reliable)
    verdict.ratio, verdict.ratio_label = ratio, label
    if len(reliable) < th.min_reliable_windows:
        verdict.reason = "too-few-reliable-windows"
        return verdict
    times = [t for t, _ in reliable]
    offsets = [o for _, o in reliable]

    slope, intercept = _line(times, offsets)
    residual = [o - (slope * t + intercept) for t, o in zip(times, offsets)]
    drift = slope * (times[-1] - times[0])
    verdict.metrics.update(
        regressionSlope=round(slope * 1000, 4),  # seconds per 1000 s
        rawSlope=round((offsets[-1] - offsets[0]) / (times[-1] - times[0]) * 1000, 4) if times[-1] > times[0] else 0.0,
        residualDrift=round(drift, 3),
        windowRms=round(_rms(residual), 3),
        windowMax=round(max(abs(r) for r in residual), 3),
    )
    if abs(drift) > th.max_residual_drift:
        verdict.reason = "residual-drift"
        return verdict
    if _rms(residual) > th.max_window_rms:
        verdict.reason = "unstable-timeline"
        return verdict

    verdict.offset = _median(offsets)
    if ratio == 1.0:
        verdict.result, verdict.reason = Eligibility.ACCEPT_TIMELINE_COMPATIBLE, "timeline-compatible"
    else:
        verdict.result, verdict.reason = Eligibility.REJECT_TIMEBASE_MISMATCH, "canonical-timebase"
    return verdict


def cross_check(first: Reference, second: Reference, video_rate: float | None, th: Thresholds = DEFAULT) -> str:
    """Whether a second embedded track tells the same timeline: "confirmed",
    "ambiguous" (it does not), or "single" (it cannot say)."""
    if len(second.times) < th.min_second_reference_events:
        return "single"
    pseudo = [(t, t + 0.001) for t in second.times]
    against = Reference(first.kind, first.times, first.track)
    verdict = evaluate(pseudo, against, video_rate, None, th=_second_thresholds(th, len(second.times)))
    if verdict.result.accepted:
        return "confirmed"
    if verdict.result in (Eligibility.REJECT_TIMEBASE_MISMATCH, Eligibility.REJECT_WRONG_RELEASE):
        return "ambiguous"
    return "single"


def _second_thresholds(th: Thresholds, events: int) -> Thresholds:
    # A forced-only track has a few dozen events: its windows are fewer and
    # thinner, and its end says nothing about coverage.
    return replace(
        th,
        min_window_points=max(3, min(th.min_window_points, events // (2 * th.windows))),
        min_reliable_windows=2,
        partial_uncovered=1.1,
        partial_named_uncovered=1.1,
        min_coverage=0.0,
    )


def from_audio(result: dict[str, Any], video_rate: float | None, th: Thresholds = DEFAULT) -> Verdict:
    """The same question answered by the sound, for a video with no usable
    embedded track: the audio engine's own model, read under the same rules.
    It only ever tries the canonical ratios of this video."""
    model = str(result.get("model", ""))
    decision = str(result.get("decision", ""))
    reason = str(result.get("reason", ""))
    scale = float(result.get("scale") or 1.0)
    verdict = Verdict(Eligibility.INCONCLUSIVE, reason or "audio", reference="audio", video_rate=video_rate)
    verdict.metrics = {
        "model": model,
        "decision": decision,
        "confidence": result.get("confidence"),
        "windows": result.get("windows"),
        "inliers": result.get("inliers"),
    }
    if reason in ("does-not-cover-film",):
        verdict.result = Eligibility.REJECT_PARTIAL
        return verdict
    if (decision == "apply" and model == "piecewise") or reason.endswith("unexplained-region"):
        verdict.result, verdict.reason = Eligibility.REJECT_WRONG_RELEASE, reason or "piecewise"
        return verdict
    if decision != "apply":
        return verdict
    verdict.offset = float(result.get("offset") or 0.0)
    if model == "offset" and abs(scale - 1.0) < 1e-9:
        verdict.result, verdict.reason = Eligibility.ACCEPT_TIMELINE_COMPATIBLE, "timeline-compatible"
        verdict.ratio, verdict.ratio_label = 1.0, "1"
        return verdict
    for label, ratio in canonical_ratios(video_rate, th)[1:]:
        if abs(scale - ratio) <= th.linear_ratio_tolerance:
            verdict.result, verdict.reason = Eligibility.REJECT_TIMEBASE_MISMATCH, "canonical-timebase"
            verdict.ratio, verdict.ratio_label = ratio, label
            return verdict
    verdict.reason = "noncanonical-drift"
    verdict.ratio = scale
    return verdict
