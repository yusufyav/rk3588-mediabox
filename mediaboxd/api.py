"""Versioned HTTP API and server-sent event transport."""

from __future__ import annotations

import json
import logging
import mimetypes
import os
import queue
from dataclasses import dataclass
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path, PurePosixPath
from typing import Any, Callable
from urllib.parse import unquote, urlsplit

from .errors import APIError, InvalidRequest
from .events import EventBroker


LOG = logging.getLogger(__name__)
MAX_BODY_BYTES = 64 * 1024
VERSION = "0.1.0"


@dataclass(slots=True)
class APIContext:
    kodi: Any
    lifecycle: Any
    telemetry: Any
    events: EventBroker
    system_actions: Any
    webui_root: Path | None = None


def _kodi_time_seconds(value: Any) -> float | None:
    if not isinstance(value, dict):
        return None
    parts = ("hours", "minutes", "seconds", "milliseconds")
    if any(isinstance(value.get(part, 0), bool) or not isinstance(value.get(part, 0), (int, float)) for part in parts):
        return None
    return (
        float(value.get("hours", 0)) * 3600
        + float(value.get("minutes", 0)) * 60
        + float(value.get("seconds", 0))
        + float(value.get("milliseconds", 0)) / 1000
    )


def kodi_response(raw: dict[str, Any]) -> dict[str, Any]:
    """Map the Kodi implementation state to the stable browser wire contract."""
    running = bool(raw.get("running"))
    reachable = bool(raw.get("jsonrpc_reachable"))
    players = raw.get("active_players")
    has_player = isinstance(players, list) and bool(players)
    if not running or not reachable:
        state = "offline"
    elif not has_player:
        state = "idle"
    elif raw.get("speed") == 0:
        state = "paused"
    else:
        state = "playing"

    response: dict[str, Any] = {"serviceActive": running, "state": state}
    if has_player:
        item = raw.get("current_item") if isinstance(raw.get("current_item"), dict) else {}
        player: dict[str, Any] = {}
        title = item.get("label") or item.get("title")
        if isinstance(title, str) and title:
            player["title"] = title
        media_type = item.get("type")
        if isinstance(media_type, str) and media_type:
            player["mediaType"] = media_type
        position = _kodi_time_seconds(raw.get("time"))
        duration = _kodi_time_seconds(raw.get("total_time"))
        if position is not None:
            player["position"] = position
        if duration is not None:
            player["duration"] = duration
        response["player"] = player
    return response


def system_response(raw: dict[str, Any]) -> dict[str, Any]:
    load = raw.get("load") if isinstance(raw.get("load"), dict) else {}
    memory = raw.get("memory") if isinstance(raw.get("memory"), dict) else {}
    filesystem = (
        raw.get("root_filesystem") if isinstance(raw.get("root_filesystem"), dict) else {}
    )
    return {
        "hostname": raw.get("hostname"),
        "kernel": raw.get("kernel"),
        "architecture": raw.get("architecture"),
        "uptimeSeconds": raw.get("uptime_seconds"),
        "cpu": {
            "temperatureC": raw.get("cpu_temperature_celsius"),
            "loadAverage": [load.get("1m"), load.get("5m"), load.get("15m")],
            "cores": raw.get("cpu_cores") or os.cpu_count(),
        },
        "memory": {
            "totalBytes": memory.get("total_bytes"),
            "usedBytes": memory.get("used_bytes"),
            "availableBytes": memory.get("available_bytes"),
        },
        "storage": [
            {
                "mountpoint": "/",
                "totalBytes": filesystem.get("total_bytes"),
                "usedBytes": filesystem.get("used_bytes"),
            }
        ],
    }


def network_response(raw: dict[str, Any]) -> dict[str, Any]:
    interfaces = []
    for item in raw.get("interfaces", []):
        if not isinstance(item, dict) or not isinstance(item.get("name"), str):
            continue
        name = item["name"]
        interface_type = "loopback" if name == "lo" else "wifi" if item.get("wireless") else "ethernet"
        interface = {
            "name": name,
            "type": interface_type,
            "up": item.get("link_state") == "up",
            "ipv4": item.get("ipv4", []),
        }
        for source, target in (("mac", "mac"), ("link_speed_mbps", "linkSpeedMbps")):
            if item.get(source) is not None:
                interface[target] = item[source]
        if item.get("wireless") and raw.get("active_wifi_ssid"):
            interface["ssid"] = raw["active_wifi_ssid"]
        interfaces.append(interface)
    route = raw.get("default_route") if isinstance(raw.get("default_route"), dict) else None
    default_route = None
    if route:
        default_route = {"interface": route.get("interface"), "gateway": route.get("gateway")}
    return {
        "online": bool(default_route and any(item.get("up") for item in interfaces)),
        "defaultRoute": default_route,
        "interfaces": interfaces,
    }


def display_response(raw: dict[str, Any]) -> dict[str, Any]:
    colour = raw.get("colour") if isinstance(raw.get("colour"), dict) else {}
    colour_parts = [colour.get("encoding"), colour.get("range"), colour.get("bus_format")]
    colour_text = " · ".join(str(part) for part in colour_parts if part)
    raw_hdr = raw.get("hdr")
    hdr: dict[str, Any] | None = None
    if isinstance(raw_hdr, dict):
        hdr = {"active": bool(raw_hdr.get("active"))}
        eotf = raw_hdr.get("mode")
        if eotf:
            hdr["eotf"] = str(eotf)
    return {
        "connector": raw.get("connector"),
        "connected": bool(raw.get("connected")),
        "mode": raw.get("mode"),
        "refreshHz": raw.get("refresh_hz"),
        "colorDepth": raw.get("depth_bits_per_component"),
        "colorimetry": colour_text or None,
        "hdr": hdr,
    }


def health_response(context: APIContext) -> dict[str, Any]:
    power_enabled = bool(getattr(context.system_actions, "enabled", False))
    actions: dict[str, Any] = {
        "kodiStart": True,
        "kodiStop": True,
        "kodiRestart": True,
        "reboot": power_enabled,
        "shutdown": power_enabled,
    }
    if not power_enabled:
        actions["reason"] = "Sistem güç işlemleri yapılandırmada devre dışı"
    return {"status": "ok", "version": VERSION, "actions": actions}


class MediaBoxHTTPServer(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True


def handler_factory(context: APIContext) -> type[BaseHTTPRequestHandler]:
    class Handler(BaseHTTPRequestHandler):
        server_version = "mediaboxd/0.1"
        sys_version = ""
        protocol_version = "HTTP/1.1"

        def log_message(self, message: str, *args: Any) -> None:
            LOG.info("%s %s", self.address_string(), message % args)

        def do_GET(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
            path = urlsplit(self.path).path
            try:
                if path == "/api/v1/health":
                    self._json(HTTPStatus.OK, health_response(context))
                elif path == "/api/v1/system":
                    self._json(HTTPStatus.OK, system_response(context.telemetry.system()))
                elif path == "/api/v1/network":
                    self._json(HTTPStatus.OK, network_response(context.telemetry.network()))
                elif path == "/api/v1/kodi":
                    self._json(HTTPStatus.OK, kodi_response(context.kodi.status()))
                elif path == "/api/v1/display":
                    self._json(HTTPStatus.OK, display_response(context.telemetry.display()))
                elif path == "/api/v1/events":
                    self._events()
                elif path == "/":
                    self._redirect("/ui/")
                elif path == "/ui":
                    self._redirect("/ui/")
                elif path.startswith("/ui/"):
                    self._static(path)
                else:
                    raise APIError("NOT_FOUND", "Endpoint not found", 404)
            except APIError as exc:
                self._json(exc.status, exc.as_dict())
            except Exception:
                LOG.exception("Unhandled GET failure for %s", path)
                self._json(500, {"error": {"code": "INTERNAL_ERROR", "message": "Internal server error"}})

        def do_POST(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
            path = urlsplit(self.path).path
            try:
                routes: dict[str, Callable[[dict[str, Any]], Any]] = {
                    "/api/v1/kodi/playpause": lambda body: context.kodi.play_pause(),
                    "/api/v1/kodi/stop": lambda body: context.kodi.stop(),
                    "/api/v1/kodi/seek": lambda body: context.kodi.seek(
                        self._required(body, "seconds")
                    ),
                    "/api/v1/kodi/open": lambda body: context.kodi.open(
                        self._required(body, "url"), body.get("resume_seconds", 0)
                    ),
                    "/api/v1/kodi/start": lambda body: context.lifecycle.start(),
                    "/api/v1/kodi/stop-service": lambda body: context.lifecycle.stop(),
                    "/api/v1/kodi/restart": lambda body: context.lifecycle.restart(),
                    "/api/v1/system/reboot": lambda body: context.system_actions.run(
                        "reboot", self.client_address[0]
                    ),
                    "/api/v1/system/shutdown": lambda body: context.system_actions.run(
                        "shutdown", self.client_address[0]
                    ),
                }
                action = routes.get(path)
                if action is None:
                    raise APIError("NOT_FOUND", "Endpoint not found", 404)
                body = self._body()
                result = action(body)
                payload = {"status": "ok", "result": result}
                self._json(HTTPStatus.OK, payload)
                context.events.publish(
                    "control.action",
                    {"path": path, "status": "ok"},
                )
            except APIError as exc:
                self._json(exc.status, exc.as_dict())
            except Exception:
                LOG.exception("Unhandled POST failure for %s", path)
                self._json(500, {"error": {"code": "INTERNAL_ERROR", "message": "Internal server error"}})

        def do_OPTIONS(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
            # Deliberately no CORS negotiation. Browsers use the same origin.
            self._json(
                HTTPStatus.METHOD_NOT_ALLOWED,
                {"error": {"code": "METHOD_NOT_ALLOWED", "message": "CORS is disabled"}},
            )

        @staticmethod
        def _required(body: dict[str, Any], key: str) -> Any:
            if key not in body:
                raise InvalidRequest(f"missing required field: {key}")
            return body[key]

        def _body(self) -> dict[str, Any]:
            length_text = self.headers.get("Content-Length", "0")
            try:
                length = int(length_text)
            except ValueError as exc:
                raise InvalidRequest("invalid Content-Length") from exc
            if length < 0 or length > MAX_BODY_BYTES:
                raise InvalidRequest("request body is too large")
            if length == 0:
                return {}
            content_type = self.headers.get("Content-Type", "").split(";", 1)[0].strip().lower()
            if content_type != "application/json":
                raise InvalidRequest("Content-Type must be application/json")
            try:
                document = json.loads(self.rfile.read(length))
            except (json.JSONDecodeError, UnicodeDecodeError) as exc:
                raise InvalidRequest("request body is not valid JSON") from exc
            if not isinstance(document, dict):
                raise InvalidRequest("request body must be a JSON object")
            return document

        def _json(self, status: int | HTTPStatus, payload: Any) -> None:
            encoded = json.dumps(payload, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
            self.send_response(int(status))
            self.send_header("Content-Type", "application/json; charset=utf-8")
            self.send_header("Content-Length", str(len(encoded)))
            self.send_header("Cache-Control", "no-store")
            self.send_header("X-Content-Type-Options", "nosniff")
            self.send_header("Referrer-Policy", "no-referrer")
            self.end_headers()
            self.wfile.write(encoded)

        def _redirect(self, location: str) -> None:
            self.send_response(HTTPStatus.PERMANENT_REDIRECT)
            self.send_header("Location", location)
            self.send_header("Content-Length", "0")
            self.send_header("Cache-Control", "no-store")
            self.end_headers()

        def _static(self, request_path: str) -> None:
            root = context.webui_root
            if root is None:
                raise APIError("NOT_FOUND", "Web UI is not installed", 404)
            decoded = unquote(request_path)
            if "\x00" in decoded:
                raise InvalidRequest("invalid static path")
            relative_text = decoded.removeprefix("/ui/")
            relative = PurePosixPath(relative_text)
            if relative.is_absolute() or ".." in relative.parts:
                raise InvalidRequest("invalid static path")

            root_path = root.resolve()
            requested = root_path.joinpath(*relative.parts).resolve()
            try:
                requested.relative_to(root_path)
            except ValueError as exc:
                raise InvalidRequest("invalid static path") from exc

            if relative_text == "":
                requested = root_path / "index.html"
            elif not requested.is_file() and relative.suffix == "":
                requested = root_path / "index.html"
            if not requested.is_file():
                raise APIError("NOT_FOUND", "Static asset not found", 404)
            try:
                payload = requested.read_bytes()
            except OSError as exc:
                raise APIError("STATIC_READ_ERROR", "Static asset could not be read", 500) from exc
            content_type = mimetypes.guess_type(requested.name)[0] or "application/octet-stream"
            self.send_response(HTTPStatus.OK)
            self.send_header("Content-Type", f"{content_type}; charset=utf-8" if content_type.startswith("text/") else content_type)
            self.send_header("Content-Length", str(len(payload)))
            self.send_header("X-Content-Type-Options", "nosniff")
            self.send_header("Referrer-Policy", "no-referrer")
            self.send_header(
                "Content-Security-Policy",
                "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; "
                "img-src 'self' data:; connect-src 'self' ws: wss:",
            )
            self.send_header(
                "Cache-Control",
                "no-store" if requested.name == "index.html" else "public, max-age=3600",
            )
            self.end_headers()
            self.wfile.write(payload)

        def _events(self) -> None:
            self.send_response(HTTPStatus.OK)
            self.send_header("Content-Type", "text/event-stream; charset=utf-8")
            self.send_header("Cache-Control", "no-cache, no-transform")
            self.send_header("Connection", "keep-alive")
            self.send_header("X-Accel-Buffering", "no")
            self.end_headers()
            try:
                with context.events.subscribe() as subscriber:
                    connected = {
                        "id": 0,
                        "type": "connected",
                        "data": {"status": "ok"},
                    }
                    self.wfile.write(context.events.encode(connected))
                    self.wfile.flush()
                    while True:
                        try:
                            event = subscriber.get(timeout=15)
                            self.wfile.write(context.events.encode(event))
                        except queue.Empty:
                            self.wfile.write(b": heartbeat\n\n")
                        self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError, TimeoutError):
                return

    return Handler
