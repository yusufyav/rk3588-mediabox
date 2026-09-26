"""The account's record of watched episodes, read and written as stremio-core does."""

from __future__ import annotations

import unittest

from ..stremio.adapter import _watch_state
from ..stremio.models import Meta, Video
from ..stremio.watched import WatchedBitField, ordered_video_ids

NINE = [f"tt2934286:1:{n}" for n in range(1, 10)]


class WatchedBitFieldTest(unittest.TestCase):
    # The vectors are the stremio-watched-bitfield crate's own tests.
    def test_the_crates_own_example_reads_and_writes_back_unchanged(self):
        serialized = "tt2934286:1:5:5:eJyTZwAAAEAAIA=="
        field = WatchedBitField.parse(serialized, NINE)
        self.assertTrue(field.get_video("tt2934286:1:5"))
        self.assertFalse(field.get_video("tt2934286:1:6"))
        self.assertEqual(field.watched_ids(), NINE[:5])
        self.assertEqual(field.serialize(), serialized)
        field.set_video("tt2934286:1:6", True)
        self.assertTrue(field.get_video("tt2934286:1:6"))

    def test_an_empty_field_is_the_crates_empty_field(self):
        self.assertEqual(WatchedBitField.empty([]).serialize(), "undefined:1:eJwDAAAAAAE=")

    def test_half_set_survives_a_round_trip(self):
        ids = [f"tt1:1:{n}" for n in range(1, 500)]
        field = WatchedBitField.empty(ids)
        for index in range(len(ids)):
            field.set(index, index % 2 == 0)
        again = WatchedBitField.parse(field.serialize(), ids)
        self.assertEqual([again.get(i) for i in range(len(ids))], [i % 2 == 0 for i in range(len(ids))])

    def test_an_episode_added_in_front_shifts_the_bits_with_it(self):
        field = WatchedBitField.parse("tt2934286:1:5:5:eJyTZwAAAEAAIA==", NINE)
        # A special appears before the first episode: every index moves by one.
        grown = ["tt2934286:0:1", *NINE]
        realigned = WatchedBitField.parse(field.serialize(), grown)
        self.assertEqual(realigned.watched_ids(), NINE[:5])

    def test_an_unreadable_field_is_nothing_watched(self):
        for broken in (None, "", "tt1", "tt1:x:abc", "tt2934286:1:5:5:not-base64!"):
            with self.subTest(broken=broken):
                self.assertEqual(WatchedBitField.parse(broken, NINE).watched_ids(), [])

    def test_the_anchor_missing_from_the_list_is_nothing_watched(self):
        self.assertEqual(WatchedBitField.parse("tt9:1:1:5:eJyTZwAAAEAAIA==", NINE).watched_ids(), [])


class OrderTest(unittest.TestCase):
    def test_bits_follow_season_then_episode_then_release(self):
        videos = [
            Video(id="s2e1", season=2, episode=1),
            Video(id="s0e1", season=0, episode=1),
            Video(id="s1e2", season=1, episode=2),
            Video(id="s1e1", season=1, episode=1),
            Video(id="loose"),
        ]
        self.assertEqual(ordered_video_ids(videos), ["loose", "s0e1", "s1e1", "s1e2", "s2e1"])


class WatchStateTest(unittest.TestCase):
    def test_a_series_record_names_its_watched_episodes_and_where_it_stopped(self):
        meta = Meta(id="tt2934286", type="series", name="S",
                    videos=tuple(Video(id=v, season=1, episode=n) for n, v in enumerate(NINE, 1)))
        record = {
            "_id": "tt2934286",
            "removed": False,
            "temp": False,
            "state": {
                "video_id": "tt2934286:1:6",
                "timeOffset": 600000,
                "duration": 2400000,
                "watched": "tt2934286:1:5:5:eJyTZwAAAEAAIA==",
            },
        }
        state = _watch_state(record, meta)
        self.assertTrue(state["inLibrary"])
        self.assertEqual(state["videoId"], "tt2934286:1:6")
        self.assertEqual(state["timeOffset"], 600000)
        self.assertEqual(state["watched"], NINE[:5])

    def test_no_record_is_nothing_watched_and_not_in_the_library(self):
        state = _watch_state(None, None)
        self.assertFalse(state["known"])
        self.assertFalse(state["inLibrary"])
        self.assertEqual(state["watched"], [])

    def test_a_temporary_record_is_not_in_the_library(self):
        state = _watch_state({"_id": "tt1", "removed": True, "temp": True, "state": {}}, None)
        self.assertTrue(state["known"])
        self.assertFalse(state["inLibrary"])


if __name__ == "__main__":
    unittest.main()
