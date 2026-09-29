"""The sync engine against films whose right answer is known.

Every case is a synthetic film (`subtitle_synth`): speech placed at random,
a detector that misses lines and hears effects, and a subtitle with lead-ins,
merged and untranslated lines and sign captions. The subtitle is then moved,
stretched or cut the way real releases differ, and the engine is asked to put
it back, listening through the same staged plan the worker uses.

A case passes on the answer, not on the model's name: nine lines in ten of
the subtitle have to land within half a second of where they belong. And the
one rule that is never relaxed: nothing wrong is ever applied.
"""

from __future__ import annotations

import math
import os
import unittest

from media.subtitles import sync
from media.subtitles.sync import Params, Window, align, merge, next_windows, plan_windows

from .subtitle_synth import cut, film, invert, listen


NTSC = 24000 / 1001


def synchronise(sub, f, *, seekable=True, available_until=None):
    spans: list[tuple[float, float]] = []
    result = None
    while True:
        more = next_windows(
            result, f.duration, spans, seekable=seekable, available_until=available_until
        )
        if not more:
            break
        spans = sorted(spans + more)
        result = align(sub, listen(f.heard, spans), duration=f.duration)
    return result, spans


def p90_error(result, pairs):
    mapping = result.mapping
    errors = sorted(abs(mapping(s) - v) for s, v in pairs)
    return errors[int(len(errors) * 0.9)]


def shifted(f, offset, scale=1.0):
    """The subtitle for `f` with video = scale * subtitle + offset."""
    sub = invert(f.reference, lambda v: (v - offset) / scale)
    return sub, [(s, scale * s + offset) for s, _ in sub]


def cut_pairs(f, edits):
    pairs = []
    for v, _ in f.reference:
        if any(s < 0 and at <= v < at - s for at, s in edits):
            continue
        pairs.append((v + sum(s for at, s in edits if v >= at), v))
    return pairs


class ConstantOffset(unittest.TestCase):
    def assert_corrected(self, seed, offset):
        f = film(seed)
        sub, pairs = shifted(f, offset)
        result, _ = synchronise(sub, f)
        self.assertEqual(result.decision, "apply", result.as_dict())
        # "linear" with a residual slope from the detector's noise is the same
        # answer: the rate is the film's own and the offset is right.
        self.assertIn(result.model, ("offset", "linear"))
        self.assertLess(abs(result.scale - 1.0), 1e-4)
        self.assertLess(abs(result.offset - offset), 0.3)
        self.assertLess(p90_error(result, pairs), 0.5)
        self.assertGreaterEqual(result.confidence, Params().apply_threshold)
        return result

    def test_perfect_alignment_is_left_where_it_is(self):
        result = self.assert_corrected(3, 0.0)
        self.assertLess(abs(result.offset), 0.3)

    def test_a_late_subtitle_is_brought_forward(self):
        self.assert_corrected(1, 4.2)

    def test_an_early_subtitle_is_held_back(self):
        self.assert_corrected(9, -7.3)

    def test_a_large_offset_is_found(self):
        self.assert_corrected(5, 96.0)


class FrameRateDrift(unittest.TestCase):
    def assert_linear(self, seed, scale, offset):
        f = film(seed)
        sub, pairs = shifted(f, offset, scale)
        result, _ = synchronise(sub, f)
        self.assertEqual(result.decision, "apply", result.as_dict())
        self.assertEqual(result.model, "linear")
        self.assertLess(abs(result.scale - scale), 2e-4)
        self.assertLess(p90_error(result, pairs), 0.5)

    def test_a_pal_subtitle_on_an_ntsc_film(self):
        # Timed at 25 fps, played at 23.976: four minutes late by the end.
        self.assert_linear(4, 25 / NTSC, 0.0)

    def test_an_ntsc_subtitle_on_a_pal_film(self):
        self.assert_linear(5, NTSC / 25, 0.0)

    def test_drift_and_an_offset_together(self):
        self.assert_linear(7, 25 / NTSC, -12.5)


class DifferentCut(unittest.TestCase):
    def test_one_scene_missing_from_this_video(self):
        f = film(1)
        edits = [(1837.0, 45.0)]
        sub = cut(f.reference, edits)
        result, _ = synchronise(sub, f)
        self.assertEqual(result.decision, "apply", result.as_dict())
        self.assertEqual(result.model, "piecewise")
        self.assertEqual(len(result.segments), 2)
        self.assertLess(abs(result.segments[0].offset), 0.3)
        self.assertLess(abs(result.segments[1].offset + 45.0), 0.3)
        self.assertLess(p90_error(result, cut_pairs(f, edits)), 0.5)
        # The first region runs to where the subtitle's release has a scene
        # this video lacks, give or take the stretch nobody listened to.
        self.assertLess(abs(result.segments[0].source_end - 1837.0), 400.0)

    def test_two_cuts(self):
        # A cleaner recording than the others: with three regions to find,
        # the default detector noise leaves too little per region, and the
        # engine refuses rather than guesses (`test_calibration`).
        f = film(0, miss_rate=0.05, false_rate=1 / 60)
        edits = [(1500.0, -30.0), (3600.0, 20.0)]
        sub = cut(f.reference, edits)
        result, _ = synchronise(sub, f)
        self.assertEqual(result.decision, "apply", result.as_dict())
        self.assertEqual(result.model, "piecewise")
        self.assertEqual([round(s.offset) for s in result.segments], [0, 30, 10])
        self.assertLess(p90_error(result, cut_pairs(f, edits)), 0.5)

    def test_lines_of_a_missing_scene_are_dropped_from_the_corrected_timeline(self):
        segments = (
            sync.Segment(-math.inf, 100.0, 1.0, 0.0),
            sync.Segment(100.0, math.inf, 1.0, -30.0),
        )
        mapping = sync.Mapping(segments)
        starts = [10.0, 95.0, 101.0, 135.0, 150.0]
        mapper = mapping.cue_mapper(starts)
        self.assertEqual(mapper(10.0, 12.0), (10.0, 12.0))
        # 95 s maps after the next region's first line (101 - 30 = 71): gone.
        self.assertIsNone(mapper(95.0, 97.0))
        self.assertEqual(mapper(135.0, 137.0), (105.0, 107.0))


class WrongSubtitle(unittest.TestCase):
    def test_a_subtitle_for_another_film_is_rejected(self):
        for seed in range(4):
            with self.subTest(seed=seed):
                f = film(seed)
                other = film(seed + 1000)
                result, _ = synchronise(list(other.reference), f)
                self.assertNotEqual(result.decision, "apply")
                self.assertEqual(result.model, "rejected")
                self.assertLess(result.confidence, Params().reject_threshold)

    def test_a_short_subtitle_is_not_trusted(self):
        f = film(1)
        result = align(list(f.reference[:4]), listen(f.heard, plan_windows(f.duration, count=16, length=30)))
        self.assertEqual(result.decision, "reject")
        self.assertEqual(result.reason, "too-few-cues")

    def test_no_evidence_is_not_a_correction(self):
        f = film(1)
        silent = [Window(a, b, ()) for a, b in plan_windows(f.duration, count=16, length=30)]
        result = align(list(f.reference), silent)
        self.assertNotEqual(result.decision, "apply")


class SparseDialogue(unittest.TestCase):
    def test_a_quiet_film_is_corrected_when_it_can_be_and_left_alone_when_not(self):
        applied = 0
        for seed in range(6):
            f = film(seed, mean_gap=14.0, quiet_scenes=30)
            sub, pairs = shifted(f, 3.0)
            result, _ = synchronise(sub, f)
            if result.decision == "apply":
                applied += 1
                self.assertLess(p90_error(result, pairs), 0.5, seed)
        self.assertGreaterEqual(applied, 2)


class TorrentSampling(unittest.TestCase):
    def test_only_what_has_been_played_is_listened_to(self):
        f = film(1)
        sub, pairs = shifted(f, 2.0)
        result, spans = synchronise(sub, f, seekable=False, available_until=1500.0)
        self.assertTrue(spans)
        self.assertTrue(all(end <= 1500.0 for _, end in spans), spans)
        # Enough of the first quarter of the film to be sure of an offset;
        # the rest of the film is not needed for that.
        self.assertIn(result.decision, ("apply", "gather"))
        if result.decision == "apply":
            self.assertLess(abs(result.offset - 2.0), 0.3)

    def test_nothing_is_listened_to_before_anything_has_played(self):
        f = film(1)
        self.assertEqual(
            next_windows(None, f.duration, [], seekable=False, available_until=20.0), []
        )


class Plumbing(unittest.TestCase):
    def test_merge_joins_close_neighbours(self):
        self.assertEqual(merge([(0, 1), (1.1, 2), (5, 6)], gap=0.2), [(0, 2), (5, 6)])

    def test_windows_are_spread_and_fill_in_between(self):
        first = plan_windows(6000, count=4, length=30)
        self.assertEqual(len(first), 4)
        more = plan_windows(6000, count=4, length=30, taken=first)
        everything = sorted(first + more)
        for (a, b), (c, _) in zip(everything, everything[1:]):
            self.assertGreaterEqual(c, b)

    def test_the_answer_describes_itself(self):
        f = film(1)
        sub, _ = shifted(f, 4.2)
        result, _ = synchronise(sub, f)
        answer = result.as_dict()
        self.assertEqual(answer["algorithm"], sync.ALGORITHM)
        for key in ("model", "decision", "confidence", "offset", "scale", "windows", "inliers", "reason"):
            self.assertIn(key, answer)


class Calibration(unittest.TestCase):
    """The guard behind the thresholds: across kinds of error and seeds,
    nothing wrong is applied. Two seeds here; the forty-seed run the
    thresholds were set from is `python3 -m media.tests.subtitle_calibration`,
    or this test with MEDIABOX_SLOW_TESTS=1."""

    def test_nothing_wrong_is_ever_applied(self):
        from .subtitle_calibration import run

        seeds = range(40) if os.environ.get("MEDIABOX_SLOW_TESTS") == "1" else (10, 11)
        table = run(seeds)
        wrong = [
            (kind, result.as_dict())
            for kind, rows in table.items()
            for _, is_wrong, result, *_ in rows
            if is_wrong
        ]
        self.assertEqual(wrong, [])
        self.assertTrue(any(right for right, *_ in table["offset"]))


if __name__ == "__main__":
    unittest.main()
