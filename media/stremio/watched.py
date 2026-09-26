"""Which episodes of a series have been watched, as the account records it.

The account keeps this per title, in the library record's `state.watched`,
in the format stremio-core's `stremio-watched-bitfield` crate writes:

    {anchor video id}:{anchor length}:{base64 of the zlib-compressed bits}

Bit `i` is episode `i` of the title's videos, ordered by season, then episode,
then release date (`LibraryItemState::watched_bitfield`). The anchor is the
last watched episode and how long the list was up to it, so that a list that
has grown or shifted since -- a new season, a special added in front -- can be
realigned rather than misread: the bits are shifted by however far the anchor
has moved.
"""

from __future__ import annotations

import base64
import binascii
import zlib
from datetime import datetime
from typing import Iterable

from .models import Video


def _released_millis(video: Video) -> float:
    if not video.released:
        return float("-inf")
    try:
        return datetime.fromisoformat(video.released.replace("Z", "+00:00")).timestamp() * 1000.0
    except ValueError:
        return float("-inf")


def ordered_video_ids(videos: Iterable[Video]) -> list[str]:
    """The order the bits are in: season, episode, release date."""

    def key(video: Video) -> tuple[float, float, float]:
        season = float(video.season) if video.season is not None else float("-inf")
        episode = float(video.episode) if video.episode is not None else float("-inf")
        return (season, episode, _released_millis(video))

    # Python's sort is stable, as `sorted_by` is.
    return [video.id for video in sorted(videos, key=key)]


class WatchedBitField:
    def __init__(self, values: bytearray, video_ids: list[str]) -> None:
        self.values = values
        self.video_ids = video_ids

    @classmethod
    def empty(cls, video_ids: list[str]) -> "WatchedBitField":
        return cls(bytearray((len(video_ids) + 7) // 8), video_ids)

    @classmethod
    def parse(cls, serialized: str | None, video_ids: list[str]) -> "WatchedBitField":
        """The account's field realigned to `video_ids`; nothing watched if unreadable."""
        if not isinstance(serialized, str):
            return cls.empty(video_ids)
        parts = serialized.split(":")
        if len(parts) < 3:
            return cls.empty(video_ids)
        encoded = parts.pop()
        try:
            anchor_length = int(parts.pop())
            values = bytearray(zlib.decompress(base64.b64decode(encoded)))
        except (ValueError, binascii.Error, zlib.error):
            return cls.empty(video_ids)
        anchor_video = ":".join(parts)

        if anchor_video not in video_ids:
            return cls.empty(video_ids)
        offset = anchor_length - video_ids.index(anchor_video) - 1
        stored = cls(values, video_ids)
        if offset == 0:
            needed = (len(video_ids) + 7) // 8
            if len(values) < needed:
                values.extend(bytes(needed - len(values)))
            return stored
        realigned = cls.empty(video_ids)
        stored_length = len(values) * 8
        for index in range(len(video_ids)):
            previous = index + offset
            if 0 <= previous < stored_length:
                realigned.set(index, stored.get(previous))
        return realigned

    def get(self, index: int) -> bool:
        byte = index // 8
        return byte < len(self.values) and bool((self.values[byte] >> (index % 8)) & 1)

    def set(self, index: int, watched: bool) -> None:
        byte = index // 8
        if byte >= len(self.values):
            self.values.extend(bytes(byte - len(self.values) + 1))
        mask = 1 << (index % 8)
        if watched:
            self.values[byte] |= mask
        else:
            self.values[byte] &= ~mask & 0xFF

    def get_video(self, video_id: str) -> bool:
        try:
            return self.get(self.video_ids.index(video_id))
        except ValueError:
            return False

    def set_video(self, video_id: str, watched: bool) -> None:
        if video_id in self.video_ids:
            self.set(self.video_ids.index(video_id), watched)

    def watched_ids(self) -> list[str]:
        return [video_id for index, video_id in enumerate(self.video_ids) if self.get(index)]

    def serialize(self) -> str:
        """The account's format, anchored on the last watched episode."""
        last = next(
            (index for index in range(len(self.video_ids) - 1, -1, -1) if self.get(index)),
            0,
        )
        anchor = self.video_ids[last] if last < len(self.video_ids) else "undefined"
        packed = base64.b64encode(zlib.compress(bytes(self.values), 6)).decode("ascii")
        return f"{anchor}:{last + 1}:{packed}"
