"""ffprobe-shaped fixtures.

The media core's hardest cases are the ones no asset on the workstation
carries: Dolby Vision Profile 5, Profile 7, Profile 8 with each base-layer
compatibility id, HDR10+, and colour metadata that contradicts itself. Those
are written here as the exact JSON ffprobe produces for them, so the policy is
tested against the real shape rather than against hand-built model objects.
"""

from __future__ import annotations

from typing import Any

from ..inspector.model import ProbeResult
from ..inspector.parse import build_media_info


def disposition(**flags: int) -> dict[str, int]:
    base = {
        "default": 0,
        "dub": 0,
        "original": 0,
        "comment": 0,
        "forced": 0,
        "attached_pic": 0,
    }
    base.update(flags)
    return base


def video_stream(
    *,
    index: int = 0,
    codec_name: str = "hevc",
    profile: str | None = "Main 10",
    width: int = 3840,
    height: int = 2160,
    pix_fmt: str = "yuv420p10le",
    color_primaries: str | None = "bt2020",
    color_transfer: str | None = "smpte2084",
    color_space: str | None = "bt2020nc",
    color_range: str | None = "tv",
    r_frame_rate: str = "24000/1001",
    level: int = 150,
    codec_tag_string: str = "[0][0][0][0]",
    side_data_list: list[dict[str, Any]] | None = None,
    default: int = 1,
) -> dict[str, Any]:
    stream: dict[str, Any] = {
        "index": index,
        "codec_name": codec_name,
        "codec_type": "video",
        "codec_tag_string": codec_tag_string,
        "width": width,
        "height": height,
        "pix_fmt": pix_fmt,
        "level": level,
        "r_frame_rate": r_frame_rate,
        "disposition": disposition(default=default),
    }
    if profile is not None:
        stream["profile"] = profile
    if color_primaries is not None:
        stream["color_primaries"] = color_primaries
    if color_transfer is not None:
        stream["color_transfer"] = color_transfer
    if color_space is not None:
        stream["color_space"] = color_space
    if color_range is not None:
        stream["color_range"] = color_range
    if side_data_list:
        stream["side_data_list"] = side_data_list
    return stream


def audio_stream(
    *,
    index: int = 1,
    codec_name: str = "ac3",
    profile: str | None = None,
    channels: int = 6,
    channel_layout: str = "5.1(side)",
    sample_rate: int = 48000,
    bit_rate: int | None = 640000,
    language: str | None = "eng",
    default: int = 1,
    title: str | None = None,
) -> dict[str, Any]:
    stream: dict[str, Any] = {
        "index": index,
        "codec_name": codec_name,
        "codec_type": "audio",
        "channels": channels,
        "channel_layout": channel_layout,
        "sample_rate": str(sample_rate),
        "disposition": disposition(default=default),
    }
    if profile is not None:
        stream["profile"] = profile
    if bit_rate is not None:
        stream["bit_rate"] = str(bit_rate)
    tags: dict[str, str] = {}
    if language:
        tags["language"] = language
    if title:
        tags["title"] = title
    if tags:
        stream["tags"] = tags
    return stream


def subtitle_stream(
    *, index: int = 2, codec_name: str = "subrip", language: str = "eng", forced: int = 0, default: int = 0
) -> dict[str, Any]:
    return {
        "index": index,
        "codec_name": codec_name,
        "codec_type": "subtitle",
        "disposition": disposition(default=default, forced=forced),
        "tags": {"language": language},
    }


MASTERING_DISPLAY = {
    "side_data_type": "Mastering display metadata",
    "red_x": "35400/50000",
    "red_y": "14600/50000",
    "green_x": "8500/50000",
    "green_y": "39850/50000",
    "blue_x": "6550/50000",
    "blue_y": "2300/50000",
    "white_point_x": "15635/50000",
    "white_point_y": "16450/50000",
    "min_luminance": "50/10000",
    "max_luminance": "10000000/10000",
}

CONTENT_LIGHT = {
    "side_data_type": "Content light level metadata",
    "max_content": 1000,
    "max_average": 400,
}

HDR10_PLUS = {"side_data_type": "HDR Dynamic Metadata SMPTE2094-40 (HDR10+)"}


def dovi(
    *,
    profile: int,
    level: int = 6,
    compatibility_id: int = 0,
    rpu: int = 1,
    el: int = 0,
    bl: int = 1,
) -> dict[str, Any]:
    return {
        "side_data_type": "DOVI configuration record",
        "dv_version_major": 1,
        "dv_version_minor": 0,
        "dv_profile": profile,
        "dv_level": level,
        "rpu_present_flag": rpu,
        "el_present_flag": el,
        "bl_present_flag": bl,
        "dv_bl_signal_compatibility_id": compatibility_id,
    }


def probe_result(
    streams: list[dict[str, Any]],
    *,
    format_name: str = "matroska,webm",
    duration: str = "6343.520000",
    bit_rate: str = "15236005",
    size: str = "12081238172",
    frames: list[dict[str, Any]] | None = None,
    warnings: list[str] | None = None,
) -> ProbeResult:
    return ProbeResult(
        format={
            "format_name": format_name,
            "format_long_name": "Matroska / WebM",
            "duration": duration,
            "bit_rate": bit_rate,
            "size": size,
        },
        streams=streams,
        frames=frames or [],
        warnings=list(warnings or []),
        duration_seconds=0.1,
    )


def media(
    streams: list[dict[str, Any]],
    *,
    source: str = "https://example.invalid/title.mkv",
    frames: list[dict[str, Any]] | None = None,
    **kwargs: Any,
):
    return build_media_info(source, probe_result(streams, frames=frames, **kwargs))


# ---------------------------------------------------------------- named sources


def hdr10_hevc_eac3():
    """The canonical appliance asset: 4K HEVC Main10 HDR10 with E-AC-3 5.1."""
    return media(
        [video_stream(), audio_stream(codec_name="eac3", bit_rate=640000)],
        frames=[{"side_data_list": [MASTERING_DISPLAY, CONTENT_LIGHT]}],
    )


def hdr10_hevc_truehd_atmos():
    return media(
        [
            video_stream(),
            audio_stream(
                codec_name="truehd",
                profile="Dolby TrueHD + Dolby Atmos",
                channels=8,
                channel_layout="7.1",
                bit_rate=4000000,
            ),
        ],
        frames=[{"side_data_list": [MASTERING_DISPLAY, CONTENT_LIGHT]}],
    )


def hdr10_hevc_truehd_and_ac3():
    return media(
        [
            video_stream(),
            audio_stream(
                index=1,
                codec_name="truehd",
                profile="Dolby TrueHD + Dolby Atmos",
                channels=8,
                channel_layout="7.1",
                bit_rate=4000000,
                default=1,
            ),
            audio_stream(index=2, codec_name="ac3", channels=6, bit_rate=640000, default=0),
        ],
        frames=[{"side_data_list": [MASTERING_DISPLAY, CONTENT_LIGHT]}],
    )


def sdr_h264_aac():
    return media(
        [
            video_stream(
                codec_name="h264",
                profile="High",
                width=1920,
                height=1080,
                pix_fmt="yuv420p",
                color_primaries="bt709",
                color_transfer="bt709",
                color_space="bt709",
                r_frame_rate="24000/1001",
            ),
            audio_stream(codec_name="aac", profile="LC", channels=2, channel_layout="stereo", bit_rate=128000),
        ],
        format_name="mov,mp4,m4a,3gp,3g2,mj2",
        bit_rate="4500000",
    )


def hdr10_plus_hevc_ac3():
    return media(
        [video_stream(), audio_stream(codec_name="ac3")],
        frames=[{"side_data_list": [MASTERING_DISPLAY, CONTENT_LIGHT, HDR10_PLUS]}],
    )


def hlg_hevc_aac():
    return media(
        [
            video_stream(color_transfer="arib-std-b67"),
            audio_stream(codec_name="aac", profile="LC", channels=2, channel_layout="stereo"),
        ]
    )


def dolby_vision_profile5():
    return media(
        [
            video_stream(codec_tag_string="dvhe", side_data_list=[dovi(profile=5, compatibility_id=0)]),
            audio_stream(codec_name="eac3"),
        ]
    )


def dolby_vision_profile7():
    return media(
        [
            video_stream(side_data_list=[dovi(profile=7, compatibility_id=0, el=1)]),
            audio_stream(codec_name="truehd", profile="Dolby TrueHD + Dolby Atmos", channels=8),
        ],
        frames=[{"side_data_list": [MASTERING_DISPLAY, CONTENT_LIGHT]}],
    )


def dolby_vision_profile8(compatibility_id: int = 1):
    return media(
        [
            video_stream(side_data_list=[dovi(profile=8, compatibility_id=compatibility_id)]),
            audio_stream(codec_name="eac3"),
        ],
        frames=[{"side_data_list": [MASTERING_DISPLAY, CONTENT_LIGHT]}],
    )


def conflicting_color_metadata():
    """BT.2020 primaries with a BT.709 transfer: the file contradicts itself."""
    return media(
        [
            video_stream(color_primaries="bt2020", color_transfer="bt709", color_space="bt2020nc"),
            audio_stream(codec_name="ac3"),
        ]
    )


def unknown_color_metadata():
    return media(
        [
            video_stream(color_primaries=None, color_transfer=None, color_space=None),
            audio_stream(codec_name="ac3"),
        ]
    )
