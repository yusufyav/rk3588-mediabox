"""The account's "Devam Et": the list every official Stremio client shows.

The rule is stremio-core's `LibraryItem::is_in_continue_watching` and its
continue-watching preview; these records are shaped like the ones the account
API returns, and the cases are the ones that made this television disagree with
the phone.
"""

from __future__ import annotations

import unittest

from ..stremio.adapter import HeadlessStremio


def record(item_id: str, *, mtime: str, offset: int = 0, duration: int = 0, **overrides) -> dict:
    base = {
        "_id": item_id,
        "name": item_id,
        "type": "movie",
        "removed": False,
        "temp": False,
        "_mtime": mtime,
        "state": {"timeOffset": offset, "duration": duration, "lastWatched": mtime},
    }
    base.update(overrides)
    return base


class FakeAPI:
    def __init__(self, records: list[dict]) -> None:
        self.records = records

    def library(self) -> list[dict]:
        return self.records


def listing(records: list[dict]) -> tuple[list[str], list[str]]:
    stremio = HeadlessStremio()
    stremio.api = FakeAPI(records)
    library, watching = stremio.library_listing()
    return [item["id"] for item in library], [item["id"] for item in watching]


class ContinueWatchingTests(unittest.TestCase):
    def test_a_film_played_without_being_added_is_still_being_watched(self):
        # What a client writes when something is played from a catalogue: a
        # temporary record, marked removed. Not in the library; in "Devam Et".
        library, watching = listing(
            [record("tt1", mtime="2026-09-20T13:20:00Z", offset=5, duration=10, removed=True, temp=True)]
        )
        self.assertEqual(library, [])
        self.assertEqual(watching, ["tt1"])

    def test_a_title_removed_from_the_library_is_gone_from_both(self):
        library, watching = listing(
            [record("tt1", mtime="2026-09-20T13:20:00Z", offset=5, duration=10, removed=True)]
        )
        self.assertEqual((library, watching), ([], []))

    def test_a_position_is_enough_and_the_duration_is_not_asked_for(self):
        # A series episode often carries a position and no duration, and so
        # does a film a client stopped before it learned how long it was.
        _, watching = listing([record("tt1", mtime="2026-09-23T17:46:00Z", offset=1000, duration=0)])
        self.assertEqual(watching, ["tt1"])

    def test_nearly_finished_is_still_unfinished(self):
        _, watching = listing([record("tt1", mtime="2026-09-20T13:20:00Z", offset=98, duration=100)])
        self.assertEqual(watching, ["tt1"])

    def test_nothing_watched_is_not_continued(self):
        library, watching = listing([record("tt1", mtime="2026-09-20T13:20:00Z")])
        self.assertEqual((library, watching), (["tt1"], []))

    def test_live_channels_and_other_are_not_continued(self):
        _, watching = listing(
            [
                record("tv1", mtime="2026-09-20T13:20:00Z", offset=5, type="tv"),
                record("ch1", mtime="2026-09-20T13:20:00Z", offset=5, behaviorHints={"isLive": True}),
                record("o1", mtime="2026-09-20T13:20:00Z", offset=5, type="other"),
            ]
        )
        self.assertEqual(watching, [])

    def test_newest_modification_first(self):
        # By when the record last changed, as the official clients order it,
        # not by when it was last watched.
        _, watching = listing(
            [
                record("old", mtime="2026-08-28T19:09:00Z", offset=5),
                record("new", mtime="2026-09-23T20:37:00Z", offset=5,
                       state={"timeOffset": 5, "duration": 0, "lastWatched": "2026-01-01T00:00:00Z"}),
            ]
        )
        self.assertEqual(watching, ["new", "old"])

    def test_cut_to_the_official_preview_size(self):
        records = [record(f"tt{n:03}", mtime=f"2026-09-01T00:{n // 60:02}:{n % 60:02}Z", offset=5) for n in range(130)]
        _, watching = listing(records)
        self.assertEqual(len(watching), 100)
        self.assertEqual(watching[0], "tt129")


if __name__ == "__main__":
    unittest.main()
