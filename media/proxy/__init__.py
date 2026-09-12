"""Media sessions and the processes they own."""

from __future__ import annotations

from .ffmpeg import (
    FFmpegConfig,
    VideoCopyViolation,
    assert_video_copy,
    build_argv,
    build_audio_transcode_argv,
    build_remux_argv,
)
from .security import SourcePolicy, validate_session_id, validate_source_url
from .session import MediaSession, SessionManager, SessionMode, SessionState

__all__ = [
    "FFmpegConfig",
    "MediaSession",
    "SessionManager",
    "SessionMode",
    "SessionState",
    "SourcePolicy",
    "VideoCopyViolation",
    "assert_video_copy",
    "build_argv",
    "build_audio_transcode_argv",
    "build_remux_argv",
    "validate_session_id",
    "validate_source_url",
]
