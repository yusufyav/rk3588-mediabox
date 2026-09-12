"""Ranking: which rendition of one title the appliance should actually play."""

from __future__ import annotations

import unittest

from ..policy import get_profile, rank_sources
from ..policy.decide import PlaybackMode
from ..policy.ranking import RankTier
from . import fixtures as F


PROFILE = get_profile()


def hdr10_4k_ac3():
    return F.media(
        [F.video_stream(), F.audio_stream(codec_name="ac3")],
        source="https://example.invalid/hdr10-4k.mkv",
        frames=[{"side_data_list": [F.MASTERING_DISPLAY, F.CONTENT_LIGHT]}],
    )


def dv_p5_4k():
    return F.media(
        [
            F.video_stream(side_data_list=[F.dovi(profile=5, compatibility_id=0)]),
            F.audio_stream(codec_name="eac3"),
        ],
        source="https://example.invalid/dv-p5-4k.mkv",
    )


def hdr10_4k_truehd():
    return F.media(
        [
            F.video_stream(),
            F.audio_stream(codec_name="truehd", profile="Dolby TrueHD + Dolby Atmos", channels=8),
        ],
        source="https://example.invalid/hdr10-4k-truehd.mkv",
        frames=[{"side_data_list": [F.MASTERING_DISPLAY, F.CONTENT_LIGHT]}],
    )


def sdr_1080_ac3():
    return F.media(
        [
            F.video_stream(
                width=1920,
                height=1080,
                pix_fmt="yuv420p",
                profile="Main",
                color_primaries="bt709",
                color_transfer="bt709",
                color_space="bt709",
            ),
            F.audio_stream(codec_name="ac3"),
        ],
        source="https://example.invalid/sdr-1080.mkv",
        bit_rate="8000000",
    )


def unsupported_444():
    return F.media(
        [F.video_stream(pix_fmt="yuv444p12le"), F.audio_stream(codec_name="ac3")],
        source="https://example.invalid/broken.mkv",
    )


def remux_needed():
    return F.media(
        [F.video_stream(), F.audio_stream(codec_name="ac3")],
        source="https://example.invalid/raw.bin",
        format_name="rawvideo",
        frames=[{"side_data_list": [F.MASTERING_DISPLAY, F.CONTENT_LIGHT]}],
    )


class RankingTests(unittest.TestCase):
    def _rank(self, *sources):
        return rank_sources([(info.source_url, info) for info in sources], PROFILE)

    def test_a_safe_4k_hdr10_source_beats_a_risky_4k_dolby_vision_one(self):
        ranked = self._rank(dv_p5_4k(), hdr10_4k_ac3())
        self.assertEqual(ranked[0].info.source_url, "https://example.invalid/hdr10-4k.mkv")
        self.assertIs(ranked[0].tier, RankTier.SAFE_DIRECT)
        self.assertIs(ranked[-1].tier, RankTier.RISKY)

    def test_resolution_alone_does_not_win(self):
        """The DV source is 4K and the safe one is 1080p; safety still wins."""
        ranked = self._rank(dv_p5_4k(), sdr_1080_ac3())
        self.assertEqual(ranked[0].info.source_url, "https://example.invalid/sdr-1080.mkv")

    def test_audio_that_needs_no_encoder_beats_audio_that_does(self):
        ranked = self._rank(hdr10_4k_truehd(), hdr10_4k_ac3())
        self.assertEqual(ranked[0].info.source_url, "https://example.invalid/hdr10-4k.mkv")
        self.assertIs(ranked[0].tier, RankTier.SAFE_DIRECT)
        self.assertIs(ranked[1].tier, RankTier.SAFE_AUDIO_TRANSCODE)

    def test_an_sdr_rendition_loses_to_an_hdr_one_of_the_same_title(self):
        ranked = self._rank(sdr_1080_ac3(), hdr10_4k_ac3())
        self.assertIs(ranked[0].tier, RankTier.SAFE_DIRECT)
        self.assertIs(ranked[1].tier, RankTier.SAFE_SDR_ALTERNATIVE)

    def test_an_sdr_rendition_is_a_first_class_choice_when_it_is_the_only_one(self):
        ranked = self._rank(sdr_1080_ac3())
        self.assertIs(ranked[0].tier, RankTier.SAFE_DIRECT)

    def test_the_full_tier_order_is_the_documented_one(self):
        ranked = self._rank(
            unsupported_444(),
            dv_p5_4k(),
            remux_needed(),
            sdr_1080_ac3(),
            hdr10_4k_truehd(),
            hdr10_4k_ac3(),
        )
        self.assertEqual(
            [item.tier for item in ranked],
            [
                RankTier.SAFE_DIRECT,
                RankTier.SAFE_AUDIO_TRANSCODE,
                RankTier.SAFE_SDR_ALTERNATIVE,
                RankTier.SAFE_REMUX,
                RankTier.RISKY,
                RankTier.UNSUPPORTED,
            ],
        )

    def test_ranking_is_deterministic_regardless_of_input_order(self):
        sources = [unsupported_444(), dv_p5_4k(), sdr_1080_ac3(), hdr10_4k_truehd(), hdr10_4k_ac3()]
        forward = [item.identity for item in self._rank(*sources)]
        backward = [item.identity for item in self._rank(*reversed(sources))]
        self.assertEqual(forward, backward)

    def test_identical_renditions_still_order_totally(self):
        a = F.media([F.video_stream(), F.audio_stream()], source="https://example.invalid/a.mkv")
        b = F.media([F.video_stream(), F.audio_stream()], source="https://example.invalid/b.mkv")
        ranked = self._rank(b, a)
        self.assertEqual(
            [item.identity for item in ranked],
            ["https://example.invalid/a.mkv", "https://example.invalid/b.mkv"],
        )

    def test_a_risky_source_is_still_offered_when_it_is_all_there_is(self):
        ranked = self._rank(dv_p5_4k())
        self.assertEqual(len(ranked), 1)
        self.assertIs(ranked[0].decision.mode, PlaybackMode.FALLBACK_SOURCE_PREFERRED)

    def test_ranking_nothing_is_an_empty_list(self):
        self.assertEqual(rank_sources([], PROFILE), [])


if __name__ == "__main__":
    unittest.main()
