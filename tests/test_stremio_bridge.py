"""Stremio proxy, cast target and Kodi handoff.

Everything here runs against real localhost HTTP servers: a stand-in streaming
server on one port and mediaboxd on another, so the proxy is exercised over a
real socket rather than through a mocked transport.
"""

from __future__ import annotations

import http.client
import json
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from mediaboxd.api import APIContext, MediaBoxHTTPServer, handler_factory
from mediaboxd.config import Config, StremioConfig, load_config, validate_upstream
from mediaboxd.errors import APIError, InvalidRequest
from mediaboxd.events import EventBroker
from mediaboxd.kodi import validate_media_url
from mediaboxd.lifecycle import SystemActions
from mediaboxd.stremio import StremioBridge

from test_mediaboxd import FakeKodi, FakeLifecycle, FakeTelemetry


UPSTREAM_CASTING = [
    {"id": "chromecast-1", "name": "Living room TV", "type": "chromecast"},
    {"id": "dlna-1", "name": "Bedroom", "type": "tv"},
]

MEDIA_BODY = b"0123456789" * 64


class FakeStreamingServer(BaseHTTPRequestHandler):
    """The handful of server.js endpoints the bridge actually depends on."""

    protocol_version = "HTTP/1.1"

    def log_message(self, *args):  # noqa: D102 - silence the test log
        return

    def _send(self, status, payload, content_type="application/json"):
        body = payload if isinstance(payload, bytes) else json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        # server.js emits CORS headers. The proxy must not relay them.
        self.send_header("Access-Control-Allow-Origin", "*")
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):  # noqa: N802
        self.server.seen.append(("GET", self.path, dict(self.headers)))
        if self.path == "/casting":
            self._send(200, UPSTREAM_CASTING)
        elif self.path == "/settings":
            self._send(200, {"values": {"serverVersion": "4.21.0"}})
        elif self.path.startswith("/stats.json"):
            self._send(200, {"streamLen": len(MEDIA_BODY)})
        elif self.path.startswith("/proxy"):
            self._send(200, {"relayed": True})
        elif self.path.startswith("/abcdef"):
            rng = self.headers.get("Range")
            if rng:
                start = int(rng.split("=")[1].split("-")[0])
                chunk = MEDIA_BODY[start:]
                self.send_response(206)
                self.send_header("Content-Type", "video/mp4")
                self.send_header("Content-Length", str(len(chunk)))
                self.send_header(
                    "Content-Range", f"bytes {start}-{len(MEDIA_BODY) - 1}/{len(MEDIA_BODY)}"
                )
                self.send_header("Accept-Ranges", "bytes")
                self.end_headers()
                self.wfile.write(chunk)
            else:
                self._send(200, MEDIA_BODY, "video/mp4")
        else:
            self._send(404, {"error": "not found"})

    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0"))
        raw = self.rfile.read(length) if length else b""
        self.server.seen.append(("POST", self.path, raw))
        self._send(200, {"upstreamPlayed": True})


class BridgeTestCase(unittest.TestCase):
    def setUp(self):
        self.upstream = ThreadingHTTPServer(("127.0.0.1", 0), FakeStreamingServer)
        self.upstream.seen = []
        self.upstream_thread = threading.Thread(
            target=self.upstream.serve_forever, daemon=True
        )
        self.upstream_thread.start()
        upstream_url = f"http://127.0.0.1:{self.upstream.server_address[1]}"

        self.kodi = FakeKodi()
        self.events = EventBroker()
        self.bridge = StremioBridge(
            StremioConfig(upstream=upstream_url), self.kodi, self.events
        )
        context = APIContext(
            kodi=self.kodi,
            lifecycle=FakeLifecycle(),
            telemetry=FakeTelemetry(),
            events=self.events,
            system_actions=SystemActions(Config()),
            webui_root=None,
            stremio=self.bridge,
        )
        self.server = MediaBoxHTTPServer(("127.0.0.1", 0), handler_factory(context))
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.port = self.server.server_address[1]

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)
        self.upstream.shutdown()
        self.upstream.server_close()
        self.upstream_thread.join(timeout=2)

    def request(self, method, path, body=None, headers=None):
        connection = http.client.HTTPConnection("127.0.0.1", self.port, timeout=5)
        encoded = None if body is None else json.dumps(body).encode()
        request_headers = dict(headers or {})
        if body is not None:
            request_headers.setdefault("Content-Type", "application/json")
        connection.request(method, path, body=encoded, headers=request_headers)
        response = connection.getresponse()
        payload = response.read()
        status = response.status
        # Header names arrive as sent; compare them case-insensitively.
        result_headers = {name.lower(): value for name, value in response.getheaders()}
        connection.close()
        return status, result_headers, payload

    def json_request(self, method, path, body=None, headers=None):
        status, result_headers, payload = self.request(method, path, body, headers)
        return status, result_headers, json.loads(payload)


class CastingListTest(BridgeTestCase):
    def test_mediabox_device_is_offered_as_external(self):
        """`external` is the only device type stremio-web shows in a browser."""
        status, _headers, payload = self.json_request("GET", "/server/casting")
        self.assertEqual(status, 200)
        self.assertEqual(payload[0]["id"], "mediabox-tv")
        self.assertEqual(payload[0]["type"], "external")

    def test_upstream_devices_are_preserved(self):
        _status, _headers, payload = self.json_request("GET", "/server/casting")
        ids = [item["id"] for item in payload]
        self.assertEqual(ids, ["mediabox-tv", "chromecast-1", "dlna-1"])

    def test_device_list_survives_a_dead_streaming_server(self):
        self.upstream.shutdown()
        self.upstream.server_close()
        _status, _headers, payload = self.json_request("GET", "/server/casting")
        self.assertEqual([item["id"] for item in payload], ["mediabox-tv"])


class KodiHandoffTest(BridgeTestCase):
    def play(self, body, device="mediabox-tv"):
        return self.json_request("POST", f"/server/casting/{device}/player", body)

    def test_handoff_opens_the_stream_in_kodi(self):
        source = f"http://127.0.0.1:{self.port}/server/abcdef/0?tr=udp%3A%2F%2Ftr"
        status, _headers, payload = self.play({"source": source, "time": 0})
        self.assertEqual(status, 200)
        self.assertEqual([call[0] for call in self.kodi.calls], ["open"])
        self.assertEqual(payload["source"], source)

    def test_kodi_receives_the_same_source_with_a_loopback_origin(self):
        """Same path and query; only the origin differs.

        The browser can only reach the mediaboxd origin, so Stremio resolves
        stream URLs against the proxy mount. Kodi runs on the appliance, so it
        is handed the streaming server directly and mediaboxd stays out of the
        production media path.
        """
        source = f"http://127.0.0.1:{self.port}/server/abcdef/0?tr=udp%3A%2F%2Ftr&f=x"
        _status, _headers, payload = self.play({"source": source, "time": 0})
        opened_url = self.kodi.calls[0][1][0]
        self.assertEqual(opened_url, payload["kodiSource"])

        from urllib.parse import urlsplit

        preview = urlsplit(source)
        handed = urlsplit(opened_url)
        self.assertEqual(preview.path.removeprefix("/server"), handed.path)
        self.assertEqual(preview.query, handed.query)
        self.assertEqual(handed.netloc, urlsplit(self.bridge.upstream).netloc)

    def test_position_is_forwarded_as_seconds(self):
        """Upstream sends milliseconds; Kodi's resume takes seconds."""
        source = "https://example.test/movie.mp4"
        self.play({"source": source, "time": 754000})
        _url, resume = self.kodi.calls[0][1]
        self.assertAlmostEqual(resume, 754.0, places=3)

    def test_absent_time_defaults_to_the_start(self):
        self.play({"source": "https://example.test/movie.mp4"})
        self.assertEqual(self.kodi.calls[0][1][1], 0.0)

    def test_direct_addon_stream_is_handed_over_unchanged(self):
        source = "https://cdn.example.test/legal/BigBuckBunny.mp4"
        self.play({"source": source, "time": 0})
        self.assertEqual(self.kodi.calls[0][1][0], source)

    def test_unknown_cast_target_is_not_played_and_not_invented(self):
        status, _headers, payload = self.play(
            {"source": "https://example.test/a.mp4"}, device="does-not-exist"
        )
        # Unknown ids are forwarded to the streaming server, which owns the real
        # Chromecast and DLNA devices. Kodi must not be started for them.
        self.assertEqual(status, 200)
        self.assertEqual(self.kodi.calls, [])
        self.assertTrue(payload["upstreamPlayed"])

    def test_magnet_source_is_refused(self):
        status, _headers, payload = self.play(
            {"source": "magnet:?xt=urn:btih:" + "0" * 40, "time": 0}
        )
        self.assertEqual(status, 400)
        self.assertEqual(payload["error"]["code"], "INVALID_REQUEST")
        self.assertEqual(self.kodi.calls, [])

    def test_missing_source_is_refused(self):
        status, _headers, _payload = self.play({"time": 10})
        self.assertEqual(status, 400)
        self.assertEqual(self.kodi.calls, [])

    def test_header_injection_in_source_is_refused(self):
        status, _headers, _payload = self.play(
            {"source": "http://example.test/a\r\nX-Injected: 1", "time": 0}
        )
        self.assertEqual(status, 400)
        self.assertEqual(self.kodi.calls, [])

    def test_negative_and_non_numeric_time_are_refused(self):
        for bad in (-1, "10", True, None):
            with self.subTest(time=bad):
                status, _headers, _payload = self.play(
                    {"source": "https://example.test/a.mp4", "time": bad}
                )
                self.assertEqual(status, 400)
        self.assertEqual(self.kodi.calls, [])

    def test_handoff_announces_itself_before_opening_kodi(self):
        """The preview must be stopped before a second reader hits the engine."""
        with self.events.subscribe() as subscriber:
            self.play({"source": "https://example.test/a.mp4", "time": 1000})
            first = subscriber.get(timeout=3)
        self.assertEqual(first["type"], "cast.handoff")
        self.assertEqual(first["data"]["phase"], "starting")

    def test_session_is_readable_after_handoff(self):
        source = "https://example.test/a.mp4"
        self.play({"source": source, "time": 5000})
        _status, _headers, payload = self.json_request("GET", "/api/v1/cast")
        self.assertEqual(payload["session"]["source"], source)
        self.assertAlmostEqual(payload["session"]["resumeSeconds"], 5.0)


class ShellCastEndpointTest(BridgeTestCase):
    """The shell's own handoff, which can supply the live preview position."""

    def test_shell_endpoint_hands_over_with_position(self):
        source = "https://example.test/a.mp4"
        status, _headers, payload = self.json_request(
            "POST", "/api/v1/cast/kodi", {"source": source, "time": 12345}
        )
        self.assertEqual(status, 200)
        self.assertEqual(payload["status"], "ok")
        self.assertAlmostEqual(self.kodi.calls[0][1][1], 12.345, places=3)

    def test_shell_endpoint_rejects_a_foreign_device(self):
        status, _headers, _payload = self.json_request(
            "POST",
            "/api/v1/cast/kodi",
            {"source": "https://example.test/a.mp4", "deviceId": "chromecast-1"},
        )
        self.assertEqual(status, 404)
        self.assertEqual(self.kodi.calls, [])


class ProxyTest(BridgeTestCase):
    def test_passthrough_reaches_upstream(self):
        status, headers, payload = self.request("GET", "/server/stats.json?x=1")
        self.assertEqual(status, 200)
        self.assertEqual(json.loads(payload)["streamLen"], len(MEDIA_BODY))
        self.assertIn(("GET", "/stats.json?x=1"), [(m, p) for m, p, _ in self.upstream.seen])

    def test_range_requests_are_forwarded_so_seeking_works(self):
        status, headers, payload = self.request(
            "GET", "/server/abcdef/0", headers={"Range": "bytes=100-"}
        )
        self.assertEqual(status, 206)
        self.assertEqual(payload, MEDIA_BODY[100:])
        self.assertIn("bytes", headers.get("accept-ranges", ""))

    def test_cors_headers_from_the_streaming_server_are_not_relayed(self):
        _status, headers, _payload = self.request("GET", "/server/stats.json")
        self.assertNotIn("access-control-allow-origin", headers)

    def test_open_relay_endpoint_is_refused(self):
        """server.js /proxy fetches any URL; proxying it would relay anything."""
        status, _headers, payload = self.json_request(
            "GET", "/server/proxy/d=https%3A%2F%2Fexample.test"
        )
        self.assertEqual(status, 403)
        self.assertEqual(payload["error"]["code"], "PROXY_DENIED")
        self.assertEqual(self.upstream.seen, [])

    def test_unreachable_streaming_server_is_reported_as_such(self):
        self.upstream.shutdown()
        self.upstream.server_close()
        status, _headers, payload = self.json_request("GET", "/server/stats.json")
        self.assertEqual(status, 502)
        self.assertEqual(payload["error"]["code"], "STREAMING_SERVER_UNREACHABLE")

    def test_method_outside_the_allowlist_is_refused(self):
        status, _headers, _payload = self.request("DELETE", "/server/stats.json")
        self.assertIn(status, (404, 405, 501))

    def test_api_paths_are_never_owned_by_the_proxy(self):
        for path in ("/api/v1/health", "/api/v1/kodi", "/ui/index.html"):
            with self.subTest(path=path):
                self.assertFalse(self.bridge.owns(path))


class StatusTest(BridgeTestCase):
    def test_status_reports_the_streaming_server_version(self):
        _status, _headers, payload = self.json_request("GET", "/api/v1/stremio")
        self.assertTrue(payload["reachable"])
        self.assertEqual(payload["serverVersion"], "4.21.0")
        self.assertEqual(payload["castDevice"]["type"], "external")

    def test_health_advertises_the_media_capabilities(self):
        _status, _headers, payload = self.json_request("GET", "/api/v1/health")
        self.assertTrue(payload["media"]["stremio"])
        self.assertTrue(payload["media"]["castToKodi"])
        self.assertEqual(payload["media"]["serverMount"], "/server/")

    def test_status_survives_a_dead_streaming_server(self):
        self.upstream.shutdown()
        self.upstream.server_close()
        _status, _headers, payload = self.json_request("GET", "/api/v1/stremio")
        self.assertFalse(payload["reachable"])
        self.assertIsNone(payload["serverVersion"])


class UpstreamValidationTest(unittest.TestCase):
    """The proxy target comes from configuration and must stay loopback."""

    def test_non_loopback_upstream_is_refused(self):
        for value in ("http://10.27.27.25:11470", "http://example.test", "http://0.0.0.0:1"):
            with self.subTest(value=value):
                with self.assertRaises(ValueError):
                    validate_upstream(value)

    def test_upstream_with_a_path_is_refused(self):
        with self.assertRaises(ValueError):
            validate_upstream("http://127.0.0.1:11470/some/path")

    def test_non_http_upstream_is_refused(self):
        with self.assertRaises(ValueError):
            validate_upstream("file:///etc/passwd")

    def test_loopback_origins_are_accepted(self):
        for value in ("http://127.0.0.1:11470", "http://localhost:11470/", "http://[::1]:11470"):
            with self.subTest(value=value):
                self.assertTrue(validate_upstream(value).startswith("http://"))

    def test_mount_cannot_shadow_the_api(self):
        import tempfile
        from pathlib import Path

        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "c.toml"
            path.write_text('[stremio]\nmount = "/api/v1/"\n', encoding="utf-8")
            with self.assertRaises(ValueError):
                load_config(path)


if __name__ == "__main__":
    unittest.main()


class StaticRangeTest(unittest.TestCase):
    """A media appliance serves media, and media is fetched in ranges."""

    def test_range_is_resolved_against_the_entity(self):
        from mediaboxd.api import _parse_range

        self.assertEqual(_parse_range("bytes=0-99", 1000), (0, 99))
        self.assertEqual(_parse_range("bytes=500-", 1000), (500, 999))
        self.assertEqual(_parse_range("bytes=-100", 1000), (900, 999))
        self.assertEqual(_parse_range("bytes=900-5000", 1000), (900, 999))

    def test_unusable_ranges_fall_back_to_the_whole_entity(self):
        from mediaboxd.api import _parse_range

        for header in (None, "", "items=0-1", "bytes=abc", "bytes=0-1,5-6", "bytes=5000-6000"):
            with self.subTest(header=header):
                self.assertEqual(_parse_range(header, 1000), (0, 999))
