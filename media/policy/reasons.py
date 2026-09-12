"""Reason codes.

Every decision the media core makes carries reasons, and every reason has a
stable code. The code is the contract: a user interface, a log line and a test
all key on the code, and only the message is allowed to change wording.
"""

from __future__ import annotations

from dataclasses import dataclass
from enum import Enum
from typing import Any


class Severity(str, Enum):
    INFO = "info"
    #: Playable, but something is lost or uncertain.
    WARNING = "warning"
    #: Playable only in a way this appliance has not verified. A risk is what
    #: makes a source lose to a safe alternative in ranking.
    RISK = "risk"
    BLOCKING = "blocking"


@dataclass(frozen=True, slots=True)
class Reason:
    code: str
    severity: Severity
    message: str
    details: dict[str, Any] | None = None

    def as_dict(self) -> dict[str, Any]:
        payload: dict[str, Any] = {
            "code": self.code,
            "severity": self.severity.value,
            "message": self.message,
        }
        if self.details:
            payload["details"] = self.details
        return payload


def info(code: str, message: str, **details: Any) -> Reason:
    return Reason(code, Severity.INFO, message, details or None)


def warning(code: str, message: str, **details: Any) -> Reason:
    return Reason(code, Severity.WARNING, message, details or None)


def risk(code: str, message: str, **details: Any) -> Reason:
    return Reason(code, Severity.RISK, message, details or None)


def blocking(code: str, message: str, **details: Any) -> Reason:
    return Reason(code, Severity.BLOCKING, message, details or None)


# -- video / colour -----------------------------------------------------------
VIDEO_CODEC_SUPPORTED = "VIDEO_CODEC_SUPPORTED"
VIDEO_CODEC_UNSUPPORTED = "VIDEO_CODEC_UNSUPPORTED"
VIDEO_PROFILE_UNSUPPORTED = "VIDEO_PROFILE_UNSUPPORTED"
VIDEO_BIT_DEPTH_UNSUPPORTED = "VIDEO_BIT_DEPTH_UNSUPPORTED"
VIDEO_CHROMA_UNSUPPORTED = "VIDEO_CHROMA_UNSUPPORTED"
VIDEO_RESOLUTION_UNSUPPORTED = "VIDEO_RESOLUTION_UNSUPPORTED"
VIDEO_FRAMERATE_UNSUPPORTED = "VIDEO_FRAMERATE_UNSUPPORTED"
VIDEO_TRACK_MISSING = "VIDEO_TRACK_MISSING"
VIDEO_COLOR_RANGE_FULL = "VIDEO_COLOR_RANGE_FULL"

HDR_FORMAT_SUPPORTED = "HDR_FORMAT_SUPPORTED"
HDR10_PLUS_BASE_LAYER_ONLY = "HDR10_PLUS_BASE_LAYER_ONLY"
HDR_FORMAT_UNSUPPORTED = "HDR_FORMAT_UNSUPPORTED"
COLOR_METADATA_INCOMPLETE = "COLOR_METADATA_INCOMPLETE"
COLOR_METADATA_CONFLICT = "COLOR_METADATA_CONFLICT"

DV_UNSUPPORTED_PIPELINE = "DV_UNSUPPORTED_PIPELINE"
DV_PROFILE5_NO_BASE_LAYER = "DV_PROFILE5_NO_BASE_LAYER"
DV_PROFILE7_BASE_LAYER_HDR10 = "DV_PROFILE7_BASE_LAYER_HDR10"
DV_BASE_LAYER_COMPATIBLE = "DV_BASE_LAYER_COMPATIBLE"
DV_PROFILE_UNKNOWN = "DV_PROFILE_UNKNOWN"
DV_DYNAMIC_METADATA_LOST = "DV_DYNAMIC_METADATA_LOST"

# -- audio --------------------------------------------------------------------
AUDIO_PASSTHROUGH = "AUDIO_PASSTHROUGH"
AUDIO_DECODE_PCM = "AUDIO_DECODE_PCM"
AUDIO_TRANSCODE_AC3 = "AUDIO_TRANSCODE_AC3"
AUDIO_NATIVE_TRACK_PREFERRED = "AUDIO_NATIVE_TRACK_PREFERRED"
AUDIO_OBJECT_METADATA_LOST = "AUDIO_OBJECT_METADATA_LOST"
AUDIO_LOSSLESS_TO_LOSSY = "AUDIO_LOSSLESS_TO_LOSSY"
AUDIO_CHANNELS_REDUCED = "AUDIO_CHANNELS_REDUCED"
AUDIO_CODEC_UNKNOWN = "AUDIO_CODEC_UNKNOWN"
AUDIO_TRACK_MISSING = "AUDIO_TRACK_MISSING"
AUDIO_SAMPLE_RATE_UNSUPPORTED = "AUDIO_SAMPLE_RATE_UNSUPPORTED"

# -- container / delivery -----------------------------------------------------
CONTAINER_SUPPORTED = "CONTAINER_SUPPORTED"
CONTAINER_REMUX_REQUIRED = "CONTAINER_REMUX_REQUIRED"
SOURCE_UNRESOLVED = "SOURCE_UNRESOLVED"
SAFER_ALTERNATIVE_EXPECTED = "SAFER_ALTERNATIVE_EXPECTED"

# -- preview ------------------------------------------------------------------
PREVIEW_BROWSER_DIRECT = "PREVIEW_BROWSER_DIRECT"
PREVIEW_BROWSER_REMUX = "PREVIEW_BROWSER_REMUX"
PREVIEW_UNSUPPORTED = "PREVIEW_UNSUPPORTED"
PREVIEW_SOFTWARE_TRANSCODE_REFUSED = "PREVIEW_SOFTWARE_TRANSCODE_REFUSED"
