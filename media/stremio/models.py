"""Normalized Stremio types.

These are the shapes the rest of MediaBox sees. They are deliberately not the
wire format: an addon may send anything, and a future MediaBox interface must
not have to know which addon it came from, let alone what a Stremio web page
looks like.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import Any


@dataclass(frozen=True, slots=True)
class AddonCatalog:
    type: str
    id: str
    name: str | None = None
    #: Extra properties the catalogue accepts, e.g. `search`, `genre`, `skip`.
    extra_supported: tuple[str, ...] = ()
    extra_required: tuple[str, ...] = ()

    @property
    def supports_search(self) -> bool:
        return "search" in self.extra_supported

    def as_dict(self) -> dict[str, Any]:
        return {
            "type": self.type,
            "id": self.id,
            "name": self.name,
            "extraSupported": list(self.extra_supported),
            "extraRequired": list(self.extra_required),
        }


@dataclass(frozen=True, slots=True)
class Addon:
    """One installed addon and what it can answer."""

    transport_url: str
    base_url: str
    id: str
    name: str
    version: str | None = None
    description: str | None = None
    resources: tuple[str, ...] = ()
    types: tuple[str, ...] = ()
    id_prefixes: tuple[str, ...] = ()
    catalogs: tuple[AddonCatalog, ...] = ()
    #: Per-resource overrides, when the manifest gives a resource its own
    #: `types` / `idPrefixes` instead of inheriting the manifest's.
    resource_types: dict[str, tuple[str, ...]] = field(default_factory=dict)
    resource_id_prefixes: dict[str, tuple[str, ...]] = field(default_factory=dict)

    def supports(self, resource: str, type_name: str, item_id: str | None = None) -> bool:
        if resource not in self.resources:
            return False
        types = self.resource_types.get(resource, self.types)
        if types and type_name not in types:
            return False
        prefixes = self.resource_id_prefixes.get(resource, self.id_prefixes)
        if item_id and prefixes and not any(item_id.startswith(p) for p in prefixes):
            return False
        return True

    def as_dict(self) -> dict[str, Any]:
        return {
            "id": self.id,
            "name": self.name,
            "version": self.version,
            "description": self.description,
            "transportUrl": self.transport_url,
            "resources": list(self.resources),
            "types": list(self.types),
            "idPrefixes": list(self.id_prefixes),
            "catalogs": [catalog.as_dict() for catalog in self.catalogs],
        }


@dataclass(frozen=True, slots=True)
class MetaPreview:
    """A catalogue entry: enough to draw a row, not enough to play anything."""

    id: str
    type: str
    name: str
    poster: str | None = None
    background: str | None = None
    logo: str | None = None
    description: str | None = None
    release_info: str | None = None
    imdb_rating: str | None = None
    genres: tuple[str, ...] = ()
    addon_id: str | None = None

    def as_dict(self) -> dict[str, Any]:
        return {
            "id": self.id,
            "type": self.type,
            "name": self.name,
            "poster": self.poster,
            "background": self.background,
            "logo": self.logo,
            "description": self.description,
            "releaseInfo": self.release_info,
            "imdbRating": self.imdb_rating,
            "genres": list(self.genres),
            "addonId": self.addon_id,
        }


@dataclass(frozen=True, slots=True)
class Video:
    """One episode / chapter of a series-like item."""

    id: str
    title: str | None = None
    season: int | None = None
    episode: int | None = None
    released: str | None = None
    overview: str | None = None

    def as_dict(self) -> dict[str, Any]:
        return {
            "id": self.id,
            "title": self.title,
            "season": self.season,
            "episode": self.episode,
            "released": self.released,
            "overview": self.overview,
        }


@dataclass(frozen=True, slots=True)
class Meta:
    id: str
    type: str
    name: str
    poster: str | None = None
    background: str | None = None
    logo: str | None = None
    description: str | None = None
    release_info: str | None = None
    runtime: str | None = None
    imdb_rating: str | None = None
    genres: tuple[str, ...] = ()
    cast: tuple[str, ...] = ()
    director: tuple[str, ...] = ()
    writer: tuple[str, ...] = ()
    videos: tuple[Video, ...] = ()
    addon_id: str | None = None

    def as_dict(self) -> dict[str, Any]:
        return {
            "id": self.id,
            "type": self.type,
            "name": self.name,
            "poster": self.poster,
            "background": self.background,
            "logo": self.logo,
            "description": self.description,
            "releaseInfo": self.release_info,
            "runtime": self.runtime,
            "imdbRating": self.imdb_rating,
            "genres": list(self.genres),
            "cast": list(self.cast),
            "director": list(self.director),
            "writer": list(self.writer),
            "videos": [video.as_dict() for video in self.videos],
            "addonId": self.addon_id,
        }


class StreamKind(str, Enum):
    #: A playable URL the addon supplies directly.
    URL = "url"
    #: A torrent, which only the streaming server can turn into a URL.
    TORRENT = "torrent"
    YOUTUBE = "youtube"
    #: A link into somebody else's app or website. Never playable here.
    EXTERNAL = "external"
    UNKNOWN = "unknown"


@dataclass(frozen=True, slots=True)
class Stream:
    """One way to watch one item, as the addon described it."""

    kind: StreamKind
    addon_id: str | None = None
    name: str | None = None
    title: str | None = None
    description: str | None = None
    url: str | None = None
    info_hash: str | None = None
    file_idx: int | None = None
    yt_id: str | None = None
    external_url: str | None = None
    announce: tuple[str, ...] = ()
    behavior_hints: dict[str, Any] = field(default_factory=dict)

    @property
    def identity(self) -> str:
        """A stable id for this stream, used for ranking tiebreaks and logs."""
        if self.kind is StreamKind.URL and self.url:
            return f"url:{self.url}"
        if self.kind is StreamKind.TORRENT and self.info_hash:
            return f"torrent:{self.info_hash}:{self.file_idx if self.file_idx is not None else '-'}"
        if self.kind is StreamKind.YOUTUBE and self.yt_id:
            return f"yt:{self.yt_id}"
        if self.external_url:
            return f"external:{self.external_url}"
        return f"unknown:{self.name or self.title or ''}"

    @property
    def is_playable(self) -> bool:
        return self.kind in (StreamKind.URL, StreamKind.TORRENT, StreamKind.YOUTUBE)

    def as_dict(self) -> dict[str, Any]:
        return {
            "kind": self.kind.value,
            "identity": self.identity,
            "addonId": self.addon_id,
            "name": self.name,
            "title": self.title,
            "description": self.description,
            "url": self.url,
            "infoHash": self.info_hash,
            "fileIdx": self.file_idx,
            "ytId": self.yt_id,
            "externalUrl": self.external_url,
            "behaviorHints": dict(self.behavior_hints),
            "playable": self.is_playable,
        }


@dataclass(frozen=True, slots=True)
class ResolvedStream:
    """A stream turned into something ffprobe and a player can open."""

    stream: Stream
    url: str
    via: str
    #: True when the streaming server has to stay up for this URL to work.
    needs_streaming_server: bool = False
    notes: tuple[str, ...] = ()

    def as_dict(self) -> dict[str, Any]:
        return {
            "stream": self.stream.as_dict(),
            "url": self.url,
            "via": self.via,
            "needsStreamingServer": self.needs_streaming_server,
            "notes": list(self.notes),
        }


@dataclass(frozen=True, slots=True)
class Subtitle:
    id: str
    url: str
    language: str | None = None
    addon_id: str | None = None

    def as_dict(self) -> dict[str, Any]:
        return {"id": self.id, "url": self.url, "language": self.language, "addonId": self.addon_id}


@dataclass(frozen=True, slots=True)
class SessionStatus:
    authenticated: bool
    user_id: str | None = None
    email: str | None = None
    addon_count: int = 0
    streaming_server_reachable: bool = False
    streaming_server_version: str | None = None
    api_reachable: bool = False
    notes: tuple[str, ...] = ()

    def as_dict(self) -> dict[str, Any]:
        return {
            "authenticated": self.authenticated,
            "userId": self.user_id,
            "email": self.email,
            "addonCount": self.addon_count,
            "streamingServer": {
                "reachable": self.streaming_server_reachable,
                "version": self.streaming_server_version,
            },
            "apiReachable": self.api_reachable,
            "notes": list(self.notes),
        }
