"""Watching a title changes the account's record the way stremio-core changes it."""

from __future__ import annotations

import unittest
from datetime import datetime, timedelta, timezone

from ..stremio import library_state as ls
from ..stremio.adapter import HeadlessStremio
from ..stremio.models import Meta, Video
from ..stremio.watched import WatchedBitField, ordered_video_ids

NOW = datetime(2026, 9, 26, 12, 0, tzinfo=timezone.utc)
MINUTE = 60_000


def series_meta() -> Meta:
    return Meta(
        id="tt1",
        type="series",
        name="Show",
        videos=(
            Video(id="tt1:1:1", season=1, episode=1, released="2020-01-01T00:00:00.000Z"),
            Video(id="tt1:1:2", season=1, episode=2, released="2020-01-08T00:00:00.000Z"),
            Video(id="tt1:2:1", season=2, episode=1, released="2021-01-01T00:00:00.000Z"),
            Video(id="tt1:2:2", season=2, episode=2, released="2099-01-01T00:00:00.000Z"),
            Video(id="tt1:0:1", season=0, episode=1, released="2020-06-01T00:00:00.000Z"),
        ),
    )


def watched_ids(record, meta):
    return WatchedBitField.parse(
        record["state"].get("watched"), ordered_video_ids(meta.videos)
    ).watched_ids()


class ProgressTest(unittest.TestCase):
    def play(self, record, meta, video, minutes, *, step=1, duration=40):
        for minute in range(step, minutes + 1, step):
            ls.progress(record, meta, video, minute * MINUTE, duration * MINUTE, seek=False, now=NOW)

    def test_a_new_title_gets_a_temporary_removed_record(self):
        record = ls.new_record("tt9", "movie", "Film", None, NOW)
        self.assertTrue(record["removed"] and record["temp"])
        self.assertEqual(record["state"]["lastWatched"], "2026-09-26T12:00:00.000Z")

    def test_the_position_is_where_playback_got_to(self):
        record = ls.new_record("tt9", "movie", "Film", None, NOW)
        self.play(record, None, "tt9", 10)
        self.assertEqual(record["state"]["timeOffset"], 10 * MINUTE)
        self.assertEqual(record["state"]["duration"], 40 * MINUTE)
        self.assertEqual(record["state"]["timeWatched"], 10 * MINUTE)
        # Still temporary and removed: in "Devam Et", not in the library.
        self.assertTrue(record["removed"] and record["temp"])

    def test_watched_is_time_spent_watching_not_where_the_position_is(self):
        record = ls.new_record("tt9", "movie", "Film", None, NOW)
        # Skipping to the end is a seek; it moves the position and not the time watched.
        ls.progress(record, None, "tt9", 39 * MINUTE, 40 * MINUTE, seek=True, now=NOW)
        self.assertEqual(record["state"]["flaggedWatched"], 0)
        record2 = ls.new_record("tt9", "movie", "Film", None, NOW)
        self.play(record2, None, "tt9", 29)
        self.assertEqual(record2["state"]["timesWatched"], 1, "29 of 40 minutes is past 70%")
        self.assertEqual(record2["state"]["flaggedWatched"], 1)
        self.assertFalse(record2["removed"] and not record2["temp"])

    def test_an_episode_past_the_threshold_is_marked_in_the_bitfield(self):
        meta = series_meta()
        record = ls.new_record("tt1", "series", "Show", None, NOW)
        self.play(record, meta, "tt1:1:1", 30)
        self.assertEqual(watched_ids(record, meta), ["tt1:1:1"])
        self.assertEqual(record["state"]["video_id"], "tt1:1:1")

    def test_a_new_episode_restarts_the_per_episode_count(self):
        meta = series_meta()
        record = ls.new_record("tt1", "series", "Show", None, NOW)
        self.play(record, meta, "tt1:1:1", 30)
        ls.progress(record, meta, "tt1:1:2", 1 * MINUTE, 40 * MINUTE, seek=False, now=NOW)
        state = record["state"]
        self.assertEqual((state["video_id"], state["flaggedWatched"]), ("tt1:1:2", 0))
        self.assertEqual(state["timeWatched"], 1 * MINUTE, "counted from the new episode's start")

    def test_a_backwards_seek_moves_the_position_back(self):
        record = ls.new_record("tt9", "movie", "Film", None, NOW)
        self.play(record, None, "tt9", 20)
        ls.progress(record, None, "tt9", 5 * MINUTE, 40 * MINUTE, seek=True, now=NOW)
        self.assertEqual(record["state"]["timeOffset"], 5 * MINUTE)


class ClosingTest(unittest.TestCase):
    def test_closing_past_the_credits_moves_a_series_on(self):
        meta = series_meta()
        record = ls.new_record("tt1", "series", "Show", None, NOW)
        ls.progress(record, meta, "tt1:1:2", 37 * MINUTE, 40 * MINUTE, seek=True, now=NOW)
        ls.closed(record, meta, NOW)
        # The next episode, one millisecond in: still in "Devam Et".
        self.assertEqual(record["state"]["video_id"], "tt1:2:1")
        self.assertEqual(record["state"]["timeOffset"], 1)

    def test_closing_before_the_credits_keeps_the_position(self):
        record = ls.new_record("tt9", "movie", "Film", None, NOW)
        ls.progress(record, None, "tt9", 20 * MINUTE, 40 * MINUTE, seek=True, now=NOW)
        ls.closed(record, None, NOW)
        self.assertEqual(record["state"]["timeOffset"], 20 * MINUTE)

    def test_a_finished_film_leaves_devam_et(self):
        record = ls.new_record("tt9", "movie", "Film", None, NOW)
        ls.progress(record, None, "tt9", 39 * MINUTE, 40 * MINUTE, seek=True, now=NOW)
        ls.closed(record, None, NOW)
        self.assertEqual(record["state"]["timeOffset"], 0)

    def test_an_unreleased_episode_is_not_moved_onto(self):
        meta = series_meta()
        record = ls.new_record("tt1", "series", "Show", None, NOW)
        ls.progress(record, meta, "tt1:2:1", 39 * MINUTE, 40 * MINUTE, seek=True, now=NOW)
        ls.closed(record, meta, NOW)
        self.assertEqual(record["state"]["video_id"], "tt1:2:1")
        self.assertEqual(record["state"]["timeOffset"], 0)

    def test_the_specials_are_not_the_next_episode(self):
        meta = Meta(id="tt1", type="series", name="S", videos=(
            Video(id="a", season=1, episode=1), Video(id="sp", season=0, episode=1)))
        self.assertIsNone(ls.next_video(meta, "a", NOW))


class MarkingTest(unittest.TestCase):
    def test_marking_a_film_watched_clears_its_position(self):
        record = ls.new_record("tt9", "movie", "Film", None, NOW)
        record["state"]["timeOffset"] = 5 * MINUTE
        ls.mark_title_watched(record, True, NOW)
        self.assertEqual((record["state"]["timesWatched"], record["state"]["timeOffset"]), (1, 0))
        ls.mark_title_watched(record, False, NOW)
        self.assertEqual(record["state"]["timesWatched"], 0)

    def test_marking_the_devam_et_episode_moves_it_on(self):
        meta = series_meta()
        record = ls.new_record("tt1", "series", "Show", None, NOW)
        record["state"]["video_id"] = "tt1:1:1"
        record["state"]["timeOffset"] = 5 * MINUTE
        ls.mark_videos_watched(record, meta, [meta.videos[0]], True, NOW)
        self.assertEqual(watched_ids(record, meta), ["tt1:1:1"])
        self.assertEqual((record["state"]["video_id"], record["state"]["timeOffset"]), ("tt1:1:2", 1))

    def test_marking_a_season_skips_to_the_first_unwatched_released_episode(self):
        meta = series_meta()
        record = ls.new_record("tt1", "series", "Show", None, NOW)
        record["state"]["video_id"] = "tt1:1:1"
        ls.mark_videos_watched(record, meta, [v for v in meta.videos if v.season == 1], True, NOW, season=1)
        self.assertEqual(set(watched_ids(record, meta)), {"tt1:1:1", "tt1:1:2"})
        self.assertEqual(record["state"]["video_id"], "tt1:2:1")

    def test_unmarking_leaves_the_pointer_alone(self):
        meta = series_meta()
        record = ls.new_record("tt1", "series", "Show", None, NOW)
        ls.mark_videos_watched(record, meta, [meta.videos[0]], True, NOW)
        record["state"]["video_id"] = "tt1:1:1"
        ls.mark_videos_watched(record, meta, [meta.videos[0]], False, NOW)
        self.assertEqual(watched_ids(record, meta), [])
        self.assertEqual(record["state"]["video_id"], "tt1:1:1")

    def test_library_and_rewind(self):
        record = ls.new_record("tt9", "movie", "Film", None, NOW)
        ls.set_in_library(record, True)
        self.assertEqual((record["removed"], record["temp"]), (False, False))
        record["state"]["timeOffset"] = 5 * MINUTE
        ls.rewind(record)
        self.assertEqual(record["state"]["timeOffset"], 0)
        ls.set_in_library(record, False)
        self.assertEqual((record["removed"], record["temp"]), (True, False))


class _FakeAccount:
    def __init__(self, records):
        self.records = {r["_id"]: r for r in records}
        self.written = []

    def library_item(self, item_id, collection="libraryItem"):
        found = self.records.get(item_id)
        return None if found is None else dict(found, state=dict(found.get("state", {})))

    def datastore_put(self, changes, collection="libraryItem"):
        self.written.extend(changes)


class AdapterWriteTest(unittest.TestCase):
    def test_a_position_is_written_back_with_the_accounts_other_fields_intact(self):
        adapter = HeadlessStremio()
        adapter.api = _FakeAccount([
            {"_id": "tt9", "type": "movie", "name": "Film", "removed": False, "temp": False,
             "someField": {"kept": True}, "state": {"timeOffset": 0, "duration": 0}},
        ])
        state = adapter.record_progress("movie", "tt9", video_id=None, time_ms=MINUTE, duration_ms=40 * MINUTE)
        written = adapter.api.written[-1]
        self.assertEqual(written["someField"], {"kept": True})
        self.assertEqual(written["state"]["timeOffset"], MINUTE)
        self.assertEqual(written["state"]["video_id"], "tt9")
        self.assertTrue(state["inLibrary"])
        self.assertEqual(state["timeOffset"], MINUTE)

    def test_a_title_the_account_has_never_seen_is_created_temporary(self):
        adapter = HeadlessStremio()
        adapter.api = _FakeAccount([])
        adapter.record_progress("movie", "tt9", video_id=None, time_ms=MINUTE,
                                duration_ms=40 * MINUTE, name="Film", poster="https://p")
        written = adapter.api.written[-1]
        self.assertEqual((written["_id"], written["name"], written["poster"]), ("tt9", "Film", "https://p"))
        self.assertTrue(written["removed"] and written["temp"])

    def test_rewinding_a_title_with_no_record_writes_nothing(self):
        adapter = HeadlessStremio()
        adapter.api = _FakeAccount([])
        with self.assertRaises(Exception):
            adapter.rewind("movie", "tt9")
        self.assertEqual(adapter.api.written, [])


if __name__ == "__main__":
    unittest.main()
