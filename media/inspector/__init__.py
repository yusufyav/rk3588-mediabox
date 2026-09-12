"""Normalized inspection of a playable source."""

from __future__ import annotations

from ..errors import ProbeError, ProbeTimeout
from .ffprobe import FFprobeConfig, probe
from .model import (
    AudioTrack,
    ChromaSubsampling,
    ColorRange,
    Container,
    DolbyVision,
    HdrFormat,
    MasteringDisplay,
    MediaInfo,
    ProbeResult,
    SubtitleTrack,
    VideoTrack,
)
from .parse import build_media_info, classify_hdr


def inspect(source_url: str, config: FFprobeConfig | None = None) -> MediaInfo:
    """Probe one source and return its normalized description."""
    return build_media_info(source_url, probe(source_url, config))


__all__ = [
    "AudioTrack",
    "ChromaSubsampling",
    "ColorRange",
    "Container",
    "DolbyVision",
    "FFprobeConfig",
    "HdrFormat",
    "MasteringDisplay",
    "MediaInfo",
    "ProbeError",
    "ProbeResult",
    "ProbeTimeout",
    "SubtitleTrack",
    "VideoTrack",
    "build_media_info",
    "classify_hdr",
    "inspect",
    "probe",
]
