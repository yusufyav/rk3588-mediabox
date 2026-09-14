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

    def test_a_dolby_vision_profile5_source_cannot_be_made_into_a_session(self):
        response = self.core.handle(
            "POST", "/media/session", body=json.dumps({"url": "https://cdn.example/dv5.mkv"}).encode()
        )
        self.assertEqual(response.status, 409)
        self.assertEqual(body_of(response)["error"]["code"], "SOURCE_NOT_PREFERRED")

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


if __name__ == "__main__":
    unittest.main()
