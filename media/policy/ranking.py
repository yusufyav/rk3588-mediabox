"""Deterministic ranking of the renditions of one title.

Resolution is not the first criterion and never was. A 4K Dolby Vision
Profile 5 file on a device with no DV pipeline is a green-and-magenta picture;
a 1080p HDR10 file is a picture. The tiers below encode that: safety first,
then how much work playback costs, and only then how good the rendition is.

The order is total and stable. Two runs over the same inputs produce the same
list, including when every key is equal, because the source's own identity is
the final tiebreaker.
"""

from __future__ import annotations

from dataclasses import dataclass
from enum import IntEnum
from typing import Any, Iterable

from ..inspector.model import HdrFormat, MediaInfo
from .audio import AudioAction
from .capabilities import CapabilityProfile, get_profile
from .decide import PlaybackDecision, PlaybackMode, decide


class RankTier(IntEnum):
    """Lower is better. These are the brief's tiers, in order."""

    #: Safe video, and audio that needs no encoder.
    SAFE_DIRECT = 1
    #: Safe video, audio converted to AC-3.
    SAFE_AUDIO_TRANSCODE = 2
    #: Safe, but SDR where an HDR rendition of the same title exists.
    SAFE_SDR_ALTERNATIVE = 3
    #: Safe, but the container has to be remuxed.
    SAFE_REMUX = 4
    #: Playable only as a risk — Dolby Vision with no usable base layer, or
    #: colour metadata that cannot be trusted.
    RISKY = 5
    UNSUPPORTED = 6


#: How much an HDR format is worth when everything else is equal. HDR10+ and
#: HDR10 tie because this pipeline plays the same base layer either way.
_HDR_VALUE = {
    HdrFormat.HDR10_PLUS: 3,
    HdrFormat.HDR10: 3,
    HdrFormat.HLG: 2,
    HdrFormat.DOLBY_VISION: 2,
    HdrFormat.SDR: 1,
    HdrFormat.UNKNOWN: 0,
}


@dataclass(frozen=True, slots=True)
class RankedSource:
    """One rendition, its decision, and where it placed."""

    identity: str
    info: MediaInfo
    decision: PlaybackDecision
    tier: RankTier
    #: The full sort key, exposed so a report can show why one won.
    key: tuple[int, int, int, int, int, str]

    def as_dict(self) -> dict[str, Any]:
        return {
            "identity": self.identity,
            "tier": int(self.tier),
            "tierName": self.tier.name,
            "decision": self.decision.as_dict(),
            "sortKey": list(self.key),
        }


def _tier(decision: PlaybackDecision, *, hdr_alternative_exists: bool) -> RankTier:
    if decision.mode is PlaybackMode.UNSUPPORTED:
        return RankTier.UNSUPPORTED
    if decision.mode is PlaybackMode.FALLBACK_SOURCE_PREFERRED:
        return RankTier.RISKY
    if decision.mode is PlaybackMode.REMUX:
        return RankTier.SAFE_REMUX
    is_sdr = decision.video.hdr is HdrFormat.SDR
    if is_sdr and hdr_alternative_exists:
        return RankTier.SAFE_SDR_ALTERNATIVE
    if decision.audio.action is AudioAction.TRANSCODE_AC3:
        return RankTier.SAFE_AUDIO_TRANSCODE
    return RankTier.SAFE_DIRECT


def _sort_key(identity: str, info: MediaInfo, decision: PlaybackDecision, tier: RankTier):
    video = info.primary_video
    pixels = (video.width or 0) * (video.height or 0) if video else 0
    hdr_value = _HDR_VALUE.get(decision.video.hdr, 0)
    channels = decision.audio.track.channels if decision.audio.track else 0
    bitrate = info.container.bit_rate or 0
    # Negated so that "more is better" still sorts ascending, and the identity
    # makes the order total even when two renditions are indistinguishable.
    return (int(tier), -pixels, -hdr_value, -(channels or 0), -bitrate, identity)


def rank_sources(
    sources: Iterable[tuple[str, MediaInfo]],
    profile: CapabilityProfile | None = None,
    *,
    preferred_language: str | None = None,
) -> list[RankedSource]:
    """Rank renditions of one title, best first."""
    profile = profile or get_profile()
    prepared = [
        (identity, info, decide(info, profile, preferred_language=preferred_language))
        for identity, info in sources
    ]

    # "Is there an HDR rendition to lose to?" is a property of the set, so it is
    # computed once here rather than guessed per source.
    hdr_alternative_exists = any(
        decision.mode
        in (PlaybackMode.DIRECT, PlaybackMode.DIRECT_WITH_AUDIO_TRANSCODE, PlaybackMode.REMUX)
        and decision.video.hdr in (HdrFormat.HDR10, HdrFormat.HDR10_PLUS, HdrFormat.HLG)
        for _, _, decision in prepared
    )

    ranked: list[RankedSource] = []
    for identity, info, decision in prepared:
        tier = _tier(decision, hdr_alternative_exists=hdr_alternative_exists)
        ranked.append(
            RankedSource(identity, info, decision, tier, _sort_key(identity, info, decision, tier))
        )
    ranked.sort(key=lambda item: item.key)
    return ranked
