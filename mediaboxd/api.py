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
VERSION = "0.2.0"

#: Types the stdlib map does not reliably carry. WebAssembly streaming
#: instantiation refuses anything but application/wasm.
STATIC_MEDIA_TYPES = {
    ".wasm": "application/wasm",
    ".mjs": "text/javascript",
    ".json": "application/json",
    ".woff2": "font/woff2",
    ".webmanifest": "application/manifest+json",
}

#: Content Security Policy for the media surface.
#:
#: The appliance shell alone would run under `default-src 'self'`, but the media
#: experience is the upstream Stremio web app, and Stremio is by design a client
#: for third-party addons: catalogue metadata, posters and stream descriptors
#: come from hosts that are only known at runtime, from whatever addons the user
#: has installed. So `img-src`/`connect-src`/`media-src` admit https sources,
#: while the parts that decide what executes stay closed: scripts and workers
#: are same-origin only (plus wasm, which stremio-core needs), there is no
#: 'unsafe-eval', no 'unsafe-inline' script, no plugins, and no framing.
#:
#: This is not CORS. mediaboxd still emits no Access-Control-Allow-* header and
#: still rejects OPTIONS, so no other origin can read anything from this one.
MEDIA_APP_CSP = (
    "default-src 'self'; "
    "script-src 'self' 'wasm-unsafe-eval'; "
    "worker-src 'self' blob:; "
    "style-src 'self' 'unsafe-inline'; "
    "font-src 'self' data:; "
    "img-src 'self' data: blob: https:; "
    "media-src 'self' blob: https:; "
    "connect-src 'self' ws: wss: https:; "
    "object-src 'none'; "
    "base-uri 'self'; "
    "form-action 'self'; "
    "frame-ancestors 'none'"
)


@dataclass(slots=True)
class APIContext:
    kodi: Any
    lifecycle: Any
    telemetry: Any
    events: EventBroker
    system_actions: Any
    webui_root: Path | None = None
    stremio: Any = None


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


def _parse_range(header: str | None, size: int) -> tuple[int, int]:
    """Resolve a single byte range against `size`.

    Anything unparseable, multi-range or unsatisfiable falls back to the whole
    entity, which is what a server is allowed to do and what every player
    handles.
    """
    if not header or size == 0 or not header.startswith("bytes="):
        return 0, max(size - 1, 0)
    spec = header[len("bytes=") :].strip()
    if "," in spec or "-" not in spec:
        return 0, max(size - 1, 0)
    first, _, last = spec.partition("-")
    try:
        if not first:
            length = int(last)
            if length <= 0:
                return 0, max(size - 1, 0)
            return max(size - length, 0), size - 1
        start = int(first)
        end = int(last) if last else size - 1
    except ValueError:
        return 0, max(size - 1, 0)
    if start < 0 or start >= size or end < start:
        return 0, max(size - 1, 0)
    return start, min(end, size - 1)


def stremio_response(context: APIContext) -> dict[str, Any]:
    if context.stremio is None:
        return {"enabled": False, "reachable": False}
    return context.stremio.status()


def _cast_session(context: APIContext) -> dict[str, Any] | None:
    return None if context.stremio is None else context.stremio.session()


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
    stremio = context.stremio
    media: dict[str, Any] = {
        "stremio": bool(stremio is not None and stremio.config.enabled),
        "castToKodi": bool(stremio is not None and stremio.config.enabled),
    }
    if stremio is not None:
        media["serverMount"] = stremio.mount
        media["castDeviceId"] = stremio.config.cast_device_id
    return {
        "status": "ok",
        "version": VERSION,
        "actions": actions,
        "media": media,
    }


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
                elif path == "/api/v1/stremio":
                    self._json(HTTPStatus.OK, stremio_response(context))
                elif path == "/api/v1/cast":
                    self._json(HTTPStatus.OK, {"session": _cast_session(context)})
                elif path == "/api/v1/events":
                    self._events()
                elif path == "/":
                    self._redirect("/ui/")
                elif path == "/ui":
                    self._redirect("/ui/")
                elif path.startswith("/ui/"):
                    self._static(path)
                elif context.stremio is not None and context.stremio.owns(path):
                    self._stremio(path)
                else:
                    raise APIError("NOT_FOUND", "Endpoint not found", 404)
            except APIError as exc:
                self._json(exc.status, exc.as_dict())
            except Exception:
                LOG.exception("Unhandled GET failure for %s", path)
                self._json(500, {"error": {"code": "INTERNAL_ERROR", "message": "Internal server error"}})

        def do_HEAD(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
            path = urlsplit(self.path).path
            try:
                if context.stremio is not None and context.stremio.owns(path):
                    self._stremio(path)
                else:
                    raise APIError("NOT_FOUND", "Endpoint not found", 404)
            except APIError as exc:
                self._json(exc.status, exc.as_dict())
            except Exception:
                LOG.exception("Unhandled HEAD failure for %s", path)
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
                if path == "/api/v1/preview/stop":
                    if context.stremio is None:
                        raise APIError("NOT_FOUND", "Endpoint not found", 404)
                    result = context.stremio.stop_preview("client")
                    self._json(HTTPStatus.OK, {"status": "ok", "result": result})
                    return
                if path == "/api/v1/cast/kodi":
                    self._cast_from_shell()
                    return
                if context.stremio is not None and context.stremio.owns(path):
                    self._stremio(path)
                    return
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
            content_type = (
                STATIC_MEDIA_TYPES.get(requested.suffix.lower())
                or mimetypes.guess_type(requested.name)[0]
                or "application/octet-stream"
            )
            try:
                size = requested.stat().st_size
                # A media appliance serves media: without ranges no player can
                # seek, and some refuse to start at all.
                start, end = _parse_range(self.headers.get("Range"), size)
                partial = start != 0 or end != size - 1
                with requested.open("rb") as handle:
                    handle.seek(start)
                    payload = handle.read(end - start + 1)
            except OSError as exc:
                raise APIError("STATIC_READ_ERROR", "Static asset could not be read", 500) from exc
            self.send_response(HTTPStatus.PARTIAL_CONTENT if partial else HTTPStatus.OK)
            self.send_header("Content-Type", f"{content_type}; charset=utf-8" if content_type.startswith("text/") else content_type)
            self.send_header("Content-Length", str(len(payload)))
            self.send_header("Accept-Ranges", "bytes")
            if partial:
                self.send_header("Content-Range", f"bytes {start}-{end}/{size}")
            self.send_header("X-Content-Type-Options", "nosniff")
            self.send_header("Referrer-Policy", "no-referrer")
            self.send_header("Content-Security-Policy", MEDIA_APP_CSP)
            self.send_header(
                "Cache-Control",
                "no-store" if requested.name == "index.html" else "public, max-age=3600",
            )
            self.end_headers()
            self.wfile.write(payload)

        def _cast_from_shell(self) -> None:
            """Hand a stream to Kodi from the MediaBox shell.

            Same handoff as the Stremio cast target, but reachable from the
            appliance shell so it can supply the live preview position that the
            upstream options menu does not pass on.
            """
            if context.stremio is None:
                raise APIError("NOT_FOUND", "Endpoint not found", 404)
            body = self._body()
            device_id = body.get("deviceId", context.stremio.config.cast_device_id)
            if not isinstance(device_id, str):
                raise InvalidRequest("deviceId must be a string")
            result = context.stremio.play_on_device(device_id, body, self.headers.get("Host"))
            self._json(HTTPStatus.OK, {"status": "ok", "result": result})
            context.events.publish("control.action", {"path": "/api/v1/cast/kodi", "status": "ok"})

        def _stremio(self, path: str) -> None:
            bridge = context.stremio
            split = urlsplit(self.path)
            relative = bridge.upstream_path(path)

            if self.command == "POST":
                device_id = bridge.cast_player_device(relative)
                if device_id is not None and device_id == bridge.config.cast_device_id:
                    result = bridge.play_on_device(
                        device_id, self._body(), self.headers.get("Host")
                    )
                    self._json(HTTPStatus.OK, result)
                    return

            if self.command == "GET" and relative == "casting":
                self._json(HTTPStatus.OK, bridge.casting_devices())
                return

            body: bytes | None = None
            if self.command == "POST":
                length_text = self.headers.get("Content-Length", "0")
                try:
                    length = int(length_text)
                except ValueError as exc:
                    raise InvalidRequest("invalid Content-Length") from exc
                if length < 0 or length > MAX_BODY_BYTES:
                    raise InvalidRequest("request body is too large")
                body = self.rfile.read(length) if length else b""

            bridge.note_preview(relative)

            headers = {name.lower(): value for name, value in self.headers.items()}
            status, out_headers, stream, _response = bridge.forward(
                self.command, relative, split.query, headers, body
            )
            self.send_response(status)
            has_length = any(name.lower() == "content-length" for name, _ in out_headers)
            for name, value in out_headers:
                self.send_header(name, value)
            if not has_length:
                self.send_header("Connection", "close")
                self.close_connection = True
            self.end_headers()
            if self.command == "HEAD":
                return
            try:
                for block in stream:
                    self.wfile.write(block)
            except (BrokenPipeError, ConnectionResetError):
                # The browser seeked or closed the tab mid-stream; not an error.
                self.close_connection = True

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
