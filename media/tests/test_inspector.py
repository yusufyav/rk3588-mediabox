"""The inspector: ffprobe invocation, parsing, and what it refuses to guess."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import textwrap
import time
import unittest
from pathlib import Path

from ..errors import ProbeError, ProbeTimeout
from ..inspector import build_media_info, inspect, probe
from ..inspector.ffprobe import FFprobeConfig, build_argv, run_ffprobe
from ..inspector.model import ChromaSubsampling, ColorRange, HdrFormat, ProbeResult
from . import fixtures as F


def _fake_ffprobe(body: str) -> str:
    """Write a Python script that behaves like ffprobe for one test."""
    handle = tempfile.NamedTemporaryFile(
        "w", suffix=".py", delete=False, prefix="fake-ffprobe-"
    )
    handle.write("#!" + sys.executable + "\n" + textwrap.dedent(body))
    handle.close()
    os.chmod(handle.name, 0o755)
    return handle.name


class ParsingTests(unittest.TestCase):
    def test_container_and_tracks_are_normalized(self):
        info = F.hdr10_hevc_eac3()
        self.assertEqual(info.container.format_name, "matroska,webm")
        self.assertAlmostEqual(info.container.duration_seconds, 6343.52, places=2)
        self.assertEqual(info.container.bit_rate, 15236005)

        video = info.primary_video
        self.assertEqual(video.codec, "hevc")
        self.assertEqual(video.profile, "Main 10")
        self.assertEqual(video.bit_depth, 10)
        self.assertIs(video.chroma, ChromaSubsampling.YUV420)
        self.assertIs(video.color_range, ColorRange.LIMITED)
        self.assertAlmostEqual(video.fps, 23.976, places=3)
        self.assertIs(video.hdr, HdrFormat.HDR10)

        self.assertEqual(len(info.audio), 1)
        self.assertEqual(info.audio[0].codec, "eac3")
        self.assertEqual(info.audio[0].channels, 6)
        self.assertEqual(info.audio[0].language, "eng")
        self.assertTrue(info.audio[0].is_default)

    def test_frame_side_data_supplies_mastering_display_and_light_level(self):
        video = F.hdr10_hevc_eac3().primary_video
        self.assertIsNotNone(video.mastering_display)
        self.assertEqual(video.mastering_display.max_luminance, 1000.0)
        self.assertEqual(video.mastering_display.min_luminance, 0.005)
        self.assertEqual(video.max_cll, 1000)
        self.assertEqual(video.max_fall, 400)

    def test_rationals_and_string_numbers_become_numbers(self):
        info = F.sdr_h264_aac()
        self.assertIsInstance(info.audio[0].sample_rate, int)
        self.assertIsInstance(info.audio[0].bit_rate, int)
        self.assertIsInstance(info.primary_video.fps, float)

    def test_subtitles_are_reported_with_flags(self):
        info = F.media(
            [
                F.video_stream(),
                F.audio_stream(),
                F.subtitle_stream(index=2, language="eng", forced=1),
                F.subtitle_stream(index=3, language="tur", default=1),
            ]
        )
        self.assertEqual([s.language for s in info.subtitles], ["eng", "tur"])
        self.assertTrue(info.subtitles[0].is_forced)
        self.assertTrue(info.subtitles[1].is_default)

    def test_cover_art_is_not_a_video_track(self):
        cover = F.video_stream(index=2, codec_name="mjpeg", profile=None, default=0)
        cover["disposition"]["attached_pic"] = 1
        info = F.media([F.video_stream(), F.audio_stream(), cover])
        self.assertEqual(len(info.video), 1)

    def test_hdr10_plus_is_distinguished_from_hdr10(self):
        self.assertIs(F.hdr10_plus_hevc_ac3().primary_video.hdr, HdrFormat.HDR10_PLUS)
        self.assertIs(F.hdr10_hevc_eac3().primary_video.hdr, HdrFormat.HDR10)

    def test_hlg_is_recognised(self):
        self.assertIs(F.hlg_hevc_aac().primary_video.hdr, HdrFormat.HLG)

    def test_dolby_vision_configuration_record_is_read(self):
        video = F.dolby_vision_profile8(compatibility_id=1).primary_video
        self.assertIs(video.hdr, HdrFormat.DOLBY_VISION)
        self.assertEqual(video.dolby_vision.profile, 8)
        self.assertEqual(video.dolby_vision.bl_signal_compatibility_id, 1)
        self.assertTrue(video.dolby_vision.rpu_present)

    def test_dolby_vision_codec_tag_without_a_record_is_flagged_not_guessed(self):
        info = F.media(
            [F.video_stream(codec_tag_string="dvh1"), F.audio_stream()],
        )
        video = info.primary_video
        self.assertIsNotNone(video.dolby_vision)
        self.assertIsNone(video.dolby_vision.profile)
        self.assertTrue(any("configuration record" in w for w in info.warnings))

    def test_contradictory_colour_metadata_is_unknown_not_sdr(self):
        info = F.conflicting_color_metadata()
        self.assertIs(info.primary_video.hdr, HdrFormat.UNKNOWN)
        self.assertTrue(any("inconsistent" in w for w in info.warnings))

    def test_absent_colour_metadata_is_unknown_not_sdr(self):
        info = F.unknown_color_metadata()
        self.assertIs(info.primary_video.hdr, HdrFormat.UNKNOWN)

    def test_object_audio_is_only_claimed_when_the_profile_says_so(self):
        atmos = F.hdr10_hevc_truehd_atmos().audio[0]
        self.assertTrue(atmos.object_audio)
        plain = F.media([F.video_stream(), F.audio_stream(codec_name="truehd", profile=None)]).audio[0]
        self.assertIsNone(plain.object_audio)
        self.assertIn("not determinable", plain.object_audio_source or "")

    def test_eight_channel_ac3_is_not_called_atmos(self):
        track = F.media(
            [F.video_stream(), F.audio_stream(codec_name="ac3", channels=8, channel_layout="7.1")]
        ).audio[0]
        self.assertIsNone(track.object_audio)

    def test_malformed_probe_output_produces_warnings_not_exceptions(self):
        result = ProbeResult(
            format={"duration": "not-a-number", "bit_rate": None},
            streams=[
                {"codec_type": "video", "index": "x", "width": "wide", "r_frame_rate": "1/0"},
                {"codec_type": "audio", "index": 1, "channels": "many", "sample_rate": ""},
            ],
        )
        info = build_media_info("https://example.invalid/x", result)
        self.assertIsNone(info.container.duration_seconds)
        self.assertIsNone(info.primary_video.width)
        self.assertIsNone(info.primary_video.fps)
        self.assertIsNone(info.audio[0].channels)

    def test_a_source_without_audio_is_described_and_flagged(self):
        info = F.media([F.video_stream()])
        self.assertEqual(info.audio, ())
        self.assertTrue(any("no audio track" in w for w in info.warnings))


class ArgvTests(unittest.TestCase):
    def test_the_source_is_one_argument_after_minus_i(self):
        argv = build_argv("http://host/a b?c=d;rm -rf /", FFprobeConfig(), frames=False)
        self.assertEqual(argv[-2], "-i")
        self.assertEqual(argv[-1], "http://host/a b?c=d;rm -rf /")

    def test_reads_are_bounded(self):
        argv = build_argv("http://host/x", FFprobeConfig(), frames=False)
        self.assertIn("-probesize", argv)
        self.assertIn("-analyzeduration", argv)

    def test_the_frame_pass_reads_an_interval_not_the_file(self):
        argv = build_argv("http://host/x", FFprobeConfig(), frames=True)
        self.assertIn("-read_intervals", argv)
        self.assertEqual(argv[argv.index("-read_intervals") + 1], "%+#2")


class ProcessTests(unittest.TestCase):
    """A probe must not be able to outlive the call that started it."""

    def setUp(self):
        self._scripts: list[str] = []

    def tearDown(self):
        for path in self._scripts:
            Path(path).unlink(missing_ok=True)

    def _script(self, body: str) -> str:
        path = _fake_ffprobe(body)
        self._scripts.append(path)
        return path

    def _children(self) -> list[int]:
        try:
            output = subprocess.run(
                ["pgrep", "-P", str(os.getpid())], capture_output=True, text=True, timeout=10
            ).stdout
        except (OSError, subprocess.SubprocessError):
            return []
        return [int(line) for line in output.split() if line.strip().isdigit()]

    def test_a_hanging_probe_is_killed_and_reaped(self):
        script = self._script(
            """
            import time
            time.sleep(600)
            """
        )
        before = set(self._children())
        started = time.monotonic()
        with self.assertRaises(ProbeTimeout):
            run_ffprobe("http://x", FFprobeConfig(binary=script, timeout_seconds=1.0), frames=False)
        elapsed = time.monotonic() - started
        self.assertLess(elapsed, 15.0, "the deadline was not enforced")
        time.sleep(0.2)
        self.assertEqual(
            set(self._children()) - before, set(), "the probe left a child behind"
        )

    def test_a_probe_that_spawns_a_child_takes_it_down_too(self):
        script = self._script(
            """
            import subprocess, sys, time
            subprocess.Popen([sys.executable, "-c", "import time; time.sleep(600)"])
            time.sleep(600)
            """
        )
        with self.assertRaises(ProbeTimeout):
            run_ffprobe("http://x", FFprobeConfig(binary=script, timeout_seconds=1.0), frames=False)
        time.sleep(0.3)
        grandchildren = subprocess.run(
            ["pgrep", "-f", "import time; time.sleep(600)"], capture_output=True, text=True
        ).stdout.split()
        self.assertEqual(grandchildren, [], "the probe's own child survived the kill")

    def test_a_nonzero_exit_is_an_error_with_the_stderr_kept(self):
        script = self._script(
            """
            import sys
            sys.stderr.write("Invalid data found when processing input\\n")
            sys.exit(1)
            """
        )
        with self.assertRaises(ProbeError) as caught:
            run_ffprobe("http://x", FFprobeConfig(binary=script, timeout_seconds=10.0), frames=False)
        self.assertIn("Invalid data", caught.exception.details["stderr"])

    def test_output_that_is_not_json_is_an_error(self):
        script = self._script(
            """
            print("<html>404 not found</html>")
            """
        )
        with self.assertRaises(ProbeError):
            run_ffprobe("http://x", FFprobeConfig(binary=script, timeout_seconds=10.0), frames=False)

    def test_empty_output_is_an_error(self):
        script = self._script("pass")
        with self.assertRaises(ProbeError):
            run_ffprobe("http://x", FFprobeConfig(binary=script, timeout_seconds=10.0), frames=False)

    def test_a_flood_of_stderr_does_not_deadlock_the_probe(self):
        script = self._script(
            """
            import json, sys
            sys.stderr.write("x" * (4 * 1024 * 1024))
            sys.stderr.flush()
            print(json.dumps({"format": {"format_name": "matroska,webm"}, "streams": [
                {"index": 0, "codec_type": "video", "codec_name": "hevc", "width": 1920,
                 "height": 1080, "pix_fmt": "yuv420p", "r_frame_rate": "24/1"}]}))
            """
        )
        parsed, _ = run_ffprobe(
            "http://x", FFprobeConfig(binary=script, timeout_seconds=30.0), frames=False
        )
        self.assertEqual(parsed["streams"][0]["codec_name"], "hevc")

    def test_a_missing_binary_is_reported_not_raised_as_oserror(self):
        with self.assertRaises(ProbeError):
            run_ffprobe(
                "http://x",
                FFprobeConfig(binary="/nonexistent/ffprobe", timeout_seconds=5.0),
                frames=False,
            )

    def test_probe_refuses_a_source_with_no_streams(self):
        script = self._script(
            """
            import json
            print(json.dumps({"format": {"format_name": "data"}, "streams": []}))
            """
        )
        with self.assertRaises(ProbeError):
            probe("http://x", FFprobeConfig(binary=script, timeout_seconds=10.0))

    def test_a_failing_frame_pass_is_a_warning_not_a_failure(self):
        script = self._script(
            """
            import json, sys
            if "-show_frames" in sys.argv:
                sys.stderr.write("Server returned 403 Forbidden\\n")
                sys.exit(1)
            print(json.dumps({"format": {"format_name": "matroska,webm", "duration": "10.0"},
                              "streams": [{"index": 0, "codec_type": "video",
                                           "codec_name": "hevc", "profile": "Main 10",
                                           "width": 3840, "height": 2160,
                                           "pix_fmt": "yuv420p10le", "r_frame_rate": "24/1",
                                           "color_transfer": "smpte2084",
                                           "color_primaries": "bt2020"}]}))
            """
        )
        info = inspect("http://x", FFprobeConfig(binary=script, timeout_seconds=10.0))
        self.assertIs(info.primary_video.hdr, HdrFormat.HDR10)
        self.assertTrue(any("frame-level" in w for w in info.warnings))


if __name__ == "__main__":
    unittest.main()
