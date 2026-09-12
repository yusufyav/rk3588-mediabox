"""Headless Stremio: the data plane only, never the web application."""

from __future__ import annotations

from .adapter import CatalogRow, HeadlessStremio, video_id_for
from .addons import AddonClient, parse_manifest, parse_stream
from .api import SessionStore, StremioAPI
from .models import (
    Addon,
    AddonCatalog,
    Meta,
    MetaPreview,
    ResolvedStream,
    SessionStatus,
    Stream,
    StreamKind,
    Subtitle,
    Video,
)
from .server import StreamingServer, TorrentStats

__all__ = [
    "Addon",
    "AddonCatalog",
    "AddonClient",
    "CatalogRow",
    "HeadlessStremio",
    "Meta",
    "MetaPreview",
    "ResolvedStream",
    "SessionStatus",
    "SessionStore",
    "Stream",
    "StreamKind",
    "StreamingServer",
    "StremioAPI",
    "Subtitle",
    "TorrentStats",
    "Video",
    "parse_manifest",
    "parse_stream",
    "video_id_for",
]
