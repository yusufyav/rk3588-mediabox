"""Building the ffmpeg command for a media session.

Two rules shape this module, and both are enforced by code rather than by
convention:

**The video is copied.** Advanced audio is never a reason to re-encode video.
A 4K HEVC stream re-encoded in software on this board saturates a core and
produces a worse picture, so :func:`assert_video_copy` inspects the finished
argument vector and refuses to hand back anything that would start a video
encoder. Every construction path goes through it.

**It is an argument vector.** Source URLs come from third-party addons. They
are passed to `execve` as one argument, never assembled into a shell string,
and every one is validated before it gets here.
"""

from __future__ import annotations

import os
import shutil
from dataclasses import dataclass
from typing import Any

from ..errors import MediaError
from ..policy.audio import AudioAction
from ..policy.decide import PlaybackDecision, PlaybackMode


#: Output containers by session mode. Matroska carries AC-3 beside HEVC without
#: re-encoding either and streams over HTTP to Kodi; fragmented MP4 is what a
#: browser will take.
CONTAINER_MATROSKA = "matroska"
CONTAINER_FRAGMENTED_MP4 = "mp4"

#: Anything that would put a real encoder on the video stream.
_VIDEO_ENCODE_FLAGS = ("-c:v", "-codec:v", "-vcodec", "-c:V")
_ALLOWED_VIDEO_CODEC_VALUES = {"copy"}

#: Filters are a decode/encode pipeline by definition; none may touch video.
_VIDEO_FILTER_FLAGS = ("-vf", "-filter:v", "-filter_complex", "-lavfi")


class VideoCopyViolation(MediaError):
    """Raised when a command would re-encode video. Never caught; it is a bug."""

    def __init__(self, message: str) -> None:
        super().__init__("VIDEO_COPY_VIOLATION", message, 500)


def assert_video_copy(argv: list[str]) -> list[str]:
    """Refuse any argument vector that could start a video encoder."""
    for index, token in enumerate(argv):
        if token in _VIDEO_FILTER_FLAGS:
            raise VideoCopyViolation(f"{token} would decode and re-encode the video")
        if token in _VIDEO_ENCODE_FLAGS:
            value = argv[index + 1] if index + 1 < len(argv) else ""
            if value not in _ALLOWED_VIDEO_CODEC_VALUES:
                raise VideoCopyViolation(
                    f"{token} {value!r} would re-encode the video; only 'copy' is allowed"
                )
        if token == "-c" or token == "-codec":
            value = argv[index + 1] if index + 1 < len(argv) else ""
            if value not in _ALLOWED_VIDEO_CODEC_VALUES:
                raise VideoCopyViolation(
                    f"{token} {value!r} sets every stream's codec, including video"
                )
    if not any(token in _VIDEO_ENCODE_FLAGS for token in argv):
        raise VideoCopyViolation("the command does not state that video is copied")
    return argv


@dataclass(frozen=True, slots=True)
class FFmpegConfig:
    binary: str = "ffmpeg"
    #: Threads for the audio encoder. AC-3 at 640 kbit/s needs one; more only
    #: buys a bigger share of a board that also has to decode video downstream.
    threads: int = 2
    #: How long ffmpeg may spend before producing its first output byte.
    start_timeout_seconds: float = 45.0
    user_agent: str = "MediaBox/2.0"


def resolve_binary(config: FFmpegConfig) -> str:
    found = shutil.which(config.binary) if os.path.sep not in config.binary else config.binary
    if not found or not os.path.exists(found):
        raise MediaError("FFMPEG_MISSING", f"ffmpeg binary not found: {config.binary}", 500)
    return found


def _input_options(source: str, config: FFmpegConfig) -> list[str]:
    """Protocol options that only exist for the protocol in use.

    ffmpeg rejects the whole invocation when given an option the input
    protocol does not define — `-user_agent` on a `file:` URL is an error, not
    a no-op — so these are chosen from the scheme rather than always sent.
    """
    if source.split(":", 1)[0].lower() in ("http", "https"):
        return [
            "-user_agent",
            config.user_agent,
            "-reconnect",
            "1",
            "-reconnect_streamed",
            "1",
            "-reconnect_delay_max",
            "5",
        ]
    return []


def build_audio_transcode_argv(
    source: str,
    decision: PlaybackDecision,
    config: FFmpegConfig | None = None,
    *,
    container: str = CONTAINER_MATROSKA,
    start_seconds: float | None = None,
) -> list[str]:
    """Video copy, audio to AC-3. The command the appliance actually runs.

    Track mapping is by absolute stream index (`0:<index>`), taken from the
    decision, so the encoder works on the track the policy chose rather than on
    whichever track happens to be first.
    """
    config = config or FFmpegConfig()
    audio = decision.audio
    if audio.action is not AudioAction.TRANSCODE_AC3 or audio.track is None:
        raise MediaError(
            "SESSION_MODE_MISMATCH",
            "an audio transcode was requested for a decision that does not need one",
            500,
        )
    video_index = _video_index(decision)

    argv: list[str] = [resolve_binary(config), "-hide_banner", "-nostdin", "-loglevel", "error"]
    if start_seconds:
        # Before -i so ffmpeg seeks the input rather than decoding to the point.
        argv += ["-ss", f"{max(0.0, float(start_seconds)):.3f}"]
    argv += _input_options(source, config)
    argv += [
        "-i",
        source,
        "-map",
        f"0:{video_index}",
        "-map",
        f"0:{audio.track.stream_index}",
        "-c:v",
        "copy",
        "-c:a",
        decision.audio.target_codec or "ac3",
        "-ac",
        str(audio.target_channels or 6),
        "-b:a",
        str(audio.target_bitrate or 640_000),
        "-ar",
        str(audio.target_sample_rate or 48000),
        "-threads",
        str(config.threads),
        # Metadata and chapters come from the source's own containers and are
        # not needed downstream; dropping them keeps the muxer predictable.
        "-map_metadata",
        "-1",
        "-map_chapters",
        "-1",
        "-f",
        container,
    ]
    if container == CONTAINER_FRAGMENTED_MP4:
        argv += ["-movflags", "frag_keyframe+empty_moov+default_base_moof"]
    argv.append("pipe:1")
    return assert_video_copy(argv)


def build_remux_argv(
    source: str,
    decision: PlaybackDecision,
    config: FFmpegConfig | None = None,
    *,
    container: str = CONTAINER_MATROSKA,
    start_seconds: float | None = None,
) -> list[str]:
    """Copy every selected stream into another container. No encoder at all."""
    config = config or FFmpegConfig()
    video_index = _video_index(decision)
    argv: list[str] = [resolve_binary(config), "-hide_banner", "-nostdin", "-loglevel", "error"]
    if start_seconds:
        argv += ["-ss", f"{max(0.0, float(start_seconds)):.3f}"]
    argv += _input_options(source, config)
    argv += ["-i", source, "-map", f"0:{video_index}"]
    if decision.audio.track is not None:
        argv += ["-map", f"0:{decision.audio.track.stream_index}"]
    argv += [
        "-c:v",
        "copy",
        "-c:a",
        "copy",
        "-map_metadata",
        "-1",
        "-map_chapters",
        "-1",
        "-f",
        container,
    ]
    if container == CONTAINER_FRAGMENTED_MP4:
        argv += ["-movflags", "frag_keyframe+empty_moov+default_base_moof"]
    argv.append("pipe:1")
    return assert_video_copy(argv)


def build_argv(
    source: str,
    decision: PlaybackDecision,
    config: FFmpegConfig | None = None,
    **kwargs: Any,
) -> list[str]:
    """Pick the right command for a decision that needs a process."""
    if decision.mode is PlaybackMode.DIRECT_WITH_AUDIO_TRANSCODE:
        return build_audio_transcode_argv(source, decision, config, **kwargs)
    if decision.mode is PlaybackMode.REMUX:
        return build_remux_argv(source, decision, config, **kwargs)
    raise MediaError(
        "SESSION_MODE_MISMATCH",
        f"{decision.mode.value} does not need a media-core process",
        400,
    )


def _video_index(decision: PlaybackDecision) -> int:
    index = decision.video.stream_index
    if index is None:
        raise MediaError(
            "SESSION_MODE_MISMATCH",
            "the decision names no video stream to copy",
            500,
        )
    return index
