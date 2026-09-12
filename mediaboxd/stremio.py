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

#: How long a preview session may go untouched before it is torn down.
#:
#: The browser is asked to close its own session, but a closed tab, a killed
#: browser or a lost network cannot ask for anything. Without a deadline here a
#: transcoder outlives its only viewer and keeps a core busy indefinitely — on
#: this board every hardware encode profile is rejected, so a transcode that
#: nobody is watching is a software encode that nobody is watching.
PREVIEW_IDLE_TIMEOUT_SECONDS = 45.0
PREVIEW_REAPER_INTERVAL_SECONDS = 5.0


@dataclass(slots=True)
class PreviewSession:
    """One transcoding session on the streaming server, and when it was last used."""

    session_id: str
    started_at: float
    last_seen: float

    def as_dict(self) -> dict[str, Any]:
        return {
            "sessionId": self.session_id,
            "startedAt": self.started_at,
            "lastSeen": self.last_seen,
        }


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


#: Paths mediaboxd answers itself. At a root mount everything else belongs to
#: the streaming server, so this list is what separates the two.
#:
#: `/media/` is the V2 media core's own surface. It is reserved here rather
#: than left to the proxy because the media core is not the streaming server:
#: it decides what this appliance can play and owns the sessions that make it
#: playable, and a request for one must never be forwarded to the other.
RESERVED_PREFIXES = ("/api/", "/ui/", "/media/")
RESERVED_EXACT = ("/", "/ui", "/api", "/media")


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
        self._preview: PreviewSession | None = None
        self._reaper: threading.Thread | None = None
        self._stopping = threading.Event()

    # ---------------------------------------------------------------- routing

    @property
    def rooted(self) -> bool:
        """True when the streaming server occupies this origin's root.

        Upstream needs this for transcoding: stremio-video builds its HLS URLs
        as `url.resolve(streamingServerURL, '/hlsv2/…')`, and a leading slash
        resolves against the origin, discarding any subpath. A server behind a
        subpath therefore cannot transcode, so the default is the root and the
        reserved prefixes above are what mediaboxd keeps for itself.
        """
        return self.mount == "/"

    def owns(self, path: str) -> bool:
        if not self.config.enabled:
            return False
        if self.rooted:
            return not (path in RESERVED_EXACT or path.startswith(RESERVED_PREFIXES))
        return path == self.mount.rstrip("/") or path.startswith(self.mount)

    def upstream_path(self, path: str) -> str:
        """The path below the mount, without a leading slash."""
        if self.rooted:
            return path.lstrip("/")
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

    def play_on_device(
        self, device_id: str, body: dict[str, Any], request_host: str | None = None
    ) -> dict[str, Any]:
        """Hand a Stremio-resolved stream to Kodi.

        `body` is the upstream `{source, time}` contract. `time` is milliseconds.
        `request_host` is the authority the browser used, which is what makes it
        possible to tell our own streaming URLs from a third-party addon's.
        """
        if device_id != self.config.cast_device_id:
            raise APIError("UNKNOWN_CAST_TARGET", "Unknown cast target", 404)
        source = body.get("source")
        safe_source = validate_media_url(source)
        resume_seconds = _cast_time_seconds(body.get("time", 0))
        kodi_source = self.to_kodi_source(safe_source, request_host)

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

        # The browser is asked to stop its own player, but the transcoder lives
        # on the appliance and has to be ended here — otherwise the television
        # plays while a software encode nobody is watching keeps a core busy.
        self.stop_preview("handoff")

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

    def to_kodi_source(self, source: str, request_host: str | None = None) -> str:
        """Point Kodi at the streaming server directly instead of at the proxy.

        Preview runs in a browser that can only reach the mediaboxd origin, so
        Stremio resolves stream URLs against the proxy. Kodi runs on the
        appliance itself, so it is given the loopback streaming-server origin:
        identical path and query, no proxy in the production media path.

        Only a URL that came back through this origin is rewritten. A stream an
        addon serves from its own host is handed over untouched — rewriting one
        of those would point Kodi at a path the streaming server never had.
        """
        parsed = urlsplit(source)
        if request_host is not None and parsed.netloc != request_host:
            return source
        if not self.owns(parsed.path):
            return source
        relative = self.upstream_path(parsed.path)
        upstream = urlsplit(self.upstream)
        return urlunsplit(
            (upstream.scheme, upstream.netloc, "/" + relative, parsed.query, "")
        )

    # ---------------------------------------------------------------- preview

    def note_preview(self, relative: str) -> None:
        """Record that a transcoding session is being read.

        Session ids appear in the path as `hlsv2/{id}/…`. Only one preview may
        be alive at a time: starting a second one tears the first down, because
        two transcoders is two busy cores for one person watching one thing.
        """
        parts = relative.split("/")
        if len(parts) < 2 or parts[0] != "hlsv2" or parts[1] in {"probe", ""}:
            return
        session_id = parts[1]
        now = time.time()
        previous: str | None = None
        with self._lock:
            if self._preview is not None and self._preview.session_id != session_id:
                previous = self._preview.session_id
            if self._preview is None or self._preview.session_id != session_id:
                self._preview = PreviewSession(session_id, now, now)
            else:
                self._preview.last_seen = now
        if previous is not None:
            self._destroy_preview(previous)
        self._ensure_reaper()

    def stop_preview(self, reason: str = "requested") -> dict[str, Any]:
        """Tear down the active preview session, if there is one."""
        with self._lock:
            session = self._preview
            self._preview = None
        if session is None:
            return {"stopped": False}
        self._destroy_preview(session.session_id)
        self._publish("preview.stopped", {"sessionId": session.session_id, "reason": reason})
        return {"stopped": True, "sessionId": session.session_id, "reason": reason}

    def preview(self) -> dict[str, Any] | None:
        with self._lock:
            return self._preview.as_dict() if self._preview else None

    def _destroy_preview(self, session_id: str) -> None:
        """Ask the streaming server to end the session and reap its children."""
        request = urllib.request.Request(
            f"{self.upstream}/hlsv2/{session_id}/destroy", method="GET"
        )
        try:
            with urllib.request.urlopen(
                request, timeout=self.config.request_timeout_seconds
            ) as response:
                response.read(1024)
        except (urllib.error.URLError, TimeoutError, OSError) as exc:
            LOG.warning("Preview session %s could not be destroyed: %s", session_id, exc)

    def _ensure_reaper(self) -> None:
        with self._lock:
            if self._reaper is not None and self._reaper.is_alive():
                return
            self._reaper = threading.Thread(
                target=self._reap_loop, name="preview-reaper", daemon=True
            )
            self._reaper.start()

    def _reap_loop(self) -> None:
        while not self._stopping.wait(PREVIEW_REAPER_INTERVAL_SECONDS):
            with self._lock:
                session = self._preview
                idle = session is not None and (
                    time.time() - session.last_seen > PREVIEW_IDLE_TIMEOUT_SECONDS
                )
            if session is None:
                return
            if idle:
                LOG.info("Reaping idle preview session %s", session.session_id)
                self.stop_preview("idle")
                return

    def shutdown(self) -> None:
        self._stopping.set()
        self.stop_preview("shutdown")

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
            "preview": self.preview(),
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
