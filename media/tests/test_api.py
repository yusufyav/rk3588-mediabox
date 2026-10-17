"""The media API surface: routing, validation, and what it refuses."""

from __future__ import annotations

import json
import unittest
from unittest import mock
from typing import Any

from ..api import MediaCore, MediaCoreConfig
from ..inspector import FFprobeConfig
from ..policy import decide, get_profile
from . import fixtures as F


PROFILE = get_profile()


def body_of(response) -> Any:
    return json.loads(response.body.decode("utf-8"))


class _StubStremio:
    def __init__(self) -> None:
        self.resolved: list[dict[str, Any]] = []

    def session_status(self):
        class Status:
            @staticmethod
            def as_dict():
                return {"authenticated": False, "addonCount": 3}

        return Status()

    def resolve_descriptor(self, descriptor):
        from ..stremio.models import ResolvedStream, Stream, StreamKind

        self.resolved.append(descriptor)
        stream = Stream(kind=StreamKind.URL, url=descriptor.get("url"))
        return ResolvedStream(stream=stream, url=descriptor["url"], via="addon")


class RoutingTests(unittest.TestCase):
    def setUp(self):
        self.core = MediaCore(MediaCoreConfig(base_url="http://box:8790"))
        self.core.stremio = _StubStremio()
        self.addCleanup(self.core.shutdown)

    def test_the_core_owns_only_its_mount(self):
        self.assertTrue(self.core.owns("/media"))
        self.assertTrue(self.core.owns("/media/status"))
        self.assertFalse(self.core.owns("/mediaxyz"))
        self.assertFalse(self.core.owns("/api/v1/health"))
        self.assertFalse(self.core.owns("/ui/"))

    def test_a_path_outside_the_mount_is_not_found(self):
        self.assertEqual(self.core.handle("GET", "/api/v1/health").status, 404)

    def test_an_unknown_endpoint_is_not_found(self):
        self.assertEqual(self.core.handle("GET", "/media/nonsense").status, 404)

    def test_the_capability_profile_is_served(self):
        response = self.core.handle("GET", "/media/capabilities")
        payload = body_of(response)
        self.assertEqual(payload["name"], "rk3588_orangepi5_production")
        self.assertFalse(payload["video"]["dolbyVisionPipeline"])
        self.assertEqual(payload["audio"]["transcodeTarget"], "ac3")

    def test_status_reports_the_provider_without_naming_its_internals(self):
        payload = body_of(self.core.handle("GET", "/media/status"))
        self.assertIn("provider", payload)
        self.assertEqual(payload["capabilityProfile"], "rk3588_orangepi5_production")
        self.assertTrue(payload["available"])
        self.assertEqual(payload["torrentNetwork"]["status"], "UNKNOWN")
        self.assertTrue(payload["torrentNetwork"]["directHttpAvailable"])
        self.assertEqual(payload["sessions"], 0)

    def test_a_search_without_a_query_is_a_bad_request(self):
        self.assertEqual(self.core.handle("GET", "/media/search").status, 400)

    def test_an_unsupported_method_is_refused(self):
        self.assertEqual(self.core.handle("PUT", "/media/status").status, 405)

    def test_a_body_that_is_not_json_is_a_bad_request(self):
        response = self.core.handle("POST", "/media/inspect", body=b"<html>")
        self.assertEqual(response.status, 400)

    def test_a_body_that_is_not_an_object_is_a_bad_request(self):
        response = self.core.handle("POST", "/media/inspect", body=b"[1,2,3]")
        self.assertEqual(response.status, 400)

    def test_an_oversized_body_is_refused(self):
        response = self.core.handle("POST", "/media/inspect", body=b"x" * (300 * 1024))
        self.assertEqual(response.status, 400)

    def test_inspect_without_a_source_is_a_bad_request(self):
        self.assertEqual(self.core.handle("POST", "/media/inspect", body=b"{}").status, 400)

    def test_a_magnet_source_is_refused_by_the_api(self):
        payload = json.dumps({"url": "magnet:?xt=urn:btih:" + "a" * 40}).encode()
        response = self.core.handle("POST", "/media/inspect", body=payload)
        self.assertEqual(response.status, 400)
        self.assertEqual(body_of(response)["error"]["code"], "INVALID_REQUEST")

    def test_a_loopback_source_that_is_not_the_streaming_server_is_refused(self):
        payload = json.dumps({"url": "http://127.0.0.1:8080/jsonrpc"}).encode()
        self.assertEqual(self.core.handle("POST", "/media/inspect", body=payload).status, 400)

    def test_rank_needs_sources(self):
        self.assertEqual(self.core.handle("POST", "/media/rank", body=b"{}").status, 400)

    def test_rank_refuses_more_sources_than_it_will_probe(self):
        payload = json.dumps({"sources": ["https://h/%d.mkv" % i for i in range(50)]}).encode()
        self.assertEqual(self.core.handle("POST", "/media/rank", body=payload).status, 400)

    def test_a_malformed_session_id_never_reaches_the_manager(self):
        response = self.core.handle("GET", "/media/session/..%2F..%2Fetc%2Fpasswd")
        self.assertIn(response.status, (400, 404))

    def test_an_unknown_session_is_not_found(self):
        self.assertEqual(self.core.handle("GET", "/media/session/" + "a" * 32).status, 404)

    def test_every_json_response_forbids_sniffing_and_caching(self):
        for path in ("/media/status", "/media/capabilities"):
            headers = dict(self.core.handle("GET", path).headers)
            self.assertEqual(headers["X-Content-Type-Options"], "nosniff")
            self.assertEqual(headers["Cache-Control"], "no-store")

    def test_the_api_emits_no_cors_header(self):
        for path in ("/media/status", "/media/capabilities"):
            names = {name.lower() for name, _ in self.core.handle("GET", path).headers}
            self.assertFalse(any(name.startswith("access-control-") for name in names))


class _FakeProbeCore(MediaCore):
    """A core whose inspector answers from a table instead of from ffprobe."""

    def __init__(self, table, **kwargs):
        super().__init__(MediaCoreConfig(base_url="http://box:8790", **kwargs))
        self._table = table
        self.stremio = _StubStremio()

    def inspect_url(self, url: str):
        from ..proxy.security import validate_source_url

        validate_source_url(url, self.source_policy)
        if url not in self._table:
            from ..errors import ProbeError

            raise ProbeError(f"no fixture for {url}")
        info = self._table[url]
        from dataclasses import replace

        return replace(info, source_url=url)


class PlanAndRankTests(unittest.TestCase):
    def setUp(self):
        self.table = {
            "https://cdn.example/hdr10.mkv": F.hdr10_hevc_eac3(),
            "https://cdn.example/atmos.mkv": F.hdr10_hevc_truehd_atmos(),
            "https://cdn.example/dv5.mkv": F.dolby_vision_profile5(),
            "https://cdn.example/sdr.mp4": F.sdr_h264_aac(),
        }
        self.core = _FakeProbeCore(self.table)
        self.addCleanup(self.core.shutdown)

    def _post(self, path, payload):
        return body_of(self.core.handle("POST", path, body=json.dumps(payload).encode()))

    def test_plan_answers_media_playback_and_preview_at_once(self):
        payload = self._post("/media/plan", {"url": "https://cdn.example/hdr10.mkv"})
        self.assertEqual(payload["media"]["video"][0]["hdr"], "HDR10")
        self.assertEqual(payload["playback"]["mode"], "Direct")
        self.assertEqual(payload["preview"]["mode"], "Unsupported")
        self.assertTrue(payload["playback"]["videoCopied"])

    def test_plan_on_an_atmos_source_states_the_audio_conversion(self):
        payload = self._post("/media/plan", {"url": "https://cdn.example/atmos.mkv"})
        self.assertEqual(payload["playback"]["mode"], "DirectWithAudioTranscode")
        self.assertEqual(payload["playback"]["audio"]["target"]["codec"], "ac3")
        self.assertEqual(payload["playback"]["audio"]["target"]["channels"], 6)
        codes = {r["code"] for r in payload["playback"]["audio"]["reasons"]}
        self.assertIn("AUDIO_OBJECT_METADATA_LOST", codes)

    def test_plan_chooses_the_track_by_what_the_player_passes_through(self):
        self.table["https://cdn.example/remux.mkv"] = F.remux_truehd_atmos_with_native_tracks()
        url = "https://cdn.example/remux.mkv"

        def chosen(passthrough):
            body = {"url": url}
            if passthrough is not None:
                body["passthrough"] = passthrough
                body["ac3Encode"] = {"minChannels": 3}
            audio = self._post("/media/plan", body)["playback"]["audio"]
            return audio["action"], audio["track"]["streamIndex"]

        # The board's own profile, and a player that says the same.
        self.assertEqual(chosen(None), ("Passthrough", 2))
        self.assertEqual(chosen(["ac3"]), ("Passthrough", 2))
        # A receiver that takes TrueHD is given the Atmos track as it is.
        self.assertEqual(chosen(["ac3", "eac3", "truehd"]), ("Passthrough", 1))
        # Nothing passed through: the film's fullest mix, encoded by the player.
        self.assertEqual(chosen([]), ("TranscodeToAC3", 1))

    def test_a_format_the_sound_setting_turned_off_is_never_passed_through(self):
        # The Plus's television declares E-AC-3 and DTS; its sound setting
        # passes AC-3 alone, on purpose. The plan keeps to the setting.
        self.table.update(
            {
                "https://cdn.example/ddp.mkv": F.media(
                    [F.video_stream(), F.audio_stream(index=1, codec_name="eac3")]
                ),
                "https://cdn.example/dts.mkv": F.media(
                    [F.video_stream(), F.audio_stream(index=1, codec_name="dts")]
                ),
                "https://cdn.example/ddp-and-ac3.mkv": F.media(
                    [
                        F.video_stream(),
                        F.audio_stream(index=1, codec_name="eac3", default=1),
                        F.audio_stream(index=2, codec_name="ac3", default=0),
                    ]
                ),
            }
        )

        def chosen(name, passthrough):
            body = {
                "url": f"https://cdn.example/{name}.mkv",
                "passthrough": passthrough,
                "ac3Encode": {"minChannels": 3},
            }
            audio = self._post("/media/plan", body)["playback"]["audio"]
            return audio["action"], audio["track"]["codec"]

        only_ac3 = ["ac3"]
        self.assertEqual(chosen("ddp", only_ac3), ("TranscodeToAC3", "eac3"))
        self.assertEqual(chosen("dts", only_ac3), ("TranscodeToAC3", "dts"))
        self.assertEqual(chosen("ddp-and-ac3", only_ac3), ("Passthrough", "ac3"))
        # Turned back on, they pass through as they are.
        all_three = ["ac3", "eac3", "dts"]
        self.assertEqual(chosen("ddp", all_three), ("Passthrough", "eac3"))
        self.assertEqual(chosen("dts", all_three), ("Passthrough", "dts"))
        self.assertEqual(chosen("ddp-and-ac3", all_three), ("Passthrough", "eac3"))

    def test_the_plan_says_what_the_player_does_with_the_sound(self):
        self.table.update(
            {
                "https://cdn.example/tenebrae.mkv": F.remux_with_a_flagged_commentary(),
                "https://cdn.example/remux.mkv": F.remux_truehd_atmos_with_native_tracks(),
            }
        )

        def audio(name, **player):
            body = {"url": f"https://cdn.example/{name}.mkv", "passthrough": ["ac3"], "language": "en", **player}
            return self._post("/media/plan", body)["playback"]["audio"]

        on = {"ac3Encode": {"minChannels": 3}}
        # Tenebrae: stereo DTS is decoded to PCM, never "converted to AC-3".
        tenebrae = audio("tenebrae", **on)
        self.assertEqual((tenebrae["action"], tenebrae["track"]["streamIndex"]), ("DecodeToPCM", 1))
        self.assertIsNone(tenebrae["target"])
        # 7.1 TrueHD with the encoder on: AC-3 5.1 at lavcac3enc's own 448 kbit/s.
        self.table["https://cdn.example/truehd.mkv"] = F.truehd_with_only_a_stereo_commentary_in_ac3()
        truehd = audio("truehd", **on)
        self.assertEqual(truehd["action"], "TranscodeToAC3")
        self.assertEqual((truehd["target"]["channels"], truehd["target"]["bitRate"]), (6, 448_000))
        # ... at the rate the setting names, when it names one.
        named = audio("truehd", ac3Encode={"minChannels": 3, "kbps": 640})
        self.assertEqual(named["target"]["bitRate"], 640_000)
        # The encoder off: PCM, whatever the channels.
        self.assertEqual(audio("truehd", ac3Encode=None)["action"], "DecodeToPCM")
        # The board's own profile, for a caller that is not the player.
        board = self._post("/media/plan", {"url": "https://cdn.example/truehd.mkv"})["playback"]["audio"]
        self.assertEqual(board["target"]["bitRate"], 640_000)
        for bad in ({"minChannels": 1}, {"minChannels": 3, "kbps": 900}, "on"):
            response = self.core.handle(
                "POST",
                "/media/plan",
                body=json.dumps({"url": "https://cdn.example/truehd.mkv", "passthrough": [], "ac3Encode": bad}).encode(),
            )
            self.assertEqual(response.status, 400, bad)

    def test_a_malformed_passthrough_list_is_refused(self):
        response = self.core.handle(
            "POST",
            "/media/plan",
            body=json.dumps({"url": "https://cdn.example/hdr10.mkv", "passthrough": "ac3"}).encode(),
        )
        self.assertEqual(response.status, 400)

    def test_the_clients_fixture_is_still_what_plan_answers(self):
        # mediabox-tv and mediabox-ui parse this file in their own tests; a
        # plan they could not read ("invalid type: map, expected a string")
        # once reached the television because nothing tied the two together.
        from pathlib import Path

        fixture = Path(__file__).with_name("data") / "plan-truehd-atmos.json"
        payload = self._post("/media/plan", {"url": "https://cdn.example/atmos.mkv"})
        self.assertEqual(payload, json.loads(fixture.read_text(encoding="utf-8")))

    def test_rank_puts_the_safe_hdr10_source_ahead_of_the_dolby_vision_one(self):
        payload = self._post(
            "/media/rank",
            {"sources": ["https://cdn.example/dv5.mkv", "https://cdn.example/hdr10.mkv"]},
        )
        self.assertEqual(payload["best"]["identity"], "https://cdn.example/hdr10.mkv")
        self.assertEqual(payload["ranked"][-1]["tierName"], "RISKY")

    def test_a_source_that_cannot_be_probed_is_reported_not_dropped(self):
        payload = self._post(
            "/media/rank",
            {"sources": ["https://cdn.example/hdr10.mkv", "https://cdn.example/missing.mkv"]},
        )
        self.assertEqual(len(payload["ranked"]), 1)
        self.assertEqual(len(payload["unprobed"]), 1)
        self.assertEqual(payload["unprobed"][0]["error"]["code"], "PROBE_FAILED")

    def test_a_direct_source_produces_a_session_with_no_process(self):
        payload = self._post("/media/session", {"url": "https://cdn.example/hdr10.mkv"})
        self.assertEqual(payload["mode"], "Direct")
        self.assertIsNone(payload["pid"])
        self.assertEqual(payload["playbackUrl"], "https://cdn.example/hdr10.mkv")

    def test_a_dolby_vision_profile5_source_is_played_when_it_is_chosen(self):
        # Stremio opens it, green and magenta as it then is; the risk is for
        # ranking, not a refusal.
        payload = self._post("/media/session", {"url": "https://cdn.example/dv5.mkv"})
        self.assertEqual(payload["mode"], "Direct")
        self.assertEqual(payload["playbackUrl"], "https://cdn.example/dv5.mkv")

    def test_a_session_carries_everything_a_handoff_needs(self):
        payload = self._post("/media/session", {"url": "https://cdn.example/atmos.mkv"})
        handoff = payload["handoff"]
        self.assertEqual(handoff["sourceIdentity"], "https://cdn.example/atmos.mkv")
        self.assertEqual(handoff["kodiMode"], "DirectWithAudioTranscode")
        self.assertEqual(handoff["previewMode"], "Unsupported")
        self.assertEqual(handoff["selectedTracks"]["video"]["action"], "copy")
        self.assertEqual(handoff["selectedTracks"]["audio"]["target"]["codec"], "ac3")
        self.assertTrue(handoff["playbackUrl"].startswith("http://box:8790/media/session/"))
        self.assertTrue(handoff["reasons"])

    def test_the_kodi_url_is_rewritten_to_loopback_when_one_is_configured(self):
        core = _FakeProbeCore(self.table, loopback_base_url="http://127.0.0.1:8790")
        self.addCleanup(core.shutdown)
        payload = body_of(
            core.handle(
                "POST",
                "/media/session",
                body=json.dumps({"url": "https://cdn.example/atmos.mkv"}).encode(),
            )
        )
        self.assertTrue(
            payload["handoff"]["kodiPlaybackUrl"].startswith("http://127.0.0.1:8790/media/session/")
        )

    def test_a_session_for_a_resolved_stream_descriptor_works(self):
        payload = self._post(
            "/media/session", {"stream": {"url": "https://cdn.example/hdr10.mkv"}}
        )
        self.assertEqual(payload["source"], "https://cdn.example/hdr10.mkv")
        self.assertEqual(self.core.stremio.resolved[0]["url"], "https://cdn.example/hdr10.mkv")

    def test_stopping_a_session_is_reported_and_idempotent(self):
        created = self._post("/media/session", {"url": "https://cdn.example/hdr10.mkv"})
        first = body_of(self.core.handle("DELETE", f"/media/session/{created['sessionId']}"))
        second = body_of(self.core.handle("DELETE", f"/media/session/{created['sessionId']}"))
        self.assertTrue(first["stopped"])
        self.assertFalse(second["stopped"])

    def test_reading_a_direct_session_relays_the_source_over_loopback(self):
        """A direct session used to answer 409 and name the source.

        That was right while Kodi was the only player: it carries its own TLS
        and opens an HTTPS link itself. The player MediaBox owns is built
        against the appliance's Rockchip ffmpeg, which has none and cannot open
        one, so the bytes come through here instead. The range goes upstream
        with the request, because a player that cannot seek is not a player.
        """
        created = self._post("/media/session", {"url": "https://cdn.example/hdr10.mkv"})
        asked = {}

        def fake_relay(url, range_header, timeout=30.0):
            asked["url"] = url
            asked["range"] = range_header
            return 206, [("Content-Type", "video/x-matroska")], iter([b"bytes"])

        with mock.patch("media.api.relay", fake_relay):
            response = self.core.handle(
                "GET",
                f"/media/session/{created['sessionId']}",
                headers={"range": "bytes=100-"},
            )

        self.assertEqual(response.status, 206)
        self.assertEqual(asked["url"], "https://cdn.example/hdr10.mkv")
        self.assertEqual(asked["range"], "bytes=100-")
        self.assertEqual(b"".join(response.stream), b"bytes")


class _Clock:
    """A monotonic clock the test moves by hand."""

    def __init__(self) -> None:
        self.now = 1000.0

    def __call__(self) -> float:
        return self.now


def _upstream(asked: list):
    """A source that answers every range with a body that has not ended."""

    def fake_relay(url, range_header, timeout=30.0):
        asked.append(range_header)
        return 206, [("Content-Type", "video/x-matroska")], iter([b"a", b"b", b"c"])

    return fake_relay


class SessionLifetimeTests(unittest.TestCase):
    """The film that closed itself on a seek, 2026-09-27.

    The appliance's own player read a direct session through this proxy. The
    reaper had measured the session's idleness from the moment it was
    created, because nothing a direct relay did ever touched it, and stopped
    it two minutes into the film. Ten minutes later the viewer seeked, the
    new range got a 404, and the player took the failed read for the end of
    the film:

        20:04:38  media session a306b91d... stopped (idle)
        20:14:25  GET /media/session/a306b91d... 404
                  Seek failed (to 5820343187, size 68)

    A session is kept while the player it was opened for holds it, or while
    anybody is reading it; only one that nobody holds or reads expires.
    """

    def setUp(self):
        from ..proxy.session import SessionManager

        self.clock = _Clock()
        self.core = _FakeProbeCore({"https://cdn.example/hdr10.mkv": F.hdr10_hevc_eac3()})
        self.core.sessions.shutdown()
        self.core.sessions = SessionManager(
            source_policy=self.core.source_policy,
            idle_timeout_seconds=45.0,
            base_url="http://box:8790",
            clock=self.clock,
        )
        self.addCleanup(self.core.shutdown)
        self.asked: list = []
        patcher = mock.patch("media.api.relay", _upstream(self.asked))
        patcher.start()
        self.addCleanup(patcher.stop)

    def _create(self, **extra) -> str:
        response = self.core.handle(
            "POST",
            "/media/session",
            body=json.dumps({"url": "https://cdn.example/hdr10.mkv", **extra}).encode(),
        )
        self.assertEqual(response.status, 201)
        return body_of(response)["sessionId"]

    def _get(self, session_id: str, at: str):
        return self.core.handle(
            "GET", f"/media/session/{session_id}", headers={"range": f"bytes={at}-"}
        )

    def test_a_held_direct_session_still_seeks_after_ten_minutes(self):
        session_id = self._create(owner="player")
        first = self._get(session_id, "0")
        self.assertEqual(first.status, 206)
        next(iter(first.stream))
        # The player's cache is full; it reads nothing for a while.
        first.stream.close()
        self.clock.now += 11 * 60
        self.assertEqual(self.core.sessions.reap_once(), [])
        seek = self._get(session_id, "5820343187")
        self.assertEqual(seek.status, 206)
        self.assertEqual(self.asked[-1], "bytes=5820343187-")
        seek.stream.close()

    def test_a_session_being_read_is_kept_whoever_opened_it(self):
        session_id = self._create()
        reading = self._get(session_id, "0")
        next(iter(reading.stream))
        self.clock.now += 11 * 60
        self.assertEqual(self.core.sessions.reap_once(), [])
        reading.stream.close()
        self.assertEqual(self._get(session_id, "100").status, 206)

    def test_a_released_session_nobody_reads_is_cleaned_up_after_its_ttl(self):
        session_id = self._create(owner="player")
        released = self.core.handle("POST", f"/media/session/{session_id}/release", body=b"{}")
        self.assertEqual(released.status, 200)
        self.clock.now += 45.0 * 4 - 1
        self.assertEqual(self.core.sessions.reap_once(), [])
        self.clock.now += 2
        self.assertEqual(self.core.sessions.reap_once(), [session_id])
        gone = self._get(session_id, "100")
        self.assertEqual(gone.status, 410)
        self.assertEqual(body_of(gone)["error"]["code"], "SESSION_STOPPED")

    def test_no_held_session_is_made_for_a_caller_that_gave_up(self):
        # The Plus, 2026-10-16: the control plane's request timed out at
        # 20:04:55 while ffprobe went on; the session was made at 20:05:12,
        # held by the player under an id nobody had, and never reaped.
        import time

        for owner in ("player", "kodi"):
            late = self.core.handle(
                "POST",
                "/media/session",
                body=json.dumps(
                    {"url": "https://cdn.example/hdr10.mkv", "owner": owner, "answerBy": time.time() - 1}
                ).encode(),
            )
            self.assertEqual(late.status, 504)
            self.assertEqual(body_of(late)["error"]["code"], "CALLER_GAVE_UP")
        self.assertEqual(self.core.sessions.list(), [])
        # In time, or with no deadline at all, the session is made as before
        # (the same source twice is the same session).
        made = self._create(owner="player", answerBy=time.time() + 30)
        self.assertEqual(self._create(owner="kodi"), made)
        self.assertEqual([s["sessionId"] for s in self.core.sessions.list()], [made])

    def test_an_owner_is_a_short_name(self):
        response = self.core.handle(
            "POST",
            "/media/session",
            body=json.dumps({"url": "https://cdn.example/hdr10.mkv", "owner": "x" * 200}).encode(),
        )
        self.assertEqual(response.status, 400)


if __name__ == "__main__":
    unittest.main()
