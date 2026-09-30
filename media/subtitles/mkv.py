"""A Matroska file's index, read without reading the file.

What the pre-filter needs from a video is small: its frame rate and when each
embedded subtitle track has an event. mkvmerge writes both where they can be
reached with a few range requests -- the video track's `DefaultDuration` in
`Tracks` at the head, and a `CuePoint` per subtitle block in `Cues`, usually
at the tail. A 29 GB remux answered with about 1.3 MB read.

Every byte comes through a `Ranges` with a client-side budget. A server's
`Content-Length` is never trusted: one CDN answered `Range: bytes=0-0` with
`206`, `Content-Range: bytes 0-0/29159331995` and `Content-Length:
29159331995`; a client that believed it waited for 29 GB. Each read here asks
for a bounded span, takes at most that many bytes, and closes.
"""

from __future__ import annotations

import os
import sys
from array import array
from dataclasses import dataclass, field
from typing import Protocol

from ..errors import MediaError
from ..http import request

#: The most one video's index may cost, over all of its reads.
INDEX_BUDGET = 6 * 1024 * 1024
#: What is read first: the EBML header, SeekHead, Info and, nearly always,
#: Tracks.
HEAD_BYTES = 64 * 1024
#: The largest element fetched whole. Cues past it are not read at all.
MAX_ELEMENT_BYTES = 4 * 1024 * 1024
READ_TIMEOUT = 10.0

SEGMENT = 0x18538067
SEEK_HEAD = 0x114D9B74
SEEK = 0x4DBB
SEEK_ID = 0x53AB
SEEK_POSITION = 0x53AC
INFO = 0x1549A966
TIMESTAMP_SCALE = 0x2AD7B1
DURATION = 0x4489
TRACKS = 0x1654AE6B
TRACK_ENTRY = 0xAE
TRACK_NUMBER = 0xD7
TRACK_TYPE = 0x83
CODEC_ID = 0x86
LANGUAGE = 0x22B59C
LANGUAGE_BCP47 = 0x22B59D
NAME = 0x536E
FLAG_DEFAULT = 0x88
FLAG_FORCED = 0x55AA
DEFAULT_DURATION = 0x23E383
CUES = 0x1C53BB6B
CUE_POINT = 0xBB
CUE_TIME = 0xB3
CUE_TRACK_POSITIONS = 0xB7
CUE_TRACK = 0xF7
CLUSTER = 0x1F43B675

VIDEO, AUDIO, SUBTITLE = 1, 2, 17


class IndexUnavailable(MediaError):
    """The index cannot be read within the rules; the reason says which."""

    def __init__(self, reason: str) -> None:
        super().__init__("SUBTITLE_REFERENCE_UNAVAILABLE", reason, 422)
        self.reason = reason


class Ranges(Protocol):
    size: int
    spent: int

    def read(self, offset: int, length: int) -> bytes: ...


class _Budget:
    def __init__(self, budget: int) -> None:
        self.budget = budget
        self.spent = 0

    def claim(self, length: int) -> None:
        if length <= 0:
            raise IndexUnavailable("empty-read")
        if self.spent + length > self.budget:
            raise IndexUnavailable("byte-budget")

    def add(self, received: int) -> None:
        self.spent += received


class HttpRanges(_Budget):
    """Range reads of an HTTP source, each capped by the client."""

    def __init__(self, url: str, size: int, *, budget: int = INDEX_BUDGET, timeout: float = READ_TIMEOUT) -> None:
        super().__init__(budget)
        self.url = url
        self.size = size
        self.timeout = timeout

    def read(self, offset: int, length: int) -> bytes:
        length = min(length, self.size - offset)
        self.claim(length)
        try:
            # `max_bytes` is the cap: the body is read up to it (and one byte
            # more) and the connection closed, whatever the server said it
            # would send.
            response = request(
                self.url,
                headers={"Range": f"bytes={offset}-{offset + length - 1}", "Accept-Encoding": "identity"},
                timeout=self.timeout,
                max_bytes=length,
                truncate=True,
            )
        except MediaError as exc:
            self.add(length + 1)
            raise IndexUnavailable(f"read-failed: {exc.message}") from exc
        self.add(len(response.body) + 1)
        if response.status != 206:
            raise IndexUnavailable(f"no-range-support: HTTP {response.status}")
        answered = response.headers.get("content-range", "")
        if not answered.startswith(f"bytes {offset}-"):
            raise IndexUnavailable("range-mismatch")
        if len(response.body) != length:
            raise IndexUnavailable("short-read")
        return response.body


def remote_size(url: str, *, timeout: float = READ_TIMEOUT) -> int:
    """A remote file's size, from a one-byte range read's `Content-Range`.

    One byte is asked for and at most one is read, whatever `Content-Length`
    says -- this is the request the 29 GB answer came to.
    """
    try:
        response = request(
            url,
            headers={"Range": "bytes=0-0", "Accept-Encoding": "identity"},
            timeout=timeout,
            max_bytes=1,
            truncate=True,
        )
    except MediaError as exc:
        raise IndexUnavailable(f"read-failed: {exc.message}") from exc
    total = response.headers.get("content-range", "").rpartition("/")[2].strip()
    if response.status != 206 or not total.isdigit():
        raise IndexUnavailable(f"no-range-support: HTTP {response.status}")
    return int(total)


class FileRanges(_Budget):
    def __init__(self, path: str, *, budget: int = INDEX_BUDGET) -> None:
        super().__init__(budget)
        self.path = path
        self.size = os.path.getsize(path)

    def read(self, offset: int, length: int) -> bytes:
        length = min(length, self.size - offset)
        self.claim(length)
        with open(self.path, "rb") as handle:
            handle.seek(offset)
            data = handle.read(length)
        self.add(len(data))
        return data


# ------------------------------------------------------------------ EBML

def _vint(data: bytes, at: int, keep_marker: bool = False) -> tuple[int, int]:
    first = data[at]
    length, mask = 1, 0x80
    while length <= 8 and not first & mask:
        length += 1
        mask >>= 1
    if length > 8:
        raise ValueError("not an EBML number")
    value = first if keep_marker else first & (mask - 1)
    for k in range(1, length):
        value = (value << 8) | data[at + k]
    return value, length


def _elements(data: bytes, start: int, end: int):
    """(id, data start, data size or None if unknown) of each child."""
    at = start
    while at < end:
        try:
            eid, a = _vint(data, at, True)
            size, b = _vint(data, at + a)
        except (IndexError, ValueError):
            return
        unknown = size == (1 << (7 * b)) - 1
        begin = at + a + b
        yield eid, begin, None if unknown else size
        if unknown:
            return
        at = begin + size


def _uint(data: bytes, at: int, size: int) -> int:
    return int.from_bytes(data[at : at + size], "big")


def _float(data: bytes, at: int, size: int) -> float:
    """A big-endian IEEE float of 4 or 8 bytes."""
    value = array("f" if size == 4 else "d", data[at : at + size])
    if sys.byteorder == "little":
        value.byteswap()
    return value[0]


def _text(data: bytes, at: int, size: int) -> str:
    return data[at : at + size].rstrip(b"\0").decode("utf-8", "replace")


# ----------------------------------------------------------------- index

@dataclass(slots=True)
class Track:
    number: int
    type: int
    codec: str = ""
    language: str | None = None
    name: str | None = None
    default: bool = True
    forced: bool = False
    default_duration_ns: int | None = None


@dataclass(slots=True)
class ContainerIndex:
    duration: float | None
    tracks: list[Track]
    #: CuePoint times per track number, in seconds, sorted.
    cues: dict[int, list[float]] = field(default_factory=dict)
    bytes_read: int = 0


def read_index(ranges: Ranges) -> ContainerIndex:
    head = ranges.read(0, min(HEAD_BYTES, ranges.size))
    top = next(_elements(head, 0, len(head)), None)
    if top is None or top[0] != 0x1A45DFA3:
        raise IndexUnavailable("not-matroska")
    segment = next(((b, s) for e, b, s in _elements(head, 0, len(head)) if e == SEGMENT), None)
    if segment is None:
        raise IndexUnavailable("no-segment")
    seg = segment[0]
    seek: dict[int, int] = {}
    scale = 1_000_000
    duration_ticks: float | None = None
    tracks: list[Track] | None = None
    for eid, begin, size in _elements(head, seg, len(head)):
        if size is None or begin + size > len(head):
            break
        if eid == SEEK_HEAD:
            _read_seek_head(head, begin, size, seg, seek)
        elif eid == INFO:
            scale, duration_ticks = _read_info(head, begin, size)
        elif eid == TRACKS:
            tracks = _read_tracks(head, begin, size)
        elif eid == CLUSTER:
            break
    if SEEK_HEAD in seek and seek[SEEK_HEAD] >= len(head):
        data, begin, size = _element(ranges, seek[SEEK_HEAD], SEEK_HEAD)
        _read_seek_head(data, begin, size, seg, seek)
    if tracks is None:
        if TRACKS not in seek:
            raise IndexUnavailable("no-tracks")
        data, begin, size = _element(ranges, seek[TRACKS], TRACKS)
        tracks = _read_tracks(data, begin, size)
    if duration_ticks is None and INFO in seek:
        data, begin, size = _element(ranges, seek[INFO], INFO)
        scale, duration_ticks = _read_info(data, begin, size)
    if CUES not in seek:
        raise IndexUnavailable("no-cues")
    data, begin, size = _element(ranges, seek[CUES], CUES)
    cues: dict[int, list[float]] = {}
    for eid, point, length in _elements(data, begin, begin + size):
        if eid != CUE_POINT or length is None:
            continue
        time = None
        numbers: list[int] = []
        for child, at, n in _elements(data, point, point + length):
            if child == CUE_TIME and n:
                time = _uint(data, at, n)
            elif child == CUE_TRACK_POSITIONS and n:
                numbers.extend(_uint(data, a, m) for c, a, m in _elements(data, at, at + n) if c == CUE_TRACK and m)
        if time is None:
            continue
        for number in numbers:
            cues.setdefault(number, []).append(time * scale / 1e9)
    for times in cues.values():
        times.sort()
    duration = duration_ticks * scale / 1e9 if duration_ticks else None
    return ContainerIndex(duration, tracks, cues, ranges.spent)


def _element(ranges: Ranges, position: int, expected: int) -> tuple[bytes, int, int]:
    """The element at `position`, read whole, if it is the expected one and
    not larger than `MAX_ELEMENT_BYTES`."""
    if position >= ranges.size:
        raise IndexUnavailable("seek-outside-file")
    header = ranges.read(position, min(16, ranges.size - position))
    found = next(_elements(header, 0, len(header)), None)
    if found is None or found[0] != expected or found[2] is None:
        raise IndexUnavailable("seek-mismatch")
    begin, size = found[1], found[2]
    if begin + size > MAX_ELEMENT_BYTES:
        raise IndexUnavailable("element-too-large")
    data = ranges.read(position, begin + size)
    return data, begin, size


def _read_seek_head(data: bytes, begin: int, size: int, segment: int, into: dict[int, int]) -> None:
    for eid, at, n in _elements(data, begin, begin + size):
        if eid != SEEK or n is None:
            continue
        sid = position = None
        for child, a, m in _elements(data, at, at + n):
            if child == SEEK_ID and m:
                sid = _uint(data, a, m)
            elif child == SEEK_POSITION and m:
                position = _uint(data, a, m)
        if sid is not None and position is not None:
            into.setdefault(sid, segment + position)


def _read_info(data: bytes, begin: int, size: int) -> tuple[int, float | None]:
    scale, duration = 1_000_000, None
    for eid, at, n in _elements(data, begin, begin + size):
        if eid == TIMESTAMP_SCALE and n:
            scale = _uint(data, at, n)
        elif eid == DURATION and n in (4, 8):
            duration = _float(data, at, n)
    return scale, duration


def _read_tracks(data: bytes, begin: int, size: int) -> list[Track]:
    found: list[Track] = []
    for eid, at, n in _elements(data, begin, begin + size):
        if eid != TRACK_ENTRY or n is None:
            continue
        track = Track(0, 0, language="eng")
        for child, a, m in _elements(data, at, at + n):
            if m is None:
                continue
            if child == TRACK_NUMBER:
                track.number = _uint(data, a, m)
            elif child == TRACK_TYPE:
                track.type = _uint(data, a, m)
            elif child == CODEC_ID:
                track.codec = _text(data, a, m)
            elif child == LANGUAGE:
                track.language = _text(data, a, m)
            elif child == LANGUAGE_BCP47:
                track.language = _text(data, a, m) or track.language
            elif child == NAME:
                track.name = _text(data, a, m)
            elif child == FLAG_DEFAULT:
                track.default = bool(_uint(data, a, m))
            elif child == FLAG_FORCED:
                track.forced = bool(_uint(data, a, m))
            elif child == DEFAULT_DURATION:
                track.default_duration_ns = _uint(data, a, m)
        found.append(track)
    return found


# ------------------------------------------------------------- readings

#: The share of the video's indexed keyframes that have to sit on a rate's
#: frame grid for that rate to be believed. CALIBRATION NEEDED: four remuxes
#: measured 98.8-100 %; the wrong rates 2-9 %.
GRID_AGREEMENT = 0.95


def video_rate(index: ContainerIndex, candidates: tuple[float, ...]) -> float | None:
    """The video's frame rate: its `DefaultDuration`, confirmed by the frame
    grid its indexed keyframes sit on; or the grid alone. None if they
    disagree or nothing can be said."""
    video = next((t for t in index.tracks if t.type == VIDEO), None)
    if video is None:
        return None
    keyframes = index.cues.get(video.number, [])

    def on_grid(rate: float) -> float:
        if len(keyframes) < 20:
            return 0.0
        frame = 1.0 / rate
        # The index is in milliseconds (the usual TimestampScale): half of one
        # is what rounding may leave.
        return sum(1 for t in keyframes if abs(t - round(t / frame) * frame) <= 0.0006) / len(keyframes)

    declared = None
    if video.default_duration_ns:
        measured = 1e9 / video.default_duration_ns
        declared = min(candidates, key=lambda rate: abs(rate / measured - 1.0))
        if abs(declared / measured - 1.0) > 2e-4:
            declared = None
    if declared is not None:
        if not keyframes or on_grid(declared) >= GRID_AGREEMENT:
            return declared
        return None
    fitting = [rate for rate in candidates if on_grid(rate) >= GRID_AGREEMENT]
    return fitting[0] if len(fitting) == 1 else None


#: Codec ids that are subtitles, and what their blocks mark.
PICTURE_CODECS = ("S_HDMV/PGS", "S_VOBSUB", "S_DVBSUB")
TEXT_CODECS = ("S_TEXT/", "S_ASS", "S_SSA")


def subtitle_kind(codec: str) -> str | None:
    if codec.startswith(PICTURE_CODECS):
        return "picture"
    if codec.startswith(TEXT_CODECS):
        return "text"
    return None
