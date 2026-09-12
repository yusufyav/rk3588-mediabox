"""Deterministic playback policy for the accepted RK3588 media plane."""

from __future__ import annotations

from .audio import AudioAction, AudioDecision, ac3_target, decide_track, select_audio
from .capabilities import (
    DEFAULT_PROFILE_NAME,
    PROFILES,
    CapabilityProfile,
    get_profile,
)
from .decide import PlaybackDecision, PlaybackMode, decide
from .preview import PreviewDecision, PreviewMode, decide_preview
from .ranking import RankedSource, RankTier, rank_sources
from .reasons import Reason, Severity
from .video import VideoDecision, VideoVerdict, decide_video

__all__ = [
    "DEFAULT_PROFILE_NAME",
    "PROFILES",
    "AudioAction",
    "AudioDecision",
    "CapabilityProfile",
    "PlaybackDecision",
    "PlaybackMode",
    "PreviewDecision",
    "PreviewMode",
    "RankTier",
    "RankedSource",
    "Reason",
    "Severity",
    "VideoDecision",
    "VideoVerdict",
    "ac3_target",
    "decide",
    "decide_preview",
    "decide_track",
    "decide_video",
    "get_profile",
    "rank_sources",
    "select_audio",
]
