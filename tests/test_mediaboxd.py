from __future__ import annotations

import http.client
import json
import subprocess
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from mediaboxd.api import APIContext, MediaBoxHTTPServer, handler_factory
from mediaboxd.config import Config, KodiConfig, load_config
from mediaboxd.errors import APIError, InvalidRequest, KodiUnavailable
from mediaboxd.events import EventBroker
from mediaboxd.kodi import KodiClient, KodiEndpointResolver, validate_media_url
from mediaboxd.lifecycle import KodiLifecycle, SystemActions
from mediaboxd.telemetry import Telemetry


class FakeKodi:
    def __init__(self) -> None:
        self.calls: list[tuple[str, object]] = []
        self.status_payload = {
            "running": True,
            "pid": 42,
            "jsonrpc_reachable": True,
            "active_players": [],
            "current_item": None,
            "speed": None,
            "time": None,
            "total_time": None,
        }

    def status(self):
        return self.status_payload

    def play_pause(self):
        self.calls.append(("playpause", None))
        return {"speed": 1}

    def stop(self):
        self.calls.append(("stop", None))
        return "OK"

    def seek(self, seconds):
        if isinstance(seconds, bool) or not isinstance(seconds, (int, float)) or seconds < 0:
            raise InvalidRequest("seconds must be between 0 and 604800")
        self.calls.append(("seek", seconds))
        return {"percentage": 10}

    def open(self, url, resume_seconds=0):
        validate_media_url(url)
        self.calls.append(("open", (url, resume_seconds)))
        return "OK"


class FakeLifecycle:
    def __init__(self):
        self.calls = []

    def start(self):
        self.calls.append("start")
        return {"changed": True, "running": True, "pid": 42}

    def stop(self):
        self.calls.append("stop")
        return {"changed": True, "running": False, "pid": None}

    def restart(self):
        self.calls.append("restart")
        return {"changed": True, "running": True, "pid": 43}


class FakeTelemetry:
    def system(self):
        return {
            "hostname": "test",
            "kernel": "6.1-test",
            "architecture": "aarch64",
            "uptime_seconds": 123.5,
            "cpu_cores": 8,
            "cpu_temperature_celsius": 55.0,
            "load": {"1m": 1.0, "5m": 2.0, "15m": 3.0},
            "memory": {
                "total_bytes": 1024,
                "used_bytes": 512,
                "available_bytes": 512,
            },
            "root_filesystem": {"total_bytes": 4096, "used_bytes": 1024},
        }

    def network(self):
        return {
            "interfaces": [
                {
                    "name": "eth0",
                    "link_state": "up",
                    "ipv4": ["10.0.0.2"],
                    "wireless": False,
                    "mac": "00:11:22:33:44:55",
                    "link_speed_mbps": 1000,
                }
            ],
            "default_route": {"interface": "eth0", "gateway": "10.0.0.1"},
            "active_wifi_ssid": None,
        }

    def display(self):
        return {
            "drm_card": "/dev/dri/card0",
            "connector": "card0-HDMI-A-1",
            "connected": True,
            "mode": "3840x2160p60",
            "refresh_hz": 60.0,
            "colour": {
                "encoding": "BT2020_YCC",
                "range": "LIMITED",
                "bus_format": "YUV10_1X30",
            },
            "depth_bits_per_component": 10,
            "hdr": {"active": True, "mode": "HDR10", "eotf_tag": 2},
        }


class APITest(unittest.TestCase):
    def setUp(self):
        self.kodi = FakeKodi()
        self.lifecycle = FakeLifecycle()
        self.static_directory = tempfile.TemporaryDirectory()
        static_root = Path(self.static_directory.name)
        (static_root / "assets").mkdir()
        (static_root / "index.html").write_text("<html>MediaBox UI</html>", encoding="utf-8")
        (static_root / "assets/app.js").write_text("globalThis.mediabox=true", encoding="utf-8")
        context = APIContext(
            kodi=self.kodi,
            lifecycle=self.lifecycle,
            telemetry=FakeTelemetry(),
            events=EventBroker(),
            system_actions=SystemActions(Config()),
            webui_root=static_root,
        )
        self.server = MediaBoxHTTPServer(("127.0.0.1", 0), handler_factory(context))
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.port = self.server.server_address[1]

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)
        self.static_directory.cleanup()

    def raw_request(self, method, path, body=None, headers=None):
        connection = http.client.HTTPConnection("127.0.0.1", self.port, timeout=2)
        encoded = None if body is None else json.dumps(body)
        request_headers = headers or {}
        if body is not None:
            request_headers.setdefault("Content-Type", "application/json")
        connection.request(method, path, body=encoded, headers=request_headers)
        response = connection.getresponse()
        payload = response.read()
        status = response.status
        result_headers = dict(response.getheaders())
        connection.close()
        return status, result_headers, payload

    def request(self, method, path, body=None, headers=None):
        status, result_headers, payload = self.raw_request(method, path, body, headers)
        return status, result_headers, json.loads(payload)

    def test_health_endpoint_and_no_cors(self):
        status, headers, payload = self.request("GET", "/api/v1/health")
        self.assertEqual(status, 200)
        self.assertEqual(payload["status"], "ok")
        self.assertEqual(payload["version"], "0.2.0")
        self.assertEqual(
            payload["actions"],
            {
                "kodiStart": True,
                "kodiStop": True,
                "kodiRestart": True,
                "reboot": False,
                "shutdown": False,
                "reason": "Sistem güç işlemleri yapılandırmada devre dışı",
            },
        )
        self.assertNotIn("Access-Control-Allow-Origin", headers)

    def test_system_endpoint(self):
        status, _, payload = self.request("GET", "/api/v1/system")
        self.assertEqual(status, 200)
        self.assertEqual(payload["hostname"], "test")
        self.assertEqual(payload["uptimeSeconds"], 123.5)
        self.assertEqual(payload["cpu"]["loadAverage"], [1.0, 2.0, 3.0])
        self.assertEqual(payload["memory"]["totalBytes"], 1024)
        self.assertEqual(payload["storage"][0]["mountpoint"], "/")

    def test_network_and_display_contract(self):
        status, _, network = self.request("GET", "/api/v1/network")
        self.assertEqual(status, 200)
        self.assertTrue(network["online"])
        self.assertEqual(network["defaultRoute"]["interface"], "eth0")
        self.assertEqual(network["interfaces"][0]["linkSpeedMbps"], 1000)

        status, _, display = self.request("GET", "/api/v1/display")
        self.assertEqual(status, 200)
        self.assertEqual(display["refreshHz"], 60.0)
        self.assertEqual(display["colorDepth"], 10)
        self.assertTrue(display["hdr"]["active"])

    def test_kodi_idle_and_playing_contract(self):
        status, _, idle = self.request("GET", "/api/v1/kodi")
        self.assertEqual(status, 200)
        self.assertEqual(idle, {"serviceActive": True, "state": "idle"})

        self.kodi.status_payload.update(
            {
                "active_players": [{"playerid": 1, "type": "video"}],
                "current_item": {"label": "Film", "type": "movie"},
                "speed": 1,
                "time": {"hours": 0, "minutes": 1, "seconds": 2, "milliseconds": 500},
                "total_time": {"hours": 1, "minutes": 30, "seconds": 0, "milliseconds": 0},
            }
        )
        status, _, playing = self.request("GET", "/api/v1/kodi")
        self.assertEqual(status, 200)
        self.assertEqual(playing["state"], "playing")
        self.assertEqual(playing["player"]["position"], 62.5)
        self.assertEqual(playing["player"]["duration"], 5400.0)

    def test_seek_validation(self):
        status, _, payload = self.request("POST", "/api/v1/kodi/seek", {"seconds": -1})
        self.assertEqual(status, 400)
        self.assertEqual(payload["error"]["code"], "INVALID_REQUEST")

        status, _, _ = self.request("POST", "/api/v1/kodi/seek", {"seconds": 30})
        self.assertEqual(status, 200)
        self.assertIn(("seek", 30), self.kodi.calls)

    def test_playback_and_lifecycle_actions(self):
        self.request("POST", "/api/v1/kodi/playpause")
        self.request("POST", "/api/v1/kodi/stop")
        self.request("POST", "/api/v1/kodi/open", {"url": "file:///media/film.mkv"})
        self.request("POST", "/api/v1/kodi/start")
        self.request("POST", "/api/v1/kodi/stop-service")
        self.request("POST", "/api/v1/kodi/restart")
        self.assertIn(("playpause", None), self.kodi.calls)
        self.assertIn(("stop", None), self.kodi.calls)
        self.assertIn(("open", ("file:///media/film.mkv", 0)), self.kodi.calls)
        self.assertEqual(self.lifecycle.calls, ["start", "stop", "restart"])

    def test_invalid_url_scheme(self):
        status, _, payload = self.request(
            "POST", "/api/v1/kodi/open", {"url": "magnet:?xt=urn:btih:bad"}
        )
        self.assertEqual(status, 400)
        self.assertEqual(payload["error"]["code"], "INVALID_REQUEST")

    def test_system_actions_default_disabled(self):
        status, _, payload = self.request("POST", "/api/v1/system/reboot")
        self.assertEqual(status, 403)
        self.assertEqual(payload["error"]["code"], "SYSTEM_ACTION_DISABLED")

    def test_event_stream_is_live(self):
        connection = http.client.HTTPConnection("127.0.0.1", self.port, timeout=2)
        connection.request("GET", "/api/v1/events")
        response = connection.getresponse()
        self.assertEqual(response.status, 200)
        self.assertEqual(response.getheader("Content-Type"), "text/event-stream; charset=utf-8")
        chunk = response.readline() + response.readline()
        self.assertIn(b'"type":"connected"', chunk)
        self.assertIn(b'"payload":{"status":"ok"}', chunk)
        connection.close()

    def test_static_ui_assets_spa_fallback_and_traversal_rejection(self):
        status, headers, body = self.raw_request("GET", "/ui/")
        self.assertEqual(status, 200)
        self.assertIn(b"MediaBox UI", body)
        self.assertIn("Content-Security-Policy", headers)

        status, headers, body = self.raw_request("GET", "/ui/assets/app.js")
        self.assertEqual(status, 200)
        self.assertIn("javascript", headers["Content-Type"])
        self.assertIn(b"mediabox", body)

        status, _, body = self.raw_request("GET", "/ui/device")
        self.assertEqual(status, 200)
        self.assertIn(b"MediaBox UI", body)

        status, _, payload = self.request("GET", "/ui/%2e%2e/secret")
        self.assertEqual(status, 400)
        self.assertEqual(payload["error"]["code"], "INVALID_REQUEST")

        status, _, payload = self.request("GET", "/api/v1/not-a-route")
        self.assertEqual(status, 404)
        self.assertEqual(payload["error"]["code"], "NOT_FOUND")


class MockKodiHandler(BaseHTTPRequestHandler):
    methods: list[str] = []
    requests: list[dict] = []

    def do_POST(self):  # noqa: N802
        length = int(self.headers["Content-Length"])
        request = json.loads(self.rfile.read(length))
        self.__class__.methods.append(request["method"])
        self.__class__.requests.append(request)
        responses = {
            "JSONRPC.Ping": "pong",
            "Player.GetActivePlayers": [{"playerid": 1, "type": "video"}],
            "Player.GetProperties": {
                "speed": 1,
                "time": {"hours": 0, "minutes": 0, "seconds": 5, "milliseconds": 0},
                "totaltime": {"hours": 1, "minutes": 0, "seconds": 0, "milliseconds": 0},
            },
            "Player.GetItem": {"item": {"label": "Mock movie", "type": "movie"}},
            "Player.Seek": {"percentage": 1.0},
        }
        payload = json.dumps(
            {"jsonrpc": "2.0", "id": request["id"], "result": responses[request["method"]]}
        ).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, _format, *_args):
        pass


class KodiClientTest(unittest.TestCase):
    def setUp(self):
        MockKodiHandler.methods = []
        MockKodiHandler.requests = []
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), MockKodiHandler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)

    def test_jsonrpc_mock_success(self):
        endpoint = f"http://127.0.0.1:{self.server.server_address[1]}/jsonrpc"
        client = KodiClient(KodiConfig(endpoint=endpoint))
        self.assertTrue(client.ping())
        self.assertEqual(client.active_players()[0]["playerid"], 1)
        self.assertEqual(MockKodiHandler.methods, ["JSONRPC.Ping", "Player.GetActivePlayers"])

    def test_status_uses_required_methods(self):
        endpoint = f"http://127.0.0.1:{self.server.server_address[1]}/jsonrpc"
        status = KodiClient(KodiConfig(endpoint=endpoint)).status()
        self.assertTrue(status["jsonrpc_reachable"])
        self.assertEqual(status["current_item"]["label"], "Mock movie")
        self.assertEqual(
            MockKodiHandler.methods,
            [
                "JSONRPC.Ping",
                "Player.GetActivePlayers",
                "Player.GetProperties",
                "Player.GetItem",
            ],
        )

    def test_kodi_unavailable(self):
        closed = ThreadingHTTPServer(("127.0.0.1", 0), MockKodiHandler)
        port = closed.server_address[1]
        closed.server_close()
        client = KodiClient(
            KodiConfig(endpoint=f"http://127.0.0.1:{port}/jsonrpc", request_timeout_seconds=0.1)
        )
        with self.assertRaises(KodiUnavailable) as caught:
            client.ping()
        self.assertEqual(caught.exception.code, "KODI_UNREACHABLE")

    def test_seek_uses_kodi_time_wrapper(self):
        endpoint = f"http://127.0.0.1:{self.server.server_address[1]}/jsonrpc"
        KodiClient(KodiConfig(endpoint=endpoint)).seek(30.25)
        seek_request = MockKodiHandler.requests[-1]
        self.assertEqual(seek_request["method"], "Player.Seek")
        self.assertEqual(
            seek_request["params"]["value"],
            {
                "time": {
                    "hours": 0,
                    "minutes": 0,
                    "seconds": 30,
                    "milliseconds": 250,
                }
            },
        )

    def test_endpoint_discovery_from_xml(self):
        with tempfile.TemporaryDirectory() as directory:
            settings = Path(directory) / "guisettings.xml"
            settings.write_text(
                "<settings><setting id='services.webserver'>true</setting>"
                "<setting id='services.webserverport'>9123</setting>"
                "<setting id='services.webserverssl'>false</setting></settings>",
                encoding="utf-8",
            )
            resolver = KodiEndpointResolver(KodiConfig(settings_paths=(str(settings),)))
            self.assertEqual(resolver.resolve(), "http://127.0.0.1:9123/jsonrpc")


class TelemetryTest(unittest.TestCase):
    def test_basic_system_parser(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            proc = root / "proc"
            sys = root / "sys"
            zone = sys / "class/thermal/thermal_zone0"
            proc.mkdir()
            zone.mkdir(parents=True)
            (proc / "uptime").write_text("123.50 999.0\n", encoding="utf-8")
            (proc / "loadavg").write_text("1.00 2.00 3.00 1/10 1\n", encoding="utf-8")
            (proc / "meminfo").write_text(
                "MemTotal:       1000 kB\nMemAvailable:    400 kB\n", encoding="utf-8"
            )
            (zone / "type").write_text("soc-thermal\n", encoding="utf-8")
            (zone / "temp").write_text("55000\n", encoding="utf-8")
            result = Telemetry(proc_root=proc, sys_root=sys, root_path=root).system()
            self.assertEqual(result["uptime_seconds"], 123.5)
            self.assertEqual(result["load"]["15m"], 3.0)
            self.assertEqual(result["memory"]["used_bytes"], 600 * 1024)
            self.assertEqual(result["cpu_temperature_celsius"], 55.0)

    def test_network_parser_uses_argv(self):
        calls = []

        def runner(argv, **_kwargs):
            calls.append(argv)
            if "address" in argv:
                value = [{"ifname": "eth0", "addr_info": [{"family": "inet", "local": "10.0.0.2"}]}]
            else:
                value = [{"dev": "eth0", "gateway": "10.0.0.1", "prefsrc": "10.0.0.2"}]
            return subprocess.CompletedProcess(argv, 0, stdout=json.dumps(value), stderr="")

        with tempfile.TemporaryDirectory() as directory:
            network = Path(directory) / "class/net/eth0"
            network.mkdir(parents=True)
            (network / "operstate").write_text("up\n", encoding="utf-8")
            result = Telemetry(sys_root=Path(directory), runner=runner).network()
        self.assertEqual(result["interfaces"][0]["ipv4"], ["10.0.0.2"])
        self.assertEqual(result["default_route"]["gateway"], "10.0.0.1")
        self.assertTrue(all(isinstance(call, list) for call in calls))

    def test_network_parser_falls_back_without_netlink(self):
        def runner(argv, **_kwargs):
            raise PermissionError("AF_NETLINK blocked")

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            network = root / "sys/class/net/eth0"
            route = root / "proc/net"
            network.mkdir(parents=True)
            route.mkdir(parents=True)
            (network / "operstate").write_text("up\n", encoding="utf-8")
            (network / "address").write_text("00:11:22:33:44:55\n", encoding="utf-8")
            (network / "speed").write_text("1000\n", encoding="utf-8")
            (route / "route").write_text(
                "Iface Destination Gateway Flags RefCnt Use Metric Mask MTU Window IRTT\n"
                "eth0 00000000 0100000A 0003 0 0 100 00000000 0 0 0\n",
                encoding="utf-8",
            )
            result = Telemetry(
                proc_root=root / "proc", sys_root=root / "sys", runner=runner
            ).network()
        self.assertEqual(result["default_route"]["interface"], "eth0")
        self.assertEqual(result["default_route"]["gateway"], "10.0.0.1")
        self.assertEqual(result["interfaces"][0]["link_speed_mbps"], 1000)

    def test_display_parser_uses_vendor_summary(self):
        with tempfile.TemporaryDirectory() as directory:
            sys = Path(directory)
            connector = sys / "class/drm/card0-HDMI-A-1"
            debug = sys / "kernel/debug/dri/0"
            connector.mkdir(parents=True)
            debug.mkdir(parents=True)
            (connector / "status").write_text("connected\n", encoding="utf-8")
            (connector / "modes").write_text("3840x2160\n", encoding="utf-8")
            (debug / "summary").write_text(
                "Video Port0: ACTIVE\n"
                "  bus_format[2025]: YUYV10_1X20\n"
                "  overlay_mode[0] output_mode[f] HDR10[2] "
                "color-encoding[BT.2020] color-range[Limited]\n"
                "  Display mode: 3840x2160p23.98\n",
                encoding="utf-8",
            )
            result = Telemetry(sys_root=sys).display()
        self.assertEqual(result["mode"], "3840x2160p23.98")
        self.assertEqual(result["depth_bits_per_component"], 10)
        self.assertEqual(result["colour"]["encoding"], "BT.2020")
        self.assertEqual(result["hdr"], {"active": True, "mode": "HDR10", "eotf_tag": 2})


class SecurityTest(unittest.TestCase):
    def test_lan_bind_requires_explicit_opt_in(self):
        with tempfile.NamedTemporaryFile("w", suffix=".toml") as config:
            config.write('bind_address = "0.0.0.0"\n')
            config.flush()
            with self.assertRaises(ValueError):
                load_config(config.name)

    def test_lifecycle_arguments_do_not_invoke_shell(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            marker = root / "INJECTED"
            config = KodiConfig(
                executable="/bin/echo",
                arguments=(f";touch {marker}",),
                working_directory=str(root),
                home=str(root),
                stdout_path=str(root / "stdout.log"),
                start_timeout_seconds=0.02,
            )
            lifecycle = KodiLifecycle(config, object())
            with self.assertRaises(APIError):
                lifecycle.start()
            self.assertFalse(marker.exists())
            self.assertIn(f";touch {marker}", (root / "stdout.log").read_text())


if __name__ == "__main__":
    unittest.main()
