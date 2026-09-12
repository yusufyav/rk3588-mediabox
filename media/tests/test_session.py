"""Sessions and the ffmpeg commands they run.

Two properties matter more than the rest and are asserted from several angles:
video is never re-encoded, and no process outlives its session.
"""

from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import textwrap
import time
import unittest
from pathlib import Path

from ..errors import InvalidRequest, MediaError, NotFound, SessionError
from ..policy import decide, get_profile
from ..policy.decide import PlaybackMode
from ..proxy import FFmpegConfig, SourcePolicy, VideoCopyViolation, assert_video_copy
from ..proxy.ffmpeg import (
    CONTAINER_FRAGMENTED_MP4,
    build_argv,
    build_audio_transcode_argv,
    build_remux_argv,
)
from ..proxy.security import validate_session_id, validate_source_url
from ..proxy.session import SessionManager, SessionMode, SessionState
from . import fixtures as F


PROFILE = get_profile()
UPSTREAM = "http://127.0.0.1:11470"
POLICY = SourcePolicy(
    allowed_local_origins=frozenset({UPSTREAM}), resolve_names=False, allowed_file_prefixes=("/tmp/",)
)


#: A child that emits a first block and then stays alive. Anything waiting on
#: a producer that has written nothing would simply block, which is a property
#: of pipes rather than anything these tests are about.
SLOW_PRODUCER = """
    import sys, time
    sys.stdout.buffer.write(b"x" * 4096)
    sys.stdout.buffer.flush()
    time.sleep(600)
    """


def fake_binary(body: str) -> str:
    handle = tempfile.NamedTemporaryFile("w", suffix=".py", delete=False, prefix="fake-ffmpeg-")
    handle.write("#!" + sys.executable + "\n" + textwrap.dedent(body))
    handle.close()
    os.chmod(handle.name, 0o755)
    return handle.name


class VideoCopyInvariantTests(unittest.TestCase):
    def test_the_audio_transcode_command_copies_video(self):
        decision = decide(F.hdr10_hevc_truehd_atmos(), PROFILE)
        argv = build_audio_transcode_argv("https://host/a.mkv", decision)
        self.assertEqual(argv[argv.index("-c:v") + 1], "copy")
        self.assertEqual(argv[argv.index("-c:a") + 1], "ac3")
        self.assertEqual(argv[argv.index("-ac") + 1], "6")
        self.assertEqual(argv[argv.index("-b:a") + 1], "640000")

    def test_track_mapping_uses_the_indexes_the_policy_chose(self):
        """The encoder must work on the chosen track, not on the first one.

        The audio here sits at absolute index 3, behind a subtitle and a
        commentary track, which is what a real release looks like.
        """
        info = F.media(
            [
                F.video_stream(index=0),
                F.subtitle_stream(index=1, language="eng"),
                F.audio_stream(index=2, codec_name="dts", channels=6, language="fre", default=0),
                F.audio_stream(index=3, codec_name="dts", channels=6, language="eng", default=1),
            ]
        )
        decision = decide(info, PROFILE, preferred_language="eng")
        self.assertEqual(decision.audio.track.stream_index, 3)
        argv = build_audio_transcode_argv("https://host/a.mkv", decision)
        maps = [argv[index + 1] for index, token in enumerate(argv) if token == "-map"]
        self.assertEqual(maps, ["0:0", "0:3"])

    def test_the_remux_command_copies_both_streams(self):
        info = F.media([F.video_stream(), F.audio_stream(codec_name="ac3")], format_name="rawvideo")
        decision = decide(info, PROFILE)
        self.assertIs(decision.mode, PlaybackMode.REMUX)
        argv = build_remux_argv("https://host/a.bin", decision)
        self.assertEqual(argv[argv.index("-c:v") + 1], "copy")
        self.assertEqual(argv[argv.index("-c:a") + 1], "copy")

    def test_a_command_that_would_encode_video_is_refused(self):
        for argv in (
            ["ffmpeg", "-i", "x", "-c:v", "libx264", "-f", "mp4", "pipe:1"],
            ["ffmpeg", "-i", "x", "-vcodec", "h264_v4l2m2m", "-f", "mp4", "pipe:1"],
            ["ffmpeg", "-i", "x", "-c", "libx265", "-f", "mp4", "pipe:1"],
            ["ffmpeg", "-i", "x", "-c:v", "copy", "-vf", "scale=1280:720", "-f", "mp4", "pipe:1"],
            ["ffmpeg", "-i", "x", "-c:v", "copy", "-filter_complex", "[0:v]null", "pipe:1"],
        ):
            with self.subTest(argv=argv[3]):
                with self.assertRaises(VideoCopyViolation):
                    assert_video_copy(argv)

    def test_a_command_that_never_mentions_the_video_codec_is_refused(self):
        with self.assertRaises(VideoCopyViolation):
            assert_video_copy(["ffmpeg", "-i", "x", "-c:a", "ac3", "-f", "mp4", "pipe:1"])

    def test_build_argv_refuses_a_decision_that_needs_no_process(self):
        decision = decide(F.hdr10_hevc_eac3(), PROFILE)
        self.assertIs(decision.mode, PlaybackMode.DIRECT)
        with self.assertRaises(MediaError):
            build_argv("https://host/a.mkv", decision)

    def test_a_fragmented_mp4_output_is_still_a_copy(self):
        decision = decide(F.hdr10_hevc_truehd_atmos(), PROFILE)
        argv = build_audio_transcode_argv(
            "https://host/a.mkv", decision, container=CONTAINER_FRAGMENTED_MP4
        )
        self.assertIn("-movflags", argv)
        self.assertEqual(argv[argv.index("-c:v") + 1], "copy")

    def test_protocol_options_are_only_sent_for_the_protocol_that_has_them(self):
        decision = decide(F.hdr10_hevc_truehd_atmos(), PROFILE)
        http = build_audio_transcode_argv("https://host/a.mkv", decision)
        local = build_audio_transcode_argv("file:///tmp/a.mkv", decision)
        self.assertIn("-reconnect", http)
        self.assertNotIn("-reconnect", local)
        self.assertNotIn("-user_agent", local)

    def test_the_source_is_never_interpolated_into_a_string(self):
        decision = decide(F.hdr10_hevc_truehd_atmos(), PROFILE)
        hostile = "https://host/a.mkv?x=`id`;rm -rf /&y=$(whoami)"
        argv = build_audio_transcode_argv(hostile, decision)
        self.assertEqual(argv[argv.index("-i") + 1], hostile)
        self.assertEqual(sum(1 for token in argv if token == hostile), 1)

    def test_a_start_offset_seeks_the_input(self):
        decision = decide(F.hdr10_hevc_truehd_atmos(), PROFILE)
        argv = build_audio_transcode_argv("https://host/a.mkv", decision, start_seconds=30.25)
        self.assertLess(argv.index("-ss"), argv.index("-i"))
        self.assertEqual(argv[argv.index("-ss") + 1], "30.250")


class SecurityTests(unittest.TestCase):
    def test_only_http_and_https_reach_a_session(self):
        for url in (
            "magnet:?xt=urn:btih:" + "a" * 40,
            "data:video/mp4;base64,AAAA",
            "gopher://host/x",
            "ftp://host/x.mkv",
            "javascript:alert(1)",
        ):
            with self.subTest(url=url):
                with self.assertRaises(InvalidRequest):
                    validate_source_url(url, POLICY)

    def test_a_url_with_control_characters_is_refused(self):
        with self.assertRaises(InvalidRequest):
            validate_source_url("https://host/a.mkv\r\nX-Injected: 1", POLICY)

    def test_loopback_is_refused_except_for_the_streaming_server(self):
        validate_source_url(f"{UPSTREAM}/abcdef/0", POLICY)
        with self.assertRaises(InvalidRequest):
            validate_source_url("http://127.0.0.1:8080/jsonrpc", POLICY)

    def test_the_metadata_service_is_refused(self):
        with self.assertRaises(InvalidRequest):
            validate_source_url("http://169.254.169.254/latest/meta-data/", POLICY)

    def test_link_local_is_refused(self):
        with self.assertRaises(InvalidRequest):
            validate_source_url("http://169.254.10.10/x.mkv", POLICY)

    def test_file_sources_are_off_unless_a_directory_is_allowed(self):
        with self.assertRaises(InvalidRequest):
            validate_source_url("file:///etc/passwd", SourcePolicy(resolve_names=False))
        validate_source_url("file:///tmp/clip.mkv", POLICY)

    def test_a_file_url_cannot_traverse_out_of_its_directory(self):
        with self.assertRaises(InvalidRequest):
            validate_source_url("file:///tmp/../etc/passwd", POLICY)

    def test_a_file_url_cannot_name_a_remote_host(self):
        with self.assertRaises(InvalidRequest):
            validate_source_url("file://evil.example/tmp/x.mkv", POLICY)

    def test_session_ids_are_fixed_shape(self):
        validate_session_id("0" * 32)
        for bad in ("", "../../etc/passwd", "0" * 31, "G" * 32, "0" * 33, None):
            with self.subTest(value=bad):
                with self.assertRaises(InvalidRequest):
                    validate_session_id(bad)

    def test_a_public_host_is_allowed(self):
        validate_source_url("https://cdn.example.com/a.mkv", POLICY)


class SessionLifecycleTests(unittest.TestCase):
    def setUp(self):
        self.binaries: list[str] = []
        self.managers: list[SessionManager] = []

    def tearDown(self):
        for manager in self.managers:
            manager.shutdown()
        for path in self.binaries:
            Path(path).unlink(missing_ok=True)

    def _manager(self, body: str, **kwargs) -> SessionManager:
        binary = fake_binary(body)
        self.binaries.append(binary)
        manager = SessionManager(
            source_policy=POLICY, ffmpeg=FFmpegConfig(binary=binary), **kwargs
        )
        self.managers.append(manager)
        return manager

    @staticmethod
    def _transcode_decision():
        return decide(F.hdr10_hevc_truehd_atmos(), PROFILE)

    def test_a_direct_decision_starts_no_process(self):
        manager = self._manager("pass")
        session = manager.create("https://cdn.example.com/a.mkv", decide(F.hdr10_hevc_eac3(), PROFILE))
        self.assertIs(session.mode, SessionMode.DIRECT)
        self.assertIsNone(session.pid)
        self.assertEqual(session.playback_url, "https://cdn.example.com/a.mkv")

    def test_a_direct_session_has_nothing_to_relay(self):
        manager = self._manager("pass")
        session = manager.create("https://cdn.example.com/a.mkv", decide(F.hdr10_hevc_eac3(), PROFILE))
        with self.assertRaises(SessionError):
            manager.attach(session.session_id)

    def test_asking_twice_for_the_same_source_returns_one_session(self):
        manager = self._manager(SLOW_PRODUCER)
        first = manager.create("https://cdn.example.com/a.mkv", self._transcode_decision())
        second = manager.create("https://cdn.example.com/a.mkv", self._transcode_decision())
        self.assertEqual(first.session_id, second.session_id)
        self.assertEqual(len(manager.active()), 1)

    def test_a_different_source_gets_its_own_session(self):
        manager = self._manager(SLOW_PRODUCER)
        first = manager.create("https://cdn.example.com/a.mkv", self._transcode_decision())
        second = manager.create("https://cdn.example.com/b.mkv", self._transcode_decision())
        self.assertNotEqual(first.session_id, second.session_id)

    def test_the_child_starts_on_attach_and_its_output_is_relayed(self):
        manager = self._manager(
            """
            import sys
            sys.stdout.buffer.write(b"MEDIABOX" * 1024)
            sys.stdout.buffer.flush()
            """
        )
        session = manager.create("https://cdn.example.com/a.mkv", self._transcode_decision())
        self.assertIsNone(session.pid)
        body = b"".join(manager.attach(session.session_id))
        self.assertEqual(body, b"MEDIABOX" * 1024)
        self.assertIsNotNone(session.pid)
        self.assertEqual(session.clients, 0)

    def test_stop_is_idempotent(self):
        manager = self._manager(SLOW_PRODUCER)
        session = manager.create("https://cdn.example.com/a.mkv", self._transcode_decision())
        list(self._read_a_little(manager, session.session_id))
        first = manager.stop(session.session_id)
        second = manager.stop(session.session_id)
        self.assertTrue(first["stopped"])
        self.assertFalse(second["stopped"])
        self.assertIs(session.state, SessionState.STOPPED)

    def test_stopping_a_session_that_never_existed_is_harmless(self):
        manager = self._manager("pass")
        self.assertEqual(manager.stop("f" * 32)["stopped"], False)

    def test_stop_kills_the_child_and_reaps_it(self):
        manager = self._manager(
            """
            import sys, time
            sys.stdout.buffer.write(b"x" * 4096)
            sys.stdout.buffer.flush()
            time.sleep(600)
            """
        )
        session = manager.create("https://cdn.example.com/a.mkv", self._transcode_decision())
        stream = manager.attach(session.session_id)
        next(stream)
        pid = session.pid
        self.assertTrue(_alive(pid))
        manager.stop(session.session_id, "test")
        stream.close()
        self.assertFalse(_alive(pid), "the child survived stop()")
        self.assertIsNotNone(session.exit_code)

    def test_a_child_that_ignores_sigterm_is_killed(self):
        manager = self._manager(
            """
            import signal, sys, time
            signal.signal(signal.SIGTERM, signal.SIG_IGN)
            sys.stdout.buffer.write(b"x" * 4096)
            sys.stdout.buffer.flush()
            time.sleep(600)
            """
        )
        session = manager.create("https://cdn.example.com/a.mkv", self._transcode_decision())
        stream = manager.attach(session.session_id)
        next(stream)
        pid = session.pid
        manager.stop(session.session_id, "test")
        stream.close()
        self.assertFalse(_alive(pid))

    def test_a_grandchild_is_taken_down_with_the_session(self):
        manager = self._manager(
            """
            import subprocess, sys, time
            subprocess.Popen([sys.executable, "-c",
                              "import time; time.sleep(600)  # mediabox-session-grandchild"])
            sys.stdout.buffer.write(b"x" * 4096)
            sys.stdout.buffer.flush()
            time.sleep(600)
            """
        )
        session = manager.create("https://cdn.example.com/a.mkv", self._transcode_decision())
        stream = manager.attach(session.session_id)
        next(stream)
        manager.stop(session.session_id, "test")
        stream.close()
        time.sleep(0.3)
        survivors = subprocess.run(
            ["pgrep", "-f", "mediabox-session-grandchild"], capture_output=True, text=True
        ).stdout.split()
        self.assertEqual(survivors, [], "the session's grandchild survived")

    def test_an_abandoned_session_is_reaped_after_its_ttl(self):
        clock = _Clock()
        manager = self._manager(SLOW_PRODUCER, idle_timeout_seconds=45.0, clock=clock)
        session = manager.create("https://cdn.example.com/a.mkv", self._transcode_decision())
        stream = manager.attach(session.session_id)
        next(stream, None)
        manager.detach(session.session_id)
        pid = session.pid
        self.assertEqual(manager.reap_once(), [])
        clock.advance(46.0)
        self.assertEqual(manager.reap_once(), [session.session_id])
        stream.close()
        self.assertIs(session.state, SessionState.STOPPED)
        self.assertFalse(_alive(pid))

    def test_a_session_with_a_reader_is_not_reaped(self):
        clock = _Clock()
        manager = self._manager(
            """
            import sys, time
            while True:
                sys.stdout.buffer.write(b"x" * 4096)
                sys.stdout.buffer.flush()
                time.sleep(0.05)
            """,
            idle_timeout_seconds=1.0,
            clock=clock,
        )
        session = manager.create("https://cdn.example.com/a.mkv", self._transcode_decision())
        stream = manager.attach(session.session_id)
        next(stream)
        clock.advance(100.0)
        self.assertEqual(manager.reap_once(), [])
        manager.stop(session.session_id)
        stream.close()

    def test_a_child_that_exits_on_its_own_ends_the_session(self):
        manager = self._manager("import sys; sys.exit(0)")
        session = manager.create("https://cdn.example.com/a.mkv", self._transcode_decision())
        list(manager.attach(session.session_id))
        # the relay ended; the reaper notices the exited child
        self.assertEqual(manager.reap_once(), [session.session_id])
        self.assertIs(session.state, SessionState.STOPPED)

    def test_shutdown_ends_every_session(self):
        manager = self._manager(SLOW_PRODUCER)
        sessions = []
        for name in ("a", "b", "c"):
            session = manager.create(
                f"https://cdn.example.com/{name}.mkv", self._transcode_decision()
            )
            stream = manager.attach(session.session_id)
            next(stream, None)
            sessions.append((session, stream))
        pids = [session.pid for session, _ in sessions]
        manager.shutdown()
        for _, stream in sessions:
            stream.close()
        for pid in pids:
            self.assertFalse(_alive(pid), f"pid {pid} survived shutdown")

    def test_an_unsupported_source_cannot_become_a_session(self):
        manager = self._manager("pass")
        info = F.media([F.video_stream(pix_fmt="yuv444p12le"), F.audio_stream()])
        with self.assertRaises(SessionError):
            manager.create("https://cdn.example.com/a.mkv", decide(info, PROFILE))

    def test_a_risky_source_cannot_become_a_session(self):
        manager = self._manager("pass")
        with self.assertRaises(SessionError):
            manager.create(
                "https://cdn.example.com/dv.mkv", decide(F.dolby_vision_profile5(), PROFILE)
            )

    def test_an_unknown_session_id_is_not_found(self):
        manager = self._manager("pass")
        with self.assertRaises(NotFound):
            manager.get("a" * 32)

    def test_a_malformed_session_id_is_rejected_before_any_lookup(self):
        manager = self._manager("pass")
        with self.assertRaises(InvalidRequest):
            manager.get("../../etc/passwd")

    def test_a_session_cannot_be_created_for_a_refused_source(self):
        manager = self._manager("pass")
        with self.assertRaises(InvalidRequest):
            manager.create("magnet:?xt=urn:btih:" + "a" * 40, self._transcode_decision())

    def test_a_failing_binary_reports_instead_of_leaking(self):
        manager = SessionManager(
            source_policy=POLICY, ffmpeg=FFmpegConfig(binary="/nonexistent/ffmpeg")
        )
        self.managers.append(manager)
        with self.assertRaises(MediaError):
            manager.create("https://cdn.example.com/a.mkv", self._transcode_decision())

    def test_stopped_session_records_are_forgotten_eventually(self):
        clock = _Clock()
        manager = self._manager("import sys; sys.exit(0)", clock=clock)
        session = manager.create("https://cdn.example.com/a.mkv", self._transcode_decision())
        manager.stop(session.session_id)
        clock.advance(301.0)
        self.assertEqual(manager.forget_stopped(), 1)
        self.assertEqual(manager.list(), [])

    def test_a_session_reports_the_tracks_it_selected(self):
        manager = self._manager("pass")
        session = manager.create("https://cdn.example.com/a.mkv", self._transcode_decision())
        tracks = session.selected_tracks()
        self.assertEqual(tracks["video"]["action"], "copy")
        self.assertEqual(tracks["audio"]["target"]["codec"], "ac3")
        self.assertEqual(tracks["audio"]["target"]["channels"], 6)

    @staticmethod
    def _read_a_little(manager, session_id):
        stream = manager.attach(session_id)
        try:
            next(stream, None)
        finally:
            stream.close()
        return []


class _Clock:
    """A monotonic clock the test moves by hand, so TTLs need no sleeping."""

    def __init__(self) -> None:
        self._now = 1000.0

    def __call__(self) -> float:
        return self._now

    def advance(self, seconds: float) -> None:
        self._now += seconds


def _alive(pid: int | None) -> bool:
    if pid is None:
        return False
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    # A reaped child is gone; an unreaped one would still answer signal 0, so
    # the process state is checked too.
    try:
        status = Path(f"/proc/{pid}/stat").read_text()
    except OSError:
        return False
    return status.rsplit(")", 1)[1].split()[0] != "Z"


if __name__ == "__main__":
    unittest.main()
