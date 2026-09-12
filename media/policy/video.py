"""Video, colour and HDR decision for one track against one capability profile.

The decision this module makes is narrow on purpose: *can the accepted
RK3588 decode and display path carry these samples unchanged?* It never
proposes re-encoding video. If the answer is no, the answer is no, and the
source loses to a better one.
"""

from __future__ import annotations

from dataclasses import dataclass
from enum import Enum
from typing import Any

from ..inspector.model import ChromaSubsampling, ColorRange, HdrFormat, VideoTrack
from . import reasons as R
from .capabilities import CapabilityProfile


class VideoVerdict(str, Enum):
    #: Samples are handed to the decoder untouched.
    DIRECT = "Direct"
    #: Decodable, but something about the presentation is unverified. Playable,
    #: and beaten by any Direct alternative during ranking.
    RISKY = "Risky"
    UNSUPPORTED = "Unsupported"


@dataclass(frozen=True, slots=True)
class VideoDecision:
    verdict: VideoVerdict
    reasons: tuple[R.Reason, ...]
    hdr: HdrFormat
    #: The track the decision is about. Carried rather than re-derived so the
    #: session layer maps the stream the policy actually judged.
    track: VideoTrack | None = None
    #: Set when the source should be avoided in favour of another rendition.
    prefer_alternative: bool = False

    @property
    def stream_index(self) -> int | None:
        return None if self.track is None else self.track.stream_index

    def as_dict(self) -> dict[str, Any]:
        return {
            "verdict": self.verdict.value,
            "hdr": self.hdr.value,
            "streamIndex": self.stream_index,
            "preferAlternative": self.prefer_alternative,
            "track": self.track.as_dict() if self.track else None,
            "reasons": [reason.as_dict() for reason in self.reasons],
        }


#: Dolby Vision base-layer compatibility ids, from the DOVI configuration
#: record. The id — not the profile number — is what says whether a decoder
#: with no RPU support may use the base layer as it stands.
DV_COMPAT_NONE = 0
DV_COMPAT_HDR10 = 1
DV_COMPAT_SDR = 2
DV_COMPAT_HLG = 4
DV_COMPAT_BLURAY = 6

_DV_COMPAT_FORMAT = {
    DV_COMPAT_HDR10: HdrFormat.HDR10,
    DV_COMPAT_SDR: HdrFormat.SDR,
    DV_COMPAT_HLG: HdrFormat.HLG,
    DV_COMPAT_BLURAY: HdrFormat.HDR10,
}


def _dolby_vision_decision(
    track: VideoTrack, profile: CapabilityProfile
) -> tuple[VideoVerdict, list[R.Reason], HdrFormat, bool]:
    """Decide what a Dolby Vision stream is worth on a device with no DV path.

    The failure this guards against is the green-and-magenta picture. Profile 5
    encodes its base layer in IPTPQc2 with a cross-talk matrix that only a DV
    decoder undoes; a plain HEVC Main 10 decoder produces a picture with the
    channels in the wrong places, which is exactly the reported symptom. So a
    Profile 5 stream must never be chosen as Direct on this device.

    Profiles 7 and 8 are different, and the difference is stated in the stream:
    Profile 7's base layer is HDR10 by definition of the profile, and Profile 8
    says what its base layer is compatible with in
    `dv_bl_signal_compatibility_id`. Where the stream says nothing, nothing is
    assumed.
    """
    dv = track.dolby_vision
    assert dv is not None
    notes: list[R.Reason] = []

    if profile.video.dolby_vision_pipeline:
        notes.append(
            R.info(
                R.HDR_FORMAT_SUPPORTED,
                "Dolby Vision is carried end to end by this profile",
                dvProfile=dv.profile,
            )
        )
        return VideoVerdict.DIRECT, notes, HdrFormat.DOLBY_VISION, False

    notes.append(
        R.info(
            R.DV_UNSUPPORTED_PIPELINE,
            "this device has no verified Dolby Vision pipeline; only the base "
            "layer can be played",
            capabilityProfile=profile.name,
        )
    )

    if dv.profile is None:
        notes.append(
            R.risk(
                R.DV_PROFILE_UNKNOWN,
                "the stream signals Dolby Vision but not which profile; the base "
                "layer cannot be shown to be viewable",
            )
        )
        return VideoVerdict.RISKY, notes, HdrFormat.DOLBY_VISION, True

    compat = dv.bl_signal_compatibility_id

    if dv.profile == 5:
        notes.append(
            R.risk(
                R.DV_PROFILE5_NO_BASE_LAYER,
                "Dolby Vision Profile 5 has no backward-compatible base layer: "
                "its IPTPQc2 samples decoded without an RPU produce the known "
                "green/magenta picture",
                dvProfile=5,
                blSignalCompatibilityId=compat,
            )
        )
        return VideoVerdict.RISKY, notes, HdrFormat.DOLBY_VISION, True

    if dv.profile == 7:
        # Profile 7 is dual-layer with an HDR10 base layer by definition. Only
        # the enhancement layer and RPU are lost, and those are what this
        # device could not use anyway.
        if dv.bl_present is False:
            notes.append(
                R.risk(
                    R.DV_PROFILE5_NO_BASE_LAYER,
                    "Profile 7 stream that does not carry its base layer",
                    dvProfile=7,
                )
            )
            return VideoVerdict.RISKY, notes, HdrFormat.DOLBY_VISION, True
        notes.append(
            R.warning(
                R.DV_PROFILE7_BASE_LAYER_HDR10,
                "Dolby Vision Profile 7 carries an HDR10 base layer; it plays as "
                "HDR10 with the enhancement layer and RPU dropped",
                dvProfile=7,
            )
        )
        notes.append(
            R.warning(R.DV_DYNAMIC_METADATA_LOST, "Dolby Vision dynamic metadata is not applied")
        )
        return VideoVerdict.DIRECT, notes, HdrFormat.HDR10, False

    if compat in _DV_COMPAT_FORMAT:
        fallback = _DV_COMPAT_FORMAT[compat]
        if fallback not in profile.video.hdr_formats:
            notes.append(
                R.risk(
                    R.HDR_FORMAT_UNSUPPORTED,
                    f"the base layer is {fallback.value}, which this profile does not carry",
                    dvProfile=dv.profile,
                    blSignalCompatibilityId=compat,
                )
            )
            return VideoVerdict.RISKY, notes, HdrFormat.DOLBY_VISION, True
        notes.append(
            R.warning(
                R.DV_BASE_LAYER_COMPATIBLE,
                f"the Dolby Vision base layer is signalled {fallback.value} compatible "
                f"and plays as {fallback.value}",
                dvProfile=dv.profile,
                blSignalCompatibilityId=compat,
            )
        )
        notes.append(
            R.warning(R.DV_DYNAMIC_METADATA_LOST, "Dolby Vision dynamic metadata is not applied")
        )
        return VideoVerdict.DIRECT, notes, fallback, False

    notes.append(
        R.risk(
            R.DV_PROFILE5_NO_BASE_LAYER
            if compat == DV_COMPAT_NONE
            else R.DV_PROFILE_UNKNOWN,
            "the Dolby Vision base layer is not signalled compatible with any "
            "format this device displays"
            + (
                ""
                if compat is None
                else f" (bl_signal_compatibility_id={compat})"
            ),
            dvProfile=dv.profile,
            blSignalCompatibilityId=compat,
        )
    )
    return VideoVerdict.RISKY, notes, HdrFormat.DOLBY_VISION, True


def decide_video(
    track: VideoTrack | None, profile: CapabilityProfile, *, source_warnings: tuple[str, ...] = ()
) -> VideoDecision:
    notes: list[R.Reason] = []

    if track is None:
        return VideoDecision(
            verdict=VideoVerdict.UNSUPPORTED,
            reasons=(R.blocking(R.VIDEO_TRACK_MISSING, "the source has no video track"),),
            hdr=HdrFormat.UNKNOWN,
            track=None,
        )

    caps = profile.video
    blocking: list[R.Reason] = []

    codec = (track.codec or "").lower()
    if codec not in caps.codecs:
        blocking.append(
            R.blocking(
                R.VIDEO_CODEC_UNSUPPORTED,
                f"{track.codec or 'unknown'} is not decoded by this profile",
                codec=track.codec,
            )
        )
    else:
        allowed = caps.profiles.get(codec)
        if allowed is not None and track.profile is not None and track.profile not in allowed:
            blocking.append(
                R.blocking(
                    R.VIDEO_PROFILE_UNSUPPORTED,
                    f"{track.codec} profile '{track.profile}' is outside the hardware decoder's profiles",
                    codec=track.codec,
                    profile=track.profile,
                )
            )
        else:
            notes.append(
                R.info(
                    R.VIDEO_CODEC_SUPPORTED,
                    f"{track.codec} {track.profile or ''}".strip() + " is hardware decoded",
                    codec=track.codec,
                    profile=track.profile,
                )
            )

    if track.bit_depth is not None and track.bit_depth not in caps.bit_depths:
        blocking.append(
            R.blocking(
                R.VIDEO_BIT_DEPTH_UNSUPPORTED,
                f"{track.bit_depth}-bit video is not supported by this profile",
                bitDepth=track.bit_depth,
            )
        )

    if track.chroma is ChromaSubsampling.UNKNOWN:
        notes.append(
            R.warning(
                R.COLOR_METADATA_INCOMPLETE,
                "the chroma subsampling could not be determined from the pixel format",
                pixelFormat=track.pixel_format,
            )
        )
    elif track.chroma not in caps.chroma:
        blocking.append(
            R.blocking(
                R.VIDEO_CHROMA_UNSUPPORTED,
                f"{track.chroma.value} chroma subsampling is not decoded by this profile",
                chroma=track.chroma.value,
            )
        )

    if (track.width or 0) > caps.max_width or (track.height or 0) > caps.max_height:
        blocking.append(
            R.blocking(
                R.VIDEO_RESOLUTION_UNSUPPORTED,
                f"{track.width}x{track.height} exceeds {caps.max_width}x{caps.max_height}",
                width=track.width,
                height=track.height,
            )
        )

    if track.fps is not None and track.fps > caps.max_fps + 0.5:
        blocking.append(
            R.blocking(
                R.VIDEO_FRAMERATE_UNSUPPORTED,
                f"{track.fps:.3f} fps exceeds {caps.max_fps:g} fps",
                fps=track.fps,
            )
        )

    if track.color_range is ColorRange.FULL:
        notes.append(
            R.warning(
                R.VIDEO_COLOR_RANGE_FULL,
                "the stream signals full-range samples; the HDMI output path is "
                "limited range and the driver must convert",
            )
        )

    # Colour / HDR. Dolby Vision replaces the plain HDR branch entirely.
    hdr = track.hdr
    prefer_alternative = False
    if track.dolby_vision is not None:
        dv_verdict, dv_notes, hdr, prefer_alternative = _dolby_vision_decision(track, profile)
        notes.extend(dv_notes)
        if blocking:
            return VideoDecision(VideoVerdict.UNSUPPORTED, tuple(blocking + notes), hdr, track)
        return VideoDecision(dv_verdict, tuple(notes), hdr, track, prefer_alternative)

    if hdr is HdrFormat.UNKNOWN:
        conflict = any(
            "inconsistent" in text or "does not define" in text or "expects" in text
            for text in source_warnings
        )
        notes.append(
            R.warning(
                R.COLOR_METADATA_CONFLICT if conflict else R.COLOR_METADATA_INCOMPLETE,
                "the colour metadata is "
                + ("self-contradictory" if conflict else "incomplete")
                + "; the samples decode, but the display may not be driven in the "
                "intended colour volume",
                colorPrimaries=track.color_primaries,
                colorTransfer=track.color_transfer,
                colorMatrix=track.color_matrix,
            )
        )
    elif hdr is HdrFormat.HDR10_PLUS:
        notes.append(
            R.warning(
                R.HDR10_PLUS_BASE_LAYER_ONLY,
                "HDR10+ dynamic metadata is not applied; the HDR10 base layer plays",
            )
        )
    elif hdr not in caps.hdr_formats:
        blocking.append(
            R.blocking(
                R.HDR_FORMAT_UNSUPPORTED,
                f"{hdr.value} is not carried by this display pipeline",
                hdr=hdr.value,
            )
        )
    else:
        notes.append(
            R.info(R.HDR_FORMAT_SUPPORTED, f"{hdr.value} is carried end to end", hdr=hdr.value)
        )

    if blocking:
        return VideoDecision(VideoVerdict.UNSUPPORTED, tuple(blocking + notes), hdr, track)
    return VideoDecision(VideoVerdict.DIRECT, tuple(notes), hdr, track, prefer_alternative)
