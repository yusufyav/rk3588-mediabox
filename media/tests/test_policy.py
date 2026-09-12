"""Policy: what plays, what converts, what is refused, and why."""

from __future__ import annotations

import dataclasses
import unittest

from ..inspector.model import HdrFormat
from ..policy import decide, decide_preview, decide_video, get_profile, select_audio
from ..policy import reasons as R
from ..policy.audio import AudioAction, ac3_target, decide_track
from ..policy.capabilities import RK3588_ORANGEPI5_PRODUCTION
from ..policy.decide import PlaybackMode
from ..policy.preview import PreviewMode
from ..policy.video import VideoVerdict
from . import fixtures as F


PROFILE = get_profile()


def codes(*groups) -> set[str]:
    found: set[str] = set()
    for group in groups:
        for reason in group:
            found.add(reason.code)
    return found


def without_eac3_passthrough():
    """A profile whose sink does not take E-AC-3, to exercise the AC-3 path."""
    audio = dataclasses.replace(
        RK3588_ORANGEPI5_PRODUCTION.audio, passthrough_codecs=frozenset({"ac3"})
    )
    return dataclasses.replace(RK3588_ORANGEPI5_PRODUCTION, audio=audio)


class VideoPolicyTests(unittest.TestCase):
    def test_h264_sdr_is_direct(self):
        decision = decide_video(F.sdr_h264_aac().primary_video, PROFILE)
        self.assertIs(decision.verdict, VideoVerdict.DIRECT)
        self.assertIs(decision.hdr, HdrFormat.SDR)

    def test_hevc_main10_hdr10_is_direct(self):
        info = F.hdr10_hevc_eac3()
        decision = decide_video(info.primary_video, PROFILE)
        self.assertIs(decision.verdict, VideoVerdict.DIRECT)
        self.assertIs(decision.hdr, HdrFormat.HDR10)
        self.assertIn(R.HDR_FORMAT_SUPPORTED, codes(decision.reasons))

    def test_hdr10_plus_plays_as_its_base_layer_and_says_so(self):
        decision = decide_video(F.hdr10_plus_hevc_ac3().primary_video, PROFILE)
        self.assertIs(decision.verdict, VideoVerdict.DIRECT)
        self.assertIn(R.HDR10_PLUS_BASE_LAYER_ONLY, codes(decision.reasons))

    def test_hlg_is_direct(self):
        decision = decide_video(F.hlg_hevc_aac().primary_video, PROFILE)
        self.assertIs(decision.verdict, VideoVerdict.DIRECT)
        self.assertIs(decision.hdr, HdrFormat.HLG)

    def test_twelve_bit_video_is_unsupported(self):
        info = F.media([F.video_stream(pix_fmt="yuv420p12le"), F.audio_stream()])
        decision = decide_video(info.primary_video, PROFILE)
        self.assertIs(decision.verdict, VideoVerdict.UNSUPPORTED)
        self.assertIn(R.VIDEO_BIT_DEPTH_UNSUPPORTED, codes(decision.reasons))

    def test_422_and_444_chroma_are_unsupported(self):
        for pix_fmt in ("yuv422p10le", "yuv444p10le"):
            with self.subTest(pix_fmt=pix_fmt):
                info = F.media([F.video_stream(pix_fmt=pix_fmt), F.audio_stream()])
                decision = decide_video(info.primary_video, PROFILE)
                self.assertIs(decision.verdict, VideoVerdict.UNSUPPORTED)
                self.assertIn(R.VIDEO_CHROMA_UNSUPPORTED, codes(decision.reasons))

    def test_hevc_range_extension_profile_is_unsupported(self):
        info = F.media(
            [F.video_stream(profile="Rext", pix_fmt="yuv422p10le"), F.audio_stream()]
        )
        decision = decide_video(info.primary_video, PROFILE)
        self.assertIs(decision.verdict, VideoVerdict.UNSUPPORTED)

    def test_h264_high10_is_outside_the_hardware_decoder(self):
        info = F.media(
            [
                F.video_stream(codec_name="h264", profile="High 10", pix_fmt="yuv420p10le"),
                F.audio_stream(),
            ]
        )
        decision = decide_video(info.primary_video, PROFILE)
        self.assertIs(decision.verdict, VideoVerdict.UNSUPPORTED)
        self.assertIn(R.VIDEO_PROFILE_UNSUPPORTED, codes(decision.reasons))

    def test_beyond_4k_is_unsupported(self):
        info = F.media([F.video_stream(width=7680, height=4320), F.audio_stream()])
        self.assertIs(decide_video(info.primary_video, PROFILE).verdict, VideoVerdict.UNSUPPORTED)

    def test_full_range_is_a_warning_not_a_refusal(self):
        info = F.media([F.video_stream(color_range="pc"), F.audio_stream()])
        decision = decide_video(info.primary_video, PROFILE)
        self.assertIs(decision.verdict, VideoVerdict.DIRECT)
        self.assertIn(R.VIDEO_COLOR_RANGE_FULL, codes(decision.reasons))

    def test_unknown_colour_metadata_plays_but_is_flagged(self):
        decision = decide_video(F.unknown_color_metadata().primary_video, PROFILE)
        self.assertIs(decision.verdict, VideoVerdict.DIRECT)
        self.assertIn(R.COLOR_METADATA_INCOMPLETE, codes(decision.reasons))

    def test_contradictory_colour_metadata_is_reported_as_a_conflict(self):
        info = F.conflicting_color_metadata()
        decision = decide_video(info.primary_video, PROFILE, source_warnings=info.warnings)
        self.assertIn(R.COLOR_METADATA_CONFLICT, codes(decision.reasons))

    def test_a_source_with_no_video_track_is_unsupported(self):
        self.assertIs(decide_video(None, PROFILE).verdict, VideoVerdict.UNSUPPORTED)


class DolbyVisionTests(unittest.TestCase):
    """The green/magenta guard, stated as tests."""

    def test_profile5_is_never_direct_on_a_device_without_a_dv_pipeline(self):
        info = F.dolby_vision_profile5()
        decision = decide_video(info.primary_video, PROFILE)
        self.assertIsNot(decision.verdict, VideoVerdict.DIRECT)
        self.assertTrue(decision.prefer_alternative)
        self.assertIn(R.DV_PROFILE5_NO_BASE_LAYER, codes(decision.reasons))

    def test_profile5_never_becomes_a_playable_session(self):
        decision = decide(F.dolby_vision_profile5(), PROFILE)
        self.assertIs(decision.mode, PlaybackMode.FALLBACK_SOURCE_PREFERRED)
        self.assertIn(R.SAFER_ALTERNATIVE_EXPECTED, codes(decision.reasons))

    def test_profile7_plays_as_hdr10_from_its_base_layer(self):
        decision = decide_video(F.dolby_vision_profile7().primary_video, PROFILE)
        self.assertIs(decision.verdict, VideoVerdict.DIRECT)
        self.assertIs(decision.hdr, HdrFormat.HDR10)
        self.assertIn(R.DV_PROFILE7_BASE_LAYER_HDR10, codes(decision.reasons))
        self.assertIn(R.DV_DYNAMIC_METADATA_LOST, codes(decision.reasons))

    def test_profile8_hdr10_compatible_plays_as_hdr10(self):
        decision = decide_video(F.dolby_vision_profile8(1).primary_video, PROFILE)
        self.assertIs(decision.verdict, VideoVerdict.DIRECT)
        self.assertIs(decision.hdr, HdrFormat.HDR10)
        self.assertIn(R.DV_BASE_LAYER_COMPATIBLE, codes(decision.reasons))

    def test_profile8_sdr_compatible_plays_as_sdr(self):
        decision = decide_video(F.dolby_vision_profile8(2).primary_video, PROFILE)
        self.assertIs(decision.verdict, VideoVerdict.DIRECT)
        self.assertIs(decision.hdr, HdrFormat.SDR)

    def test_profile8_hlg_compatible_plays_as_hlg(self):
        decision = decide_video(F.dolby_vision_profile8(4).primary_video, PROFILE)
        self.assertIs(decision.hdr, HdrFormat.HLG)

    def test_profile8_with_no_compatible_base_layer_is_risky(self):
        decision = decide_video(F.dolby_vision_profile8(0).primary_video, PROFILE)
        self.assertIs(decision.verdict, VideoVerdict.RISKY)
        self.assertTrue(decision.prefer_alternative)

    def test_an_unrecognised_compatibility_id_is_not_invented_into_support(self):
        decision = decide_video(F.dolby_vision_profile8(99).primary_video, PROFILE)
        self.assertIs(decision.verdict, VideoVerdict.RISKY)
        self.assertIn(R.DV_PROFILE_UNKNOWN, codes(decision.reasons))

    def test_dolby_vision_without_a_profile_is_risky(self):
        info = F.media([F.video_stream(codec_tag_string="dvhe"), F.audio_stream()])
        decision = decide_video(info.primary_video, PROFILE)
        self.assertIs(decision.verdict, VideoVerdict.RISKY)
        self.assertIn(R.DV_PROFILE_UNKNOWN, codes(decision.reasons))

    def test_a_device_with_a_dv_pipeline_would_play_profile5_directly(self):
        video = dataclasses.replace(RK3588_ORANGEPI5_PRODUCTION.video, dolby_vision_pipeline=True)
        capable = dataclasses.replace(
            RK3588_ORANGEPI5_PRODUCTION, name="hypothetical_dv", video=video
        )
        decision = decide_video(F.dolby_vision_profile5().primary_video, capable)
        self.assertIs(decision.verdict, VideoVerdict.DIRECT)


class AudioPolicyTests(unittest.TestCase):
    def test_ac3_is_passed_through(self):
        decision = decide_track(F.hdr10_plus_hevc_ac3().audio[0], PROFILE)
        self.assertIs(decision.action, AudioAction.PASSTHROUGH)
        self.assertIn(R.AUDIO_PASSTHROUGH, codes(decision.reasons))

    def test_stereo_aac_is_decoded_by_the_player(self):
        decision = decide_track(F.sdr_h264_aac().audio[0], PROFILE)
        self.assertIs(decision.action, AudioAction.DECODE_PCM)

    def test_multichannel_aac_becomes_ac3_because_the_sink_is_silent_on_6ch_pcm(self):
        track = F.media(
            [F.video_stream(), F.audio_stream(codec_name="aac", profile="LC", channels=6)]
        ).audio[0]
        decision = decide_track(track, PROFILE)
        self.assertIs(decision.action, AudioAction.TRANSCODE_AC3)
        self.assertIn(R.AUDIO_CHANNELS_REDUCED, codes(decision.reasons))

    def test_eac3_is_passed_through_on_this_sink(self):
        decision = decide_track(F.hdr10_hevc_eac3().audio[0], PROFILE)
        self.assertIs(decision.action, AudioAction.PASSTHROUGH)

    def test_eac3_becomes_ac3_when_the_sink_cannot_take_it(self):
        decision = decide_track(F.hdr10_hevc_eac3().audio[0], without_eac3_passthrough())
        self.assertIs(decision.action, AudioAction.TRANSCODE_AC3)
        self.assertEqual(decision.target_codec, "ac3")
        self.assertEqual(decision.target_channels, 6)

    def test_dts_becomes_ac3(self):
        track = F.media([F.video_stream(), F.audio_stream(codec_name="dts", profile="DTS")]).audio[0]
        decision = decide_track(track, PROFILE)
        self.assertIs(decision.action, AudioAction.TRANSCODE_AC3)
        self.assertEqual(decision.target_bitrate, 640_000)

    def test_dts_hd_ma_becomes_ac3_and_the_loss_is_stated(self):
        track = F.media(
            [F.video_stream(), F.audio_stream(codec_name="dts", profile="DTS-HD MA", channels=8)]
        ).audio[0]
        decision = decide_track(track, PROFILE)
        self.assertIs(decision.action, AudioAction.TRANSCODE_AC3)
        self.assertIn(R.AUDIO_LOSSLESS_TO_LOSSY, codes(decision.reasons))
        self.assertIn(R.AUDIO_CHANNELS_REDUCED, codes(decision.reasons))

    def test_truehd_becomes_ac3(self):
        track = F.media([F.video_stream(), F.audio_stream(codec_name="truehd", channels=6)]).audio[0]
        self.assertIs(decide_track(track, PROFILE).action, AudioAction.TRANSCODE_AC3)

    def test_truehd_atmos_loses_its_objects_and_the_decision_says_so(self):
        decision = decide_track(F.hdr10_hevc_truehd_atmos().audio[0], PROFILE)
        self.assertIs(decision.action, AudioAction.TRANSCODE_AC3)
        self.assertIn(R.AUDIO_OBJECT_METADATA_LOST, codes(decision.reasons))
        self.assertIn(R.AUDIO_LOSSLESS_TO_LOSSY, codes(decision.reasons))
        self.assertEqual(decision.target_channels, 6)

    def test_multichannel_flac_becomes_ac3(self):
        track = F.media(
            [F.video_stream(), F.audio_stream(codec_name="flac", channels=6, bit_rate=None)]
        ).audio[0]
        self.assertIs(decide_track(track, PROFILE).action, AudioAction.TRANSCODE_AC3)

    def test_multichannel_pcm_becomes_ac3(self):
        track = F.media(
            [F.video_stream(), F.audio_stream(codec_name="pcm_s24le", channels=6)]
        ).audio[0]
        self.assertIs(decide_track(track, PROFILE).action, AudioAction.TRANSCODE_AC3)

    def test_an_unknown_codec_gets_an_explicit_decision(self):
        track = F.media(
            [F.video_stream(), F.audio_stream(codec_name="some_new_codec", channels=6)]
        ).audio[0]
        decision = decide_track(track, PROFILE)
        self.assertIs(decision.action, AudioAction.TRANSCODE_AC3)
        self.assertIn(R.AUDIO_CODEC_UNKNOWN, codes(decision.reasons))

    def test_a_track_with_no_codec_is_unsupported(self):
        track = F.media([F.video_stream(), F.audio_stream(codec_name="")]).audio[0]
        self.assertIs(decide_track(track, PROFILE).action, AudioAction.UNSUPPORTED)

    def test_ac3_bitrate_follows_the_channel_count(self):
        for channels, expected in ((2, 256_000), (6, 640_000), (8, 640_000)):
            with self.subTest(channels=channels):
                track = F.media(
                    [F.video_stream(), F.audio_stream(codec_name="dts", channels=channels)]
                ).audio[0]
                _, bitrate, _ = ac3_target(track, PROFILE)
                self.assertEqual(bitrate, expected)

    def test_an_odd_sample_rate_is_resampled_to_48k(self):
        track = F.media(
            [F.video_stream(), F.audio_stream(codec_name="dts", sample_rate=96000)]
        ).audio[0]
        _, _, rate = ac3_target(track, PROFILE)
        self.assertEqual(rate, 48000)


class AudioSelectionTests(unittest.TestCase):
    def test_a_native_ac3_track_is_used_instead_of_transcoding_truehd(self):
        info = F.hdr10_hevc_truehd_and_ac3()
        decision = select_audio(info.audio, PROFILE)
        self.assertIs(decision.action, AudioAction.PASSTHROUGH)
        self.assertEqual(decision.track.codec, "ac3")
        self.assertIn(R.AUDIO_NATIVE_TRACK_PREFERRED, codes(decision.reasons))

    def test_selection_is_deterministic_across_track_order(self):
        info = F.hdr10_hevc_truehd_and_ac3()
        forward = select_audio(info.audio, PROFILE)
        backward = select_audio(tuple(reversed(info.audio)), PROFILE)
        self.assertEqual(forward.track.stream_index, backward.track.stream_index)

    def test_a_requested_language_wins_over_a_cheaper_track(self):
        info = F.media(
            [
                F.video_stream(),
                F.audio_stream(index=1, codec_name="ac3", language="eng", default=1),
                F.audio_stream(index=2, codec_name="dts", language="tur", default=0),
            ]
        )
        decision = select_audio(info.audio, PROFILE, preferred_language="tur")
        self.assertEqual(decision.track.language, "tur")
        self.assertIs(decision.action, AudioAction.TRANSCODE_AC3)

    def test_an_absent_language_falls_back_to_every_track(self):
        info = F.hdr10_hevc_truehd_and_ac3()
        decision = select_audio(info.audio, PROFILE, preferred_language="jpn")
        self.assertIsNotNone(decision.track)

    def test_no_audio_is_an_answer_not_a_crash(self):
        decision = select_audio((), PROFILE)
        self.assertIs(decision.action, AudioAction.NONE)
        self.assertIn(R.AUDIO_TRACK_MISSING, codes(decision.reasons))


class PlaybackDecisionTests(unittest.TestCase):
    def test_hdr10_with_passthrough_audio_is_direct_and_needs_no_session(self):
        decision = decide(F.hdr10_hevc_eac3(), PROFILE)
        self.assertIs(decision.mode, PlaybackMode.DIRECT)
        self.assertFalse(decision.needs_session)
        self.assertTrue(decision.video_is_copied)

    def test_hdr10_with_truehd_is_direct_video_plus_audio_transcode(self):
        decision = decide(F.hdr10_hevc_truehd_atmos(), PROFILE)
        self.assertIs(decision.mode, PlaybackMode.DIRECT_WITH_AUDIO_TRANSCODE)
        self.assertTrue(decision.needs_session)
        self.assertTrue(decision.video_is_copied)
        self.assertIs(decision.video.verdict, VideoVerdict.DIRECT)

    def test_an_unopenable_container_is_remuxed_not_re_encoded(self):
        info = F.media(
            [F.video_stream(), F.audio_stream(codec_name="ac3")], format_name="rawvideo"
        )
        decision = decide(info, PROFILE)
        self.assertIs(decision.mode, PlaybackMode.REMUX)
        self.assertTrue(decision.video_is_copied)
        self.assertIn(R.CONTAINER_REMUX_REQUIRED, codes(decision.reasons))

    def test_unsupported_video_makes_the_whole_decision_unsupported(self):
        info = F.media([F.video_stream(pix_fmt="yuv444p12le"), F.audio_stream()])
        decision = decide(info, PROFILE)
        self.assertIs(decision.mode, PlaybackMode.UNSUPPORTED)
        self.assertFalse(decision.video_is_copied)

    def test_every_playable_mode_copies_video(self):
        for info in (
            F.hdr10_hevc_eac3(),
            F.hdr10_hevc_truehd_atmos(),
            F.sdr_h264_aac(),
            F.hdr10_plus_hevc_ac3(),
            F.hlg_hevc_aac(),
            F.dolby_vision_profile7(),
        ):
            with self.subTest(source=info.primary_video.codec):
                decision = decide(info, PROFILE)
                self.assertNotEqual(decision.mode, PlaybackMode.UNSUPPORTED)
                self.assertTrue(decision.video_is_copied)


class PreviewPolicyTests(unittest.TestCase):
    def test_h264_sdr_mp4_previews_directly_in_a_browser(self):
        self.assertIs(decide_preview(F.sdr_h264_aac(), PROFILE).mode, PreviewMode.BROWSER_DIRECT)

    def test_4k_hevc_hdr10_is_not_previewed_rather_than_software_encoded(self):
        preview = decide_preview(F.hdr10_hevc_eac3(), PROFILE)
        self.assertIs(preview.mode, PreviewMode.UNSUPPORTED)
        self.assertIn(R.PREVIEW_SOFTWARE_TRANSCODE_REFUSED, codes(preview.reasons))
        self.assertIn(R.PREVIEW_UNSUPPORTED, codes(preview.reasons))

    def test_preview_and_kodi_are_decided_independently(self):
        info = F.hdr10_hevc_eac3()
        self.assertIs(decide(info, PROFILE).mode, PlaybackMode.DIRECT)
        self.assertIs(decide_preview(info, PROFILE).mode, PreviewMode.UNSUPPORTED)

    def test_a_browser_codec_in_an_unopenable_container_is_remuxed_not_encoded(self):
        info = F.media(
            [
                F.video_stream(
                    codec_name="h264",
                    profile="High",
                    pix_fmt="yuv420p",
                    color_primaries="bt709",
                    color_transfer="bt709",
                    color_space="bt709",
                ),
                F.audio_stream(codec_name="aac", profile="LC", channels=2),
            ],
            format_name="mpegts",
        )
        self.assertIs(decide_preview(info, PROFILE).mode, PreviewMode.BROWSER_REMUX)

    def test_the_profile_never_permits_a_software_video_transcode_for_preview(self):
        self.assertFalse(PROFILE.browser.allow_software_video_transcode)


if __name__ == "__main__":
    unittest.main()
