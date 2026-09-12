"""Preview eligibility, computed separately from Kodi playback.

A browser and the television are two different devices with two different
decoders, and conflating them is what produced a permanently busy core: the
streaming server, finding no hardware encode profile it can use on this board,
falls back to a software encode of whatever the browser cannot decode. A 4K
HEVC software decode into an H.264 software encode is refused here rather than
started and then cleaned up.

`PREVIEW_UNSUPPORTED` is a normal answer. It means "watch it on the
television", which is what the appliance is for.
"""

from __future__ import annotations

from dataclasses import dataclass
from enum import Enum
from typing import Any

from ..inspector.model import HdrFormat, MediaInfo
from . import reasons as R
from .capabilities import CapabilityProfile, get_profile


class PreviewMode(str, Enum):
    #: The browser opens the source itself.
    BROWSER_DIRECT = "BrowserDirect"
    #: Streams are copied into a container the browser opens. No encoder.
    BROWSER_REMUX = "BrowserRemux"
    UNSUPPORTED = "Unsupported"


@dataclass(frozen=True, slots=True)
class PreviewDecision:
    mode: PreviewMode
    reasons: tuple[R.Reason, ...]

    def as_dict(self) -> dict[str, Any]:
        return {
            "mode": self.mode.value,
            "reasons": [reason.as_dict() for reason in self.reasons],
        }


def decide_preview(info: MediaInfo, profile: CapabilityProfile | None = None) -> PreviewDecision:
    profile = profile or get_profile()
    caps = profile.browser
    video = info.primary_video
    notes: list[R.Reason] = []

    if video is None:
        return PreviewDecision(
            PreviewMode.UNSUPPORTED,
            (R.blocking(R.PREVIEW_UNSUPPORTED, "the source has no video track to preview"),),
        )

    codec = (video.codec or "").lower()
    blockers: list[R.Reason] = []
    if codec not in caps.video_codecs:
        blockers.append(
            R.blocking(
                R.PREVIEW_SOFTWARE_TRANSCODE_REFUSED,
                f"{video.codec or 'this video codec'} is not decoded by the browser, and "
                "re-encoding it would be a software video transcode on a board with no "
                "hardware encoder",
                codec=video.codec,
            )
        )
    if video.bit_depth is not None and video.bit_depth > caps.max_bit_depth:
        blockers.append(
            R.blocking(
                R.PREVIEW_SOFTWARE_TRANSCODE_REFUSED,
                f"{video.bit_depth}-bit video would have to be converted to "
                f"{caps.max_bit_depth}-bit in software",
                bitDepth=video.bit_depth,
            )
        )
    if video.hdr not in caps.hdr_formats and video.hdr is not HdrFormat.UNKNOWN:
        blockers.append(
            R.blocking(
                R.PREVIEW_SOFTWARE_TRANSCODE_REFUSED,
                f"{video.hdr.value} would have to be tone mapped in software for a browser",
                hdr=video.hdr.value,
            )
        )

    if blockers:
        blockers.append(
            R.info(
                R.PREVIEW_UNSUPPORTED,
                "this source cannot be previewed in a browser; it is still playable on "
                "the television",
            )
        )
        return PreviewDecision(PreviewMode.UNSUPPORTED, tuple(blockers))

    container = (info.container.format_name or "").lower()
    container_ok = container in caps.containers or any(
        part in caps.containers for part in container.split(",")
    )
    audio_ok = any((track.codec or "").lower() in caps.audio_codecs for track in info.audio)

    if container_ok and (audio_ok or not info.audio):
        notes.append(
            R.info(R.PREVIEW_BROWSER_DIRECT, "the browser opens this source directly")
        )
        return PreviewDecision(PreviewMode.BROWSER_DIRECT, tuple(notes))

    notes.append(
        R.info(
            R.PREVIEW_BROWSER_REMUX,
            "the streams are copied into a browser-openable container; no video encoder runs",
            container=info.container.format_name,
        )
    )
    return PreviewDecision(PreviewMode.BROWSER_REMUX, tuple(notes))
