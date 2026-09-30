"""The subtitle pre-filter: which external subtitles AutoSync may time at all.

Each case is a synthetic film whose dialogue is written twice, as two
languages would write it (`eligibility_synth`): one rendition is the file's
embedded track, the other the external subtitle, put on the timeline the
case is about. The fixtures follow the measured cases in docs/subtitles.md,
"Eligibility"; their thresholds are the module's own, uncalibrated ones.
"""

from __future__ import annotations

import struct
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from media.subtitles import eligibility as e
from media.subtitles import mkv
from media.subtitles.eligibility import Eligibility

from . import eligibility_synth as syn

NTSC = 24000 / 1001
DURATION = 6700.0


class Fixture:
    """One film, its embedded tracks, and a Turkish rendition to put on a
    timeline."""

    def __init__(self, seed: int = 7) -> None:
        self.truth = syn.dialogue(seed, duration=DURATION)
        self.picture = e.Reference("picture", syn.picture_events(syn.rendition(self.truth, seed + 1)))
        self.text = e.Reference("text", syn.text_events(syn.rendition(self.truth, seed + 1)))
        # Looser than the embedded one, line by line and scene by scene: the
        # measured windows scattered 0.17-0.32 s.
        self.turkish = syn.rendition(self.truth, seed + 2, jitter=0.08, lag=0.25, scene_lag=0.25)


FIXTURE = Fixture()


def verdict(cues, reference, rate, **options):
    return e.evaluate(cues, reference, rate, DURATION, **options)


class TimelineCompatible(unittest.TestCase):
    """A: ratio 1, strong, about a second off, windows scattering."""

    def test_social_network_like(self):
        cues = syn.on_timeline(FIXTURE.turkish, 1.0, -1.2)
        found = verdict(cues, FIXTURE.text, NTSC)
        self.assertIs(found.result, Eligibility.ACCEPT_TIMELINE_COMPATIBLE, found.metrics)
        self.assertEqual(found.ratio, 1.0)
        self.assertAlmostEqual(found.offset, -1.2, delta=0.3)
        self.assertEqual(found.reference, "embedded-text")
        # The window scatter is there and is not read as drift.
        self.assertGreater(found.metrics["windowRms"], 0.1)
        self.assertLess(abs(found.metrics["residualDrift"]), e.DEFAULT.max_residual_drift)

    def test_a_large_constant_offset_against_a_picture_track(self):
        found = verdict(syn.on_timeline(FIXTURE.turkish, 1.0, 13.76), FIXTURE.picture, 24.0)
        self.assertIs(found.result, Eligibility.ACCEPT_TIMELINE_COMPATIBLE, found.metrics)
        self.assertAlmostEqual(found.offset, 13.76, delta=0.3)

    def test_a_subtitle_that_stops_at_the_credits_is_whole(self):
        # The truth has four minutes of credits with no dialogue: the
        # subtitle's last line is 4 min before the end and that is normal.
        found = verdict(syn.on_timeline(FIXTURE.turkish, 1.0, 0.0), FIXTURE.picture, 24.0)
        self.assertIs(found.result, Eligibility.ACCEPT_TIMELINE_COMPATIBLE, found.metrics)


class WrongTimebase(unittest.TestCase):
    """B and C: a canonical ratio other than 1 wins and explains the drift.
    Refused, never converted."""

    def test_b_25_on_a_24_video(self):
        found = verdict(syn.on_timeline(FIXTURE.turkish, 25 / 24, 0.4), FIXTURE.picture, 24.0)
        self.assertIs(found.result, Eligibility.REJECT_TIMEBASE_MISMATCH, found.metrics)
        self.assertEqual(found.ratio_label, "25/24")

    def test_c_23976_on_a_24_video(self):
        found = verdict(syn.on_timeline(FIXTURE.turkish, NTSC / 24, -0.2), FIXTURE.picture, 24.0)
        self.assertIs(found.result, Eligibility.REJECT_TIMEBASE_MISMATCH, found.metrics)
        self.assertEqual(found.ratio_label, "23.976/24")

    def test_only_ratios_anchored_to_the_video_are_tried(self):
        for_24 = dict(e.canonical_ratios(24.0))
        self.assertEqual(set(for_24), {"1", "23.976/24", "25/24"})
        self.assertAlmostEqual(for_24["25/24"], 25 / 24)
        # 25/23.976 is no hypothesis for a 24.000 video.
        self.assertNotIn(25 / NTSC, for_24.values())
        self.assertEqual(set(dict(e.canonical_ratios(NTSC))), {"1", "24/23.976", "25/23.976"})
        self.assertEqual(e.canonical_ratios(None), (("1", 1.0),))

    def test_rates_snap_or_are_unknown(self):
        self.assertEqual(e.snap_rate(1e9 / 41708333), NTSC)
        self.assertEqual(e.snap_rate(1e9 / 41666666), 24.0)
        self.assertEqual(e.snap_rate(24000 / 1001), NTSC)
        self.assertIsNone(e.snap_rate(23.5))
        self.assertIsNone(e.snap_rate(None))


class Partial(unittest.TestCase):
    """D: half a film that stops while the reference keeps talking."""

    def test_d_cd1(self):
        half = [line for line in FIXTURE.turkish if line[1] < 0.49 * DURATION]
        found = verdict(syn.on_timeline(half, 1.0, 0.0), FIXTURE.picture, 24.0)
        self.assertIs(found.result, Eligibility.REJECT_PARTIAL, found.metrics)
        self.assertEqual(found.reason, "reference-continues")
        self.assertGreater(found.metrics["uncoveredReference"], 0.4)

    def test_a_first_part_name_is_evidence_with_its_coverage(self):
        # Ends at 70 %: not enough on its own to call it a part ...
        most = [line for line in FIXTURE.turkish if line[1] < 0.7 * DURATION]
        cues = syn.on_timeline(most, 1.0, 0.0)
        unnamed = verdict(cues, FIXTURE.picture, 24.0)
        self.assertIsNot(unnamed.result, Eligibility.REJECT_PARTIAL, unnamed.metrics)
        # ... with "CD1" in its name, it is.
        named = verdict(cues, FIXTURE.picture, 24.0, names=("To.Rome.With.Love.2012.CD1",))
        self.assertIs(named.result, Eligibility.REJECT_PARTIAL, named.metrics)
        self.assertEqual(named.reason, "first-part-name")
        self.assertTrue(e.first_part_named("Film 1of2"))
        self.assertTrue(e.first_part_named("film.part1.srt"))
        self.assertFalse(e.first_part_named("Film 2012 1080p"))

    def test_a_trailers_lines_without_any_reference(self):
        trailer = [(10.0 + i, 11.0 + i) for i in range(40)]
        found = e.partial(trailer, DURATION, None, (("1", 1.0),))
        self.assertEqual(found[0], "does-not-cover-film")


class WrongRelease(unittest.TestCase):
    """E: structure beats any ratio."""

    def test_e_a_step_at_ratio_1(self):
        cues = syn.on_timeline(syn.with_step(FIXTURE.turkish, 0.7 * DURATION, 3.5), 1.0, 0.0)
        found = verdict(cues, FIXTURE.text, NTSC)
        self.assertIs(found.result, Eligibility.REJECT_WRONG_RELEASE, found.metrics)
        self.assertEqual(found.reason, "offset-step")
        self.assertAlmostEqual(abs(found.metrics["step"]["jump"]), 3.5, delta=0.5)

    def test_a_step_is_not_hidden_behind_the_ratio_it_also_fits(self):
        # Social Network 3921849: best under 24/23.976, 81.7 s off, and a
        # 3.4 s step. That is a release problem, not a timebase one.
        cues = syn.on_timeline(syn.with_step(FIXTURE.turkish, 0.6 * DURATION, 3.4), 24 / NTSC, -81.7)
        found = verdict(cues, FIXTURE.text, NTSC)
        self.assertIs(found.result, Eligibility.REJECT_WRONG_RELEASE, found.metrics)

    def test_an_opening_the_video_does_not_have(self):
        # Lines before the film begins once the timeline is fitted.
        opening = [(-60.0 + 4 * i, -58.0 + 4 * i) for i in range(12)]
        cues = syn.on_timeline(opening + FIXTURE.turkish, 1.0, -81.7)
        found = verdict(cues, FIXTURE.picture, 24.0)
        self.assertIs(found.result, Eligibility.REJECT_WRONG_RELEASE, found.metrics)
        self.assertEqual(found.reason, "cues-outside-video")

    def test_reversed_timings(self):
        found = verdict(syn.on_timeline(FIXTURE.turkish, 1.0, 0.0), FIXTURE.picture, 24.0, reversed_cues=2)
        self.assertIs(found.result, Eligibility.REJECT_WRONG_RELEASE)
        self.assertEqual(found.reason, "reversed-cues")


class Inconclusive(unittest.TestCase):
    """F: nothing anchored to the video explains the timeline."""

    def test_f_noncanonical_ratio(self):
        # In the Mood for Love 67750 on a 24.000 video: best fit 1.042625,
        # which is no F_sub/24 there is.
        found = verdict(syn.on_timeline(FIXTURE.turkish, 1.042625, 0.0), FIXTURE.picture, 24.0)
        self.assertIs(found.result, Eligibility.INCONCLUSIVE, found.metrics)

    def test_the_residual_drift_gate(self):
        # A timeline 0.012 % off: 0.8 s over the film. The default accepts it
        # (an offset leaves +-0.4 s); a stricter gate refuses it for its drift.
        cues = syn.on_timeline(FIXTURE.turkish, 1.00012, 0.0)
        strict = e.Thresholds(max_residual_drift=0.3)
        found = verdict(cues, FIXTURE.picture, 24.0, th=strict)
        self.assertIs(found.result, Eligibility.INCONCLUSIVE, found.metrics)
        self.assertEqual(found.reason, "residual-drift")

    def test_another_films_subtitle(self):
        other = syn.rendition(syn.dialogue(99, duration=DURATION), 3, jitter=0.08, lag=0.25)
        found = verdict(other, FIXTURE.picture, 24.0)
        self.assertIs(found.result, Eligibility.INCONCLUSIVE, found.metrics)
        self.assertEqual(found.reason, "no-timeline-match")

    def test_an_unknown_video_rate_tries_ratio_1_only(self):
        found = verdict(syn.on_timeline(FIXTURE.turkish, 25 / 24, 0.0), FIXTURE.picture, None)
        self.assertIsNot(found.result, Eligibility.ACCEPT_TIMELINE_COMPATIBLE)
        self.assertEqual(list(found.metrics["ratios"]), ["1"])


class SecondReference(unittest.TestCase):
    def test_two_renditions_of_the_same_dialogue_confirm_each_other(self):
        other = e.Reference("picture", syn.picture_events(syn.rendition(FIXTURE.truth, 31)))
        self.assertEqual(e.cross_check(FIXTURE.picture, other, 24.0), "confirmed")

    def test_a_track_on_another_timebase_makes_the_reference_ambiguous(self):
        shifted = syn.on_timeline(syn.rendition(FIXTURE.truth, 31), 25 / 24, 0.0)
        other = e.Reference("picture", syn.picture_events(shifted))
        self.assertEqual(e.cross_check(FIXTURE.picture, other, 24.0), "ambiguous")

    def test_a_handful_of_events_says_nothing(self):
        few = e.Reference("picture", FIXTURE.picture.times[:20])
        self.assertEqual(e.cross_check(FIXTURE.picture, few, 24.0), "single")


class FromTheSound(unittest.TestCase):
    """Without an embedded track the audio engine answers, read by the same
    rules: only one constant offset is ever accepted."""

    def answer(self, **fields):
        base = {"model": "offset", "decision": "apply", "offset": 1.5, "scale": 1.0, "reason": "confident"}
        base.update(fields)
        return e.from_audio(base, 24.0)

    def test_an_offset_is_accepted(self):
        found = self.answer()
        self.assertIs(found.result, Eligibility.ACCEPT_TIMELINE_COMPATIBLE)
        self.assertEqual(found.reference, "audio")

    def test_a_canonical_scale_is_a_timebase_mismatch(self):
        found = self.answer(model="linear", scale=25 / 24)
        self.assertIs(found.result, Eligibility.REJECT_TIMEBASE_MISMATCH)
        self.assertEqual(found.ratio_label, "25/24")

    def test_any_other_scale_is_inconclusive(self):
        self.assertIs(self.answer(model="linear", scale=1.0004).result, Eligibility.INCONCLUSIVE)

    def test_a_cut_is_a_wrong_release(self):
        self.assertIs(self.answer(model="piecewise").result, Eligibility.REJECT_WRONG_RELEASE)
        rejected = self.answer(model="rejected", decision="reject", reason="unresolved-unexplained-region")
        self.assertIs(rejected.result, Eligibility.REJECT_WRONG_RELEASE)

    def test_a_short_subtitle_is_partial_and_doubt_is_inconclusive(self):
        partial = self.answer(model="rejected", decision="reject", reason="does-not-cover-film")
        self.assertIs(partial.result, Eligibility.REJECT_PARTIAL)
        low = self.answer(model="rejected", decision="reject", reason="low-confidence")
        self.assertIs(low.result, Eligibility.INCONCLUSIVE)


# -------------------------------------------------------------- the index

def _id(value: int) -> bytes:
    return value.to_bytes((value.bit_length() + 7) // 8, "big")


def _size(n: int) -> bytes:
    return (0x01 << 56 | n).to_bytes(8, "big")  # 8-byte EBML size


def element(eid: int, payload: bytes) -> bytes:
    return _id(eid) + _size(len(payload)) + payload


def uint(eid: int, value: int) -> bytes:
    return element(eid, value.to_bytes(max(1, (value.bit_length() + 7) // 8), "big"))


def make_mkv(*, video_ns: int, keyframes: list[float], subtitle_times: dict[int, list[float]], filler: int = 300_000) -> bytes:
    """A small, valid Matroska file: head, one filler cluster, Cues last."""
    info = element(mkv.INFO, uint(mkv.TIMESTAMP_SCALE, 1_000_000) + element(mkv.DURATION, struct.pack(">d", DURATION * 1000)))
    tracks = element(
        mkv.TRACKS,
        element(mkv.TRACK_ENTRY, uint(mkv.TRACK_NUMBER, 1) + uint(mkv.TRACK_TYPE, 1) + element(mkv.CODEC_ID, b"V_MPEG4/ISO/AVC") + uint(mkv.DEFAULT_DURATION, video_ns))
        + element(mkv.TRACK_ENTRY, uint(mkv.TRACK_NUMBER, 5) + uint(mkv.TRACK_TYPE, 17) + element(mkv.CODEC_ID, b"S_HDMV/PGS") + element(mkv.LANGUAGE, b"hun"))
        + element(mkv.TRACK_ENTRY, uint(mkv.TRACK_NUMBER, 6) + uint(mkv.TRACK_TYPE, 17) + element(mkv.CODEC_ID, b"S_TEXT/UTF8") + element(mkv.LANGUAGE, b"eng")),
    )
    cluster = element(mkv.CLUSTER, b"\0" * filler)
    points = []
    for number, times in [(1, keyframes)] + sorted(subtitle_times.items()):
        for t in times:
            points.append((round(t * 1000), number))
    points.sort()
    cues = element(
        mkv.CUES,
        b"".join(
            element(mkv.CUE_POINT, uint(mkv.CUE_TIME, ms) + element(mkv.CUE_TRACK_POSITIONS, uint(mkv.CUE_TRACK, n)))
            for ms, n in points
        ),
    )

    # The SeekHead's size does not depend on the positions once each is
    # written in 8 bytes, so it can be sized before they are known.
    def eight(eid: int, value: int) -> bytes:
        return element(eid, value.to_bytes(8, "big"))

    def seek_head8(positions: dict[int, int]) -> bytes:
        return element(
            mkv.SEEK_HEAD,
            b"".join(element(mkv.SEEK, element(mkv.SEEK_ID, _id(k)) + eight(mkv.SEEK_POSITION, v)) for k, v in positions.items()),
        )

    placeholder = seek_head8({mkv.INFO: 0, mkv.TRACKS: 0, mkv.CUES: 0})
    body_start = len(placeholder)
    positions = {
        mkv.INFO: body_start,
        mkv.TRACKS: body_start + len(info),
        mkv.CUES: body_start + len(info) + len(tracks) + len(cluster),
    }
    segment_body = seek_head8(positions) + info + tracks + cluster + cues
    header = element(0x1A45DFA3, element(0x4282, b"matroska"))
    return header + element(mkv.SEGMENT, segment_body)


def keyframes(rate: float, count: int = 400, every: int = 48) -> list[float]:
    return [round(k * every / rate, 3) for k in range(count)]


class Index(unittest.TestCase):
    def setUp(self):
        import tempfile

        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.pgs = list(FIXTURE.picture.times)
        self.data = make_mkv(video_ns=41666666, keyframes=keyframes(24.0), subtitle_times={5: self.pgs, 6: list(FIXTURE.text.times)})
        self.path = f"{self.directory.name}/film.mkv"
        with open(self.path, "wb") as handle:
            handle.write(self.data)

    def test_the_index_of_a_local_file(self):
        index = mkv.read_index(mkv.FileRanges(self.path))
        self.assertAlmostEqual(index.duration, DURATION)
        kinds = {t.number: mkv.subtitle_kind(t.codec) for t in index.tracks if t.type == mkv.SUBTITLE}
        self.assertEqual(kinds, {5: "picture", 6: "text"})
        self.assertEqual(len(index.cues[5]), len(self.pgs))
        self.assertAlmostEqual(index.cues[5][10], self.pgs[10], delta=0.001)
        # Head and Cues, not the 300 kB cluster in between.
        self.assertLess(index.bytes_read, len(self.data) - 200_000)

    def test_the_video_rate_is_the_declared_one_on_its_grid(self):
        rates = tuple(r for _, r in e.CANONICAL_RATES)
        index = mkv.read_index(mkv.FileRanges(self.path))
        self.assertEqual(mkv.video_rate(index, rates), 24.0)
        # Declared 24, keyframes on the 23.976 grid: neither is believed.
        contradicted = make_mkv(video_ns=41666666, keyframes=keyframes(NTSC), subtitle_times={5: self.pgs})
        path = f"{self.directory.name}/contradicted.mkv"
        with open(path, "wb") as handle:
            handle.write(contradicted)
        self.assertIsNone(mkv.video_rate(mkv.read_index(mkv.FileRanges(path)), rates))

    def test_the_budget_is_the_budget(self):
        with self.assertRaises(mkv.IndexUnavailable) as caught:
            mkv.read_index(mkv.FileRanges(self.path, budget=70_000))
        self.assertEqual(caught.exception.reason, "byte-budget")

    def test_a_file_that_is_not_matroska(self):
        path = f"{self.directory.name}/film.mp4"
        with open(path, "wb") as handle:
            handle.write(b"\0\0\0\x18ftypmp42" + b"\0" * 1000)
        with self.assertRaises(mkv.IndexUnavailable) as caught:
            mkv.read_index(mkv.FileRanges(path))
        self.assertEqual(caught.exception.reason, "not-matroska")


class Server(BaseHTTPRequestHandler):
    """Serves one file three ways: honestly, as the CDN that said it would
    send 29 GB for `bytes=0-0`, and ignoring Range altogether."""

    data = b""
    mode = "honest"
    sent: list[int] = []

    def log_message(self, *args):  # noqa: D401
        pass

    def do_GET(self):  # noqa: N802
        wanted = self.headers.get("Range", "")
        start, _, end = wanted.removeprefix("bytes=").partition("-")
        start, end = int(start or 0), int(end or len(Server.data) - 1)
        total = len(Server.data)
        if Server.mode == "ignores-range":
            self.send_response(200)
            self.send_header("Content-Length", str(total))
            self.end_headers()
            self._stream(Server.data)
            return
        self.send_response(206)
        self.send_header("Content-Range", f"bytes {start}-{end}/{total}")
        if Server.mode == "lying":
            # The range answered, the whole file promised, and sent.
            self.send_header("Content-Length", str(total - start))
            self.end_headers()
            self._stream(Server.data[start:])
            return
        body = Server.data[start : end + 1]
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self._stream(body)

    def _stream(self, body: bytes) -> None:
        sent = 0
        try:
            for at in range(0, len(body), 16384):
                self.wfile.write(body[at : at + 16384])
                sent += len(body[at : at + 16384])
        except (BrokenPipeError, ConnectionResetError):
            pass
        Server.sent.append(sent)


class RemoteIndex(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        Server.data = make_mkv(
            video_ns=41708333,
            keyframes=keyframes(NTSC),
            subtitle_times={5: list(FIXTURE.picture.times)},
            filler=4_000_000,
        )
        cls.server = ThreadingHTTPServer(("127.0.0.1", 0), Server)
        cls.url = f"http://127.0.0.1:{cls.server.server_address[1]}/film.mkv"
        threading.Thread(target=cls.server.serve_forever, daemon=True).start()

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()

    def setUp(self):
        Server.sent = []

    def test_honest_ranges(self):
        Server.mode = "honest"
        size = mkv.remote_size(self.url)
        self.assertEqual(size, len(Server.data))
        ranges = mkv.HttpRanges(self.url, size)
        index = mkv.read_index(ranges)
        self.assertEqual(mkv.video_rate(index, tuple(r for _, r in e.CANONICAL_RATES)), NTSC)
        self.assertLess(ranges.spent, 400_000)

    def test_a_server_promising_the_whole_file_is_read_no_further_than_asked(self):
        Server.mode = "lying"
        size = mkv.remote_size(self.url)
        self.assertEqual(size, len(Server.data))
        ranges = mkv.HttpRanges(self.url, size)
        index = mkv.read_index(ranges)
        self.assertIn(5, index.cues)
        # The client took what it asked for, whatever the server sent after.
        self.assertLess(ranges.spent, 400_000)

    def test_a_server_that_ignores_range_is_refused_after_one_bounded_read(self):
        Server.mode = "ignores-range"
        with self.assertRaises(mkv.IndexUnavailable):
            mkv.remote_size(self.url)
        ranges = mkv.HttpRanges(self.url, len(Server.data))
        with self.assertRaises(mkv.IndexUnavailable):
            mkv.read_index(ranges)
        self.assertLessEqual(ranges.spent, mkv.HEAD_BYTES + 1)


if __name__ == "__main__":
    unittest.main()
