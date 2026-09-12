"""The appliance's own library.

Everything else the media core knows about comes from a Stremio addon. A
library is the other half of a media appliance: the titles this particular box
holds or has been pointed at, declared by whoever owns it.

The manifest is operator-supplied configuration, not addon data, so it is read
strictly: an entry that does not parse is dropped rather than guessed at, and
every source keeps the same shape an addon stream has. That is what lets one
renderer draw a library title and a catalogue title the same way, and it is why
a library source goes through exactly the same inspection, policy and session
path as anything else — nothing here is a shortcut around the media core.
"""

from __future__ import annotations

import json
import logging
from dataclasses import dataclass
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit


LOG = logging.getLogger(__name__)

#: Identifier of the library "addon". The UI shows provenance, and a library
#: title is not from Cinemeta; saying so keeps the two apart on screen.
LIBRARY_ADDON_ID = "mediabox.library"
LIBRARY_ADDON_NAME = "MediaBox Library"

MAX_MANIFEST_BYTES = 1024 * 1024
_ALLOWED_SCHEMES = ("http", "https", "file")


def _text(value: Any) -> str | None:
    return value.strip() or None if isinstance(value, str) else None


def _string_list(value: Any) -> list[str]:
    if not isinstance(value, list):
        return []
    return [item.strip() for item in value if isinstance(item, str) and item.strip()]


@dataclass(frozen=True, slots=True)
class LibrarySource:
    identity: str
    name: str
    title: str | None
    url: str

    def as_stream(self) -> dict[str, Any]:
        """The shape an addon stream has, so one renderer draws both."""
        return {
            "kind": "http",
            "identity": self.identity,
            "addonId": LIBRARY_ADDON_ID,
            "name": self.name,
            "title": self.title,
            "description": None,
            "url": self.url,
            "infoHash": None,
            "fileIdx": None,
            "ytId": None,
            "externalUrl": None,
            "behaviorHints": {},
            "playable": True,
        }


@dataclass(frozen=True, slots=True)
class LibraryItem:
    id: str
    type: str
    name: str
    poster: str | None
    background: str | None
    logo: str | None
    description: str | None
    release_info: str | None
    runtime: str | None
    genres: tuple[str, ...]
    sources: tuple[LibrarySource, ...]

    def as_preview(self) -> dict[str, Any]:
        return {
            "id": self.id,
            "type": self.type,
            "name": self.name,
            "poster": self.poster,
            "background": self.background,
            "logo": self.logo,
            "description": self.description,
            "releaseInfo": self.release_info,
            "imdbRating": None,
            "genres": list(self.genres),
            "addonId": LIBRARY_ADDON_ID,
        }

    def as_meta(self) -> dict[str, Any]:
        meta = self.as_preview()
        meta.update({"runtime": self.runtime, "cast": [], "director": [], "writer": [], "videos": []})
        return meta


def _parse_source(entry: Any, item_id: str, index: int) -> LibrarySource | None:
    if not isinstance(entry, dict):
        return None
    url = _text(entry.get("url"))
    if url is None:
        return None
    # Scheme is checked here so a bad manifest fails at load rather than at
    # play time. The full destination policy still runs when a session is
    # created; this is the cheap, early half of it.
    if urlsplit(url).scheme not in _ALLOWED_SCHEMES:
        LOG.warning("Library source %r for %r has an unusable scheme", url, item_id)
        return None
    return LibrarySource(
        identity=_text(entry.get("identity")) or f"library:{item_id}:{index}",
        name=_text(entry.get("name")) or "Yerel kaynak",
        title=_text(entry.get("title")),
        url=url,
    )


def _parse_item(entry: Any) -> LibraryItem | None:
    if not isinstance(entry, dict):
        return None
    item_id = _text(entry.get("id"))
    name = _text(entry.get("name"))
    if item_id is None or name is None:
        return None
    sources = tuple(
        source
        for index, raw in enumerate(entry.get("sources") or [])
        if (source := _parse_source(raw, item_id, index)) is not None
    )
    if not sources:
        # A library entry with nothing to play is a broken entry, and showing
        # it would be exactly the placeholder catalogue this product must not
        # have.
        LOG.warning("Library entry %r has no usable source and was dropped", item_id)
        return None
    return LibraryItem(
        id=item_id,
        type=_text(entry.get("type")) or "movie",
        name=name,
        poster=_text(entry.get("poster")),
        background=_text(entry.get("background")),
        logo=_text(entry.get("logo")),
        description=_text(entry.get("description")),
        release_info=_text(entry.get("releaseInfo")),
        runtime=_text(entry.get("runtime")),
        genres=tuple(_string_list(entry.get("genres"))),
        sources=sources,
    )


class Library:
    """The manifest, re-read when it changes on disk."""

    def __init__(self, path: str | None) -> None:
        self._path = Path(path) if path else None
        self._items: tuple[LibraryItem, ...] = ()
        self._stamp: tuple[int, int] | None = None
        self._loaded = False

    @property
    def configured(self) -> bool:
        return self._path is not None

    def items(self) -> tuple[LibraryItem, ...]:
        if self._path is None:
            return ()
        try:
            status = self._path.stat()
        except OSError:
            self._items, self._stamp, self._loaded = (), None, True
            return ()
        stamp = (status.st_mtime_ns, status.st_size)
        if self._loaded and stamp == self._stamp:
            return self._items
        self._items = self._read(status.st_size)
        self._stamp = stamp
        self._loaded = True
        return self._items

    def get(self, item_id: str) -> LibraryItem | None:
        return next((item for item in self.items() if item.id == item_id), None)

    def _read(self, size: int) -> tuple[LibraryItem, ...]:
        assert self._path is not None
        if size > MAX_MANIFEST_BYTES:
            LOG.error("Library manifest %s is too large to read", self._path)
            return ()
        try:
            document = json.loads(self._path.read_text(encoding="utf-8"))
        except (OSError, ValueError) as exc:
            LOG.error("Library manifest %s could not be read: %s", self._path, exc)
            return ()
        entries = document.get("items") if isinstance(document, dict) else document
        if not isinstance(entries, list):
            LOG.error("Library manifest %s has no items array", self._path)
            return ()
        parsed = [item for entry in entries if (item := _parse_item(entry)) is not None]
        seen: set[str] = set()
        unique: list[LibraryItem] = []
        for item in parsed:
            if item.id in seen:
                LOG.warning("Library entry %r is declared twice; keeping the first", item.id)
                continue
            seen.add(item.id)
            unique.append(item)
        return tuple(unique)

    def as_row(self) -> dict[str, Any]:
        """The library as one home-surface row, in the catalogue row shape."""
        return {
            "addonId": LIBRARY_ADDON_ID,
            "addonName": LIBRARY_ADDON_NAME,
            "catalogId": "library",
            "type": "library",
            "name": "Kitaplık",
            "items": [item.as_preview() for item in self.items()],
        }
