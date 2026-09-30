"""Subtitle files as timelines, and timelines written back as files.

The sync engine needs one thing from a subtitle: when each line is on screen.
The player needs the file itself, styling and all. So a file is parsed into
its cues *and kept*: a correction rewrites the time stamps of the original
text and touches nothing else, which is what keeps an ASS file's styles,
positions and karaoke intact through a piecewise correction.

SRT, WebVTT, ASS and SSA are read. Frame-based formats (MicroDVD) are not: a
file whose times are frame numbers has no timeline until somebody says what
the frame rate was, and guessing it is the fault this engine exists to fix.
"""

from __future__ import annotations

import gzip
import re
from dataclasses import dataclass
from typing import Callable


#: A subtitle is text. Past this it is a fault, not a subtitle.
MAX_SUBTITLE_BYTES = 8 * 1024 * 1024

#: Code pages tried when a file is not UTF-8, by language. Subtitles for these
#: languages are still published in their old Windows code page often enough
#: that a wrong guess shows as mojibake on the television.
_LEGACY_ENCODINGS = {
    "tr": "cp1254", "tur": "cp1254",
    "ru": "cp1251", "rus": "cp1251", "uk": "cp1251", "ukr": "cp1251", "bg": "cp1251", "bul": "cp1251",
    "sr": "cp1250", "srp": "cp1250", "hr": "cp1250", "hrv": "cp1250", "pl": "cp1250", "pol": "cp1250",
    "cs": "cp1250", "cze": "cp1250", "ces": "cp1250", "hu": "cp1250", "hun": "cp1250", "ro": "cp1250",
    "rum": "cp1250", "ron": "cp1250", "sl": "cp1250", "slv": "cp1250",
    "el": "cp1253", "gre": "cp1253", "ell": "cp1253",
    "he": "cp1255", "heb": "cp1255",
    "ar": "cp1256", "ara": "cp1256", "fa": "cp1256", "per": "cp1256", "fas": "cp1256",
}


class SubtitleFormatError(ValueError):
    """The bytes are not a subtitle this module can read."""


@dataclass(frozen=True, slots=True)
class Cue:
    start: float
    end: float
    #: Where the cue's time stamps are in the document, as an index into
    #: `Document.lines`. Writing back replaces that line's times only.
    line: int

    @property
    def duration(self) -> float:
        return self.end - self.start


@dataclass(frozen=True, slots=True)
class Document:
    format: str
    lines: tuple[str, ...]
    cues: tuple[Cue, ...]
    #: For ASS/SSA: which comma-separated fields of a Dialogue line are the
    #: start and the end. Read from the file's own `Format:` line.
    ass_fields: tuple[int, int, int] | None = None
    #: Timed lines that end before they start. They are not cues and are left
    #: out of `cues`; how many there were says how the file was made -- a tool
    #: that wrote "[ Skipped item nr. 129 ]" at 03:07:18 --> 00:20:46 did not
    #: time it against this film.
    reversed_cues: int = 0

    @property
    def extension(self) -> str:
        return {"srt": "srt", "vtt": "vtt", "ass": "ass", "ssa": "ssa"}[self.format]

    @property
    def mime(self) -> str:
        return {
            "srt": "application/x-subrip",
            "vtt": "text/vtt",
            "ass": "text/x-ssa",
            "ssa": "text/x-ssa",
        }[self.format]

    def text(self) -> str:
        return "\n".join(self.lines) + "\n"

    def remap(self, mapping: Callable[[float, float], tuple[float, float] | None]) -> "Document":
        """The same document with every cue's times put through `mapping`.

        `mapping` receives a cue's start and end in the file's own time and
        answers the new pair, or None to leave the cue out -- a line from a
        scene the video does not have. Nothing but the times changes.
        """
        lines = list(self.lines)
        dropped: set[int] = set()
        cues: list[Cue] = []
        for cue in self.cues:
            mapped = mapping(cue.start, cue.end)
            if mapped is None:
                dropped.add(cue.line)
                continue
            start, end = max(0.0, mapped[0]), max(0.0, mapped[1])
            if end <= start:
                end = start + 0.05
            lines[cue.line] = _rewrite(self, lines[cue.line], start, end)
            cues.append(Cue(start, end, cue.line))
        if dropped:
            lines = _drop(self, lines, dropped)
            # Line numbers moved; the cues are re-read from what was written.
            return parse_text("\n".join(lines))
        return Document(self.format, tuple(lines), tuple(cues), self.ass_fields)


# ----------------------------------------------------------------- decoding

def decode(raw: bytes, language: str | None = None) -> str:
    """Bytes as text: gzip undone, a BOM honoured, a legacy code page guessed."""
    if raw[:2] == b"\x1f\x8b":
        try:
            raw = gzip.decompress(raw)
        except (OSError, EOFError) as exc:
            raise SubtitleFormatError("the subtitle is a broken gzip file") from exc
    if len(raw) > MAX_SUBTITLE_BYTES:
        raise SubtitleFormatError("the subtitle is larger than a subtitle can be")
    if raw.startswith(b"\xef\xbb\xbf"):
        return raw[3:].decode("utf-8", errors="replace")
    if raw.startswith((b"\xff\xfe", b"\xfe\xff")):
        return raw.decode("utf-16", errors="replace")
    try:
        return raw.decode("utf-8")
    except UnicodeDecodeError:
        pass
    legacy = _LEGACY_ENCODINGS.get((language or "").strip().lower()[:3], None) or _LEGACY_ENCODINGS.get(
        (language or "").strip().lower()[:2], "cp1252"
    )
    return raw.decode(legacy, errors="replace")


def parse(raw: bytes, language: str | None = None) -> Document:
    return parse_text(decode(raw, language))


def parse_text(text: str) -> Document:
    text = text.replace("\r\n", "\n").replace("\r", "\n").lstrip("﻿")
    head = text.lstrip()[:64]
    lines = tuple(text.split("\n"))
    while lines and lines[-1] == "":
        lines = lines[:-1]
    if head.startswith("WEBVTT"):
        return _parse_vtt(lines)
    if head.startswith("[Script Info]") or re.search(r"^\[Events\]", text, re.MULTILINE):
        return _parse_ass(lines)
    if _SRT_TIMING.search(text):
        return _parse_srt(lines)
    raise SubtitleFormatError("not a subtitle format this appliance reads (SRT, WebVTT, ASS, SSA)")


# --------------------------------------------------------------------- SRT

_SRT_TIME = r"(\d{1,3}):(\d{1,2}):(\d{1,2})(?:[,.](\d{1,3}))?"
_SRT_TIMING = re.compile(rf"^\s*{_SRT_TIME}\s*-->\s*{_SRT_TIME}", re.MULTILINE)


def _hms(h: str, m: str, s: str, frac: str | None) -> float:
    fraction = int(frac.ljust(3, "0")[:3]) / 1000.0 if frac else 0.0
    return int(h) * 3600 + int(m) * 60 + int(s) + fraction


def _parse_srt(lines: tuple[str, ...]) -> Document:
    cues: list[Cue] = []
    reversed_cues = 0
    for index, line in enumerate(lines):
        match = _SRT_TIMING.match(line)
        if not match:
            continue
        start = _hms(*match.group(1, 2, 3, 4))
        end = _hms(*match.group(5, 6, 7, 8))
        if end > start:
            cues.append(Cue(start, end, index))
        elif end < start:
            reversed_cues += 1
    if not cues:
        raise SubtitleFormatError("the SRT file has no cue with a valid time")
    return Document("srt", lines, tuple(cues), reversed_cues=reversed_cues)


def _srt_stamp(seconds: float, separator: str = ",") -> str:
    millis = int(round(max(0.0, seconds) * 1000))
    h, rest = divmod(millis, 3_600_000)
    m, rest = divmod(rest, 60_000)
    s, ms = divmod(rest, 1000)
    return f"{h:02d}:{m:02d}:{s:02d}{separator}{ms:03d}"


# ------------------------------------------------------------------ WebVTT

_VTT_TIME = r"(?:(\d{1,3}):)?(\d{1,2}):(\d{1,2})[.,](\d{1,3})"
_VTT_TIMING = re.compile(rf"^\s*{_VTT_TIME}\s*-->\s*{_VTT_TIME}(.*)$")


def _parse_vtt(lines: tuple[str, ...]) -> Document:
    cues: list[Cue] = []
    reversed_cues = 0
    for index, line in enumerate(lines):
        match = _VTT_TIMING.match(line)
        if not match:
            continue
        start = _hms(match.group(1) or "0", *match.group(2, 3, 4))
        end = _hms(match.group(5) or "0", *match.group(6, 7, 8))
        if end > start:
            cues.append(Cue(start, end, index))
        elif end < start:
            reversed_cues += 1
    if not cues:
        raise SubtitleFormatError("the WebVTT file has no cue with a valid time")
    return Document("vtt", lines, tuple(cues), reversed_cues=reversed_cues)


# ----------------------------------------------------------------- ASS/SSA

_ASS_STAMP = re.compile(r"^\s*(\d+):(\d{1,2}):(\d{1,2})(?:\.(\d{1,3}))?\s*$")


def _parse_ass(lines: tuple[str, ...]) -> Document:
    fmt = "ssa"
    fields: tuple[int, int, int] | None = None
    in_events = False
    cues: list[Cue] = []
    reversed_cues = 0
    for index, line in enumerate(lines):
        stripped = line.strip()
        if stripped.lower().startswith("scripttype:") and "+" in stripped:
            fmt = "ass"
        if stripped.startswith("["):
            in_events = stripped.lower() == "[events]"
            continue
        if not in_events:
            continue
        if stripped.lower().startswith("format:"):
            names = [name.strip().lower() for name in stripped.split(":", 1)[1].split(",")]
            if "start" in names and "end" in names:
                fields = (names.index("start"), names.index("end"), len(names))
            continue
        if not stripped.startswith("Dialogue:") or fields is None:
            continue
        parts = stripped.split(":", 1)[1].split(",", fields[2] - 1)
        if len(parts) < fields[2]:
            continue
        start, end = _ass_seconds(parts[fields[0]]), _ass_seconds(parts[fields[1]])
        if start is not None and end is not None and end > start:
            cues.append(Cue(start, end, index))
        elif start is not None and end is not None and end < start:
            reversed_cues += 1
    if fields is None:
        raise SubtitleFormatError("the ASS/SSA file has no [Events] Format line")
    if not cues:
        raise SubtitleFormatError("the ASS/SSA file has no dialogue with a valid time")
    return Document(fmt, lines, tuple(cues), fields, reversed_cues)


def _ass_seconds(value: str) -> float | None:
    match = _ASS_STAMP.match(value)
    if not match:
        return None
    return _hms(*match.group(1, 2, 3), (match.group(4) or "0").ljust(2, "0")[:2] + "0")


def _ass_stamp(seconds: float) -> str:
    centis = int(round(max(0.0, seconds) * 100))
    h, rest = divmod(centis, 360_000)
    m, rest = divmod(rest, 6000)
    s, cs = divmod(rest, 100)
    return f"{h:d}:{m:02d}:{s:02d}.{cs:02d}"


# ----------------------------------------------------------------- writing

def _rewrite(document: Document, line: str, start: float, end: float) -> str:
    if document.format == "srt":
        match = _SRT_TIMING.match(line)
        rest = line[match.end():] if match else ""
        return f"{_srt_stamp(start)} --> {_srt_stamp(end)}{rest}"
    if document.format == "vtt":
        match = _VTT_TIMING.match(line)
        settings = match.group(9) if match else ""
        return f"{_srt_stamp(start, '.')} --> {_srt_stamp(end, '.')}{settings}"
    assert document.ass_fields is not None
    start_at, end_at, count = document.ass_fields
    prefix, body = line.split(":", 1)
    leading = body[: len(body) - len(body.lstrip())]
    parts = body.lstrip().split(",", count - 1)
    parts[start_at] = _ass_stamp(start)
    parts[end_at] = _ass_stamp(end)
    return f"{prefix}:{leading}{','.join(parts)}"


def _drop(document: Document, lines: list[str], dropped: set[int]) -> list[str]:
    """Remove whole cues: the timing line and, for SRT/VTT, its block."""
    if document.format in ("ass", "ssa"):
        return [line for index, line in enumerate(lines) if index not in dropped]
    keep = [True] * len(lines)
    for index in dropped:
        # The block runs from the line before the timing (an SRT counter or a
        # VTT identifier, when there is one) to the next blank line.
        begin = index
        if index > 0 and lines[index - 1].strip() and not _SRT_TIMING.match(lines[index - 1]):
            begin = index - 1
        stop = index + 1
        while stop < len(lines) and lines[stop].strip():
            stop += 1
        for at in range(begin, min(stop + 1, len(lines))):
            keep[at] = False
    return [line for index, line in enumerate(lines) if keep[index]]
