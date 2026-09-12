"""What the media core is allowed to be pointed at, and by whom.

The media proxy takes a URL that came from a third-party addon and hands it to
ffmpeg on a machine that sits inside somebody's home network. That makes three
things mandatory:

* **scheme validation** — only http and https reach a session; `file:` URLs are
  accepted only from the local-files path and only under an explicit allowlist,
  and everything else (`magnet:`, `data:`, `gopher:`, …) is refused outright.
* **destination policy** — an addon must not be able to use the appliance as a
  reader of the network it lives on. Loopback and link-local destinations are
  refused unless they are the configured streaming server, which is the one
  local origin the media core legitimately reads from.
* **identifier validation** — session ids are fixed-shape tokens, so no request
  path can become a filesystem path.
"""

from __future__ import annotations

import ipaddress
import logging
import re
import socket
from dataclasses import dataclass, field
from urllib.parse import urlsplit, urlunsplit

from ..errors import InvalidRequest


LOG = logging.getLogger(__name__)

SESSION_ID_PATTERN = re.compile(r"^[0-9a-f]{32}$")

#: Schemes a session may be created for.
STREAM_SCHEMES = ("http", "https")

#: Cloud metadata endpoints. Never a media source, always a credential probe.
_METADATA_ADDRESSES = frozenset({"169.254.169.254", "fd00:ec2::254"})


def validate_session_id(value: str) -> str:
    if not isinstance(value, str) or not SESSION_ID_PATTERN.match(value):
        raise InvalidRequest("session id must be 32 lowercase hexadecimal characters")
    return value


@dataclass(frozen=True, slots=True)
class SourcePolicy:
    """Where a media session may read from."""

    #: Origins that are allowed to be loopback — normally exactly the local
    #: Stremio streaming server.
    allowed_local_origins: frozenset[str] = frozenset()
    #: Absolute path prefixes a `file:` source may live under. Empty disables
    #: local files entirely, which is the default.
    allowed_file_prefixes: tuple[str, ...] = ()
    #: Set to False only in tests: resolving names makes validation depend on DNS.
    resolve_names: bool = True
    #: Private (RFC1918) destinations. An addon on the same LAN is unusual but
    #: legitimate for a home media setup, so this is allowed by default while
    #: loopback and link-local are not.
    allow_private: bool = True
    extra_denied_hosts: frozenset[str] = field(default_factory=frozenset)

    def origin_of(self, url: str) -> str:
        parsed = urlsplit(url)
        return urlunsplit((parsed.scheme, parsed.netloc, "", "", ""))


def _addresses_for(host: str, policy: SourcePolicy) -> list[ipaddress._BaseAddress]:
    try:
        return [ipaddress.ip_address(host)]
    except ValueError:
        pass
    if not policy.resolve_names:
        return []
    try:
        infos = socket.getaddrinfo(host, None, proto=socket.IPPROTO_TCP)
    except OSError as exc:
        # A name that does not resolve is not a policy violation, and refusing
        # it here would turn a DNS blip into "this source is not allowed". The
        # destination checks below exist to stop a *resolvable* internal
        # address being reached; a name with no address reaches nothing, and
        # the probe that follows fails with the real reason.
        #
        # `OSError` rather than `gaierror`: on the appliance this resolver
        # raises EBUSY, and every way a resolver can fail has to end here
        # rather than as a 500 from the media core.
        LOG.info("Source host %r could not be resolved during validation: %s", host, exc)
        return []
    found: list[ipaddress._BaseAddress] = []
    for info in infos:
        try:
            found.append(ipaddress.ip_address(info[4][0]))
        except ValueError:
            continue
    return found


def validate_source_url(url: str, policy: SourcePolicy | None = None) -> str:
    """Return `url` if a media session may be created for it, else raise."""
    policy = policy or SourcePolicy()
    if not isinstance(url, str) or not url:
        raise InvalidRequest("source must be a non-empty string")
    if len(url) > 4096:
        raise InvalidRequest("source is longer than the media core accepts")
    if any(character in url for character in ("\r", "\n", "\x00")):
        raise InvalidRequest("source contains forbidden control characters")

    parsed = urlsplit(url)

    if parsed.scheme == "file":
        if not policy.allowed_file_prefixes:
            raise InvalidRequest("file sources are not enabled on this appliance")
        if parsed.netloc not in {"", "localhost"}:
            raise InvalidRequest("a file url may not name a host")
        path = parsed.path
        if not path.startswith("/") or ".." in path.split("/"):
            raise InvalidRequest("file url must be an absolute path without traversal")
        if not any(path.startswith(prefix) for prefix in policy.allowed_file_prefixes):
            raise InvalidRequest("file url is outside the allowed directories")
        return url

    if parsed.scheme not in STREAM_SCHEMES:
        raise InvalidRequest(
            f"source scheme must be one of {STREAM_SCHEMES}; magnet and torrent links "
            "are resolved by the streaming server, never played directly"
        )

    host = parsed.hostname
    if not host:
        raise InvalidRequest("source must include a host")
    if host.lower() in policy.extra_denied_hosts:
        raise InvalidRequest("this host is not an allowed media source")

    origin = policy.origin_of(url)
    if origin in policy.allowed_local_origins:
        return url

    for address in _addresses_for(host, policy):
        if str(address) in _METADATA_ADDRESSES:
            raise InvalidRequest("that address is a metadata service, not a media source")
        if address.is_loopback:
            raise InvalidRequest(
                "loopback sources are only allowed for the configured streaming server"
            )
        if address.is_link_local:
            raise InvalidRequest("link-local sources are not allowed")
        if address.is_multicast or address.is_reserved or address.is_unspecified:
            raise InvalidRequest("that address is not a valid media source")
        if address.is_private and not policy.allow_private:
            raise InvalidRequest("private-network sources are not allowed by this policy")
    return url
