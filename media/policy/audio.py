"""Audio decision and audio track selection.

The reliable downstream target on this appliance is AC-3, and the reason is
measured rather than assumed: the sink plays AC-3 and E-AC-3 bitstreams, is
silent on DTS and on six-channel LPCM even though its ELD declares both, and is
silent on Kodi's own AC-3 encoder output while playing a real AC-3 file's frames
perfectly (gate MA1). So anything the sink cannot take as a bitstream, and
anything that would have to arrive as multichannel PCM, is converted to AC-3
*upstream of Kodi* — as file frames, which is the form that works.

Track selection matters as much as the decision. A source that carries both a
TrueHD track and an AC-3 track needs no encoder at all, and picking the AC-3
track is strictly better than transcoding the TrueHD one.
"""

from __future__ import annotations

from dataclasses import dataclass
from enum import Enum
from typing import Any

from ..inspector.model import AudioTrack
from . import reasons as R
from .capabilities import CapabilityProfile


class AudioAction(str, Enum):
    #: Compressed bitstream handed to the sink untouched.
    PASSTHROUGH = "Passthrough"
    #: Decoded by the player to PCM within the sink's channel limit.
    DECODE_PCM = "DecodeToPCM"
    #: Re-encoded to AC-3 by the media core before the player sees it.
    TRANSCODE_AC3 = "TranscodeToAC3"
    NONE = "None"
    UNSUPPORTED = "Unsupported"


#: Lossless codecs. Converting one to AC-3 is a real loss and is reported.
LOSSLESS_CODECS = frozenset(
    {
        "truehd",
        "mlp",
        "flac",
        "alac",
        "pcm_s16le",
        "pcm_s24le",
        "pcm_s32le",
        "pcm_f32le",
        "pcm_bluray",
        "pcm_dvd",
        "dts",  # only when the profile says DTS-HD MA; refined below
    }
)

#: DTS extensions whose core is lossy but whose full stream is not.
_DTS_LOSSLESS_PROFILES = {"dts-hd ma", "dts-hd ma + dts:x", "dts-hd ma + dts:x imax"}

#: Codecs the media core knows how to reason about at all. Anything outside
#: this set gets an explicit decision rather than an optimistic one.
KNOWN_CODECS = frozenset(
    {
        "ac3",
        "eac3",
        "dts",
        "truehd",
        "mlp",
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
        "pcm_bluray",
        "pcm_dvd",
    }
)

#: AC-3 bitrate by channel count. 640 kbit/s is the maximum the format defines
#: and the maximum this sink's ELD declares, so a 5.1 mix uses all of it.
AC3_BITRATE_BY_CHANNELS = {1: 192_000, 2: 256_000, 3: 384_000, 4: 448_000, 5: 448_000, 6: 640_000}
AC3_SAMPLE_RATES = (32000, 44100, 48000)


@dataclass(frozen=True, slots=True)
class AudioDecision:
    action: AudioAction
    track: AudioTrack | None
    reasons: tuple[R.Reason, ...]
    #: Set for TRANSCODE_AC3: what the encoder is asked to produce.
    target_codec: str | None = None
    target_channels: int | None = None
    target_bitrate: int | None = None
    target_sample_rate: int | None = None

    @property
    def requires_encoder(self) -> bool:
        return self.action is AudioAction.TRANSCODE_AC3

    def as_dict(self) -> dict[str, Any]:
        return {
            "action": self.action.value,
            "track": self.track.as_dict() if self.track else None,
            "target": None
            if self.target_codec is None
            else {
                "codec": self.target_codec,
                "channels": self.target_channels,
                "bitRate": self.target_bitrate,
                "sampleRate": self.target_sample_rate,
            },
            "reasons": [reason.as_dict() for reason in self.reasons],
        }


def is_lossless(track: AudioTrack) -> bool:
    codec = (track.codec or "").lower()
    if codec == "dts":
        return (track.profile or "").lower() in _DTS_LOSSLESS_PROFILES
    return codec in LOSSLESS_CODECS


def ac3_target(track: AudioTrack, profile: CapabilityProfile) -> tuple[int, int, int]:
    """Channels, bitrate and sample rate for the AC-3 the encoder must produce."""
    source_channels = track.channels or 2
    channels = max(1, min(source_channels, profile.audio.transcode_max_channels))
    bitrate = AC3_BITRATE_BY_CHANNELS.get(channels, 640_000)
    rate = track.sample_rate or 48000
    sample_rate = rate if rate in AC3_SAMPLE_RATES else 48000
    return channels, bitrate, sample_rate


def _transcode_reasons(track: AudioTrack, profile: CapabilityProfile, why: R.Reason) -> list[R.Reason]:
    channels, bitrate, _ = ac3_target(track, profile)
    notes = [why]
    notes.append(
        R.info(
            R.AUDIO_TRANSCODE_AC3,
            f"{track.codec} is converted to AC-3 {channels}ch at {bitrate // 1000} kbit/s; "
            "the video is copied",
            fromCodec=track.codec,
            channels=channels,
            bitRate=bitrate,
        )
    )
    if track.object_audio:
        notes.append(
            R.warning(
                R.AUDIO_OBJECT_METADATA_LOST,
                "object audio metadata is discarded by the conversion; the bed is "
                "rendered to channels",
                source=track.object_audio_source,
            )
        )
    if is_lossless(track):
        notes.append(
            R.warning(
                R.AUDIO_LOSSLESS_TO_LOSSY,
                f"{track.codec} is lossless and AC-3 is not; this conversion loses "
                "information that cannot be recovered",
            )
        )
    if (track.channels or 0) > channels:
        notes.append(
            R.warning(
                R.AUDIO_CHANNELS_REDUCED,
                f"{track.channels} channels are downmixed to {channels}",
                fromChannels=track.channels,
                toChannels=channels,
            )
        )
    return notes


def decide_track(track: AudioTrack, profile: CapabilityProfile) -> AudioDecision:
    """Decide what happens to one audio track."""
    caps = profile.audio
    codec = (track.codec or "").lower()
    channels = track.channels or 0

    if not codec:
        return AudioDecision(
            AudioAction.UNSUPPORTED,
            track,
            (R.blocking(R.AUDIO_CODEC_UNKNOWN, "the audio track declares no codec"),),
        )

    if codec not in KNOWN_CODECS:
        # An unknown codec gets a decision, not an assumption. Converting is the
        # safe one: ffmpeg either decodes it — in which case AC-3 comes out — or
        # it does not, and the session fails loudly instead of playing silence.
        return AudioDecision(
            AudioAction.TRANSCODE_AC3,
            track,
            tuple(
                _transcode_reasons(
                    track,
                    profile,
                    R.warning(
                        R.AUDIO_CODEC_UNKNOWN,
                        f"'{track.codec}' is not a codec this profile classifies; it is "
                        "converted to AC-3 rather than assumed playable",
                        codec=track.codec,
                    ),
                )
            ),
            **_target_kwargs(track, profile),
        )

    if codec in caps.passthrough_codecs:
        if channels and channels > caps.max_passthrough_channels:
            return AudioDecision(
                AudioAction.TRANSCODE_AC3,
                track,
                tuple(
                    _transcode_reasons(
                        track,
                        profile,
                        R.warning(
                            R.AUDIO_CHANNELS_REDUCED,
                            f"{channels} channels exceed the {caps.max_passthrough_channels} "
                            "the passthrough carrier accepts",
                        ),
                    )
                ),
                **_target_kwargs(track, profile),
            )
        return AudioDecision(
            AudioAction.PASSTHROUGH,
            track,
            (
                R.info(
                    R.AUDIO_PASSTHROUGH,
                    f"{track.codec} {track.channel_layout or ''}".strip()
                    + " is passed through to the sink as a bitstream",
                    codec=track.codec,
                    channels=track.channels,
                ),
            ),
        )

    if codec in caps.decode_codecs:
        if channels and channels > caps.max_pcm_channels:
            return AudioDecision(
                AudioAction.TRANSCODE_AC3,
                track,
                tuple(
                    _transcode_reasons(
                        track,
                        profile,
                        R.warning(
                            R.AUDIO_CHANNELS_REDUCED,
                            f"{channels}-channel PCM is not reproduced by this sink "
                            f"(verified silent above {caps.max_pcm_channels} channels); "
                            "the mix is carried as AC-3 instead of being downmixed to stereo",
                            channels=channels,
                            maxPcmChannels=caps.max_pcm_channels,
                        ),
                    )
                ),
                **_target_kwargs(track, profile),
            )
        if track.sample_rate is not None and track.sample_rate not in caps.supported_sample_rates:
            return AudioDecision(
                AudioAction.TRANSCODE_AC3,
                track,
                tuple(
                    _transcode_reasons(
                        track,
                        profile,
                        R.warning(
                            R.AUDIO_SAMPLE_RATE_UNSUPPORTED,
                            f"{track.sample_rate} Hz is outside the sink's rates",
                            sampleRate=track.sample_rate,
                        ),
                    )
                ),
                **_target_kwargs(track, profile),
            )
        return AudioDecision(
            AudioAction.DECODE_PCM,
            track,
            (
                R.info(
                    R.AUDIO_DECODE_PCM,
                    f"{track.codec} is decoded to PCM by the player",
                    codec=track.codec,
                    channels=track.channels,
                ),
            ),
        )

    return AudioDecision(
        AudioAction.TRANSCODE_AC3,
        track,
        tuple(
            _transcode_reasons(
                track,
                profile,
                R.info(
                    R.AUDIO_TRANSCODE_AC3,
                    f"{track.codec} is neither passed through nor decoded to PCM by this profile",
                    codec=track.codec,
                ),
            )
        ),
        **_target_kwargs(track, profile),
    )


def _target_kwargs(track: AudioTrack, profile: CapabilityProfile) -> dict[str, Any]:
    channels, bitrate, rate = ac3_target(track, profile)
    return {
        "target_codec": profile.audio.transcode_target,
        "target_channels": channels,
        "target_bitrate": bitrate,
        "target_sample_rate": rate,
    }


#: Cheapest first. A source carrying both TrueHD and AC-3 should use the AC-3.
_ACTION_COST = {
    AudioAction.PASSTHROUGH: 0,
    AudioAction.DECODE_PCM: 1,
    AudioAction.TRANSCODE_AC3: 2,
    AudioAction.UNSUPPORTED: 3,
    AudioAction.NONE: 4,
}


def select_audio(
    tracks: tuple[AudioTrack, ...] | list[AudioTrack],
    profile: CapabilityProfile,
    *,
    preferred_language: str | None = None,
) -> AudioDecision:
    """Choose the track that needs the least work, then decide what to do with it.

    Language is the first filter when one is asked for, because the wrong
    language played perfectly is still the wrong track. Within a language, a
    track that needs no encoder beats one that does — that is what stops a
    TrueHD Atmos track being transcoded while a native AC-3 track sits unused
    in the same file.
    """
    if not tracks:
        return AudioDecision(
            AudioAction.NONE,
            None,
            (R.warning(R.AUDIO_TRACK_MISSING, "the source has no audio track"),),
        )

    candidates = list(tracks)
    if preferred_language:
        wanted = preferred_language.lower()
        matching = [t for t in candidates if (t.language or "").lower() == wanted]
        if matching:
            candidates = matching

    decisions = [(track, decide_track(track, profile)) for track in candidates]

    def rank(item: tuple[AudioTrack, AudioDecision]) -> tuple[int, int, int, int]:
        track, decision = item
        return (
            _ACTION_COST[decision.action],
            -(track.channels or 0),
            0 if track.is_default else 1,
            track.stream_index,
        )

    decisions.sort(key=rank)
    best_track, best = decisions[0]

    if len(decisions) > 1 and not best.requires_encoder:
        would_encode = [d for _, d in decisions[1:] if d.requires_encoder]
        if would_encode:
            reasons = list(best.reasons)
            reasons.insert(
                0,
                R.info(
                    R.AUDIO_NATIVE_TRACK_PREFERRED,
                    f"a native {best_track.codec} track is used instead of converting "
                    f"{would_encode[0].track.codec if would_encode[0].track else 'another track'}",
                    selectedIndex=best_track.stream_index,
                    selectedCodec=best_track.codec,
                    avoidedCodec=would_encode[0].track.codec if would_encode[0].track else None,
                ),
            )
            best = AudioDecision(
                best.action,
                best.track,
                tuple(reasons),
                best.target_codec,
                best.target_channels,
                best.target_bitrate,
                best.target_sample_rate,
            )
    return best
