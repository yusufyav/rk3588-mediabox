"""Versioned HTTP API and server-sent event transport."""

from __future__ import annotations

import json
import logging
import queue
from dataclasses import dataclass
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any, Callable
from urllib.parse import urlsplit

from .errors import APIError, InvalidRequest
from .events import EventBroker


LOG = logging.getLogger(__name__)
MAX_BODY_BYTES = 64 * 1024


@dataclass(slots=True)
class APIContext:
    kodi: Any
    lifecycle: Any
    telemetry: Any
    events: EventBroker
    system_actions: Any


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
                    self._json(HTTPStatus.OK, {"status": "ok"})
                elif path == "/api/v1/system":
                    self._json(HTTPStatus.OK, context.telemetry.system())
                elif path == "/api/v1/network":
                    self._json(HTTPStatus.OK, context.telemetry.network())
                elif path == "/api/v1/kodi":
                    self._json(HTTPStatus.OK, context.kodi.status())
                elif path == "/api/v1/display":
                    self._json(HTTPStatus.OK, context.telemetry.display())
                elif path == "/api/v1/events":
                    self._events()
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
