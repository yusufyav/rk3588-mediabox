"""The headless Stremio adapter.

This is the whole of MediaBox's dependency on Stremio, and it is a data
dependency: an HTTP API for accounts and addons, the addon protocol for
content, and the local streaming server for turning torrents into URLs.

What this module is *not* is as important as what it is. It does not open a
page, does not run upstream JavaScript, does not read a DOM, and does not know
that Stremio has a web interface. Anything MediaBox builds on top of it talks
to :mod:`media.api` and could be given a different provider tomorrow without
noticing.

The contract:

    session_status()                 who we are, and what is reachable
    addons()                         what can answer questions
    home()                           catalogue rows worth showing first
    catalog(type, id, ...)           one catalogue
    search(query)                    every searchable catalogue at once
    meta(type, id)                   one item in full
    streams(type, id, video_id)      the ways to watch it
    resolve(stream)                  a URL a player or ffprobe can open
    subtitles(type, id, ...)         subtitle tracks
"""

from __future__ import annotations

import logging
import threading
import time
from dataclasses import dataclass
from typing import Any, Iterable

from ..errors import InvalidRequest, NotFound, UpstreamError
from .addons import AddonClient, addons_supporting, parse_manifest
from .api import DEFAULT_API_URL, SessionStore, StremioAPI
from .models import (
    Addon,
    Meta,
    MetaPreview,
    ResolvedStream,
    SessionStatus,
    Stream,
    StreamKind,
    Subtitle,
)
from .server import StreamingServer


LOG = logging.getLogger(__name__)

#: How long the addon collection is reused before it is fetched again. The
#: collection changes when somebody installs an addon, which is rare, and
#: fetching it on every catalogue request would put api.strem.io in the path of
#: every screen.
ADDON_CACHE_SECONDS = 300.0

#: Catalogues shown on the home surface, per addon, so one addon with forty
#: catalogues cannot fill the screen by itself.
HOME_CATALOGS_PER_ADDON = 4
HOME_ITEMS_PER_CATALOG = 20

SEARCH_RESULTS_PER_CATALOG = 20

#: A series episode id is `{seriesId}:{season}:{episode}`. Composing it here
#: keeps the shape in one place.
def video_id_for(item_id: str, season: int, episode: int) -> str:
    return f"{item_id}:{int(season)}:{int(episode)}"


@dataclass(frozen=True, slots=True)
class CatalogRow:
    addon_id: str
    addon_name: str
    catalog_id: str
    type: str
    name: str
    items: tuple[MetaPreview, ...]

    def as_dict(self) -> dict[str, Any]:
        return {
            "addonId": self.addon_id,
            "addonName": self.addon_name,
            "catalogId": self.catalog_id,
            "type": self.type,
            "name": self.name,
            "items": [item.as_dict() for item in self.items],
        }


class HeadlessStremio:
    def __init__(
        self,
        *,
        streaming_server_url: str = "http://127.0.0.1:11470",
        api_url: str | None = None,
        session_path: str | None = None,
        timeout: float = 15.0,
        clock: Any = time.monotonic,
    ) -> None:
        self.api = StremioAPI(
            api_url=api_url or DEFAULT_API_URL,
            store=SessionStore(session_path),
            timeout=timeout,
        )
        self.addons_client = AddonClient(timeout=timeout)
        self.server = StreamingServer(streaming_server_url, timeout=timeout)
        self._clock = clock
        self._lock = threading.Lock()
        self._addons: tuple[Addon, ...] = ()
        self._addons_fetched_at: float | None = None

    # ------------------------------------------------------------------ session

    def session_status(self) -> SessionStatus:
        stored = self.api.store.get()
        notes: list[str] = []
        api_reachable = True
        try:
            addons = self.addons()
        except UpstreamError as exc:
            api_reachable = False
            addons = ()
            notes.append(f"the Stremio API is unreachable: {exc.message}")

        version = self.server.version()
        if version is None:
            notes.append(
                "the local streaming server is not answering; addon-hosted streams still "
                "play, torrents do not"
            )
        if not stored.auth_key:
            notes.append(
                "no Stremio account is signed in; Stremio's default addon collection is in use"
            )
        return SessionStatus(
            authenticated=bool(stored.auth_key),
            user_id=stored.user_id,
            email=stored.email,
            addon_count=len(addons),
            streaming_server_reachable=version is not None,
            streaming_server_version=version,
            api_reachable=api_reachable,
            notes=tuple(notes),
        )

    def login(self, email: str, password: str) -> SessionStatus:
        self.api.login(email, password)
        self.invalidate_addons()
        return self.session_status()

    def logout(self) -> SessionStatus:
        self.api.logout()
        self.invalidate_addons()
        return self.session_status()

    # ------------------------------------------------------------------- addons

    def invalidate_addons(self) -> None:
        with self._lock:
            self._addons = ()
            self._addons_fetched_at = None

    def addons(self, *, refresh: bool = False) -> tuple[Addon, ...]:
        now = self._clock()
        with self._lock:
            fresh = (
                self._addons
                and self._addons_fetched_at is not None
                and now - self._addons_fetched_at < ADDON_CACHE_SECONDS
            )
            if fresh and not refresh:
                return self._addons

        entries = self.api.addon_collection()
        parsed: list[Addon] = []
        for entry in entries:
            manifest = entry.get("manifest")
            transport = entry.get("transportUrl")
            if not isinstance(manifest, dict) or not isinstance(transport, str):
                continue
            try:
                parsed.append(parse_manifest(transport, manifest))
            except Exception:  # a malformed manifest must not lose the others
                LOG.warning("Skipping an addon with an unusable manifest: %s", transport)
        with self._lock:
            self._addons = tuple(parsed)
            self._addons_fetched_at = now
        return self._addons

    def addon_by_id(self, addon_id: str) -> Addon:
        for addon in self.addons():
            if addon.id == addon_id:
                return addon
        raise NotFound(f"no installed addon with id {addon_id!r}")

    # ---------------------------------------------------------------- catalogue

    def catalog(
        self,
        type_name: str,
        catalog_id: str,
        *,
        addon_id: str | None = None,
        extra: dict[str, Any] | None = None,
        limit: int | None = None,
    ) -> list[MetaPreview]:
        candidates = (
            [self.addon_by_id(addon_id)]
            if addon_id
            else [
                addon
                for addon in self.addons()
                if any(
                    catalog.type == type_name and catalog.id == catalog_id
                    for catalog in addon.catalogs
                )
            ]
        )
        if not candidates:
            raise NotFound(f"no installed addon offers the {type_name}/{catalog_id} catalogue")
        errors: list[str] = []
        for addon in candidates:
            try:
                items = self.addons_client.catalog(addon, type_name, catalog_id, extra)
            except UpstreamError as exc:
                errors.append(f"{addon.name}: {exc.message}")
                continue
            return items[:limit] if limit else items
        raise UpstreamError(
            f"the {type_name}/{catalog_id} catalogue could not be fetched", {"errors": errors}
        )

    def home(self, *, types: Iterable[str] = ("movie", "series")) -> list[CatalogRow]:
        """Catalogue rows for a home surface, gathered from every addon.

        A failing addon costs its own rows and nothing else. An appliance whose
        home screen is empty because one third-party service is down is not an
        appliance.
        """
        wanted = set(types)
        rows: list[CatalogRow] = []
        for addon in self.addons():
            taken = 0
            for catalog in addon.catalogs:
                if taken >= HOME_CATALOGS_PER_ADDON:
                    break
                if wanted and catalog.type not in wanted:
                    continue
                if catalog.extra_required:
                    continue  # a catalogue that needs a genre is not a home row
                try:
                    items = self.addons_client.catalog(addon, catalog.type, catalog.id)
                except UpstreamError as exc:
                    LOG.info("Home row %s/%s unavailable: %s", addon.id, catalog.id, exc.message)
                    continue
                if not items:
                    continue
                rows.append(
                    CatalogRow(
                        addon_id=addon.id,
                        addon_name=addon.name,
                        catalog_id=catalog.id,
                        type=catalog.type,
                        name=catalog.name or f"{addon.name} {catalog.type}",
                        items=tuple(items[:HOME_ITEMS_PER_CATALOG]),
                    )
                )
                taken += 1
        return rows

    def search(self, query: str, *, types: Iterable[str] | None = None) -> list[CatalogRow]:
        if not isinstance(query, str) or not query.strip():
            raise InvalidRequest("a search needs a query")
        text = query.strip()[:200]
        wanted = set(types) if types else None
        rows: list[CatalogRow] = []
        for addon in self.addons():
            for catalog in addon.catalogs:
                if not catalog.supports_search:
                    continue
                if wanted and catalog.type not in wanted:
                    continue
                try:
                    items = self.addons_client.catalog(
                        addon, catalog.type, catalog.id, {"search": text}
                    )
                except UpstreamError as exc:
                    LOG.info("Search on %s/%s failed: %s", addon.id, catalog.id, exc.message)
                    continue
                if not items:
                    continue
                rows.append(
                    CatalogRow(
                        addon_id=addon.id,
                        addon_name=addon.name,
                        catalog_id=catalog.id,
                        type=catalog.type,
                        name=catalog.name or f"{addon.name} {catalog.type}",
                        items=tuple(items[:SEARCH_RESULTS_PER_CATALOG]),
                    )
                )
        return rows

    # --------------------------------------------------------------------- meta

    def meta(self, type_name: str, item_id: str) -> Meta:
        candidates = addons_supporting(self.addons(), "meta", type_name, item_id)
        errors: list[str] = []
        for addon in candidates:
            try:
                found = self.addons_client.meta(addon, type_name, item_id)
            except UpstreamError as exc:
                errors.append(f"{addon.name}: {exc.message}")
                continue
            if found is not None:
                return found
        raise NotFound(f"no addon has metadata for {type_name}/{item_id}")

    # ------------------------------------------------------------------ streams

    def streams(self, type_name: str, item_id: str, video_id: str | None = None) -> list[Stream]:
        """Every way to watch one item, from every addon that offers one.

        `video_id` is what selects an episode: for a series the addon is asked
        about `tt1234:1:2`, not about the series.
        """
        target = video_id or item_id
        found: list[Stream] = []
        seen: set[str] = set()
        for addon in addons_supporting(self.addons(), "stream", type_name, target):
            try:
                streams = self.addons_client.streams(addon, type_name, target)
            except UpstreamError as exc:
                LOG.info("Streams from %s unavailable: %s", addon.id, exc.message)
                continue
            for stream in streams:
                if stream.identity in seen:
                    continue
                seen.add(stream.identity)
                found.append(stream)
        return found

    def resolve(self, stream: Stream) -> ResolvedStream:
        return self.server.resolve(stream)

    def resolve_descriptor(self, descriptor: dict[str, Any]) -> ResolvedStream:
        """Resolve a stream given as the wire descriptor a client held on to."""
        from .addons import parse_stream

        if not isinstance(descriptor, dict):
            raise InvalidRequest("a stream descriptor must be an object")
        stream = parse_stream(descriptor, descriptor.get("addonId"))
        if stream is None or stream.kind is StreamKind.UNKNOWN:
            raise InvalidRequest("that stream descriptor offers nothing playable")
        return self.resolve(stream)

    # ---------------------------------------------------------------- subtitles

    def subtitles(
        self, type_name: str, item_id: str, *, video_id: str | None = None, extra: dict[str, Any] | None = None
    ) -> list[Subtitle]:
        target = video_id or item_id
        found: list[Subtitle] = []
        seen: set[str] = set()
        for addon in addons_supporting(self.addons(), "subtitles", type_name, target):
            try:
                subtitles = self.addons_client.subtitles(addon, type_name, target, extra)
            except UpstreamError as exc:
                LOG.info("Subtitles from %s unavailable: %s", addon.id, exc.message)
                continue
            for subtitle in subtitles:
                if subtitle.url in seen:
                    continue
                seen.add(subtitle.url)
                found.append(subtitle)
        return found

    # ------------------------------------------------------------------ library

    def library(self) -> list[dict[str, Any]]:
        return self.api.library()
