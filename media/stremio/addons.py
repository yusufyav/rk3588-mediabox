"""The Stremio addon protocol, spoken directly.

An addon is an HTTP service that answers four kinds of request under its
transport URL:

    /manifest.json
    /catalog/{type}/{id}.json           (+ /{extra}.json)
    /meta/{type}/{id}.json
    /stream/{type}/{id}.json
    /subtitles/{type}/{id}.json         (+ /{extra}.json)

That is the whole data plane of Stremio, and it is what this module uses. No
page is rendered, no script is run, and nothing here knows that Stremio has a
web interface at all.
"""

from __future__ import annotations

import logging
from typing import Any, Iterable
from urllib.parse import quote

from ..errors import UpstreamError
from ..http import get_json
from .models import Addon, AddonCatalog, Meta, MetaPreview, Stream, StreamKind, Subtitle, Video


LOG = logging.getLogger(__name__)

MANIFEST_SUFFIX = "/manifest.json"


def base_url_of(transport_url: str) -> str:
    """The addon's base, i.e. its transport URL without `/manifest.json`."""
    url = transport_url.strip()
    if url.endswith(MANIFEST_SUFFIX):
        return url[: -len(MANIFEST_SUFFIX)]
    return url.rstrip("/")


def _strings(value: Any) -> tuple[str, ...]:
    if isinstance(value, str):
        return (value,)
    if isinstance(value, list):
        return tuple(item for item in value if isinstance(item, str))
    return ()


def _text(value: Any) -> str | None:
    if isinstance(value, str) and value.strip():
        return value.strip()
    if isinstance(value, (int, float)) and not isinstance(value, bool):
        return str(value)
    return None


def _int(value: Any) -> int | None:
    if isinstance(value, bool):
        return None
    if isinstance(value, int):
        return value
    if isinstance(value, str):
        try:
            return int(value)
        except ValueError:
            return None
    return None


def parse_manifest(transport_url: str, manifest: dict[str, Any]) -> Addon:
    """Turn a manifest into an :class:`Addon`, including per-resource scoping."""
    resources: list[str] = []
    resource_types: dict[str, tuple[str, ...]] = {}
    resource_id_prefixes: dict[str, tuple[str, ...]] = {}
    for entry in manifest.get("resources", []) or []:
        if isinstance(entry, str):
            resources.append(entry)
        elif isinstance(entry, dict):
            name = _text(entry.get("name"))
            if not name:
                continue
            resources.append(name)
            types = _strings(entry.get("types"))
            if types:
                resource_types[name] = types
            prefixes = _strings(entry.get("idPrefixes"))
            if prefixes:
                resource_id_prefixes[name] = prefixes

    catalogs: list[AddonCatalog] = []
    for entry in manifest.get("catalogs", []) or []:
        if not isinstance(entry, dict):
            continue
        catalog_type = _text(entry.get("type"))
        catalog_id = _text(entry.get("id"))
        if not catalog_type or catalog_id is None:
            continue
        supported = list(_strings(entry.get("extraSupported")))
        required = list(_strings(entry.get("extraRequired")))
        for extra in entry.get("extra", []) or []:
            if isinstance(extra, dict):
                name = _text(extra.get("name"))
                if not name:
                    continue
                supported.append(name)
                if extra.get("isRequired"):
                    required.append(name)
        catalogs.append(
            AddonCatalog(
                type=catalog_type,
                id=catalog_id,
                name=_text(entry.get("name")),
                extra_supported=tuple(dict.fromkeys(supported)),
                extra_required=tuple(dict.fromkeys(required)),
            )
        )

    return Addon(
        transport_url=transport_url,
        base_url=base_url_of(transport_url),
        id=_text(manifest.get("id")) or transport_url,
        name=_text(manifest.get("name")) or "addon",
        version=_text(manifest.get("version")),
        description=_text(manifest.get("description")),
        resources=tuple(dict.fromkeys(resources)),
        types=_strings(manifest.get("types")),
        id_prefixes=_strings(manifest.get("idPrefixes")),
        catalogs=tuple(catalogs),
        resource_types=resource_types,
        resource_id_prefixes=resource_id_prefixes,
    )


def encode_extra(extra: dict[str, Any] | None) -> str:
    """`{"search": "dune", "skip": 20}` -> `search=dune&skip=20`, path-encoded.

    Ordering is sorted rather than insertion-ordered so the same query always
    produces the same URL, which is what makes caching and logs comparable.
    """
    if not extra:
        return ""
    parts = []
    for key in sorted(extra):
        value = extra[key]
        if value is None or value == "":
            continue
        parts.append(f"{quote(str(key), safe='')}={quote(str(value), safe='')}")
    return "&".join(parts)


def _resource_url(
    addon: Addon, resource: str, type_name: str, item_id: str, extra: dict[str, Any] | None = None
) -> str:
    path = f"{addon.base_url}/{resource}/{quote(type_name, safe='')}/{quote(item_id, safe='')}"
    encoded = encode_extra(extra)
    if encoded:
        path = f"{path}/{encoded}"
    return f"{path}.json"


def _meta_preview(entry: dict[str, Any], addon_id: str | None) -> MetaPreview | None:
    item_id = _text(entry.get("id"))
    item_type = _text(entry.get("type"))
    if not item_id or not item_type:
        return None
    return MetaPreview(
        id=item_id,
        type=item_type,
        name=_text(entry.get("name")) or item_id,
        poster=_text(entry.get("poster")),
        background=_text(entry.get("background")),
        logo=_text(entry.get("logo")),
        description=_text(entry.get("description")),
        release_info=_text(entry.get("releaseInfo")) or _text(entry.get("year")),
        imdb_rating=_text(entry.get("imdbRating")),
        genres=_strings(entry.get("genres")) or _strings(entry.get("genre")),
        addon_id=addon_id,
    )


def _video(entry: dict[str, Any]) -> Video | None:
    video_id = _text(entry.get("id"))
    if not video_id:
        return None
    return Video(
        id=video_id,
        title=_text(entry.get("title")) or _text(entry.get("name")),
        season=_int(entry.get("season")),
        episode=_int(entry.get("episode")) or _int(entry.get("number")),
        released=_text(entry.get("released")),
        overview=_text(entry.get("overview")) or _text(entry.get("description")),
    )


def parse_meta(entry: dict[str, Any], addon_id: str | None) -> Meta | None:
    item_id = _text(entry.get("id"))
    item_type = _text(entry.get("type"))
    if not item_id or not item_type:
        return None
    videos = tuple(
        video
        for video in (
            _video(item) for item in entry.get("videos", []) or [] if isinstance(item, dict)
        )
        if video is not None
    )
    return Meta(
        id=item_id,
        type=item_type,
        name=_text(entry.get("name")) or item_id,
        poster=_text(entry.get("poster")),
        background=_text(entry.get("background")),
        logo=_text(entry.get("logo")),
        description=_text(entry.get("description")),
        release_info=_text(entry.get("releaseInfo")) or _text(entry.get("year")),
        runtime=_text(entry.get("runtime")),
        imdb_rating=_text(entry.get("imdbRating")),
        genres=_strings(entry.get("genres")) or _strings(entry.get("genre")),
        cast=_strings(entry.get("cast")),
        director=_strings(entry.get("director")),
        writer=_strings(entry.get("writer")),
        videos=videos,
        addon_id=addon_id,
    )


def parse_stream(entry: dict[str, Any], addon_id: str | None) -> Stream | None:
    """Classify one stream descriptor by what it actually offers.

    The order matters: a descriptor with both a `url` and an `externalUrl` is
    playable, and one with only an `externalUrl` is a link to somewhere else.
    """
    url = _text(entry.get("url"))
    info_hash = _text(entry.get("infoHash"))
    yt_id = _text(entry.get("ytId"))
    external = _text(entry.get("externalUrl"))

    if url:
        kind = StreamKind.URL
    elif info_hash:
        kind = StreamKind.TORRENT
    elif yt_id:
        kind = StreamKind.YOUTUBE
    elif external:
        kind = StreamKind.EXTERNAL
    else:
        kind = StreamKind.UNKNOWN

    hints = entry.get("behaviorHints")
    announce: list[str] = []
    if isinstance(hints, dict):
        announce.extend(_strings(hints.get("announce")))
    announce.extend(_strings(entry.get("announce")))
    sources = _strings(entry.get("sources"))
    announce.extend(item[8:] for item in sources if item.startswith("tracker:"))

    return Stream(
        kind=kind,
        addon_id=addon_id,
        name=_text(entry.get("name")),
        title=_text(entry.get("title")) or _text(entry.get("description")),
        description=_text(entry.get("description")),
        url=url,
        info_hash=info_hash.lower() if info_hash else None,
        file_idx=_int(entry.get("fileIdx")),
        yt_id=yt_id,
        external_url=external,
        announce=tuple(dict.fromkeys(announce)),
        behavior_hints=hints if isinstance(hints, dict) else {},
    )


class AddonClient:
    """Talks the addon protocol. One instance per media core, no state."""

    def __init__(self, *, timeout: float = 15.0) -> None:
        self._timeout = timeout

    def manifest(self, transport_url: str) -> Addon:
        payload = get_json(transport_url, timeout=self._timeout)
        if not isinstance(payload, dict):
            raise UpstreamError(f"{transport_url} is not a Stremio manifest")
        return parse_manifest(transport_url, payload)

    def catalog(
        self,
        addon: Addon,
        type_name: str,
        catalog_id: str,
        extra: dict[str, Any] | None = None,
    ) -> list[MetaPreview]:
        url = _resource_url(addon, "catalog", type_name, catalog_id, extra)
        payload = get_json(url, timeout=self._timeout)
        if not isinstance(payload, dict):
            raise UpstreamError(f"{url} did not return a catalogue object")
        entries = payload.get("metas")
        if not isinstance(entries, list):
            return []
        previews = [
            _meta_preview(entry, addon.id) for entry in entries if isinstance(entry, dict)
        ]
        return [preview for preview in previews if preview is not None]

    def meta(self, addon: Addon, type_name: str, item_id: str) -> Meta | None:
        url = _resource_url(addon, "meta", type_name, item_id)
        payload = get_json(url, timeout=self._timeout)
        if not isinstance(payload, dict):
            return None
        entry = payload.get("meta")
        if not isinstance(entry, dict):
            return None
        return parse_meta(entry, addon.id)

    def streams(self, addon: Addon, type_name: str, item_id: str) -> list[Stream]:
        url = _resource_url(addon, "stream", type_name, item_id)
        payload = get_json(url, timeout=self._timeout)
        if not isinstance(payload, dict):
            return []
        entries = payload.get("streams")
        if not isinstance(entries, list):
            return []
        streams = [parse_stream(entry, addon.id) for entry in entries if isinstance(entry, dict)]
        return [stream for stream in streams if stream is not None]

    def subtitles(
        self,
        addon: Addon,
        type_name: str,
        item_id: str,
        extra: dict[str, Any] | None = None,
    ) -> list[Subtitle]:
        url = _resource_url(addon, "subtitles", type_name, item_id, extra)
        payload = get_json(url, timeout=self._timeout)
        if not isinstance(payload, dict):
            return []
        entries = payload.get("subtitles")
        if not isinstance(entries, list):
            return []
        found: list[Subtitle] = []
        for entry in entries:
            if not isinstance(entry, dict):
                continue
            subtitle_url = _text(entry.get("url"))
            if not subtitle_url:
                continue
            found.append(
                Subtitle(
                    id=_text(entry.get("id")) or subtitle_url,
                    url=subtitle_url,
                    language=_text(entry.get("lang")) or _text(entry.get("language")),
                    addon_id=addon.id,
                )
            )
        return found


def addons_supporting(
    addons: Iterable[Addon], resource: str, type_name: str, item_id: str | None = None
) -> list[Addon]:
    return [addon for addon in addons if addon.supports(resource, type_name, item_id)]
