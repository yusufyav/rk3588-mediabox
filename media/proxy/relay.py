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


def relay(url: str, range_header: str | None, timeout: float = 30.0):
    """Open `url` and return `(status, headers, chunks)` for a local reply.

    The generator owns the upstream connection and closes it when the client
    stops reading — which is what happens on every seek, because the player
    abandons the response and asks for a new range.
    """
    if urllib.parse.urlsplit(url).scheme == "file":
        return _relay_file(url, range_header)

    request = urllib.request.Request(url, method="GET")
    if range_header:
        request.add_header("Range", range_header)
    # Some hosts answer differently, or not at all, without one.
    request.add_header("User-Agent", "MediaBox/1.0")

    try:
        response = urllib.request.urlopen(request, timeout=timeout)
    except urllib.error.HTTPError as error:
        # A 416 or a 404 is the upstream's answer, not a failure of ours, and
        # the player needs to see it rather than a made-up 502.
        if error.code in (404, 416):
            error.close()
            raise MediaError(
                "SOURCE_REFUSED",
                f"the source refused the request ({error.code})",
                error.code,
            ) from error
        error.close()
        raise MediaError(
            "SOURCE_UNAVAILABLE", f"the source answered {error.code}", 502
        ) from error
    except Exception as exc:
        raise MediaError("SOURCE_UNREACHABLE", f"{url} is unreachable", 502) from exc

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

    def chunks() -> Iterator[bytes]:
        try:
            while True:
                block = response.read(CHUNK_BYTES)
                if not block:
                    return
                yield block
        finally:
            response.close()

    return response.status, headers, chunks()
