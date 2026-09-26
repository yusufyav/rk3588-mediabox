"""Suggestions while typing, the way every Stremio client makes them.

Stremio does not ask an addon anything while a query is being typed. The
search box offers suggestions from a local index instead, and the addons are
asked once, when the search is actually made. That is stremio-core's
`LocalSearch` model: it fetches Cinemeta's `feed.json` -- a list of titles with
their rating and popularity -- builds an index over the names, and answers each
keystroke from it, five at a time (`maxResults: 5` in stremio-web's search
bar).

The index here is the same arithmetic as the `localsearch` crate stremio-core
builds it with, so the same letters suggest the same titles:

- names are tokenised by replacing `:` and `,` with spaces, removing `-`,
  splitting on whitespace and lower-casing;
- a query token is related to every indexed token it is a prefix of, and to
  every token within one edit of it;
- each related token scores a document `(1 + tf-idf) * distance boost * prefix
  boost * document boost`, where the distance boost is `(1 - distance) * 2 + 1`,
  the prefix boost `1.5 * len(query token) / len(token) + 1`, and the document
  boost `exp(0.5 * rating / best rating) * exp(0.5 * popularity / most popular)`;
- scores add up across tokens, the best `max_results` are kept, and anything at
  or below 48% of the best score is dropped.
"""

from __future__ import annotations

import bisect
import json
import logging
import math
import os
import threading
import time
from dataclasses import dataclass
from typing import Any, Callable

from ..errors import UpstreamError
from ..http import request

LOG = logging.getLogger(__name__)

#: stremio-core's `CINEMETA_CATALOGS_URL` joined with `CINEMETA_FEED_CATALOG_ID`.
FEED_URL = "https://cinemeta-catalogs.strem.io/feed.json"
#: The feed is several megabytes of JSON; the shared ceiling for an addon's
#: answer is lower than that on purpose, and this is the one caller that needs
#: more.
FEED_MAX_BYTES = 32 * 1024 * 1024
#: How long a fetched feed is used before it is fetched again. The official
#: clients fetch it once per start; this process runs for weeks.
FEED_MAX_AGE_SECONDS = 24 * 3600.0

#: stremio-web's search bar asks for this many.
MAX_RESULTS = 5

# The `localsearch` crate's defaults, and stremio-core's index options.
MAX_EDIT_DISTANCE = 1
MAX_EDIT_DISTANCE_BOOST = 2.0
MAX_PREFIX_BOOST = 1.5
SCORE_THRESHOLD = 0.48
IMDB_RATING_WEIGHT = 0.5
POPULARITY_WEIGHT = 0.5


def tokenize(text: str) -> list[str]:
    """The `localsearch` crate's `DefaultTokenizer`."""
    return text.replace(":", " ").replace(",", " ").replace("-", "").lower().split()


def _number(value: Any) -> float | None:
    try:
        number = float(value)
    except (TypeError, ValueError):
        return None
    return number if math.isfinite(number) else None


@dataclass(frozen=True, slots=True)
class Searchable:
    id: str
    name: str
    type: str
    poster: str | None = None
    imdb_rating: float | None = None
    popularity: float | None = None
    release_info: str | None = None

    def as_dict(self) -> dict[str, Any]:
        return {
            "id": self.id,
            "type": self.type,
            "name": self.name,
            "poster": self.poster,
            "imdbRating": None if self.imdb_rating is None else f"{self.imdb_rating:g}",
            "releaseInfo": self.release_info,
        }


def parse_feed(payload: Any) -> list[Searchable]:
    """The feed's entries, less the ones without a name (stremio-core drops those)."""
    if not isinstance(payload, list):
        raise UpstreamError("the search feed is not a list")
    found: list[Searchable] = []
    for entry in payload:
        if not isinstance(entry, dict):
            continue
        item_id, name, kind = entry.get("id"), entry.get("name"), entry.get("type")
        if not isinstance(item_id, str) or not isinstance(name, str) or not name.strip():
            continue
        if not isinstance(kind, str):
            continue
        poster = entry.get("poster")
        release = entry.get("releaseInfo")
        found.append(
            Searchable(
                id=item_id,
                name=name,
                type=kind,
                poster=poster if isinstance(poster, str) and poster else None,
                imdb_rating=_number(entry.get("imdbRating")),
                popularity=_number(entry.get("popularity")),
                release_info=str(release) if release not in (None, "") else None,
            )
        )
    return found


def _within_one_edit(a: str, b: str) -> int | None:
    """0 or 1 when `b` is that many edits from `a`, otherwise nothing."""
    if a == b:
        return 0
    la, lb = len(a), len(b)
    if abs(la - lb) > 1:
        return None
    if la == lb:
        diffs = [i for i in range(la) if a[i] != b[i]]
        if len(diffs) == 1:
            return 1
        # A transposition is two edits under Levenshtein, which is what the
        # crate's automaton counts.
        return None
    short, long_ = (a, b) if la < lb else (b, a)
    i = 0
    while i < len(short) and short[i] == long_[i]:
        i += 1
    return 1 if short[i:] == long_[i + 1 :] else None


class Index:
    def __init__(self, records: list[Searchable]) -> None:
        self.records = records
        best_rating = max((r.imdb_rating or 0.0 for r in records), default=0.0)
        most_popular = max((r.popularity or 0.0 for r in records), default=0.0)

        self.boosts: list[float] = []
        self.token_counts: list[int] = []
        postings: dict[str, dict[int, int]] = {}
        for doc_id, record in enumerate(records):
            rating_boost = (
                math.exp(record.imdb_rating / best_rating * IMDB_RATING_WEIGHT)
                if record.imdb_rating is not None and best_rating > 0
                else 1.0
            )
            popularity_boost = (
                math.exp(record.popularity / max(most_popular, 1.0) * POPULARITY_WEIGHT)
                if record.popularity is not None
                else 1.0
            )
            self.boosts.append(rating_boost * popularity_boost)
            tokens = tokenize(record.name)
            self.token_counts.append(max(1, len(tokens)))
            for token in tokens:
                counts = postings.setdefault(token, {})
                counts[doc_id] = counts.get(doc_id, 0) + 1
        self.postings = postings
        self.tokens = sorted(postings)
        self.by_length: dict[int, list[str]] = {}
        for token in self.tokens:
            self.by_length.setdefault(len(token), []).append(token)

    def _related(self, query_token: str) -> dict[str, tuple[int | None, float | None]]:
        related: dict[str, tuple[int | None, float | None]] = {}
        length = len(query_token)
        for size in (length - 1, length, length + 1):
            for token in self.by_length.get(size, ()):
                distance = _within_one_edit(query_token, token)
                if distance is not None and distance <= MAX_EDIT_DISTANCE:
                    related[token] = (distance, None)
        start = bisect.bisect_left(self.tokens, query_token)
        for token in self.tokens[start:]:
            if not token.startswith(query_token):
                break
            distance = related.get(token, (None, None))[0]
            related[token] = (distance, length / len(token))
        return related

    def search(self, query: str, max_results: int = MAX_RESULTS) -> list[Searchable]:
        scores: dict[int, float] = {}
        documents = len(self.records)
        for query_token in tokenize(query):
            for token, (distance, prefix_ratio) in self._related(query_token).items():
                distance_boost = (
                    1.0
                    if distance is None
                    else (MAX_EDIT_DISTANCE - distance) * MAX_EDIT_DISTANCE_BOOST + 1.0
                )
                prefix_boost = 1.0 if prefix_ratio is None else prefix_ratio * MAX_PREFIX_BOOST + 1.0
                pairs = self.postings[token]
                idf = math.log10(documents / len(pairs))
                for doc_id, occurrences in pairs.items():
                    tf_idf = occurrences / self.token_counts[doc_id] * idf
                    score = (1.0 + tf_idf) * distance_boost * prefix_boost * self.boosts[doc_id]
                    scores[doc_id] = scores.get(doc_id, 0.0) + score
        ranked = sorted(scores.items(), key=lambda pair: pair[1], reverse=True)[:max_results]
        if not ranked:
            return []
        floor = ranked[0][1] * SCORE_THRESHOLD
        return [self.records[doc_id] for doc_id, score in ranked if score > floor]


def _fetch_feed() -> Any:
    response = request(FEED_URL, max_bytes=FEED_MAX_BYTES, timeout=20.0)
    if response.status >= 400:
        raise UpstreamError(f"{FEED_URL} returned HTTP {response.status}")
    return response.json()


class LocalSearch:
    """The index, fetched once and kept, refreshed in the background.

    A suggestion must never wait for the network. Until the feed has arrived
    the answer is no suggestions -- which is what stremio-core answers too --
    and the fetch that fills it runs on its own thread.
    """

    def __init__(
        self,
        cache_path: str | None = None,
        *,
        fetch: Callable[[], Any] = _fetch_feed,
        clock: Callable[[], float] = time.time,
    ) -> None:
        self._cache_path = cache_path
        self._fetch = fetch
        self._clock = clock
        self._lock = threading.Lock()
        self._index: Index | None = None
        self._fetched_at: float | None = None
        self._loading = False
        self._read_cache = False

    def suggest(self, query: str, max_results: int = MAX_RESULTS) -> list[Searchable]:
        self._ensure_fresh()
        with self._lock:
            index = self._index
        if index is None or not query.strip():
            return []
        return index.search(query, max_results)

    # ----------------------------------------------------------------- loading

    def _ensure_fresh(self) -> None:
        with self._lock:
            if not self._read_cache:
                self._read_cache = True
                self._load_cache_locked()
            stale = (
                self._fetched_at is None
                or self._clock() - self._fetched_at >= FEED_MAX_AGE_SECONDS
            )
            if not stale or self._loading:
                return
            self._loading = True
        threading.Thread(target=self._refresh, name="local-search-feed", daemon=True).start()

    def _load_cache_locked(self) -> None:
        if not self._cache_path:
            return
        try:
            with open(self._cache_path, "rb") as handle:
                payload = json.load(handle)
            fetched_at = os.stat(self._cache_path).st_mtime
        except FileNotFoundError:
            return
        except (OSError, ValueError) as exc:
            LOG.info("The cached search feed is unusable: %s", exc)
            return
        try:
            self._index = Index(parse_feed(payload))
            self._fetched_at = fetched_at
        except UpstreamError as exc:
            LOG.info("The cached search feed is unusable: %s", exc.message)

    def _refresh(self) -> None:
        began = time.monotonic()
        try:
            payload = self._fetch()
            index = Index(parse_feed(payload))
        except UpstreamError as exc:
            LOG.info("The search feed could not be fetched: %s", exc.message)
            with self._lock:
                self._loading = False
            return
        except Exception as exc:  # a bad feed is no suggestions, not a crash
            LOG.info("The search feed could not be read: %s", exc)
            with self._lock:
                self._loading = False
            return
        with self._lock:
            self._index = index
            self._fetched_at = self._clock()
            self._loading = False
        LOG.info(
            "Search feed: %d titles indexed in %.0f ms",
            len(index.records),
            (time.monotonic() - began) * 1000.0,
        )
        self._write_cache(payload)

    def _write_cache(self, payload: Any) -> None:
        if not self._cache_path:
            return
        partial = f"{self._cache_path}.partial"
        try:
            with open(partial, "w", encoding="utf-8") as handle:
                json.dump(payload, handle, separators=(",", ":"))
            os.replace(partial, self._cache_path)
        except OSError as exc:
            LOG.info("The search feed could not be cached: %s", exc)
