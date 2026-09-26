"""The search box's suggestions: stremio-core's LocalSearch, answered locally."""

from __future__ import annotations

import os
import tempfile
import threading
import unittest

from ..errors import UpstreamError
from ..stremio.local_search import Index, LocalSearch, parse_feed, tokenize


def feed(*entries):
    return [
        {"id": f"tt{index}", "type": kind, "name": name, "imdbRating": rating, "popularity": popularity}
        for index, (name, kind, rating, popularity) in enumerate(entries)
    ]


class TokenizerTest(unittest.TestCase):
    def test_it_is_the_crates_default_tokenizer(self):
        self.assertEqual(tokenize("Spider-Man: Far from Home"), ["spiderman", "far", "from", "home"])


class IndexTest(unittest.TestCase):
    def setUp(self):
        self.index = Index(
            parse_feed(
                feed(
                    ("Dune", "movie", "6.3", 100),
                    ("Dune: Part Two", "movie", "8.5", 900),
                    ("Dunkirk", "movie", "7.8", 500),
                    ("Breaking Bad", "series", "9.5", 1000),
                    ("The Office", "series", "9.0", 800),
                )
            )
        )

    def names(self, query):
        return [found.name for found in self.index.search(query)]

    def test_a_prefix_suggests_every_title_it_begins(self):
        self.assertEqual(set(self.names("dun")), {"Dune", "Dune: Part Two", "Dunkirk"})

    def test_one_typo_still_finds_the_title(self):
        self.assertEqual(self.names("braking")[0], "Breaking Bad")

    def test_an_exact_word_outranks_a_longer_one_it_begins(self):
        self.assertEqual(self.names("dune")[:2], ["Dune: Part Two", "Dune"])
        self.assertNotIn("Dunkirk", self.names("dune"))

    def test_at_most_five_come_back(self):
        index = Index(parse_feed(feed(*[(f"Star {n}", "movie", "7", n) for n in range(20)])))
        self.assertEqual(len(index.search("star")), 5)

    def test_nothing_matches_nothing(self):
        self.assertEqual(self.names("zzzz"), [])

    def test_entries_without_a_name_are_not_indexed(self):
        records = parse_feed([{"id": "tt1", "type": "movie", "name": ""}, {"id": "tt2", "type": "movie"}])
        self.assertEqual(records, [])


class LocalSearchTest(unittest.TestCase):
    def test_no_suggestion_waits_for_the_network(self):
        release = threading.Event()
        fetched = threading.Event()

        def fetch():
            release.wait(2.0)
            fetched.set()
            return feed(("Dune", "movie", "6", 1))

        search = LocalSearch(None, fetch=fetch)
        # The feed has not arrived: no suggestions, and no wait for it.
        self.assertEqual(search.suggest("dune"), [])
        release.set()
        fetched.wait(2.0)
        for _ in range(100):
            if search.suggest("dune"):
                break
            threading.Event().wait(0.01)
        self.assertEqual([found.name for found in search.suggest("dune")], ["Dune"])

    def test_the_feed_is_kept_and_read_back_on_the_next_start(self):
        with tempfile.TemporaryDirectory() as directory:
            path = os.path.join(directory, "feed.json")
            first = LocalSearch(path, fetch=lambda: feed(("Dune", "movie", "6", 1)))
            first.suggest("")
            for _ in range(200):
                if os.path.exists(path):
                    break
                threading.Event().wait(0.01)

            def unreachable():
                raise UpstreamError("offline")

            second = LocalSearch(path, fetch=unreachable)
            self.assertEqual([found.name for found in second.suggest("dun")], ["Dune"])


if __name__ == "__main__":
    unittest.main()
