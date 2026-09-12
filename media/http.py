"""One bounded HTTP client for every outbound call the media core makes.

Every provider the media core talks to is somebody else's server: the Stremio
API, whatever addons the user installed, the local streaming server. None of
them is trusted to be fast, small, or well behaved, so all three of those are
capped here instead of at each call site:

    * a connect/read deadline on every request,
    * a hard byte ceiling on every response body,
    * a redirect limit, with the scheme re-checked at each hop.

There is no cookie jar and no shared session state: a response from one addon
must not be able to influence the next request to another.
"""

from __future__ import annotations

import gzip
import json
import logging
import urllib.error
import urllib.parse
import urllib.request
import zlib
from dataclasses import dataclass
from typing import Any
from urllib.parse import urlsplit

from .errors import InvalidRequest, UpstreamError


LOG = logging.getLogger(__name__)

#: Provider responses are catalogues and manifests, not media. A megabyte is
#: several thousand catalogue entries; anything past it is a provider fault.
MAX_JSON_BYTES = 4 * 1024 * 1024

DEFAULT_TIMEOUT_SECONDS = 15.0
MAX_REDIRECTS = 5

#: The Stremio addon protocol is plain HTTPS. `http` stays allowed because the
#: local streaming server and the local-files addon are served over loopback.
ALLOWED_SCHEMES = ("http", "https")

USER_AGENT = "MediaBox/2.0 (+headless media core)"


@dataclass(slots=True)
class HttpResponse:
    status: int
    headers: dict[str, str]
    body: bytes
    url: str

    def json(self) -> Any:
        try:
            return json.loads(self.body)
        except (json.JSONDecodeError, UnicodeDecodeError) as exc:
            raise UpstreamError(
                f"{self.url} did not return JSON", {"status": self.status}
            ) from exc


def check_scheme(url: str) -> str:
    """Reject anything that is not a plain http(s) URL with a host."""
    if not isinstance(url, str) or not url:
        raise InvalidRequest("url must be a non-empty string")
    if any(character in url for character in ("\r", "\n", "\x00")):
        raise InvalidRequest("url contains forbidden control characters")
    parsed = urlsplit(url)
    if parsed.scheme not in ALLOWED_SCHEMES:
        raise InvalidRequest(f"url scheme must be one of {ALLOWED_SCHEMES}")
    if not parsed.hostname:
        raise InvalidRequest("url must include a host")
    return url


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    """Redirects are followed by hand so each hop can be re-validated."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):  # noqa: D102
        return None


_OPENER = urllib.request.build_opener(_NoRedirect)


def request(
    url: str,
    *,
    method: str = "GET",
    body: bytes | None = None,
    headers: dict[str, str] | None = None,
    timeout: float = DEFAULT_TIMEOUT_SECONDS,
    max_bytes: int = MAX_JSON_BYTES,
    allow_redirects: bool = True,
) -> HttpResponse:
    """Perform one request and return at most `max_bytes` of its body."""
    target = check_scheme(url)
    remaining_redirects = MAX_REDIRECTS if allow_redirects else 0

    while True:
        outgoing = urllib.request.Request(target, data=body, method=method)
        outgoing.add_header("User-Agent", USER_AGENT)
        outgoing.add_header("Accept-Encoding", "gzip, deflate")
        for name, value in (headers or {}).items():
            outgoing.add_header(name, value)
        try:
            response = _OPENER.open(outgoing, timeout=timeout)
        except urllib.error.HTTPError as exc:
            response = exc
        except (urllib.error.URLError, TimeoutError, OSError) as exc:
            raise UpstreamError(f"{target} is unreachable", {"reason": str(exc)}) from exc

        with response:
            status = int(response.status)
            response_headers = {
                key.lower(): value for key, value in response.headers.items()
            }
            if status in (301, 302, 303, 307, 308) and remaining_redirects > 0:
                location = response.headers.get("Location")
                if location:
                    remaining_redirects -= 1
                    target = check_scheme(urllib.parse.urljoin(target, location))
                    if status == 303 or (status == 302 and method == "POST"):
                        method, body = "GET", None
                    continue
            # read one byte past the ceiling so an oversized body is detectable
            # rather than being silently truncated into malformed JSON.
            raw = response.read(max_bytes + 1)

        if len(raw) > max_bytes:
            raise UpstreamError(
                f"{target} returned more than {max_bytes} bytes", {"limit": max_bytes}
            )
        return HttpResponse(status, response_headers, _decode(raw, response_headers), target)


def _decode(raw: bytes, headers: dict[str, str]) -> bytes:
    encoding = headers.get("content-encoding", "").lower()
    try:
        if encoding == "gzip":
            return gzip.decompress(raw)
        if encoding == "deflate":
            return zlib.decompress(raw)
    except (OSError, zlib.error):
        LOG.warning("Could not decode a %s response body; using it verbatim", encoding)
    return raw


def get_json(url: str, *, timeout: float = DEFAULT_TIMEOUT_SECONDS) -> Any:
    response = request(url, timeout=timeout)
    if response.status >= 400:
        raise UpstreamError(f"{url} returned HTTP {response.status}", {"status": response.status})
    return response.json()


def post_json(url: str, payload: Any, *, timeout: float = DEFAULT_TIMEOUT_SECONDS) -> Any:
    response = request(
        url,
        method="POST",
        body=json.dumps(payload).encode("utf-8"),
        headers={"Content-Type": "application/json"},
        timeout=timeout,
    )
    if response.status >= 400:
        raise UpstreamError(f"{url} returned HTTP {response.status}", {"status": response.status})
    return response.json()
