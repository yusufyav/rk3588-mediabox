"""The MediaBox media API.

This is the only surface anything above the media core talks to. A future
MediaBox interface calls these routes; it never calls Stremio, never sees an
addon transport URL unless it asks for one, and above all never parses a page.

    GET    /media/status                     session, addons, streaming server
    GET    /media/capabilities               the capability profile in force
    GET    /media/home                       catalogue rows
    GET    /media/search?q=                  every searchable catalogue
    GET    /media/catalog/{type}/{id}        one catalogue
    GET    /media/meta/{type}/{id}           one item
    GET    /media/streams/{type}/{id}        ways to watch it
    GET    /media/subtitles/{type}/{id}      subtitle tracks
    POST   /media/resolve                    stream descriptor -> playable URL
    POST   /media/inspect                    URL -> MediaInfo
    POST   /media/plan                       URL -> MediaInfo + decision + preview
    POST   /media/rank                       several sources -> ranked list
    POST   /media/session                    create a playback session
    GET    /media/session                    list sessions
    GET    /media/session/{id}               the session's bytes
    GET    /media/session/{id}/status        one session
    DELETE /media/session/{id}               stop it
    POST   /media/login, /media/logout       optional Stremio account

Route names are the media core's own. Nothing in them leaks the provider.
"""

from __future__ import annotations

import json
import logging
from dataclasses import dataclass, field
from typing import Any, Callable, Iterator
from urllib.parse import parse_qs, unquote

from .errors import InvalidRequest, MediaError, NotFound
from .inspector import FFprobeConfig, MediaInfo, inspect
from .policy import (
    CapabilityProfile,
    PlaybackMode,
    decide,
    decide_preview,
    get_profile,
    rank_sources,
)
from .proxy import FFmpegConfig, SessionManager, SourcePolicy, validate_source_url
from .stremio import HeadlessStremio, ResolvedStream


LOG = logging.getLogger(__name__)

MOUNT = "/media"
MAX_BODY_BYTES = 256 * 1024

#: Sources ranked in one request. Each one costs a probe, so the ceiling is a
#: real protection and not a formality.
MAX_RANK_SOURCES = 12


@dataclass(slots=True)
class Response:
    status: int
    headers: list[tuple[str, str]]
    body: bytes | None = None
    stream: Iterator[bytes] | None = None


def json_response(status: int, payload: Any) -> Response:
    body = json.dumps(payload, ensure_ascii=False).encode("utf-8")
    return Response(
        status,
        [
            ("Content-Type", "application/json; charset=utf-8"),
            ("Content-Length", str(len(body))),
            ("X-Content-Type-Options", "nosniff"),
            ("Cache-Control", "no-store"),
        ],
        body=body,
    )


@dataclass(slots=True)
class MediaCoreConfig:
    streaming_server_url: str = "http://127.0.0.1:11470"
    capability_profile: str | None = None
    session_state_path: str | None = None
    base_url: str = ""
    #: Loopback origin Kodi uses; the session URL handed to the television is
    #: rewritten to it so production playback never leaves the appliance.
    loopback_base_url: str = ""
    ffprobe: FFprobeConfig = field(default_factory=FFprobeConfig)
    ffmpeg: FFmpegConfig = field(default_factory=FFmpegConfig)
    idle_timeout_seconds: float = 45.0
    allowed_file_prefixes: tuple[str, ...] = ()
    torrent_network_status: str = "UNKNOWN"


class MediaCore:
    """Everything the media core exposes, behind one dispatcher."""

    def __init__(self, config: MediaCoreConfig | None = None) -> None:
        self.config = config or MediaCoreConfig()
        self.profile: CapabilityProfile = get_profile(self.config.capability_profile)
        self.stremio = HeadlessStremio(
            streaming_server_url=self.config.streaming_server_url,
            session_path=self.config.session_state_path,
        )
        self.source_policy = SourcePolicy(
            allowed_local_origins=frozenset({self.config.streaming_server_url.rstrip("/")}),
            allowed_file_prefixes=self.config.allowed_file_prefixes,
        )
        self.sessions = SessionManager(
            source_policy=self.source_policy,
            ffmpeg=self.config.ffmpeg,
            idle_timeout_seconds=self.config.idle_timeout_seconds,
            base_url=self.config.base_url,
        )

    # ------------------------------------------------------------------ helpers

    def owns(self, path: str) -> bool:
        return path == MOUNT or path.startswith(MOUNT + "/")

    def shutdown(self) -> None:
        self.sessions.shutdown()

    def inspect_url(self, url: str) -> MediaInfo:
        validate_source_url(url, self.source_policy)
        return inspect(url, self.config.ffprobe)

    def plan(self, url: str, *, preferred_language: str | None = None) -> dict[str, Any]:
        """Everything the appliance knows about one source, in one answer."""
        info = self.inspect_url(url)
        decision = decide(info, self.profile, preferred_language=preferred_language)
        preview = decide_preview(info, self.profile)
        return {
            "media": info.as_dict(),
            "playback": decision.as_dict(),
            "preview": preview.as_dict(),
        }

    # --------------------------------------------------------------- dispatcher

    def handle(
        self,
        method: str,
        path: str,
        query: str = "",
        body: bytes | None = None,
    ) -> Response:
        try:
            return self._route(method, path, query, body)
        except MediaError as exc:
            return json_response(exc.status, exc.as_dict())
        except Exception:
            LOG.exception("Unhandled media-core failure for %s %s", method, path)
            return json_response(
                500, {"error": {"code": "INTERNAL_ERROR", "message": "Internal server error"}}
            )

    def _route(self, method: str, path: str, query: str, body: bytes | None) -> Response:
        if not self.owns(path):
            raise NotFound("not a media-core path")
        relative = path[len(MOUNT) :].strip("/")
        parts = [unquote(part) for part in relative.split("/") if part != ""]
        params = parse_qs(query, keep_blank_values=True)

        if method == "GET":
            return self._get(parts, params)
        if method == "POST":
            return self._post(parts, self._json_body(body))
        if method == "DELETE":
            return self._delete(parts)
        raise MediaError("METHOD_NOT_ALLOWED", "Method is not allowed on the media core", 405)

    # --------------------------------------------------------------------- GET

    def _get(self, parts: list[str], params: dict[str, list[str]]) -> Response:
        if not parts:
            return json_response(200, {"mount": MOUNT, "profile": self.profile.name})

        head = parts[0]

        if head == "status" and len(parts) == 1:
            return json_response(
                200,
                {
                    "available": True,
                    "provider": self.stremio.session_status().as_dict(),
                    "capabilityProfile": self.profile.name,
                    "sessions": len(self.sessions.active()),
                    "torrentNetwork": {
                        "status": self.config.torrent_network_status,
                        "directHttpAvailable": True,
                    },
                },
            )

        if head == "capabilities" and len(parts) == 1:
            return json_response(200, self.profile.as_dict())

        if head == "home" and len(parts) == 1:
            types = tuple(_split_csv(params.get("type", []))) or ("movie", "series")
            rows = self.stremio.home(types=types)
            return json_response(200, {"rows": [row.as_dict() for row in rows]})

        if head == "search" and len(parts) == 1:
            query = _one(params, "q")
            if query is None:
                raise InvalidRequest("q is required")
            types = tuple(_split_csv(params.get("type", []))) or None
            rows = self.stremio.search(query, types=types)
            return json_response(
                200, {"query": query, "rows": [row.as_dict() for row in rows]}
            )

        if head == "catalog" and len(parts) == 3:
            extra = {
                key: values[0]
                for key, values in params.items()
                if key not in {"addon", "limit"} and values
            }
            items = self.stremio.catalog(
                parts[1],
                parts[2],
                addon_id=_one(params, "addon"),
                extra=extra or None,
                limit=_int_param(params, "limit"),
            )
            return json_response(200, {"items": [item.as_dict() for item in items]})

        if head == "meta" and len(parts) == 3:
            return json_response(200, {"meta": self.stremio.meta(parts[1], parts[2]).as_dict()})

        if head == "streams" and len(parts) == 3:
            streams = self.stremio.streams(parts[1], parts[2], _one(params, "videoId"))
            return json_response(
                200,
                {
                    "streams": [stream.as_dict() for stream in streams],
                    "playable": sum(1 for stream in streams if stream.is_playable),
                },
            )

        if head == "subtitles" and len(parts) == 3:
            extra = {key: values[0] for key, values in params.items() if key != "videoId" and values}
            subtitles = self.stremio.subtitles(
                parts[1], parts[2], video_id=_one(params, "videoId"), extra=extra or None
            )
            return json_response(200, {"subtitles": [s.as_dict() for s in subtitles]})

        if head == "session":
            if len(parts) == 1:
                return json_response(200, {"sessions": self.sessions.list()})
            if len(parts) == 2:
                return self._serve_session(parts[1])
            if len(parts) == 3 and parts[2] == "status":
                return json_response(200, self.sessions.get(parts[1]).as_dict())

        raise NotFound("no such media-core endpoint")

    def _serve_session(self, session_id: str) -> Response:
        session = self.sessions.get(session_id)
        if session.mode.value == "Direct":
            # Nothing to relay: the player opens the source itself. Saying so
            # is better than a redirect the caller may not have expected.
            return json_response(
                409,
                {
                    "error": {
                        "code": "SESSION_IS_DIRECT",
                        "message": "this session plays directly from the source",
                        "details": {"playbackUrl": session.playback_url},
                    }
                },
            )
        stream = self.sessions.attach(session_id)
        content_type = (
            "video/x-matroska" if "matroska" in " ".join(session.argv) else "video/mp4"
        )
        return Response(
            200,
            [
                ("Content-Type", content_type),
                ("Cache-Control", "no-store"),
                ("X-Content-Type-Options", "nosniff"),
                # The body is produced by a live muxer; its length is unknown
                # and it cannot be seeked, which is what these two say.
                ("Accept-Ranges", "none"),
                ("Connection", "close"),
            ],
            stream=stream,
        )

    # -------------------------------------------------------------------- POST

    def _post(self, parts: list[str], body: dict[str, Any]) -> Response:
        if not parts:
            raise NotFound("no such media-core endpoint")
        head = parts[0]

        if head == "login" and len(parts) == 1:
            status = self.stremio.login(body.get("email", ""), body.get("password", ""))
            return json_response(200, status.as_dict())

        if head == "logout" and len(parts) == 1:
            return json_response(200, self.stremio.logout().as_dict())

        if head == "resolve" and len(parts) == 1:
            resolved = self._resolve_from_body(body)
            return json_response(200, resolved.as_dict())

        if head == "inspect" and len(parts) == 1:
            url = self._source_from_body(body)
            return json_response(200, {"media": self.inspect_url(url).as_dict()})

        if head == "plan" and len(parts) == 1:
            url = self._source_from_body(body)
            return json_response(
                200, self.plan(url, preferred_language=body.get("language"))
            )

        if head == "rank" and len(parts) == 1:
            return json_response(200, self._rank(body))

        if head == "session" and len(parts) == 1:
            return json_response(201, self._create_session(body))

        if head == "session" and len(parts) == 3 and parts[2] == "stop":
            return json_response(200, self.sessions.stop(parts[1], "requested"))

        raise NotFound("no such media-core endpoint")

    def _delete(self, parts: list[str]) -> Response:
        if len(parts) == 2 and parts[0] == "session":
            return json_response(200, self.sessions.stop(parts[1], "requested"))
        raise NotFound("no such media-core endpoint")

    # ----------------------------------------------------------------- helpers

    @staticmethod
    def _json_body(body: bytes | None) -> dict[str, Any]:
        if not body:
            return {}
        if len(body) > MAX_BODY_BYTES:
            raise InvalidRequest("request body is too large")
        try:
            parsed = json.loads(body)
        except (json.JSONDecodeError, UnicodeDecodeError) as exc:
            raise InvalidRequest("request body must be JSON") from exc
        if not isinstance(parsed, dict):
            raise InvalidRequest("request body must be a JSON object")
        return parsed

    def _resolve_from_body(self, body: dict[str, Any]) -> ResolvedStream:
        stream = body.get("stream")
        if isinstance(stream, dict):
            return self.stremio.resolve_descriptor(stream)
        raise InvalidRequest("resolve needs a stream descriptor")

    def _source_from_body(self, body: dict[str, Any]) -> str:
        url = body.get("url")
        if isinstance(url, str) and url:
            return validate_source_url(url, self.source_policy)
        stream = body.get("stream")
        if isinstance(stream, dict):
            return validate_source_url(
                self.stremio.resolve_descriptor(stream).url, self.source_policy
            )
        raise InvalidRequest("a url or a stream descriptor is required")

    def _rank(self, body: dict[str, Any]) -> dict[str, Any]:
        """Rank several renditions of one title.

        Each entry may be a URL or a stream descriptor. A source that cannot be
        probed is reported rather than silently dropped: "we could not look at
        it" is a different answer from "it is unsupported".
        """
        entries = body.get("sources")
        if not isinstance(entries, list) or not entries:
            raise InvalidRequest("rank needs a non-empty sources array")
        if len(entries) > MAX_RANK_SOURCES:
            raise InvalidRequest(f"at most {MAX_RANK_SOURCES} sources may be ranked at once")

        probed: list[tuple[str, MediaInfo]] = []
        failures: list[dict[str, Any]] = []
        for entry in entries:
            try:
                if isinstance(entry, str):
                    identity, url = entry, entry
                elif isinstance(entry, dict):
                    if isinstance(entry.get("url"), str):
                        url = entry["url"]
                        identity = entry.get("identity") or url
                    elif isinstance(entry.get("stream"), dict):
                        resolved = self.stremio.resolve_descriptor(entry["stream"])
                        url = resolved.url
                        identity = entry.get("identity") or resolved.stream.identity
                    else:
                        raise InvalidRequest("a source needs a url or a stream descriptor")
                else:
                    raise InvalidRequest("a source must be a URL or an object")
                probed.append((identity, self.inspect_url(url)))
            except MediaError as exc:
                failures.append({"source": entry, "error": exc.as_dict()["error"]})

        ranked = rank_sources(
            probed, self.profile, preferred_language=body.get("language")
        )
        return {
            "ranked": [item.as_dict() for item in ranked],
            "best": ranked[0].as_dict() if ranked else None,
            "unprobed": failures,
        }

    def _create_session(self, body: dict[str, Any]) -> dict[str, Any]:
        url = self._source_from_body(body)
        info = self.inspect_url(url)
        decision = decide(info, self.profile, preferred_language=body.get("language"))
        session = self.sessions.create(
            url,
            decision,
            start_seconds=_as_float(body.get("startSeconds")),
        )
        preview = decide_preview(info, self.profile)
        payload = session.as_dict()
        payload["media"] = info.as_dict()
        payload["playback"] = decision.as_dict()
        payload["preview"] = preview.as_dict()
        payload["handoff"] = self._handoff_payload(session, decision, preview, info)
        return payload

    def _handoff_payload(
        self, session: Any, decision: Any, preview: Any, info: MediaInfo
    ) -> dict[str, Any]:
        """Everything a preview -> television handoff needs, and nothing more.

        The media core does not drive Kodi. It states the source identity, the
        URL to open, which tracks were chosen and why, and which modes apply on
        each side; whoever owns Kodi does the opening.
        """
        kodi_url = session.playback_url
        if (
            self.config.loopback_base_url
            and self.config.base_url
            and kodi_url
            and kodi_url.startswith(self.config.base_url)
        ):
            kodi_url = self.config.loopback_base_url.rstrip("/") + kodi_url[len(self.config.base_url) :]
        return {
            "sourceIdentity": session.source,
            "resolvedInput": info.source_url,
            "playbackUrl": session.playback_url,
            "kodiPlaybackUrl": kodi_url,
            "selectedTracks": session.selected_tracks(),
            "previewMode": preview.mode.value,
            "kodiMode": decision.mode.value,
            "resumeSeconds": 0.0,
            "reasons": [
                reason.as_dict()
                for reason in (*decision.reasons, *decision.video.reasons, *decision.audio.reasons)
            ],
        }


def _one(params: dict[str, list[str]], key: str) -> str | None:
    values = params.get(key)
    return values[0] if values and values[0] != "" else None


def _split_csv(values: list[str]) -> list[str]:
    found: list[str] = []
    for value in values:
        found.extend(part for part in value.split(",") if part)
    return found


def _int_param(params: dict[str, list[str]], key: str) -> int | None:
    value = _one(params, key)
    if value is None:
        return None
    try:
        parsed = int(value)
    except ValueError as exc:
        raise InvalidRequest(f"{key} must be an integer") from exc
    return parsed if parsed > 0 else None


def _as_float(value: Any) -> float | None:
    if value is None:
        return None
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise InvalidRequest("startSeconds must be a number")
    if value < 0 or value > 604800:
        raise InvalidRequest("startSeconds is out of range")
    return float(value)
