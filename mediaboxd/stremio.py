"""Stremio streaming-server boundary: reverse proxy, cast target, Kodi handoff.

The browser never talks to the Stremio streaming server or to Kodi directly.
Everything the Stremio web app needs is reachable below one mount point on the
mediaboxd origin, and exactly two paths below it are answered by mediaboxd
itself instead of being forwarded:

    GET  {mount}casting                      upstream list + the MediaBox device
    POST {mount}casting/{device}/player      handed to Kodi, never forwarded

Everything else is forwarded verbatim to the configured loopback upstream. The
upstream is a fixed loopback origin taken from configuration, never from the
request, so this is a bounded reverse proxy and not an open relay.
"""

from __future__ import annotations

import json
import logging
import threading
import time
import urllib.error
import urllib.request
from dataclasses import dataclass
from typing import Any, Iterator
from urllib.parse import urlsplit, urlunsplit

from .config import StremioConfig, validate_upstream
from .errors import APIError, InvalidRequest
from .kodi import validate_media_url


LOG = logging.getLogger(__name__)

#: Bytes moved per read while a media response is relayed to the browser.
STREAM_CHUNK_BYTES = 64 * 1024

#: Request headers worth forwarding upstream. Range is what makes seeking work.
FORWARD_REQUEST_HEADERS = ("range", "if-range", "accept", "accept-encoding", "content-type")

#: Response headers worth forwarding back. Access-Control-* is deliberately
#: dropped: server.js emits CORS headers, and mediaboxd must not relay them.
FORWARD_RESPONSE_HEADERS = (
    "content-type",
    "content-length",
    "content-range",
    "accept-ranges",
    "last-modified",
    "etag",
    "cache-control",
)

#: server.js exposes a general purpose fetch-any-URL relay at /proxy. Forwarding
#: it would turn the mediaboxd mount into an open proxy, so it is refused. No
#: MediaBox flow needs it: torrents and direct addon streams do not go through it.
DENIED_UPSTREAM_PREFIXES = ("proxy/", "proxy")

MAX_CAST_BODY_BYTES = 64 * 1024


@dataclass(slots=True)
class CastSession:
    """The one handoff mediaboxd is currently responsible for.

    Stremio stays the source of truth for what is being watched; this only
    records what was handed to Kodi so the shell can show it and so acceptance
    can prove preview and Kodi received the same source.
    """

    source: str
    kodi_source: str
    resume_seconds: float
    device_id: str
    started_at: float

    def as_dict(self) -> dict[str, Any]:
        return {
            "source": self.source,
            "kodiSource": self.kodi_source,
            "resumeSeconds": self.resume_seconds,
            "deviceId": self.device_id,
            "startedAt": self.started_at,
        }


def _normalise_mount(mount: str) -> str:
    if not mount.startswith("/"):
        mount = "/" + mount
    if not mount.endswith("/"):
        mount = mount + "/"
    return mount


class StremioBridge:
    """Reverse proxy plus the MediaBox cast device that hands streams to Kodi."""

    def __init__(self, config: StremioConfig, kodi: Any, events: Any) -> None:
        self.config = config
        self.mount = _normalise_mount(config.mount)
        self.upstream = validate_upstream(config.upstream)
        self._kodi = kodi
        self._events = events
        self._lock = threading.Lock()
        self._session: CastSession | None = None

    # ---------------------------------------------------------------- routing

    def owns(self, path: str) -> bool:
        return self.config.enabled and (path == self.mount.rstrip("/") or path.startswith(self.mount))

    def upstream_path(self, path: str) -> str:
        """The path below the mount, without a leading slash."""
        if path == self.mount.rstrip("/"):
            return ""
        return path[len(self.mount) :]

    def cast_player_device(self, relative: str) -> str | None:
        """Return the device id when `relative` is a `casting/{id}/player` post."""
        parts = relative.split("/")
        if len(parts) == 3 and parts[0] == "casting" and parts[2] == "player":
            return parts[1]
        return None

    # ---------------------------------------------------------------- casting

    def casting_devices(self) -> list[dict[str, Any]]:
        """Upstream playback devices with the MediaBox device in front.

        The MediaBox entry is typed `external` on purpose. stremio-web renders
        `chromecast` and `tv` devices only inside its desktop shell, while
        `external` devices are offered in a plain browser as well.
        """
        mediabox = {
            "id": self.config.cast_device_id,
            "name": self.config.cast_device_name,
            "type": "external",
        }
        devices: list[dict[str, Any]] = [mediabox]
        try:
            raw = self._upstream_json("casting")
        except APIError:
            # The streaming server may be down; the Kodi target still works for
            # streams that do not need it, so this is not an error for the app.
            LOG.info("Upstream casting list unavailable; serving MediaBox device only")
            return devices
        if isinstance(raw, list):
            for item in raw:
                if not isinstance(item, dict):
                    continue
                if item.get("id") == self.config.cast_device_id:
                    continue
                devices.append(item)
        return devices

    def play_on_device(self, device_id: str, body: dict[str, Any]) -> dict[str, Any]:
        """Hand a Stremio-resolved stream to Kodi.

        `body` is the upstream `{source, time}` contract. `time` is milliseconds.
        """
        if device_id != self.config.cast_device_id:
            raise APIError("UNKNOWN_CAST_TARGET", "Unknown cast target", 404)
        source = body.get("source")
        safe_source = validate_media_url(source)
        resume_seconds = _cast_time_seconds(body.get("time", 0))
        kodi_source = self.to_kodi_source(safe_source)

        # S0-A safety rule: a single torrent engine must not feed a browser
        # preview and Kodi at two different seek positions. The shell stops the
        # preview when it sees this event, so it is published before Player.Open.
        self._publish(
            "cast.handoff",
            {
                "phase": "starting",
                "deviceId": device_id,
                "source": safe_source,
                "kodiSource": kodi_source,
                "resumeSeconds": resume_seconds,
            },
        )
        time.sleep(0.25)

        self._kodi.open(kodi_source, resume_seconds)

        session = CastSession(
            source=safe_source,
            kodi_source=kodi_source,
            resume_seconds=resume_seconds,
            device_id=device_id,
            started_at=time.time(),
        )
        with self._lock:
            self._session = session
        self._publish("cast.handoff", {"phase": "started", **session.as_dict()})
        return session.as_dict()

    def to_kodi_source(self, source: str) -> str:
        """Point Kodi at the streaming server directly instead of at the proxy.

        Preview runs in a browser that can only reach the mediaboxd origin, so
        Stremio resolves stream URLs against the proxy mount. Kodi runs on the
        appliance itself, so it is given the loopback streaming-server origin:
        identical path and query, no proxy in the production media path.
        """
        parsed = urlsplit(source)
        if not parsed.path.startswith(self.mount):
            return source
        upstream = urlsplit(self.upstream)
        return urlunsplit(
            (
                upstream.scheme,
                upstream.netloc,
                "/" + parsed.path[len(self.mount) :],
                parsed.query,
                "",
            )
        )

    def session(self) -> dict[str, Any] | None:
        with self._lock:
            return self._session.as_dict() if self._session else None

    def clear_session(self) -> None:
        with self._lock:
            self._session = None

    def status(self) -> dict[str, Any]:
        """Streaming-server reachability, for Settings → Diagnostics."""
        state: dict[str, Any] = {
            "enabled": self.config.enabled,
            "mount": self.mount,
            "reachable": False,
            "serverVersion": None,
            "castDevice": {
                "id": self.config.cast_device_id,
                "name": self.config.cast_device_name,
                "type": "external",
            },
        }
        try:
            raw = self._upstream_json("settings")
        except APIError:
            return state
        state["reachable"] = True
        if isinstance(raw, dict):
            values = raw.get("values")
            if isinstance(values, dict):
                version = values.get("serverVersion")
                if isinstance(version, str):
                    state["serverVersion"] = version
        return state

    # ------------------------------------------------------------ passthrough

    def forward(
        self,
        method: str,
        relative: str,
        query: str,
        headers: dict[str, str],
        body: bytes | None,
    ) -> tuple[int, list[tuple[str, str]], Iterator[bytes], Any]:
        """Relay one request upstream and return a streaming response."""
        if method not in {"GET", "HEAD", "POST"}:
            raise APIError("METHOD_NOT_ALLOWED", "Method is not allowed on the proxy", 405)
        if relative.startswith(DENIED_UPSTREAM_PREFIXES):
            raise APIError("PROXY_DENIED", "This upstream path is not proxied", 403)
        if any(character in relative or character in query for character in ("\r", "\n", "\x00")):
            raise InvalidRequest("invalid proxy path")

        target = f"{self.upstream}/{relative}"
        if query:
            target = f"{target}?{query}"
        request = urllib.request.Request(target, data=body, method=method)
        for name in FORWARD_REQUEST_HEADERS:
            value = headers.get(name)
            if value is not None:
                request.add_header(name, value)

        try:
            response = urllib.request.urlopen(request, timeout=self.config.stream_timeout_seconds)
        except urllib.error.HTTPError as exc:
            # An upstream 4xx/5xx is a real answer; relay it rather than masking it.
            response = exc
        except (urllib.error.URLError, TimeoutError, OSError) as exc:
            raise APIError(
                "STREAMING_SERVER_UNREACHABLE",
                "The Stremio streaming server is unreachable",
                502,
            ) from exc

        out_headers = [
            (name, response.headers[name])
            for name in FORWARD_RESPONSE_HEADERS
            if response.headers.get(name) is not None
        ]
        out_headers.append(("X-Content-Type-Options", "nosniff"))
        return int(response.status), out_headers, _chunks(response), response

    # ---------------------------------------------------------------- private

    def _upstream_json(self, relative: str) -> Any:
        request = urllib.request.Request(f"{self.upstream}/{relative}", method="GET")
        try:
            with urllib.request.urlopen(
                request, timeout=self.config.request_timeout_seconds
            ) as response:
                payload = response.read(MAX_CAST_BODY_BYTES * 16)
        except (urllib.error.URLError, TimeoutError, OSError) as exc:
            raise APIError(
                "STREAMING_SERVER_UNREACHABLE",
                "The Stremio streaming server is unreachable",
                502,
            ) from exc
        try:
            return json.loads(payload)
        except (json.JSONDecodeError, UnicodeDecodeError) as exc:
            raise APIError(
                "STREAMING_SERVER_INVALID", "The streaming server returned invalid JSON", 502
            ) from exc

    def _publish(self, event: str, payload: dict[str, Any]) -> None:
        if self._events is not None:
            self._events.publish(event, payload)


def _chunks(response: Any) -> Iterator[bytes]:
    try:
        while True:
            block = response.read(STREAM_CHUNK_BYTES)
            if not block:
                return
            yield block
    finally:
        try:
            response.close()
        except Exception:  # pragma: no cover - close is best effort
            pass


def _cast_time_seconds(value: Any) -> float:
    """Upstream sends the resume position in milliseconds."""
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise InvalidRequest("time must be a number of milliseconds")
    seconds = float(value) / 1000.0
    if seconds < 0 or seconds > 604800:
        raise InvalidRequest("time must be between 0 and 604800000 milliseconds")
    return seconds
