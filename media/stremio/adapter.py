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

import itertools
from datetime import datetime, timezone
import logging
import os
import threading
import time
from dataclasses import dataclass, replace
from typing import Any, Iterable

from ..errors import InvalidRequest, NotFound, UpstreamError
from .addons import AddonClient, addons_supporting, parse_manifest
from .api import DEFAULT_API_URL, SessionStore, StremioAPI
from .local_search import LocalSearch
from . import library_state
from .watched import WatchedBitField, ordered_video_ids
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


# The account's own library, as opposed to the appliance's manifest. The two
# are different sources wearing the same word, and the id is what keeps them
# apart on the way to the interface.
STREMIO_LIBRARY_ADDON_ID = "stremio.library"


def _text(value: Any) -> str | None:
    """A string, or nothing. Numbers become strings; everything else is dropped.

    Library records come from whatever wrote them, which over the years has
    been several different clients: a release year arrives as 1921 from one and
    "1921" from another, and a missing one as null, "" or absent.
    """
    if value is None or isinstance(value, (dict, list, bool)):
        return None
    text = str(value).strip()
    return text or None


def _as_millis(value: Any) -> int | None:
    """A playback position in milliseconds, or nothing.

    Stremio stores these as milliseconds. A negative value means the client
    never wrote one, which is not the same as the beginning of the film.
    """
    try:
        millis = int(value)
    except (TypeError, ValueError):
        return None
    return millis if millis >= 0 else None


# stremio-core's CATALOG_PREVIEW_SIZE, which is what its continue-watching
# preview is cut to.
CONTINUE_WATCHING_SIZE = 100


def _in_continue_watching(record: dict[str, Any], removed: bool, temp: bool) -> bool:
    """stremio-core's `LibraryItem::is_in_continue_watching`, and its `!is_live()`."""
    kind = record.get("type")
    hints = record.get("behaviorHints") if isinstance(record.get("behaviorHints"), dict) else {}
    if kind == "other" or kind == "tv" or hints.get("isLive") is True:
        return False
    state = record.get("state") if isinstance(record.get("state"), dict) else {}
    return (not removed or temp) and (_as_millis(state.get("timeOffset")) or 0) > 0


def _library_preview(record: Any) -> dict[str, Any] | None:
    """One library record in the catalogue preview shape, or nothing."""
    if not isinstance(record, dict):
        return None
    item_id = record.get("_id") or record.get("id")
    name = record.get("name")
    if not item_id or not name:
        return None
    state = record.get("state") if isinstance(record.get("state"), dict) else {}
    return {
        "id": str(item_id),
        "type": record.get("type") or "movie",
        "name": str(name),
        "poster": record.get("poster"),
        "background": record.get("background"),
        "logo": record.get("logo"),
        "description": None,
        "releaseInfo": _text(record.get("year")),
        "imdbRating": None,
        "genres": [],
        "addonId": STREMIO_LIBRARY_ADDON_ID,
        "state": {
            "timeOffset": _as_millis(state.get("timeOffset")),
            "duration": _as_millis(state.get("duration")),
            "lastWatched": _text(state.get("lastWatched")),
        },
    }


def _watch_state(record: dict[str, Any] | None, meta: Meta | None) -> dict[str, Any]:
    state = record.get("state") if isinstance(record, dict) and isinstance(record.get("state"), dict) else {}
    removed = bool(record.get("removed")) if isinstance(record, dict) else True
    temp = bool(record.get("temp")) if isinstance(record, dict) else False
    watched: list[str] = []
    if meta is not None and meta.videos:
        watched = WatchedBitField.parse(
            state.get("watched"), ordered_video_ids(meta.videos)
        ).watched_ids()
    return {
        "known": record is not None,
        "inLibrary": record is not None and not removed and not temp,
        "videoId": _text(state.get("video_id")),
        "timeOffset": _as_millis(state.get("timeOffset")),
        "duration": _as_millis(state.get("duration")),
        "timesWatched": _as_millis(state.get("timesWatched")) or 0,
        "flaggedWatched": bool(_as_millis(state.get("flaggedWatched"))),
        "lastWatched": _text(state.get("lastWatched")),
        "watched": watched,
    }


LOG = logging.getLogger(__name__)

#: How many addons may be asked for streams at the same time.
STREAM_FAN_OUT = 8
#: The longest the whole fan-out may take. Past this the reply goes out with
#: what has arrived; a source list that is nearly complete now beats a complete
#: one after a dead host's connect timeout.
STREAM_DEADLINE = 6.0
#: How long an addon that just failed is left out of the fan-out.
ADDON_PENALTY_SECONDS = 300.0

#: How long the addon collection is reused before it is fetched again. The
#: collection changes when somebody installs an addon, which is rare, and
#: fetching it on every catalogue request would put api.strem.io in the path of
#: every screen.
ADDON_CACHE_SECONDS = 300.0

#: Catalogues shown on the home surface, per addon, so one addon with forty
#: catalogues cannot fill the screen by itself.
HOME_CATALOGS_PER_ADDON = 4
HOME_ITEMS_PER_CATALOG = 20
#: How many catalogues may be fetched at the same time, and how long the whole
#: home surface may take. The same shape as the stream fan-out above and for
#: the same reason: asked one after another, the wait was the sum of every
#: catalogue rather than the slowest one. Measured on the appliance, fourteen
#: shelves: 5.15 s serial, 1.31 s here.
#:
#: Widening it was tried and does not help. This box has fourteen catalogues
#: and eight workers, so the last six are fetched as the first eight finish —
#: but the floor is one catalogue that takes 1.0-1.3 s on its own, and at
#: sixteen the median was 1.33 s against 1.31 s. The remaining second is a
#: third-party host, not the shape of this loop.
HOME_FAN_OUT = 8
HOME_DEADLINE = 6.0

SEARCH_RESULTS_PER_CATALOG = 20
#: A title's record is asked for twice when its page opens -- once for the page
#: and once to read which of its episodes were watched -- and again whenever
#: the page is reopened. Kept briefly, so that is one request to the addon.
META_CACHE_SECONDS = 600.0
META_CACHE_SIZE = 64
#: A search asks every searchable catalogue at once, as the home surface does
#: and for the same reason: asked one after another, the wait was the sum of
#: every catalogue, fifteen seconds for each one whose host had gone away.
SEARCH_FAN_OUT = 8
SEARCH_DEADLINE = 8.0

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
    #: Why this catalogue has nothing to show, when it failed rather than
    #: answered with nothing. The reference keeps such a row and says so.
    error: str | None = None

    def as_dict(self) -> dict[str, Any]:
        return {
            "addonId": self.addon_id,
            "addonName": self.addon_name,
            "catalogId": self.catalog_id,
            "type": self.type,
            "name": self.name,
            "items": [item.as_dict() for item in self.items],
            "error": self.error,
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
        # The feed the search box suggests from, kept beside the session file.
        self.local_search = LocalSearch(
            os.path.join(os.path.dirname(session_path), "search-feed.json")
            if session_path
            else None
        )
        self._clock = clock
        self._lock = threading.Lock()
        self._addons: tuple[Addon, ...] = ()
        self._addons_fetched_at: float | None = None
        # Addons that have just failed to answer, and when. An addon whose host
        # has gone away costs a full connect timeout every time it is asked,
        # and asking it again thirty seconds later buys nothing.
        self._addon_failures: dict[str, float] = {}
        # Home keeps its own record rather than sharing the one above. An addon
        # that is slow to answer for streams is not necessarily slow for a
        # catalogue — they are different endpoints and often different hosts —
        # and one table would mean each surface penalising the other's addons
        # for failures it never saw.
        self._home_failures: dict[str, float] = {}
        self._meta_cache: dict[tuple[str, str], tuple[float, Meta]] = {}

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

        Asked one catalogue after another, the home screen cost the *sum* of
        every catalogue on every installed addon: measured on the appliance at a
        median of 5.1 s across ten calls, for fourteen rows — about 370 ms each,
        every time the television was turned on. The wait is now the slowest
        single catalogue, capped, which is the same arrangement `_gather_streams`
        already had and for the same reason.

        The order is the serial loop's order, not the order the answers arrive
        in. Shelves that rearranged themselves on every start would be worse
        than shelves that are slow.
        """
        wanted = set(types)

        # The work list, in the order the rows will appear.
        jobs: list[tuple[Addon, Any]] = []
        for addon in self.addons():
            if self._recently_failed(addon.id, self._home_failures):
                continue
            taken = 0
            for catalog in addon.catalogs:
                if taken >= HOME_CATALOGS_PER_ADDON:
                    break
                if wanted and catalog.type not in wanted:
                    continue
                if catalog.extra_required:
                    continue  # a catalogue that needs a genre is not a home row
                jobs.append((addon, catalog))
                taken += 1

        def ask(addon: Addon, catalog: Any) -> list[MetaPreview]:
            began = self._clock()
            try:
                return self.addons_client.catalog(addon, catalog.type, catalog.id)
            finally:
                LOG.info(
                    "Home row %s/%s took %.0f ms",
                    addon.id,
                    catalog.id,
                    (self._clock() - began) * 1000.0,
                )

        results, answered = self._fan_out(jobs, ask, HOME_FAN_OUT, HOME_DEADLINE)

        collected: list[list[MetaPreview] | None] = []
        for (addon, catalog), result in zip(jobs, results):
            if isinstance(result, BaseException):
                LOG.info("Home row %s/%s unavailable: %s", addon.id, catalog.id, result)
            collected.append(result if isinstance(result, list) else None)

        # Whoever was still hanging when the deadline came is the one worth not
        # asking again for a while: it is the addon that costs the home screen
        # a full wait every time the television is turned on.
        for index, done in enumerate(answered):
            if not done:
                addon, catalog = jobs[index]
                LOG.info(
                    "Home row %s/%s did not answer within %.0fs",
                    addon.id,
                    catalog.id,
                    HOME_DEADLINE,
                )
                self._note_failure(addon.id, self._home_failures)

        rows: list[CatalogRow] = []
        for (addon, catalog), items in zip(jobs, collected):
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
        return rows

    def search(self, query: str, *, types: Iterable[str] | None = None) -> list[CatalogRow]:
        """Every searchable catalogue asked once, all at the same time.

        stremio-core's `CatalogsWithExtra` with a `search` extra: one row per
        catalogue that supports the extra, in the order the addons are
        installed and the catalogues listed. A catalogue that answered with
        nothing is left out; one that failed keeps its row with the reason, as
        the reference's search page does.
        """
        if not isinstance(query, str) or not query.strip():
            raise InvalidRequest("a search needs a query")
        text = query.strip()[:200]
        wanted = set(types) if types else None
        jobs: list[tuple[Addon, Any]] = [
            (addon, catalog)
            for addon in self.addons()
            for catalog in addon.catalogs
            if catalog.supports_search and (not wanted or catalog.type in wanted)
        ]

        def ask(addon: Addon, catalog: Any) -> list[MetaPreview]:
            return self.addons_client.catalog(addon, catalog.type, catalog.id, {"search": text})

        results, answered = self._fan_out(jobs, ask, SEARCH_FAN_OUT, SEARCH_DEADLINE)

        rows: list[CatalogRow] = []
        for (addon, catalog), result, done in zip(jobs, results, answered):
            error: str | None = None
            items: list[MetaPreview] = []
            if not done:
                error = "yanıt vermedi"
            elif isinstance(result, BaseException):
                LOG.info("Search on %s/%s failed: %s", addon.id, catalog.id, result)
                error = "yanıt vermedi"
            elif result:
                items = result
            if not items and error is None:
                continue
            rows.append(
                CatalogRow(
                    addon_id=addon.id,
                    addon_name=addon.name,
                    catalog_id=catalog.id,
                    type=catalog.type,
                    name=catalog.name or f"{addon.name} {catalog.type}",
                    items=tuple(items[:SEARCH_RESULTS_PER_CATALOG]),
                    error=error,
                )
            )
        return rows

    def discover_catalogs(self) -> list[dict[str, Any]]:
        """What Discover can browse: stremio-core's `CatalogWithFilters` selectables.

        Every installed catalogue whose required extras can all be given a
        value -- the first of their options, which is the default -- in the
        order the addons are installed. A catalogue that requires an extra
        with no options (a search catalogue needs a query) is not one that can
        be browsed. Each carries the extras that can be chosen: those with
        options, less `skip`, which is the paging and not a filter.
        """
        found: list[dict[str, Any]] = []
        for addon in self.addons():
            for catalog in addon.catalogs:
                defaults: dict[str, str] = {}
                browsable = True
                for name in catalog.extra_required:
                    options = catalog.options_for(name)
                    if not options:
                        browsable = False
                        break
                    defaults[name] = options[0]
                if not browsable:
                    continue
                found.append(
                    {
                        "addonId": addon.id,
                        "addonName": addon.name,
                        "type": catalog.type,
                        "id": catalog.id,
                        "name": catalog.name or catalog.id,
                        "pages": "skip" in catalog.extra_supported,
                        "defaults": defaults,
                        "extra": [
                            {
                                "name": name,
                                "required": name in catalog.extra_required,
                                "options": list(options),
                            }
                            for name, options in catalog.extra_options
                            if name != "skip" and options
                        ],
                    }
                )
        return found

    def suggest(self, query: str) -> list[dict[str, Any]]:
        """What the search box offers while it is being typed into; no addon is asked."""
        if not isinstance(query, str):
            raise InvalidRequest("a suggestion needs a query")
        return [found.as_dict() for found in self.local_search.suggest(query[:200])]

    def _fan_out(
        self,
        jobs: list[tuple[Addon, Any]],
        ask: Any,
        width: int,
        deadline_seconds: float,
    ) -> tuple[list[Any], list[bool]]:
        """Runs `ask(addon, catalog)` for every job, `width` at a time, until the deadline.

        Each result is what `ask` returned, or the exception it raised. A job
        that had not finished by the deadline is `None` with `answered` false.

        A fixed number of daemon threads take work off a shared counter, rather
        than one thread per catalogue: there are more catalogues than addons,
        and threading only the first few would leave the rest to be fetched one
        after another. They are joined with a deadline and never shut down --
        a pool's shutdown joins every worker, and a straggler must not be able
        to hold the screen. It finishes into a list nobody reads any more.
        """
        results: list[Any] = [None] * len(jobs)
        answered = [False] * len(jobs)
        next_job = itertools.count()

        def work() -> None:
            for index in next_job:
                if index >= len(jobs):
                    return
                addon, catalog = jobs[index]
                try:
                    results[index] = ask(addon, catalog)
                except Exception as exc:  # a broken addon is not a broken appliance
                    results[index] = exc
                finally:
                    # Answered, even if the answer was "no". A refusal that
                    # came back in eighty milliseconds is not a reason to stop
                    # asking -- only a host that hangs is, and that is caught
                    # at the deadline.
                    answered[index] = True

        deadline = self._clock() + deadline_seconds
        threads = []
        for slot in range(min(width, len(jobs))):
            thread = threading.Thread(target=work, name=f"fan-out-{slot}", daemon=True)
            thread.start()
            threads.append(thread)
        for thread in threads:
            thread.join(timeout=max(0.0, deadline - self._clock()))
        # Read once, so a straggler answering after this cannot make two calls
        # with the same answers produce different screens.
        return list(results), list(answered)

    def meta(self, type_name: str, item_id: str) -> Meta:
        key = (type_name, item_id)
        with self._lock:
            cached = self._meta_cache.get(key)
            if cached and self._clock() - cached[0] < META_CACHE_SECONDS:
                return cached[1]
        found = self._meta_uncached(type_name, item_id)
        with self._lock:
            self._meta_cache[key] = (self._clock(), found)
            if len(self._meta_cache) > META_CACHE_SIZE:
                oldest = min(self._meta_cache, key=lambda k: self._meta_cache[k][0])
                self._meta_cache.pop(oldest, None)
        return found

    def watch_state(self, type_name: str, item_id: str) -> dict[str, Any]:
        """How far this title has been watched, as the account records it.

        The account's own record of the title -- which episode was last
        played, where in it, and, for a series, which episodes are watched --
        read the way stremio-core reads it. The watched episodes are decoded
        against the title's episode list, which is why the record is asked for
        as well: the account stores bits, and bits mean nothing without the
        list they index.
        """
        record: dict[str, Any] | None = None
        meta: Meta | None = None
        errors: list[str] = []

        def read_record() -> None:
            nonlocal record
            try:
                record = self.api.library_item(item_id)
            except UpstreamError as exc:
                errors.append(exc.message)

        def read_meta() -> None:
            nonlocal meta
            if type_name != "series":
                return
            try:
                meta = self.meta(type_name, item_id)
            except (UpstreamError, NotFound) as exc:
                errors.append(str(exc))

        threads = [threading.Thread(target=job, daemon=True) for job in (read_record, read_meta)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join(timeout=self.api._timeout + 1.0)
        if record is None and errors:
            raise UpstreamError("the account's record of this title is unavailable", {"errors": errors})
        return _watch_state(record, meta)

    # -------------------------------------------------- the account's record

    def record_progress(
        self,
        type_name: str,
        item_id: str,
        *,
        video_id: str | None,
        time_ms: int,
        duration_ms: int,
        seek: bool = False,
        closed: bool = False,
        name: str = "",
        poster: str | None = None,
    ) -> dict[str, Any]:
        """Where playback has got to, written to the account as stremio-core writes it."""
        if time_ms < 0 or duration_ms < 0:
            raise InvalidRequest("a position cannot be negative")

        def change(record: dict[str, Any], meta: Meta | None, now: Any) -> None:
            library_state.progress(
                record, meta, video_id or item_id, int(time_ms), int(duration_ms), seek=seek, now=now
            )
            if closed:
                library_state.closed(record, meta, now)

        return self._change_record(type_name, item_id, name, poster, change)

    def mark_watched(
        self,
        type_name: str,
        item_id: str,
        *,
        watched: bool,
        video_id: str | None = None,
        season: int | None = None,
        name: str = "",
        poster: str | None = None,
    ) -> dict[str, Any]:
        """A film, an episode, or a whole season, marked watched or not."""

        def change(record: dict[str, Any], meta: Meta | None, now: Any) -> None:
            if meta is None or (video_id is None and season is None):
                library_state.mark_title_watched(record, watched, now)
                return
            if season is not None:
                videos = [v for v in meta.videos if v.season == season]
                library_state.mark_videos_watched(record, meta, videos, watched, now, season=season)
                return
            video = next((v for v in meta.videos if v.id == video_id), None)
            if video is None:
                raise NotFound(f"{item_id} has no episode {video_id}")
            library_state.mark_videos_watched(record, meta, [video], watched, now)

        return self._change_record(type_name, item_id, name, poster, change)

    def set_in_library(
        self, type_name: str, item_id: str, *, in_library: bool, name: str = "", poster: str | None = None
    ) -> dict[str, Any]:
        return self._change_record(
            type_name,
            item_id,
            name,
            poster,
            lambda record, meta, now: library_state.set_in_library(record, in_library),
            needs_meta=False,
        )

    def rewind(self, type_name: str, item_id: str) -> dict[str, Any]:
        """Out of "Devam Et": the position is cleared and nothing else is."""
        return self._change_record(
            type_name,
            item_id,
            "",
            None,
            lambda record, meta, now: library_state.rewind(record),
            needs_meta=False,
            create=False,
        )

    def _change_record(
        self,
        type_name: str,
        item_id: str,
        name: str,
        poster: str | None,
        change: Any,
        *,
        needs_meta: bool = True,
        create: bool = True,
    ) -> dict[str, Any]:
        """Read the title's record, change it, write it back, and say how it now stands.

        A title the account has never seen gets the record stremio-core makes
        for one: temporary and removed, so it is in "Devam Et" once something
        of it has been watched, and not in the library until it is added.
        """
        if not isinstance(item_id, str) or not item_id:
            raise InvalidRequest("a title id is required")
        record = self.api.library_item(item_id)
        meta: Meta | None = None
        if needs_meta and type_name == "series":
            meta = self.meta(type_name, item_id)
        now = datetime.now(timezone.utc)
        if record is None:
            if not create:
                raise NotFound(f"the account has no record of {item_id}")
            if meta is not None:
                name, poster = name or meta.name, poster or meta.poster
            record = library_state.new_record(item_id, type_name, name or item_id, poster, now)
        change(record, meta, now)
        library_state.touched(record, now)
        self.api.datastore_put([record])
        if type_name == "series" and meta is None:
            try:
                meta = self.meta(type_name, item_id)
            except (UpstreamError, NotFound):
                meta = None
        return _watch_state(record, meta)

    def _meta_uncached(self, type_name: str, item_id: str) -> Meta:
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
        addons = [
            addon
            for addon in addons_supporting(self.addons(), "stream", type_name, target)
            if not self._recently_failed(addon.id)
        ]
        answers = self._gather_streams(addons, type_name, target)

        found: list[Stream] = []
        seen: set[str] = set()
        # Addon order, not answer order: which addon replied first is a network
        # accident, and a list that reorders itself between two openings of the
        # same title is a list nobody can learn.
        for position, (addon, streams) in enumerate(zip(addons, answers)):
            for stream in streams:
                if stream.identity in seen:
                    continue
                seen.add(stream.identity)
                # Which addon answered, and where it sits in the person's own
                # collection. Fifty-five sources from two addons look like one
                # undifferentiated list until they can be separated, and only
                # the caller knows which addon was asked.
                #
                # The position is carried rather than left to be inferred from
                # the order of this list. It is the same order today -- the
                # loop above is in addon order on purpose -- but a reader that
                # infers it is a reader that breaks silently the first time a
                # stream is deduplicated away or the list is sorted. The
                # interface groups its source filter by addon and offers the
                # groups in this order, which is the order Stremio shows them
                # in, so it should be told rather than left to guess.
                found.append(
                    replace(stream, addon_name=addon.name, addon_order=position)
                )
        return found

    def _gather_streams(
        self, addons: list[Addon], type_name: str, target: str
    ) -> list[list[Stream]]:
        """Ask every addon at once, and do not let one of them hold the rest.

        Asked one after another, the wait for a title was the *sum* of the
        addons: a single installed addon whose host had gone away — measured
        on a real collection — spent fifteen seconds in a connect
        timeout, and the sources for the film did not appear until it gave up,
        however quickly the others had answered. The wait is now the slowest
        single addon, capped: whatever has not answered by the deadline is left
        out of this reply rather than delaying all of it, and an addon that
        failed is not asked again for a while.
        """
        if not addons:
            return []

        results: list[list[Stream]] = [[] for _ in addons]

        def ask(index: int, addon: Addon) -> None:
            try:
                results[index] = self.addons_client.streams(addon, type_name, target)
            except UpstreamError as exc:
                LOG.info("Streams from %s unavailable: %s", addon.id, exc.message)
                self._note_failure(addon.id)
            except Exception as exc:  # a broken addon is not a broken appliance
                LOG.info("Streams from %s failed: %s", addon.id, exc)
                self._note_failure(addon.id)

        # Plain daemon threads, joined with a deadline. A pool would have to be
        # shut down, and shutting one down joins every worker — which is
        # exactly the wait this exists to avoid. A straggler finishes into a
        # list nobody reads any more and the process does not wait for it.
        threads = []
        for index, addon in enumerate(addons[:STREAM_FAN_OUT]):
            thread = threading.Thread(
                target=ask, args=(index, addon), name=f"streams-{addon.id}", daemon=True
            )
            thread.start()
            threads.append((index, thread))
        # Anything past the fan-out width is asked in line, on this thread.
        for index, addon in enumerate(addons[STREAM_FAN_OUT:], start=STREAM_FAN_OUT):
            ask(index, addon)

        deadline = self._clock() + STREAM_DEADLINE
        for index, thread in threads:
            thread.join(timeout=max(0.0, deadline - self._clock()))
            if thread.is_alive():
                LOG.info(
                    "Streams from %s did not answer within %.0fs",
                    addons[index].id,
                    STREAM_DEADLINE,
                )
                self._note_failure(addons[index].id)
        return results

    def _recently_failed(self, addon_id: str, table: dict[str, float] | None = None) -> bool:
        table = self._addon_failures if table is None else table
        with self._lock:
            at = table.get(addon_id)
            if at is None:
                return False
            if self._clock() - at >= ADDON_PENALTY_SECONDS:
                table.pop(addon_id, None)
                return False
            return True

    def _note_failure(self, addon_id: str, table: dict[str, float] | None = None) -> None:
        table = self._addon_failures if table is None else table
        with self._lock:
            table[addon_id] = self._clock()

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

    def library_listing(self) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
        """The account's library and its "Devam Et", from one read of it.

        The library is the list the account carries between devices: what was
        added to the library on a phone, and how far a film was watched on the
        television. It is not the appliance's own manifest, which is a
        different thing wearing the same word -- those titles are the ones the
        operator put on this box and are marked with the library addon's id so
        they resolve from disk rather than through an addon. These carry their
        real type instead, because that is how they resolve. Records the
        account has removed are dropped from it, and so are the temporary ones
        a client writes for a title that was played without being added.
        Ordered by when each was last watched, most recent first.

        "Devam Et" is the official clients' continue-watching list, worked out
        the way stremio-core works it out, so that it is the same list on this
        television as on the phone, the desktop and the web:
        `is_in_continue_watching` -- not "other", not live, `(!removed ||
        temp)`, a position past zero -- newest `_mtime` first, 100 of them.
        That keeps the temporary records the library drops, because a film
        played without being added to the library is still one being watched,
        and it asks nothing of the duration. It leaves out the series with new
        episodes the official clients add from their notifications, which this
        appliance does not fetch.
        """
        library: list[tuple[str, dict[str, Any]]] = []
        watching: list[tuple[str, dict[str, Any]]] = []
        for record in self.library():
            preview = _library_preview(record)
            if preview is None:
                continue
            removed, temp = bool(record.get("removed")), bool(record.get("temp"))
            if not removed and not temp:
                library.append((preview["state"]["lastWatched"] or "", preview))
            if _in_continue_watching(record, removed, temp):
                watching.append((_text(record.get("_mtime")) or "", preview))
        library.sort(key=lambda entry: entry[0], reverse=True)
        watching.sort(key=lambda entry: entry[0], reverse=True)
        return (
            [preview for _, preview in library],
            [preview for _, preview in watching[:CONTINUE_WATCHING_SIZE]],
        )
