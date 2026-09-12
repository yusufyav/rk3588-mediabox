"""What this appliance can actually play, in one place.

Every capability statement below is either a measured result from an earlier
gate or an explicit "not verified". Nothing is here because it seemed likely.
The point of the profile is that the answer to "can we play this?" is a lookup
against one object, not a scattering of `if codec == ...` across the codebase.

Sources for the accepted values:

* `results/orangepi5-ultra-vendor/real-hdr10-playback-mp1b-2026-09-09.md`
  — 4K23.976 HEVC Main10, NV15, HDR10, BT.2020, 10-bit output over HDMI.
* `results/orangepi5-ultra-vendor/ma1-hdmi-passthrough-2026-09-11.md`
  — AC-3 and E-AC-3 passthrough audible; DTS declared by the sink's ELD but
    *silent*; 6-channel LPCM declared but *silent*; TrueHD and DTS-HD absent
    from the ELD; Kodi's own AC-3 transcode produces silence on this sink.

That last finding is the reason the media core encodes AC-3 itself: the sink
plays a real AC-3 *file's* frames perfectly and plays Kodi's encoder output not
at all, so the conversion has to happen upstream of Kodi.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any

from ..inspector.model import ChromaSubsampling, HdrFormat


@dataclass(frozen=True, slots=True)
class VideoCapabilities:
    #: Decoder codecs, keyed by ffprobe's `codec_name`.
    codecs: frozenset[str]
    #: Per-codec profile allowlist. A codec absent from this map accepts any
    #: profile; a codec present accepts only the listed ones.
    profiles: dict[str, frozenset[str]]
    bit_depths: frozenset[int]
    chroma: frozenset[ChromaSubsampling]
    max_width: int
    max_height: int
    max_fps: float
    #: HDR signalling the display pipeline is known to carry end to end.
    hdr_formats: frozenset[HdrFormat]
    #: True only when a Dolby Vision pipeline has been verified on the device.
    #: `False` does not mean "DV files fail"; it means the RPU is not applied,
    #: which is exactly what makes a profile without a compatible base layer
    #: unsafe.
    dolby_vision_pipeline: bool


@dataclass(frozen=True, slots=True)
class AudioCapabilities:
    #: Codecs handed to the sink as a compressed bitstream, bit exact.
    passthrough_codecs: frozenset[str]
    #: Codecs the player may decode to PCM for this sink.
    decode_codecs: frozenset[str]
    #: How many PCM channels the sink actually reproduces. On this board the
    #: ELD declares six and the television plays none of them, so this is 2.
    max_pcm_channels: int
    #: Channel count the passthrough carriers accept.
    max_passthrough_channels: int
    supported_sample_rates: frozenset[int]
    #: The codec every non-playable track is converted to.
    transcode_target: str = "ac3"
    transcode_max_channels: int = 6


@dataclass(frozen=True, slots=True)
class ContainerCapabilities:
    #: Containers the player opens directly over HTTP.
    direct: frozenset[str]
    #: What a remux targets when the source container is not directly usable.
    remux_target: str = "matroska"


@dataclass(frozen=True, slots=True)
class BrowserCapabilities:
    """What a plain browser on the appliance's own network can preview.

    Deliberately narrow. This board rejects every hardware encode profile the
    streaming server offers, so anything outside this set would be previewed by
    a software encoder, and a software 4K HEVC decode into an H.264 encode is
    the one thing this appliance must never do.
    """

    video_codecs: frozenset[str]
    audio_codecs: frozenset[str]
    max_bit_depth: int
    hdr_formats: frozenset[HdrFormat]
    containers: frozenset[str]
    allow_software_video_transcode: bool = False


@dataclass(frozen=True, slots=True)
class CapabilityProfile:
    name: str
    description: str
    video: VideoCapabilities
    audio: AudioCapabilities
    container: ContainerCapabilities
    browser: BrowserCapabilities
    evidence: tuple[str, ...] = ()

    def as_dict(self) -> dict[str, Any]:
        return {
            "name": self.name,
            "description": self.description,
            "video": {
                "codecs": sorted(self.video.codecs),
                "profiles": {k: sorted(v) for k, v in sorted(self.video.profiles.items())},
                "bitDepths": sorted(self.video.bit_depths),
                "chroma": sorted(c.value for c in self.video.chroma),
                "maxWidth": self.video.max_width,
                "maxHeight": self.video.max_height,
                "maxFps": self.video.max_fps,
                "hdrFormats": sorted(h.value for h in self.video.hdr_formats),
                "dolbyVisionPipeline": self.video.dolby_vision_pipeline,
            },
            "audio": {
                "passthroughCodecs": sorted(self.audio.passthrough_codecs),
                "decodeCodecs": sorted(self.audio.decode_codecs),
                "maxPcmChannels": self.audio.max_pcm_channels,
                "maxPassthroughChannels": self.audio.max_passthrough_channels,
                "sampleRates": sorted(self.audio.supported_sample_rates),
                "transcodeTarget": self.audio.transcode_target,
                "transcodeMaxChannels": self.audio.transcode_max_channels,
            },
            "container": {
                "direct": sorted(self.container.direct),
                "remuxTarget": self.container.remux_target,
            },
            "browser": {
                "videoCodecs": sorted(self.browser.video_codecs),
                "audioCodecs": sorted(self.browser.audio_codecs),
                "maxBitDepth": self.browser.max_bit_depth,
                "hdrFormats": sorted(h.value for h in self.browser.hdr_formats),
                "containers": sorted(self.browser.containers),
                "allowSoftwareVideoTranscode": self.browser.allow_software_video_transcode,
            },
            "evidence": list(self.evidence),
        }


RK3588_ORANGEPI5_PRODUCTION = CapabilityProfile(
    name="rk3588_orangepi5_production",
    description=(
        "Orange Pi 5 Ultra / RK3588, Kodi -> RKMPP -> DRM PRIME -> VOP2 -> HDMI, "
        "as accepted by gates MP1a/MP1b and MA1."
    ),
    video=VideoCapabilities(
        codecs=frozenset({"hevc", "h264", "vp9", "av1", "mpeg2video", "vc1", "mpeg4"}),
        profiles={
            # RKMPP's HEVC decoder covers Main and Main 10. Range extensions
            # (4:2:2 / 4:4:4 / 12-bit) are a different profile and are not.
            "hevc": frozenset({"Main", "Main 10", "Main Still Picture"}),
            # The H.264 hardware path is 8-bit 4:2:0 only; High 10 is not.
            "h264": frozenset({"Baseline", "Constrained Baseline", "Main", "High"}),
        },
        bit_depths=frozenset({8, 10}),
        chroma=frozenset({ChromaSubsampling.YUV420}),
        max_width=3840,
        max_height=2160,
        max_fps=60.0,
        hdr_formats=frozenset(
            {
                HdrFormat.SDR,
                HdrFormat.HDR10,
                HdrFormat.HLG,
                # An HDR10+ stream is a valid HDR10 stream with extra dynamic
                # metadata the pipeline ignores. The base layer is what plays.
                HdrFormat.HDR10_PLUS,
            }
        ),
        dolby_vision_pipeline=False,
    ),
    audio=AudioCapabilities(
        passthrough_codecs=frozenset({"ac3", "eac3"}),
        decode_codecs=frozenset(
            {
                "aac",
                "mp3",
                "mp2",
                "opus",
                "vorbis",
                "flac",
                "alac",
                "pcm_s16le",
                "pcm_s24le",
                "pcm_s32le",
                "pcm_f32le",
            }
        ),
        max_pcm_channels=2,
        max_passthrough_channels=8,
        supported_sample_rates=frozenset({32000, 44100, 48000, 88200, 96000, 176400, 192000}),
        transcode_target="ac3",
        transcode_max_channels=6,
    ),
    container=ContainerCapabilities(
        direct=frozenset(
            {
                "matroska",
                "webm",
                "matroska,webm",
                "mov,mp4,m4a,3gp,3g2,mj2",
                "mpegts",
                "hls",
                "mpegts,hls",
                "avi",
                "flv",
                "asf",
                "ogg",
            }
        ),
        remux_target="matroska",
    ),
    browser=BrowserCapabilities(
        video_codecs=frozenset({"h264", "vp8", "vp9", "av1"}),
        audio_codecs=frozenset({"aac", "mp3", "opus", "vorbis", "flac"}),
        max_bit_depth=8,
        hdr_formats=frozenset({HdrFormat.SDR}),
        containers=frozenset({"mov,mp4,m4a,3gp,3g2,mj2", "matroska,webm", "webm"}),
        allow_software_video_transcode=False,
    ),
    evidence=(
        "results/orangepi5-ultra-vendor/real-hdr10-playback-mp1b-2026-09-09.md",
        "results/orangepi5-ultra-vendor/ma1-hdmi-passthrough-2026-09-11.md",
        "results/mediabox-platform/m2-regression-fix-cpu-input-2026-09-11.md",
    ),
)

PROFILES: dict[str, CapabilityProfile] = {
    RK3588_ORANGEPI5_PRODUCTION.name: RK3588_ORANGEPI5_PRODUCTION,
}

DEFAULT_PROFILE_NAME = RK3588_ORANGEPI5_PRODUCTION.name


def get_profile(name: str | None = None) -> CapabilityProfile:
    if name is None:
        return PROFILES[DEFAULT_PROFILE_NAME]
    try:
        return PROFILES[name]
    except KeyError as exc:
        raise KeyError(f"unknown capability profile: {name}") from exc
