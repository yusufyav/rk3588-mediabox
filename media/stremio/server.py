"""The Stremio streaming server, used as a resolver rather than as a player.

The streaming server is the one component that can turn a torrent into an HTTP
URL, which is precisely why the media core uses it and precisely how far it is
allowed to go: MediaBox never hands a magnet link or an info hash to Kodi, only
the playable HTTP URL the server exposes for it.

Its transcoding endpoints (`/hlsv2/...`) are deliberately **not** used here.
Transcoding decisions belong to :mod:`media.policy`, which knows what this
board can and cannot do; the streaming server would decide with a browser's
codec list and, finding no usable hardware encode profile on this hardware,
would fall back to a software encode.
"""

from __future__ import annotations

import logging
from dataclasses import dataclass
from typing import Any
from urllib.parse import quote, urlencode

from ..errors import InvalidRequest, UpstreamError
from ..http import get_json, post_json, request
from .models import ResolvedStream, Stream, StreamKind


LOG = logging.getLogger(__name__)

#: Peer discovery bounds handed to the engine when a torrent is created. They
#: mirror the server's own defaults rather than pushing it harder: a media box
#: with a saturated uplink is a media box that stutters.
PEER_SEARCH_MIN = 40
PEER_SEARCH_MAX = 150


@dataclass(frozen=True, slots=True)
class TorrentStats:
    info_hash: str
    name: str | None
    swarm_size: int | None
    peers: int | None
    connection_tries: int | None
    downloaded: int | None
    download_speed: float | None
    sources: tuple[dict[str, Any], ...] = ()

    def as_dict(self) -> dict[str, Any]:
        return {
            "infoHash": self.info_hash,
            "name": self.name,
            "swarmSize": self.swarm_size,
            "peers": self.peers,
            "connectionTries": self.connection_tries,
            "downloaded": self.downloaded,
            "downloadSpeed": self.download_speed,
            "sources": [dict(source) for source in self.sources],
        }

    @property
    def has_peer_contact(self) -> bool:
        """True once the engine has actually tried to reach a peer.

        A tracker or the DHT answering is not peer contact: it is a list. This
        distinction is what separates "no seeders" from "outbound BitTorrent is
        blocked on this network", and the two need different reports.
        """
        return bool(self.connection_tries) or bool(self.peers) or bool(self.downloaded)


class StreamingServer:
    """A small, bounded client for the local streaming server."""

    def __init__(self, base_url: str, *, timeout: float = 10.0) -> None:
        self.base_url = base_url.rstrip("/")
        self._timeout = timeout

    # ------------------------------------------------------------------ health

    def settings(self) -> dict[str, Any]:
        payload = get_json(f"{self.base_url}/settings", timeout=self._timeout)
        return payload if isinstance(payload, dict) else {}

    def version(self) -> str | None:
        try:
            values = self.settings().get("values")
        except UpstreamError:
            return None
        if isinstance(values, dict) and isinstance(values.get("serverVersion"), str):
            return values["serverVersion"]
        return None

    def reachable(self) -> bool:
        try:
            self.settings()
            return True
        except UpstreamError:
            return False

    # ----------------------------------------------------------------- torrents

    def create_torrent(self, info_hash: str, announce: tuple[str, ...] = ()) -> None:
        """Ask the engine to exist. Idempotent on the server's side."""
        info_hash = _validate_info_hash(info_hash)
        sources = [f"dht:{info_hash}"] + [
            f"tracker:{item}" if not item.startswith("tracker:") else item for item in announce
        ]
        body = {
            "torrent": {"infoHash": info_hash},
            "peerSearch": {"sources": sources, "min": PEER_SEARCH_MIN, "max": PEER_SEARCH_MAX},
        }
        try:
            post_json(f"{self.base_url}/{info_hash}/create", body, timeout=self._timeout)
        except UpstreamError as exc:
            # The server answers this call with an empty body, which is not
            # JSON. That is success, not failure.
            if "did not return JSON" not in exc.message:
                raise

    def torrent_stats(self, info_hash: str) -> TorrentStats | None:
        info_hash = _validate_info_hash(info_hash)
        try:
            payload = get_json(f"{self.base_url}/{info_hash}/stats.json", timeout=self._timeout)
        except UpstreamError:
            return None
        if not isinstance(payload, dict) or not payload:
            return None
        sources = payload.get("sources")
        return TorrentStats(
            info_hash=info_hash,
            name=payload.get("name") if isinstance(payload.get("name"), str) else None,
            swarm_size=_as_int(payload.get("swarmSize")),
            peers=_as_int(payload.get("peers")),
            connection_tries=_as_int(payload.get("connectionTries")),
            downloaded=_as_int(payload.get("downloaded")),
            download_speed=_as_float(payload.get("downloadSpeed")),
            sources=tuple(item for item in (sources or []) if isinstance(item, dict)),
        )

    def torrent_url(self, info_hash: str, file_idx: int | None, announce: tuple[str, ...] = ()) -> str:
        """The HTTP URL the engine serves one file of a torrent at."""
        info_hash = _validate_info_hash(info_hash)
        index = 0 if file_idx is None else int(file_idx)
        if index < 0 or index > 4096:
            raise InvalidRequest("fileIdx is out of range")
        url = f"{self.base_url}/{info_hash}/{index}"
        if announce:
            query = urlencode([("tr", item) for item in announce[:16]])
            url = f"{url}?{query}"
        return url

    def youtube_url(self, yt_id: str) -> str:
        if not yt_id or len(yt_id) > 64 or any(c in yt_id for c in "/?#&"):
            raise InvalidRequest("that is not a YouTube id")
        return f"{self.base_url}/yt/{quote(yt_id, safe='')}"

    # ------------------------------------------------------------------ resolve

    def resolve(self, stream: Stream, *, create_torrent: bool = True) -> ResolvedStream:
        """Turn a Stremio stream descriptor into something openable."""
        if stream.kind is StreamKind.URL and stream.url:
            return ResolvedStream(stream=stream, url=stream.url, via="addon")

        if stream.kind is StreamKind.TORRENT and stream.info_hash:
            notes: list[str] = []
            if create_torrent:
                try:
                    self.create_torrent(stream.info_hash, stream.announce)
                except UpstreamError as exc:
                    notes.append(f"the engine could not be created: {exc.message}")
            stats = self.torrent_stats(stream.info_hash)
            if stats is not None and not stats.has_peer_contact:
                notes.append(
                    "the engine has made no peer contact yet "
                    f"(swarmSize={stats.swarm_size}, connectionTries={stats.connection_tries}, "
                    f"downloaded={stats.downloaded})"
                )
            return ResolvedStream(
                stream=stream,
                url=self.torrent_url(stream.info_hash, stream.file_idx, stream.announce),
                via="streaming-server:torrent",
                needs_streaming_server=True,
                notes=tuple(notes),
            )

        if stream.kind is StreamKind.YOUTUBE and stream.yt_id:
            return ResolvedStream(
                stream=stream,
                url=self.youtube_url(stream.yt_id),
                via="streaming-server:youtube",
                needs_streaming_server=True,
            )

        raise InvalidRequest(
            "this stream is not playable on the appliance: it only offers a link to "
            "another application"
            if stream.kind is StreamKind.EXTERNAL
            else "this stream descriptor offers nothing playable"
        )

    def probe_reachable(self, url: str, *, timeout: float | None = None) -> tuple[bool, int | None]:
        """One bounded range request, to see whether a resolved URL answers."""
        try:
            response = request(
                url,
                method="GET",
                headers={"Range": "bytes=0-1023"},
                timeout=timeout or self._timeout,
                max_bytes=4096,
            )
        except UpstreamError:
            return False, None
        return response.status < 400, response.status


def _validate_info_hash(value: str) -> str:
    text = (value or "").strip().lower()
    if len(text) != 40 or any(character not in "0123456789abcdef" for character in text):
        raise InvalidRequest("infoHash must be 40 hexadecimal characters")
    return text


def _as_int(value: Any) -> int | None:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    return int(value)


def _as_float(value: Any) -> float | None:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    return float(value)
