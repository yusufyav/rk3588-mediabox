"""The one playback decision, combining video, audio and container."""

from __future__ import annotations

from dataclasses import dataclass, replace
from enum import Enum
from typing import Any

from ..inspector.model import MediaInfo
from . import reasons as R
from .audio import AudioAction, AudioDecision, select_audio
from .capabilities import CapabilityProfile, get_profile
from .video import VideoDecision, VideoVerdict, decide_video


class PlaybackMode(str, Enum):
    #: Player opens the source URL; the media core is not in the path.
    DIRECT = "Direct"
    #: Video copied, audio re-encoded to AC-3 by the media core.
    DIRECT_WITH_AUDIO_TRANSCODE = "DirectWithAudioTranscode"
    #: Streams copied into a container the player can open.
    REMUX = "Remux"
    #: Playable in principle, but another rendition of the same title should
    #: be used instead. Never returned when nothing about the source is risky.
    FALLBACK_SOURCE_PREFERRED = "FallbackSourcePreferred"
    UNSUPPORTED = "Unsupported"


@dataclass(frozen=True, slots=True)
class PlaybackDecision:
    mode: PlaybackMode
    profile_name: str
    video: VideoDecision
    audio: AudioDecision
    reasons: tuple[R.Reason, ...]
    source_url: str
    #: True whenever the media core must own a process for this playback.
    needs_session: bool = False

    @property
    def video_is_copied(self) -> bool:
        """The invariant: video samples are never re-encoded by the media core."""
        return self.mode is not PlaybackMode.UNSUPPORTED

    def as_dict(self) -> dict[str, Any]:
        return {
            "source": self.source_url,
            "mode": self.mode.value,
            "capabilityProfile": self.profile_name,
            "needsSession": self.needs_session,
            "videoCopied": self.video_is_copied,
            "video": self.video.as_dict(),
            "audio": self.audio.as_dict(),
            "reasons": [reason.as_dict() for reason in self.reasons],
        }

    def summary(self) -> str:
        parts = [f"{self.mode.value} [{self.profile_name}]"]
        parts.append(f"video={self.video.verdict.value}/{self.video.hdr.value}")
        parts.append(f"audio={self.audio.action.value}")
        return "  ".join(parts)


def _container_supported(info: MediaInfo, profile: CapabilityProfile) -> bool:
    name = (info.container.format_name or "").lower()
    if not name:
        return False
    if name in profile.container.direct:
        return True
    # ffprobe reports comma-joined demuxer families; any member matching is enough.
    return any(part in profile.container.direct for part in name.split(","))


def mode_when_chosen(decision: PlaybackDecision) -> PlaybackMode:
    """How a source is played once the viewer has chosen it.

    ``FALLBACK_SOURCE_PREFERRED`` is advice for ranking -- another rendition
    of the same title is likely to look better -- not a refusal. Stremio plays
    whatever source the viewer picks, a Dolby Vision Profile 5 file included,
    green and magenta as it then is, and so does this appliance: the choice
    is played the way it would have been had nothing about it been risky.
    """
    if decision.mode is not PlaybackMode.FALLBACK_SOURCE_PREFERRED:
        return decision.mode
    if decision.audio.requires_encoder:
        return PlaybackMode.DIRECT_WITH_AUDIO_TRANSCODE
    if any(note.code == R.CONTAINER_REMUX_REQUIRED for note in decision.reasons):
        return PlaybackMode.REMUX
    return PlaybackMode.DIRECT


def for_a_player_that_does_its_own_audio(decision: PlaybackDecision) -> PlaybackDecision:
    """The decision for the appliance's own player, which handles sound itself.

    The embedded player passes through what the receiver declares, encodes
    multichannel sound to Dolby Digital itself (mpv's ``lavcac3enc``) when the
    viewer has turned that on, and decodes the rest -- by the appliance's
    sound setting, which this core does not know. An AC-3 encode here as well
    would decide for it, and against a receiver that takes DTS as it is. So
    the sound is copied: the transform becomes a remux when the container
    still needs one, and nothing at all when it does not.
    """
    mode = mode_when_chosen(decision)
    if mode is not PlaybackMode.DIRECT_WITH_AUDIO_TRANSCODE:
        return decision
    remux = any(note.code == R.CONTAINER_REMUX_REQUIRED for note in decision.reasons)
    return replace(
        decision,
        mode=PlaybackMode.REMUX if remux else PlaybackMode.DIRECT,
        needs_session=remux,
        reasons=decision.reasons
        + (
            R.info(
                R.AUDIO_HANDLED_BY_PLAYER,
                "the player handles the sound by the appliance's sound setting; it is copied",
            ),
        ),
    )


def decide(
    info: MediaInfo,
    profile: CapabilityProfile | None = None,
    *,
    preferred_language: str | None = None,
) -> PlaybackDecision:
    profile = profile or get_profile()
    video = decide_video(info.primary_video, profile, source_warnings=info.warnings)
    audio = select_audio(info.audio, profile, preferred_language=preferred_language)

    notes: list[R.Reason] = []
    container_ok = _container_supported(info, profile)
    if container_ok:
        notes.append(
            R.info(
                R.CONTAINER_SUPPORTED,
                f"{info.container.format_name} is opened directly",
                container=info.container.format_name,
            )
        )
    else:
        notes.append(
            R.warning(
                R.CONTAINER_REMUX_REQUIRED,
                f"{info.container.format_name or 'the source container'} is not opened "
                f"directly; the streams are remuxed into {profile.container.remux_target}",
                container=info.container.format_name,
            )
        )

    if video.verdict is VideoVerdict.UNSUPPORTED or audio.action is AudioAction.UNSUPPORTED:
        return PlaybackDecision(
            PlaybackMode.UNSUPPORTED,
            profile.name,
            video,
            audio,
            tuple(notes),
            info.source_url,
        )

    if video.verdict is VideoVerdict.RISKY or video.prefer_alternative:
        notes.append(
            R.risk(
                R.SAFER_ALTERNATIVE_EXPECTED,
                "this rendition should be played only when no safe alternative of the "
                "same title exists",
            )
        )
        return PlaybackDecision(
            PlaybackMode.FALLBACK_SOURCE_PREFERRED,
            profile.name,
            video,
            audio,
            tuple(notes),
            info.source_url,
        )

    if audio.requires_encoder:
        mode = PlaybackMode.DIRECT_WITH_AUDIO_TRANSCODE
        needs_session = True
    elif not container_ok:
        mode = PlaybackMode.REMUX
        needs_session = True
    else:
        mode = PlaybackMode.DIRECT
        needs_session = False

    return PlaybackDecision(mode, profile.name, video, audio, tuple(notes), info.source_url, needs_session)
