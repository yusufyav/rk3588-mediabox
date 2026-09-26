"""What watching does to the account's record of a title.

These are stremio-core's rules, transcribed rather than reinvented, so that a
title watched on this television reads the same on the phone, the desktop and
the web -- all of which run stremio-core:

- `Player` (models/player.rs), on every time update: a new episode restarts
  the per-episode count; otherwise the time since the last update is added to
  it. The position only moves forward on an update -- a seek moves it either
  way. Once the time actually spent watching passes 70% of the length
  (`WATCHED_THRESHOLD_COEF`), the episode or film is marked watched, once.
  A temporary record nobody has finished anything of stays removed.
- `Player`, when playback is closed: past 90% (`CREDITS_THRESHOLD_COEF`) the
  position is cleared, and a series moves on to its next episode with a
  position of 1 ms, which is what keeps it in "Devam Et".
- `MetaDetails`: marking a film watched, an episode, or a whole season.
- `Ctx`: adding to the library, removing, and rewinding a title out of
  "Devam Et".

A record is the account's own JSON object and is changed in place: fields this
module does not know about are the account's, and are written back untouched.
Times are milliseconds, as the account stores them.
"""

from __future__ import annotations

from datetime import datetime, timezone
from typing import Any, Iterable

from .models import Meta, Video
from .watched import WatchedBitField, ordered_video_ids

WATCHED_THRESHOLD_COEF = 0.7
CREDITS_THRESHOLD_COEF = 0.9


def iso(moment: datetime) -> str:
    """The way every client writes a time into a record: UTC, milliseconds, Z."""
    return moment.astimezone(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.") + f"{moment.microsecond // 1000:03d}Z"


def _int(value: Any) -> int:
    try:
        number = int(value)
    except (TypeError, ValueError):
        return 0
    return max(0, number)


def new_record(item_id: str, type_name: str, name: str, poster: str | None, now: datetime) -> dict[str, Any]:
    """stremio-core's `LibraryItem::from(meta preview)`: temporary, and removed."""
    return {
        "_id": item_id,
        "name": name,
        "type": type_name,
        "poster": poster,
        "posterShape": "poster",
        "removed": True,
        "temp": True,
        "_ctime": iso(now),
        "_mtime": iso(now),
        "state": {
            "lastWatched": iso(now),
            "timeWatched": 0,
            "timeOffset": 0,
            "overallTimeWatched": 0,
            "timesWatched": 0,
            "flaggedWatched": 0,
            "duration": 0,
            "video_id": None,
            "watched": None,
            "noNotif": False,
        },
        "behaviorHints": {"defaultVideoId": None, "featuredVideoId": None, "hasScheduledVideos": False},
    }


def state_of(record: dict[str, Any]) -> dict[str, Any]:
    state = record.get("state")
    if not isinstance(state, dict):
        state = {}
        record["state"] = state
    return state


def touched(record: dict[str, Any], now: datetime) -> dict[str, Any]:
    record["_mtime"] = iso(now)
    return record


def _released(video: Video) -> datetime | None:
    if not video.released:
        return None
    try:
        return datetime.fromisoformat(video.released.replace("Z", "+00:00"))
    except ValueError:
        return None


def next_video(meta: Meta | None, video_id: str, now: datetime) -> Video | None:
    """`MetaItem::next_video`: the next in the list, released, and not a special
    unless the current one is."""
    if meta is None:
        return None
    videos = list(meta.videos)
    for position, current in enumerate(videos):
        if current.id != video_id:
            continue
        if position + 1 >= len(videos):
            return None
        following = videos[position + 1]
        released = _released(following)
        out = (following.season or 0) != 0 or (current.season or 0) == (following.season or 0)
        if out and (released is None or released <= now):
            return following
        return None
    return None


def _bitfield(record: dict[str, Any], meta: Meta | None) -> WatchedBitField | None:
    if meta is None or not meta.videos:
        return None
    return WatchedBitField.parse(state_of(record).get("watched"), ordered_video_ids(meta.videos))


def _advance_to(state: dict[str, Any], video_id: str) -> None:
    """`LibraryItem::advance_to_video`."""
    state["video_id"] = video_id
    state["overallTimeWatched"] = _int(state.get("overallTimeWatched")) + _int(state.get("timeWatched"))
    state["timeWatched"] = 0
    state["flaggedWatched"] = 0
    state["timeOffset"] = 1


def _later_of(last_watched: Any, released: datetime | None) -> Any:
    if released is None:
        return last_watched
    if not last_watched:
        return iso(released)
    try:
        current = datetime.fromisoformat(str(last_watched).replace("Z", "+00:00"))
    except ValueError:
        return last_watched
    return iso(released) if current < released else last_watched


# ------------------------------------------------------------------- playing


def progress(
    record: dict[str, Any],
    meta: Meta | None,
    video_id: str,
    time_ms: int,
    duration_ms: int,
    *,
    seek: bool,
    now: datetime,
) -> None:
    """One time update from the player. `seek` is a jump the viewer made."""
    state = state_of(record)
    state["lastWatched"] = iso(now)
    if state.get("video_id") != video_id:
        # What `Load` does when the player opens a different video than the
        # record points at: the per-episode count starts again from nothing.
        state["video_id"] = video_id
        state["overallTimeWatched"] = _int(state.get("overallTimeWatched")) + _int(state.get("timeWatched"))
        state["timeWatched"] = 0
        state["flaggedWatched"] = 0
        state["timeOffset"] = 0
    if seek:
        # models/player.rs, Seek: the position is where the viewer put it.
        state["timeOffset"] = time_ms
        state["duration"] = duration_ms
    else:
        elapsed = max(0, time_ms - _int(state.get("timeOffset")))
        state["timeWatched"] = _int(state.get("timeWatched")) + elapsed
        state["overallTimeWatched"] = _int(state.get("overallTimeWatched")) + elapsed
        if time_ms > _int(state.get("timeOffset")):
            state["timeOffset"] = time_ms
            state["duration"] = duration_ms

    duration = _int(state.get("duration"))
    if (
        _int(state.get("flaggedWatched")) == 0
        and duration > 0
        and _int(state.get("timeWatched")) > duration * WATCHED_THRESHOLD_COEF
    ):
        state["flaggedWatched"] = 1
        state["timesWatched"] = _int(state.get("timesWatched")) + 1
        watched = _bitfield(record, meta)
        if watched is not None:
            watched.set_video(video_id, True)
            state["watched"] = watched.serialize()

    if record.get("temp") and _int(state.get("timesWatched")) == 0:
        record["removed"] = True
    if record.get("removed"):
        record["temp"] = True


def closed(record: dict[str, Any], meta: Meta | None, now: datetime) -> None:
    """Playback closed: past the credits, the position is cleared and a series
    moves on to its next episode."""
    state = state_of(record)
    duration = _int(state.get("duration"))
    if duration <= 0 or _int(state.get("timeOffset")) <= duration * CREDITS_THRESHOLD_COEF:
        return
    state["timeOffset"] = 0
    current = state.get("video_id")
    following = next_video(meta, current, now) if isinstance(current, str) else None
    if following is not None:
        _advance_to(state, following.id)


# ------------------------------------------------------------------- marking


def mark_title_watched(record: dict[str, Any], watched: bool, now: datetime) -> None:
    """`LibraryItem::mark_as_watched`: a film, or a title as a whole."""
    state = state_of(record)
    if watched:
        state["timesWatched"] = _int(state.get("timesWatched")) + 1
        state["lastWatched"] = iso(now)
        state["timeOffset"] = 0
    else:
        state["timesWatched"] = 0


def mark_videos_watched(
    record: dict[str, Any],
    meta: Meta,
    videos: Iterable[Video],
    watched: bool,
    now: datetime,
    *,
    season: int | None = None,
) -> None:
    """One episode, or a season: `MarkVideoAsWatched` and `MarkSeasonAsWatched`."""
    videos = list(videos)
    field = _bitfield(record, meta)
    if field is None or not videos:
        return
    for video in videos:
        field.set_video(video.id, watched)
    state = state_of(record)
    state["watched"] = field.serialize()
    if watched:
        state["lastWatched"] = _later_of(state.get("lastWatched"), _released(videos[-1]))

    if not watched:
        return
    current = state.get("video_id")
    if season is None:
        # A single episode: if it is the one "Devam Et" points at, move on.
        if current == videos[0].id:
            following = next_video(meta, current, now)
            if following is not None:
                _advance_to(state, following.id)
            else:
                state["timeOffset"] = 0
        return

    # `reconcile_series_resume_after_watched_change`.
    if not isinstance(current, str) or not field.get_video(current):
        return
    if not any(v.id == current and v.season == season for v in meta.videos):
        return
    while True:
        following = next_video(meta, current, now)
        if following is None:
            state["timeOffset"] = 0
            return
        if field.get_video(following.id):
            current = following.id
            continue
        _advance_to(state, following.id)
        return


# ------------------------------------------------------------------- library


def set_in_library(record: dict[str, Any], in_library: bool) -> None:
    """`AddToLibrary` / `RemoveFromLibrary`."""
    record["removed"] = not in_library
    record["temp"] = False


def rewind(record: dict[str, Any]) -> None:
    """`RewindLibraryItem`: out of "Devam Et", with nothing else forgotten."""
    state_of(record)["timeOffset"] = 0
