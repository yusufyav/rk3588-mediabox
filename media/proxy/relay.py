"""Serve a remote source to a local player over plain HTTP.

The appliance's own ffmpeg is built for the Rockchip decoder and carries no
TLS, so a player built against it can open `http://` and not `https://`. Every
source worth playing arrives as an HTTPS link.

Rather than rebuild that ffmpeg around a certificate stack it does not
otherwise need, the worker — which already speaks HTTPS — hands the bytes over
the loopback. The player opens a local address; what is on the other side is
the worker's problem, which is where it belongs.

Range requests are passed through both ways. Without them a player can start a
film and never seek in it, which is not a film player.
"""

from __future__ import annotations

import logging
import mimetypes
import os
import re
import time
import urllib.error
import urllib.parse
import urllib.request
from typing import Iterator

from ..errors import MediaError


LOG = logging.getLogger(__name__)

#: Read size. Large enough that a 4K stream is not a syscall storm, small
#: enough that abandoning a seek does not first copy a megabyte nobody wants.
CHUNK_BYTES = 256 * 1024

#: Headers worth carrying back to the player. Everything else upstream sends is
#: about its own transport and means nothing on this side.
PASSED_BACK = ("content-type", "content-length", "content-range", "accept-ranges")


#: `bytes=START-END`, either end allowed to be absent.
RANGE = re.compile(r"^bytes=(\d*)-(\d*)$")
#: A 206's `Content-Range: bytes START-END/TOTAL` (TOTAL may be `*`).
CONTENT_RANGE = re.compile(r"^bytes (\d+)-(\d+)/(\d+|\*)$")

#: How often an open that failed on the way is tried again before the player
#: is told the source is gone, and how long is waited before each try. A seek
#: is a new request through the addon's resolver and the debrid host; one of
#: them answering 5xx or not at all once ended a film at 28:26 ("kaynak
#: okunamadı") that the next request would have played on.
RETRY_DELAYS = (0.5, 1.5)
#: Upstream answers worth another try: the host was busy, not the request wrong.
TRANSIENT_STATUS = frozenset({408, 425, 429, 500, 502, 503, 504})
#: How often one reply is picked up again after its upstream connection ended
#: early, before it is let end and the player reconnects on its own.
MAX_RESUMES = 5


def _relay_file(url: str, range_header: str | None):
    """Serve a `file://` source the way an HTTP one is served.

    urllib answers a file:// URL through its own file handler, and that reply
    has no status line: `response.status` is None. That None went straight into
    `send_response()`, which wants a number, and the worker died mid-reply with
    a TypeError -- the player saw "Error reading HTTP response: End of file"
    and the film did not start. The same handler also ignores Range, so a seek
    would have silently replayed from the beginning.

    Whether this path may be read at all is decided before a session exists,
    against the worker's --allow-file-prefix list. By the time a session has a
    playback URL the question has been answered.
    """
    path = urllib.parse.unquote(urllib.parse.urlsplit(url).path)
    try:
        size = os.path.getsize(path)
    except OSError as exc:
        raise MediaError("SOURCE_UNREACHABLE", f"{url} is unreachable", 502) from exc

    start, end = 0, size - 1
    partial = False
    if range_header:
        match = RANGE.match(range_header.strip())
        if not match:
            raise MediaError("SOURCE_REFUSED", "the range is not one this source takes", 416)
        first, last = match.group(1), match.group(2)
        if first:
            start = int(first)
            if last:
                end = min(int(last), size - 1)
        elif last:
            # A suffix range: the last N bytes.
            start = max(0, size - int(last))
        if start >= size or start > end:
            raise MediaError("SOURCE_REFUSED", f"the source refused the request (416)", 416)
        partial = True

    length = end - start + 1
    content_type = mimetypes.guess_type(path)[0] or "application/octet-stream"
    headers = [
        ("Content-Type", content_type),
        ("Content-Length", str(length)),
        ("Accept-Ranges", "bytes"),
        ("Cache-Control", "no-store"),
        ("Connection", "close"),
    ]
    if partial:
        headers.append(("Content-Range", f"bytes {start}-{end}/{size}"))

    def chunks() -> Iterator[bytes]:
        remaining = length
        with open(path, "rb") as handle:
            handle.seek(start)
            while remaining > 0:
                block = handle.read(min(CHUNK_BYTES, remaining))
                if not block:
                    return
                remaining -= len(block)
                yield block

    return (206 if partial else 200), headers, chunks()


def _open(url: str, range_header: str | None, timeout: float):
    """The upstream response for `range_header`, tried again on a failure
    that is the host's rather than the request's."""
    host = urllib.parse.urlsplit(url).hostname or "?"
    for attempt in range(len(RETRY_DELAYS) + 1):
        request = urllib.request.Request(url, method="GET")
        if range_header:
            request.add_header("Range", range_header)
        # Some hosts answer differently, or not at all, without one.
        request.add_header("User-Agent", "MediaBox/1.0")
        last = attempt == len(RETRY_DELAYS)
        try:
            return urllib.request.urlopen(request, timeout=timeout)
        except urllib.error.HTTPError as error:
            error.close()
            # A 416 or a 404 is the upstream's answer, not a failure of ours,
            # and the player needs to see it rather than a made-up 502.
            if error.code in (404, 416):
                raise MediaError(
                    "SOURCE_REFUSED",
                    f"the source refused the request ({error.code})",
                    error.code,
                ) from error
            # The URL carries an account token: only its host is logged.
            LOG.warning(
                "relay from %s range=%s answered %s (try %d)", host, range_header, error.code, attempt + 1
            )
            if last or error.code not in TRANSIENT_STATUS:
                raise MediaError(
                    "SOURCE_UNAVAILABLE", f"the source answered {error.code}", 502
                ) from error
        except Exception as exc:
            LOG.warning(
                "relay from %s range=%s failed: %s: %s (try %d)",
                host, range_header, type(exc).__name__, exc, attempt + 1,
            )
            if last:
                raise MediaError("SOURCE_UNREACHABLE", f"{url} is unreachable", 502) from exc
        time.sleep(RETRY_DELAYS[attempt])
    raise AssertionError("unreachable")


def _start_of(range_header: str | None) -> tuple[int, str]:
    """Where a `bytes=START-END` range starts, and its END part ("" if open)."""
    match = RANGE.match((range_header or "").strip())
    if not match or not match.group(1):
        return 0, ""
    return int(match.group(1)), match.group(2)


def _range_length(content_range: str | None) -> tuple[int, int] | None:
    """(start, length) from a 206's Content-Range, or None if it is not one."""
    match = CONTENT_RANGE.match((content_range or "").strip())
    if not match:
        return None
    start, end = int(match.group(1)), int(match.group(2))
    if end < start or (match.group(3) != "*" and end >= int(match.group(3))):
        return None
    return start, end - start + 1


def relay(url: str, range_header: str | None, timeout: float = 30.0):
    """Open `url` and return `(status, headers, chunks)` for a local reply.

    The generator owns the upstream connection and closes it when the client
    stops reading — which is what happens on every seek, because the player
    abandons the response and asks for a new range.

    When the upstream connection ends before the body it promised -- the
    debrid host dropped it, or it went quiet past `timeout` -- the rest is
    asked for from where it stopped and the same reply carries on. The player
    never sees the cut: a film that played on had once stopped at 2.52 GB of a
    29.7 GB remux when mpv had to reconnect, and did not play on.
    """
    if urllib.parse.urlsplit(url).scheme == "file":
        return _relay_file(url, range_header)

    response = _open(url, range_header, timeout)
    headers = [
        (name.title(), value)
        for name, value in response.headers.items()
        if name.lower() in PASSED_BACK
    ]
    if not any(name.lower() == "accept-ranges" for name, _ in headers):
        headers.append(("Accept-Ranges", "bytes"))
    headers.append(("Cache-Control", "no-store"))
    # One request per connection.
    #
    # A player seeks by abandoning the response it is reading and asking for a
    # new range. On a kept-alive connection the next request then arrives on a
    # socket with a body still half-written to it, and the player reads an
    # empty answer -- "Error reading HTTP response: End of file", which is
    # exactly what the film did instead of starting.
    headers.append(("Connection", "close"))

    length = response.headers.get("Content-Length")
    declared = int(length) if length and length.isdigit() else None
    host = urllib.parse.urlsplit(url).hostname or "?"
    if response.status == 206:
        # A 206's body is what its Content-Range says, and nothing else does.
        # A CDN was seen answering a one-byte probe with
        #     Content-Range: bytes 0-0/29159331995
        #     Content-Length: 29159331995
        # and one byte of body: believing the length, the relay "resumed"
        # that byte towards 29 GB and the player waited on it. Content-Length
        # is only checked against the range; a range that cannot be read
        # means nothing is known, and nothing is resumed.
        span = _range_length(response.headers.get("Content-Range"))
        expected = span[1] if span else None
        if span is None or declared != expected:
            if declared is not None:
                LOG.warning(
                    "relay from %s: Content-Length %s disagrees with Content-Range %r; the range is believed",
                    host, declared, response.headers.get("Content-Range"),
                )
            headers = [(name, value) for name, value in headers if name.lower() != "content-length"]
            if expected is not None:
                headers.append(("Content-Length", str(expected)))
        # Resuming needs to know where the body starts in the file: where the
        # range says it does; it ends where the player asked it to.
        first, last = (span[0], _start_of(range_header)[1]) if span else (0, "")
    else:
        # A 200 is the whole file, from 0.
        expected = declared
        first, last = 0, ""

    def chunks() -> Iterator[bytes]:
        current = response
        sent = 0
        resumes = 0
        try:
            while True:
                if expected is not None and sent >= expected:
                    # All the range promised: whatever the connection still
                    # claims to have is not waited for.
                    return
                wanted = CHUNK_BYTES if expected is None else min(CHUNK_BYTES, expected - sent)
                try:
                    block = current.read(wanted)
                except Exception as exc:  # noqa: BLE001 -- the connection, not the data
                    block, cause = b"", f"{type(exc).__name__}: {exc}"
                else:
                    cause = "ended early"
                if block:
                    sent += len(block)
                    yield block
                    continue
                if expected is None or sent >= expected:
                    return
                if resumes >= MAX_RESUMES:
                    LOG.warning("relay from %s gave up after %d resumes at %d of %d", host, resumes, sent, expected)
                    return
                resumes += 1
                LOG.warning(
                    "relay from %s %s at %d of %d; resuming (%d)", host, cause, first + sent, first + expected, resumes
                )
                current.close()
                try:
                    current = _open(url, f"bytes={first + sent}-{last}", timeout)
                except MediaError as exc:
                    LOG.warning("relay from %s could not resume: %s", host, exc.message)
                    return
                answered = current.headers.get("Content-Range", "")
                if current.status != 206 or not answered.startswith(f"bytes {first + sent}-"):
                    LOG.warning("relay from %s resumed at the wrong place (%s)", host, answered or current.status)
                    return
        finally:
            current.close()

    return response.status, headers, chunks()
