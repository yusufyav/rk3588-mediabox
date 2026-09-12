"""The normalized description of one playable source.

Nothing downstream of the inspector is allowed to read ffprobe output. Policy,
ranking and the session layer all read these types instead, so ffprobe's
quirks — string numbers, `"unknown"` sentinels, side data that only appears at
frame level — are absorbed in exactly one place.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import Any


class HdrFormat(str, Enum):
    """What the video actually signals, not what the filename claims."""

    SDR = "SDR"
    HDR10 = "HDR10"
    HDR10_PLUS = "HDR10+"
    HLG = "HLG"
    DOLBY_VISION = "DolbyVision"
    #: Colour metadata is absent or self-contradictory. This is deliberately
    #: distinct from SDR: "we know it is SDR" and "we do not know" lead to
    #: different decisions.
    UNKNOWN = "Unknown"


class ChromaSubsampling(str, Enum):
    YUV420 = "4:2:0"
    YUV422 = "4:2:2"
    YUV444 = "4:4:4"
    MONO = "mono"
    RGB = "rgb"
    UNKNOWN = "unknown"


class ColorRange(str, Enum):
    LIMITED = "limited"
    FULL = "full"
    UNKNOWN = "unknown"


@dataclass(frozen=True, slots=True)
class MasteringDisplay:
    """SMPTE ST 2086 mastering display colour volume, in nits and CIE xy."""

    red: tuple[float, float] | None = None
    green: tuple[float, float] | None = None
    blue: tuple[float, float] | None = None
    white_point: tuple[float, float] | None = None
    min_luminance: float | None = None
    max_luminance: float | None = None

    def as_dict(self) -> dict[str, Any]:
        return {
            "red": list(self.red) if self.red else None,
            "green": list(self.green) if self.green else None,
            "blue": list(self.blue) if self.blue else None,
            "whitePoint": list(self.white_point) if self.white_point else None,
            "minLuminance": self.min_luminance,
            "maxLuminance": self.max_luminance,
        }


@dataclass(frozen=True, slots=True)
class DolbyVision:
    """The DOVI configuration record, as carried in the container.

    `bl_signal_compatibility_id` is the field that decides whether a non-DV
    decoder can use the base layer, and it — not the profile number — is what
    the policy keys on wherever the profile leaves it open.
    """

    profile: int | None = None
    level: int | None = None
    bl_signal_compatibility_id: int | None = None
    rpu_present: bool | None = None
    el_present: bool | None = None
    bl_present: bool | None = None

    def as_dict(self) -> dict[str, Any]:
        return {
            "profile": self.profile,
            "level": self.level,
            "blSignalCompatibilityId": self.bl_signal_compatibility_id,
            "rpuPresent": self.rpu_present,
            "elPresent": self.el_present,
            "blPresent": self.bl_present,
        }


@dataclass(frozen=True, slots=True)
class Container:
    format_name: str | None = None
    format_long_name: str | None = None
    duration_seconds: float | None = None
    bit_rate: int | None = None
    size_bytes: int | None = None

    def as_dict(self) -> dict[str, Any]:
        return {
            "format": self.format_name,
            "formatLongName": self.format_long_name,
            "durationSeconds": self.duration_seconds,
            "bitRate": self.bit_rate,
            "sizeBytes": self.size_bytes,
        }


@dataclass(frozen=True, slots=True)
class VideoTrack:
    stream_index: int
    codec: str | None = None
    profile: str | None = None
    level: int | None = None
    width: int | None = None
    height: int | None = None
    fps: float | None = None
    pixel_format: str | None = None
    bit_depth: int | None = None
    chroma: ChromaSubsampling = ChromaSubsampling.UNKNOWN
    color_range: ColorRange = ColorRange.UNKNOWN
    color_matrix: str | None = None
    color_primaries: str | None = None
    color_transfer: str | None = None
    mastering_display: MasteringDisplay | None = None
    max_cll: int | None = None
    max_fall: int | None = None
    hdr: HdrFormat = HdrFormat.UNKNOWN
    dolby_vision: DolbyVision | None = None
    codec_tag: str | None = None
    is_default: bool = False

    def as_dict(self) -> dict[str, Any]:
        return {
            "streamIndex": self.stream_index,
            "codec": self.codec,
            "profile": self.profile,
            "level": self.level,
            "width": self.width,
            "height": self.height,
            "fps": self.fps,
            "pixelFormat": self.pixel_format,
            "bitDepth": self.bit_depth,
            "chroma": self.chroma.value,
            "colorRange": self.color_range.value,
            "colorMatrix": self.color_matrix,
            "colorPrimaries": self.color_primaries,
            "colorTransfer": self.color_transfer,
            "masteringDisplay": self.mastering_display.as_dict() if self.mastering_display else None,
            "maxCLL": self.max_cll,
            "maxFALL": self.max_fall,
            "hdr": self.hdr.value,
            "dolbyVision": self.dolby_vision.as_dict() if self.dolby_vision else None,
            "codecTag": self.codec_tag,
            "default": self.is_default,
        }


@dataclass(frozen=True, slots=True)
class AudioTrack:
    stream_index: int
    codec: str | None = None
    profile: str | None = None
    channels: int | None = None
    channel_layout: str | None = None
    sample_rate: int | None = None
    bit_rate: int | None = None
    language: str | None = None
    title: str | None = None
    is_default: bool = False
    is_forced: bool = False
    #: True only when an object-audio substream is actually signalled. `None`
    #: means "not determinable from this probe", which is not the same as False
    #: and is never reported to the user as "no Atmos".
    object_audio: bool | None = None
    object_audio_source: str | None = None

    def as_dict(self) -> dict[str, Any]:
        return {
            "streamIndex": self.stream_index,
            "codec": self.codec,
            "profile": self.profile,
            "channels": self.channels,
            "channelLayout": self.channel_layout,
            "sampleRate": self.sample_rate,
            "bitRate": self.bit_rate,
            "language": self.language,
            "title": self.title,
            "default": self.is_default,
            "forced": self.is_forced,
            "objectAudio": self.object_audio,
            "objectAudioSource": self.object_audio_source,
        }


@dataclass(frozen=True, slots=True)
class SubtitleTrack:
    stream_index: int
    codec: str | None = None
    language: str | None = None
    title: str | None = None
    is_default: bool = False
    is_forced: bool = False

    def as_dict(self) -> dict[str, Any]:
        return {
            "streamIndex": self.stream_index,
            "codec": self.codec,
            "language": self.language,
            "title": self.title,
            "default": self.is_default,
            "forced": self.is_forced,
        }


@dataclass(frozen=True, slots=True)
class MediaInfo:
    source_url: str
    container: Container
    video: tuple[VideoTrack, ...] = ()
    audio: tuple[AudioTrack, ...] = ()
    subtitles: tuple[SubtitleTrack, ...] = ()
    #: Everything the probe could not settle. Empty is the normal case; a
    #: non-empty list is what the policy reads to stay honest about guessing.
    warnings: tuple[str, ...] = ()
    probe_duration_seconds: float | None = None

    @property
    def primary_video(self) -> VideoTrack | None:
        for track in self.video:
            if track.is_default:
                return track
        return self.video[0] if self.video else None

    def as_dict(self) -> dict[str, Any]:
        return {
            "source": self.source_url,
            "container": self.container.as_dict(),
            "video": [track.as_dict() for track in self.video],
            "audio": [track.as_dict() for track in self.audio],
            "subtitles": [track.as_dict() for track in self.subtitles],
            "warnings": list(self.warnings),
            "probeDurationSeconds": self.probe_duration_seconds,
        }


@dataclass(slots=True)
class ProbeResult:
    """Raw ffprobe output plus how it was obtained; parsing happens elsewhere."""

    format: dict[str, Any] = field(default_factory=dict)
    streams: list[dict[str, Any]] = field(default_factory=list)
    frames: list[dict[str, Any]] = field(default_factory=list)
    warnings: list[str] = field(default_factory=list)
    duration_seconds: float | None = None
