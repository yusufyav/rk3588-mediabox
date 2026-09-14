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
import logging
import threading
import time
from dataclasses import dataclass, replace
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

        results: list[list[MetaPreview] | None] = [None] * len(jobs)
        answered = [False] * len(jobs)

        def ask(index: int, addon: Addon, catalog: Any) -> None:
            began = self._clock()
            try:
                results[index] = self.addons_client.catalog(addon, catalog.type, catalog.id)
            except UpstreamError as exc:
                LOG.info("Home row %s/%s unavailable: %s", addon.id, catalog.id, exc.message)
            except Exception as exc:  # a broken addon is not a broken appliance
                LOG.info("Home row %s/%s failed: %s", addon.id, catalog.id, exc)
            finally:
                # Answered, even if the answer was "no". A refusal that came
                # back in eighty milliseconds is not a reason to stop asking —
                # only a host that hangs is, and that is caught at the deadline
                # below. Penalising a fast error took thirteen shelves down to
                # twelve for five minutes because one catalogue 404ed once.
                answered[index] = True
                LOG.info(
                    "Home row %s/%s took %.0f ms",
                    addon.id,
                    catalog.id,
                    (self._clock() - began) * 1000.0,
                )

        # A fixed number of daemon threads taking work off a shared counter,
        # rather than one thread per catalogue. There are more catalogues than
        # there are addons — fourteen on this box — and threading only the first
        # eight would leave the rest to be fetched one after another on this
        # thread, which is the wait being removed.
        #
        # They are joined with a deadline and never shut down: a pool's shutdown
        # joins every worker, and a straggler must not be able to hold the home
        # screen.
        next_job = itertools.count()

        def work() -> None:
            for index in next_job:
                if index >= len(jobs):
                    return
                addon, catalog = jobs[index]
                ask(index, addon, catalog)

        deadline = self._clock() + HOME_DEADLINE
        threads = []
        for slot in range(min(HOME_FAN_OUT, len(jobs))):
            thread = threading.Thread(target=work, name=f"home-{slot}", daemon=True)
            thread.start()
            threads.append(thread)

        for thread in threads:
            thread.join(timeout=max(0.0, deadline - self._clock()))

        # Read once, so a straggler answering during the walk below cannot make
        # two calls with the same answers produce different screens.
        collected = list(results)

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
        for addon, streams in zip(addons, answers):
            for stream in streams:
                if stream.identity in seen:
                    continue
                seen.add(stream.identity)
                # Which addon answered. Fifty-five sources from two addons look
                # like one undifferentiated list until they can be separated,
                # and only the caller knows which addon was asked.
                found.append(replace(stream, addon_name=addon.name))
        return found

    def _gather_streams(
        self, addons: list[Addon], type_name: str, target: str
    ) -> list[list[Stream]]:
        """Ask every addon at once, and do not let one of them hold the rest.

        Asked one after another, the wait for a title was the *sum* of the
        addons: a single installed addon whose host had gone away — RARBG, in
        the case that made this obvious — spent fifteen seconds in a connect
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

    def library_previews(self) -> list[dict[str, Any]]:
        """The operator's own Stremio library, in the catalogue preview shape.

        This is the list the account carries between devices: what was added to
        the library on a phone, and how far a film was watched on the
        television. It is not the appliance's own manifest, which is a
        different thing wearing the same word — those titles are the ones the
        operator put on this box and are marked with the library addon's id so
        they resolve from disk rather than through an addon. These carry their
        real type instead, because that is how they resolve.

        Records the account has removed are dropped: Stremio keeps a tombstone
        rather than deleting, so a removed title comes back on every sync
        unless it is filtered here. So are the temporary entries Stremio writes
        while something is merely being previewed, which were never in the
        library to begin with.

        Ordered by when each was last watched, most recent first, so the rows
        built from this need no opinion of their own about order.
        """
        previews: list[dict[str, Any]] = []
        for record in self.library():
            if not isinstance(record, dict):
                continue
            if record.get("removed") or record.get("temp"):
                continue
            item_id = record.get("_id") or record.get("id")
            name = record.get("name")
            if not item_id or not name:
                continue
            state = record.get("state") if isinstance(record.get("state"), dict) else {}
            previews.append(
                {
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
            )
        previews.sort(key=lambda item: item["state"]["lastWatched"] or "", reverse=True)
        return previews
