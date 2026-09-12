"""Turn raw ffprobe output into :class:`MediaInfo`.

ffprobe reports numbers as strings, rationals as `"35400/50000"`, missing
values as `"unknown"` or `"N/A"`, and puts some of the most important metadata
on frames rather than on streams. All of that is absorbed here so that every
consumer sees one shape.
"""

from __future__ import annotations

import logging
from fractions import Fraction
from typing import Any

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


LOG = logging.getLogger(__name__)

#: ffprobe's stand-ins for "there is no value here".
_UNSET = {"", "unknown", "N/A", "und", "unspecified", "reserved"}

SIDE_DATA_MASTERING = "Mastering display metadata"
SIDE_DATA_CONTENT_LIGHT = "Content light level metadata"
SIDE_DATA_HDR10_PLUS = "HDR Dynamic Metadata SMPTE2094-40 (HDR10+)"
SIDE_DATA_DOVI = "DOVI configuration record"

#: Pixel formats say both the depth and the subsampling; ffprobe has no
#: separate field for either.
_CHROMA_MARKERS = (
    ("444", ChromaSubsampling.YUV444),
    ("422", ChromaSubsampling.YUV422),
    ("440", ChromaSubsampling.YUV422),
    ("420", ChromaSubsampling.YUV420),
    ("411", ChromaSubsampling.YUV420),
)

#: Transfer characteristics that mean "this is PQ" / "this is HLG". ffprobe
#: uses the H.273 short names.
_PQ_TRANSFERS = {"smpte2084", "smpte st 2084"}
_HLG_TRANSFERS = {"arib-std-b67", "hlg"}
_WIDE_PRIMARIES = {"bt2020", "bt2020nc", "bt2020c", "bt2020-10", "bt2020-12"}

#: Codec profiles that carry object audio. Nothing else is treated as Atmos:
#: a filename saying "ATMOS" is not evidence, and guessing from the channel
#: count would label every 7.1 track as an object mix.
_OBJECT_AUDIO_PROFILES = {
    "dolby digital plus + dolby atmos": "eac3-joc",
    "dolby truehd + dolby atmos": "truehd-atmos",
    "dts-hd ma + dts:x": "dtsx",
    "dts-hd ma + dts:x imax": "dtsx-imax",
}


def _text(value: Any) -> str | None:
    if not isinstance(value, str):
        return None
    cleaned = value.strip()
    if not cleaned or cleaned in _UNSET:
        return None
    return cleaned


def _int(value: Any) -> int | None:
    if isinstance(value, bool):
        return None
    if isinstance(value, int):
        return value
    if isinstance(value, float):
        return int(value)
    if isinstance(value, str):
        try:
            return int(value.strip())
        except ValueError:
            try:
                return int(float(value.strip()))
            except ValueError:
                return None
    return None


def _float(value: Any) -> float | None:
    if isinstance(value, bool):
        return None
    if isinstance(value, (int, float)):
        return float(value)
    if isinstance(value, str):
        try:
            return float(value.strip())
        except ValueError:
            return None
    return None


def _rational(value: Any) -> float | None:
    """ffprobe writes rationals as `"a/b"`; a zero denominator means unset."""
    if isinstance(value, (int, float)) and not isinstance(value, bool):
        return float(value)
    text = _text(value)
    if text is None:
        return None
    try:
        fraction = Fraction(text)
    except (ValueError, ZeroDivisionError):
        return None
    return float(fraction)


def _flag(disposition: Any, key: str) -> bool:
    return bool(isinstance(disposition, dict) and disposition.get(key))


def _tag(stream: dict[str, Any], key: str) -> str | None:
    tags = stream.get("tags")
    if not isinstance(tags, dict):
        return None
    for candidate in (key, key.upper(), key.capitalize()):
        if candidate in tags:
            return _text(tags[candidate])
    return None


def _codec_tag(stream: dict[str, Any]) -> str | None:
    """ffprobe writes `[0][0][0][0]` for a container with no four-character code."""
    tag = _text(stream.get("codec_tag_string"))
    if tag is None or tag.startswith("[0]"):
        return None
    return tag


def _bit_depth(stream: dict[str, Any], pixel_format: str | None) -> int | None:
    explicit = _int(stream.get("bits_per_raw_sample"))
    if explicit and 1 <= explicit <= 16:
        return explicit
    if not pixel_format:
        return None
    for depth in (16, 14, 12, 10, 9):
        if f"p{depth}" in pixel_format or pixel_format.endswith(str(depth)):
            return depth
    if pixel_format.startswith(("yuv", "gbr", "rgb", "bgr", "gray", "nv")):
        return 8
    return None


def _chroma(pixel_format: str | None) -> ChromaSubsampling:
    if not pixel_format:
        return ChromaSubsampling.UNKNOWN
    lowered = pixel_format.lower()
    if lowered.startswith("gray"):
        return ChromaSubsampling.MONO
    if lowered.startswith(("rgb", "bgr", "gbr")):
        return ChromaSubsampling.RGB
    for marker, value in _CHROMA_MARKERS:
        if marker in lowered:
            return value
    if lowered.startswith(("nv12", "nv21", "nv15")):
        return ChromaSubsampling.YUV420
    if lowered.startswith(("nv16", "nv20", "nv24")):
        return ChromaSubsampling.YUV422
    return ChromaSubsampling.UNKNOWN


def _color_range(value: Any) -> ColorRange:
    text = (_text(value) or "").lower()
    if text in {"tv", "limited", "mpeg"}:
        return ColorRange.LIMITED
    if text in {"pc", "full", "jpeg"}:
        return ColorRange.FULL
    return ColorRange.UNKNOWN


def _side_data(container: dict[str, Any]) -> list[dict[str, Any]]:
    raw = container.get("side_data_list")
    if not isinstance(raw, list):
        return []
    return [item for item in raw if isinstance(item, dict)]


def _mastering_display(entry: dict[str, Any]) -> MasteringDisplay:
    def pair(prefix: str) -> tuple[float, float] | None:
        x = _rational(entry.get(f"{prefix}_x"))
        y = _rational(entry.get(f"{prefix}_y"))
        return (x, y) if x is not None and y is not None else None

    return MasteringDisplay(
        red=pair("red"),
        green=pair("green"),
        blue=pair("blue"),
        white_point=pair("white_point"),
        min_luminance=_rational(entry.get("min_luminance")),
        max_luminance=_rational(entry.get("max_luminance")),
    )


def _dolby_vision(entry: dict[str, Any]) -> DolbyVision:
    def bit(key: str) -> bool | None:
        value = _int(entry.get(key))
        return None if value is None else bool(value)

    return DolbyVision(
        profile=_int(entry.get("dv_profile")),
        level=_int(entry.get("dv_level")),
        bl_signal_compatibility_id=_int(entry.get("dv_bl_signal_compatibility_id")),
        rpu_present=bit("rpu_present_flag"),
        el_present=bit("el_present_flag"),
        bl_present=bit("bl_present_flag"),
    )


def classify_hdr(
    *,
    transfer: str | None,
    primaries: str | None,
    dolby_vision: DolbyVision | None,
    hdr10_plus: bool,
    bit_depth: int | None,
    mastering: MasteringDisplay | None,
) -> tuple[HdrFormat, list[str]]:
    """Decide what the video signals, and say what was contradictory.

    Dolby Vision outranks everything else because a DV stream's own transfer
    characteristics describe the base layer, not the presentation. After that
    the transfer function decides, because it is the only field that actually
    changes how a decoder must interpret the samples.
    """
    notes: list[str] = []
    transfer = (transfer or "").lower() or None
    primaries = (primaries or "").lower() or None

    if dolby_vision is not None and dolby_vision.profile is not None:
        return HdrFormat.DOLBY_VISION, notes

    if transfer in _PQ_TRANSFERS:
        if bit_depth is not None and bit_depth < 10:
            notes.append(
                f"PQ transfer signalled on an {bit_depth}-bit stream, which PQ does not define"
            )
        if primaries is not None and primaries not in _WIDE_PRIMARIES:
            notes.append(
                f"PQ transfer with {primaries} primaries; HDR10 expects BT.2020 primaries"
            )
        if hdr10_plus:
            return HdrFormat.HDR10_PLUS, notes
        if mastering is None:
            notes.append("PQ transfer without ST 2086 mastering display metadata")
        return HdrFormat.HDR10, notes

    if transfer in _HLG_TRANSFERS:
        if primaries is not None and primaries not in _WIDE_PRIMARIES:
            notes.append(f"HLG transfer with {primaries} primaries; HLG expects BT.2020")
        return HdrFormat.HLG, notes

    if hdr10_plus:
        notes.append("HDR10+ dynamic metadata present without a PQ transfer function")
        return HdrFormat.UNKNOWN, notes

    if transfer is None:
        if mastering is not None:
            # ST 2086 is only defined for PQ mastering, so its presence is real
            # evidence — but evidence is not a signalled transfer function, and
            # a display told nothing switches to nothing. Reported as unknown
            # with the reason spelled out rather than promoted to HDR10.
            notes.append(
                "ST 2086 mastering display metadata present without a signalled "
                "transfer function; the stream does not say it is PQ"
            )
            return HdrFormat.UNKNOWN, notes
        if primaries in _WIDE_PRIMARIES or (bit_depth is not None and bit_depth >= 10):
            notes.append(
                "no transfer characteristics signalled on a wide-gamut or 10-bit stream"
            )
            return HdrFormat.UNKNOWN, notes
        return HdrFormat.SDR, notes

    if primaries in _WIDE_PRIMARIES and transfer in {"bt709", "bt470bg", "smpte170m"}:
        notes.append(
            f"BT.2020 primaries with an SDR transfer ({transfer}); colour metadata is inconsistent"
        )
        return HdrFormat.UNKNOWN, notes

    return HdrFormat.SDR, notes


def _video_track(
    stream: dict[str, Any], frames: list[dict[str, Any]], warnings: list[str]
) -> VideoTrack:
    pixel_format = _text(stream.get("pix_fmt"))
    bit_depth = _bit_depth(stream, pixel_format)

    mastering: MasteringDisplay | None = None
    max_cll: int | None = None
    max_fall: int | None = None
    dolby_vision: DolbyVision | None = None
    hdr10_plus = False

    # Stream-level side data carries the DOVI record; frame-level side data
    # carries the mastering display, content light level and HDR10+.
    for entry in _side_data(stream):
        kind = entry.get("side_data_type")
        if kind == SIDE_DATA_DOVI:
            dolby_vision = _dolby_vision(entry)
        elif kind == SIDE_DATA_MASTERING:
            mastering = _mastering_display(entry)
        elif kind == SIDE_DATA_CONTENT_LIGHT:
            max_cll = _int(entry.get("max_content"))
            max_fall = _int(entry.get("max_average"))
        elif kind == SIDE_DATA_HDR10_PLUS:
            hdr10_plus = True

    for frame in frames:
        for entry in _side_data(frame):
            kind = entry.get("side_data_type")
            if kind == SIDE_DATA_MASTERING and mastering is None:
                mastering = _mastering_display(entry)
            elif kind == SIDE_DATA_CONTENT_LIGHT and max_cll is None:
                max_cll = _int(entry.get("max_content"))
                max_fall = _int(entry.get("max_average"))
            elif kind == SIDE_DATA_HDR10_PLUS:
                hdr10_plus = True
            elif kind == SIDE_DATA_DOVI and dolby_vision is None:
                dolby_vision = _dolby_vision(entry)

    codec_tag = _codec_tag(stream)
    if dolby_vision is None and codec_tag and codec_tag.lower() in {"dvh1", "dvhe", "dav1"}:
        # The container says Dolby Vision but the configuration record is
        # missing. That is exactly the case where guessing is dangerous, so it
        # is recorded as a DV stream whose profile is unknown.
        dolby_vision = DolbyVision()
        warnings.append(
            f"codec tag {codec_tag} signals Dolby Vision but no configuration record was found"
        )

    hdr, notes = classify_hdr(
        transfer=_text(stream.get("color_transfer")),
        primaries=_text(stream.get("color_primaries")),
        dolby_vision=dolby_vision,
        hdr10_plus=hdr10_plus,
        bit_depth=bit_depth,
        mastering=mastering,
    )
    warnings.extend(notes)

    return VideoTrack(
        stream_index=_int(stream.get("index")) or 0,
        codec=_text(stream.get("codec_name")),
        profile=_text(stream.get("profile")),
        level=_int(stream.get("level")),
        width=_int(stream.get("width")),
        height=_int(stream.get("height")),
        fps=_rational(stream.get("r_frame_rate")),
        pixel_format=pixel_format,
        bit_depth=bit_depth,
        chroma=_chroma(pixel_format),
        color_range=_color_range(stream.get("color_range")),
        color_matrix=_text(stream.get("color_space")),
        color_primaries=_text(stream.get("color_primaries")),
        color_transfer=_text(stream.get("color_transfer")),
        mastering_display=mastering,
        max_cll=max_cll,
        max_fall=max_fall,
        hdr=hdr,
        dolby_vision=dolby_vision,
        codec_tag=codec_tag,
        is_default=_flag(stream.get("disposition"), "default"),
    )


def _audio_track(stream: dict[str, Any]) -> AudioTrack:
    profile = _text(stream.get("profile"))
    object_audio: bool | None = None
    object_source: str | None = None
    if profile is not None:
        marker = _OBJECT_AUDIO_PROFILES.get(profile.lower())
        if marker is not None:
            object_audio, object_source = True, f"codec profile: {profile}"
    if object_audio is None and _text(stream.get("codec_name")) in {"truehd", "eac3", "dts"}:
        # These codecs *can* carry objects and ffprobe only reports it when it
        # actually parsed the substream. Unknown stays unknown.
        object_source = "not determinable from a header-only probe"

    return AudioTrack(
        stream_index=_int(stream.get("index")) or 0,
        codec=_text(stream.get("codec_name")),
        profile=profile,
        channels=_int(stream.get("channels")),
        channel_layout=_text(stream.get("channel_layout")),
        sample_rate=_int(stream.get("sample_rate")),
        bit_rate=_int(stream.get("bit_rate")) or _int(_tag(stream, "BPS")),
        language=_tag(stream, "language"),
        title=_tag(stream, "title"),
        is_default=_flag(stream.get("disposition"), "default"),
        is_forced=_flag(stream.get("disposition"), "forced"),
        object_audio=object_audio,
        object_audio_source=object_source,
    )


def _subtitle_track(stream: dict[str, Any]) -> SubtitleTrack:
    return SubtitleTrack(
        stream_index=_int(stream.get("index")) or 0,
        codec=_text(stream.get("codec_name")),
        language=_tag(stream, "language"),
        title=_tag(stream, "title"),
        is_default=_flag(stream.get("disposition"), "default"),
        is_forced=_flag(stream.get("disposition"), "forced"),
    )


def build_media_info(source_url: str, result: ProbeResult) -> MediaInfo:
    warnings = list(result.warnings)
    container = Container(
        format_name=_text(result.format.get("format_name")),
        format_long_name=_text(result.format.get("format_long_name")),
        duration_seconds=_float(result.format.get("duration")),
        bit_rate=_int(result.format.get("bit_rate")),
        size_bytes=_int(result.format.get("size")),
    )

    video: list[VideoTrack] = []
    audio: list[AudioTrack] = []
    subtitles: list[SubtitleTrack] = []
    for stream in result.streams:
        kind = stream.get("codec_type")
        if kind == "video":
            if _text(stream.get("codec_name")) in {"mjpeg", "png", "bmp", "gif"} and _flag(
                stream.get("disposition"), "attached_pic"
            ):
                continue  # cover art is not a video track
            video.append(_video_track(stream, result.frames if not video else [], warnings))
        elif kind == "audio":
            audio.append(_audio_track(stream))
        elif kind == "subtitle":
            subtitles.append(_subtitle_track(stream))

    if not video:
        warnings.append("the source has no video track")
    if not audio:
        warnings.append("the source has no audio track")

    # Deduplicate while keeping the order they were discovered in: the same
    # colour inconsistency can be reported once per probe pass.
    seen: set[str] = set()
    unique = tuple(w for w in warnings if not (w in seen or seen.add(w)))

    return MediaInfo(
        source_url=source_url,
        container=container,
        video=tuple(video),
        audio=tuple(audio),
        subtitles=tuple(subtitles),
        warnings=unique,
        probe_duration_seconds=result.duration_seconds,
    )
