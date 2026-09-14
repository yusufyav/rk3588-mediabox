"""The headless Stremio adapter: protocol parsing and resolution, offline."""

from __future__ import annotations

import time
import unittest
from typing import Any

from ..errors import InvalidRequest, NotFound, UpstreamError
from ..stremio.addons import (
    addons_supporting,
    base_url_of,
    encode_extra,
    parse_manifest,
    parse_stream,
)
from ..stremio.adapter import HeadlessStremio, video_id_for
from ..stremio.models import StreamKind
from ..stremio.server import StreamingServer


CINEMETA_MANIFEST = {
    "id": "com.linvo.cinemeta",
    "version": "3.0.12",
    "name": "Cinemeta",
    "description": "The official addon for movie and series catalogs",
    "resources": ["catalog", "meta", "addon_catalog"],
    "types": ["movie", "series"],
    "idPrefixes": ["tt"],
    "catalogs": [
        {"type": "movie", "id": "top", "name": "Popular", "extra": [{"name": "genre"}, {"name": "search"}, {"name": "skip"}]},
        {"type": "series", "id": "top", "name": "Popular", "extraSupported": ["search"]},
        {"type": "movie", "id": "year", "name": "New", "extra": [{"name": "genre", "isRequired": True}]},
    ],
}

SCOPED_MANIFEST = {
    "id": "org.example.scoped",
    "name": "Scoped",
    "types": ["movie"],
    "resources": [
        "catalog",
        {"name": "stream", "types": ["series"], "idPrefixes": ["kitsu:"]},
    ],
    "catalogs": [],
}


class ManifestTests(unittest.TestCase):
    def test_the_base_url_drops_the_manifest_suffix(self):
        self.assertEqual(
            base_url_of("https://v3-cinemeta.strem.io/manifest.json"),
            "https://v3-cinemeta.strem.io",
        )
        self.assertEqual(base_url_of("https://host/sub/"), "https://host/sub")

    def test_a_manifest_becomes_an_addon(self):
        addon = parse_manifest("https://v3-cinemeta.strem.io/manifest.json", CINEMETA_MANIFEST)
        self.assertEqual(addon.id, "com.linvo.cinemeta")
        self.assertEqual(addon.resources, ("catalog", "meta", "addon_catalog"))
        self.assertEqual(addon.types, ("movie", "series"))
        self.assertEqual(len(addon.catalogs), 3)
        self.assertTrue(addon.catalogs[0].supports_search)
        self.assertEqual(addon.catalogs[2].extra_required, ("genre",))

    def test_support_is_scoped_by_type_and_id_prefix(self):
        addon = parse_manifest("https://host/manifest.json", CINEMETA_MANIFEST)
        self.assertTrue(addon.supports("meta", "movie", "tt1160419"))
        self.assertFalse(addon.supports("meta", "movie", "kitsu:1"))
        self.assertFalse(addon.supports("stream", "movie", "tt1160419"))
        self.assertFalse(addon.supports("meta", "channel", "UC123"))

    def test_a_resource_may_override_the_manifest_scope(self):
        addon = parse_manifest("https://host/manifest.json", SCOPED_MANIFEST)
        self.assertTrue(addon.supports("stream", "series", "kitsu:42"))
        self.assertFalse(addon.supports("stream", "movie", "tt1"))
        self.assertTrue(addon.supports("catalog", "movie"))

    def test_extra_is_encoded_deterministically(self):
        self.assertEqual(encode_extra({"skip": 20, "search": "dune"}), "search=dune&skip=20")
        self.assertEqual(encode_extra({"search": "a b&c=d"}), "search=a%20b%26c%3Dd")
        self.assertEqual(encode_extra(None), "")
        self.assertEqual(encode_extra({"genre": ""}), "")

    def test_addons_supporting_filters_the_collection(self):
        cinemeta = parse_manifest("https://host/manifest.json", CINEMETA_MANIFEST)
        scoped = parse_manifest("https://other/manifest.json", SCOPED_MANIFEST)
        self.assertEqual(addons_supporting([cinemeta, scoped], "meta", "movie", "tt1"), [cinemeta])
        self.assertEqual(
            addons_supporting([cinemeta, scoped], "stream", "series", "kitsu:9"), [scoped]
        )


class StreamDescriptorTests(unittest.TestCase):
    def test_a_direct_url_stream_is_playable(self):
        stream = parse_stream({"url": "https://cdn.example/a.mkv", "name": "1080p"}, "addon")
        self.assertIs(stream.kind, StreamKind.URL)
        self.assertTrue(stream.is_playable)
        self.assertEqual(stream.identity, "url:https://cdn.example/a.mkv")

    def test_a_torrent_stream_carries_its_hash_and_file_index(self):
        stream = parse_stream(
            {"infoHash": "5D640678EAE57C72C0D096904FD7D7405ECE6653", "fileIdx": 1, "name": "1080p"},
            "addon",
        )
        self.assertIs(stream.kind, StreamKind.TORRENT)
        self.assertEqual(stream.info_hash, "5d640678eae57c72c0d096904fd7d7405ece6653")
        self.assertEqual(stream.file_idx, 1)

    def test_an_external_link_is_not_playable(self):
        stream = parse_stream({"externalUrl": "https://netflix.com/title/1", "name": "Netflix"}, "a")
        self.assertIs(stream.kind, StreamKind.EXTERNAL)
        self.assertFalse(stream.is_playable)

    def test_a_url_wins_over_an_external_link_on_the_same_descriptor(self):
        stream = parse_stream(
            {"url": "https://cdn.example/a.mkv", "externalUrl": "https://site/watch"}, "a"
        )
        self.assertIs(stream.kind, StreamKind.URL)

    def test_trackers_are_collected_from_both_places_they_appear(self):
        stream = parse_stream(
            {
                "infoHash": "a" * 40,
                "sources": ["tracker:udp://one:80", "dht:" + "a" * 40],
                "behaviorHints": {"announce": ["udp://two:80"]},
            },
            "a",
        )
        self.assertEqual(stream.announce, ("udp://two:80", "udp://one:80"))

    def test_an_empty_descriptor_is_unknown_rather_than_assumed(self):
        self.assertIs(parse_stream({}, "a").kind, StreamKind.UNKNOWN)

    def test_a_youtube_stream_is_recognised(self):
        self.assertIs(parse_stream({"ytId": "dQw4w9WgXcQ"}, "a").kind, StreamKind.YOUTUBE)


class StreamingServerTests(unittest.TestCase):
    def setUp(self):
        self.server = StreamingServer("http://127.0.0.1:11470")

    def test_a_direct_url_needs_no_streaming_server(self):
        stream = parse_stream({"url": "https://cdn.example/a.mkv"}, "a")
        resolved = self.server.resolve(stream)
        self.assertEqual(resolved.url, "https://cdn.example/a.mkv")
        self.assertFalse(resolved.needs_streaming_server)
        self.assertEqual(resolved.via, "addon")

    def test_a_torrent_resolves_to_an_http_url_on_the_local_server(self):
        stream = parse_stream({"infoHash": "b" * 40, "fileIdx": 2}, "a")
        url = self.server.torrent_url(stream.info_hash, stream.file_idx)
        self.assertEqual(url, "http://127.0.0.1:11470/" + "b" * 40 + "/2")

    def test_a_torrent_without_a_file_index_uses_the_first_file(self):
        self.assertTrue(self.server.torrent_url("c" * 40, None).endswith("/0"))

    def test_trackers_are_passed_to_the_engine_in_the_query(self):
        url = self.server.torrent_url("d" * 40, 0, ("udp://one:80", "udp://two:80"))
        self.assertIn("tr=udp%3A%2F%2Fone%3A80", url)

    def test_a_malformed_info_hash_is_refused(self):
        for value in ("", "zz", "g" * 40, "a" * 41):
            with self.subTest(value=value):
                with self.assertRaises(InvalidRequest):
                    self.server.torrent_url(value, 0)

    def test_an_out_of_range_file_index_is_refused(self):
        with self.assertRaises(InvalidRequest):
            self.server.torrent_url("e" * 40, -1)

    def test_an_external_only_stream_cannot_be_resolved(self):
        stream = parse_stream({"externalUrl": "https://site/watch"}, "a")
        with self.assertRaises(InvalidRequest):
            self.server.resolve(stream)

    def test_a_youtube_id_with_a_path_separator_is_refused(self):
        with self.assertRaises(InvalidRequest):
            self.server.youtube_url("../../etc/passwd")

    def test_peer_contact_is_distinguished_from_a_populated_swarm(self):
        from ..stremio.server import TorrentStats

        listed_only = TorrentStats("a" * 40, None, swarm_size=55, peers=0, connection_tries=0, downloaded=0, download_speed=0.0)
        self.assertFalse(listed_only.has_peer_contact)
        contacted = TorrentStats("a" * 40, None, swarm_size=55, peers=0, connection_tries=3, downloaded=0, download_speed=0.0)
        self.assertTrue(contacted.has_peer_contact)


class _FakeAddonClient:
    """Answers the addon protocol from a table instead of from the network."""

    def __init__(self, table: dict[str, Any], failing: set[str] | None = None) -> None:
        self.table = table
        self.failing = failing or set()
        self.calls: list[tuple[str, ...]] = []

    def catalog(self, addon, type_name, catalog_id, extra=None):
        self.calls.append(("catalog", addon.id, type_name, catalog_id, str(extra)))
        if addon.id in self.failing:
            raise UpstreamError(f"{addon.id} is down")
        return self.table.get(("catalog", addon.id, type_name, catalog_id), [])

    def meta(self, addon, type_name, item_id):
        self.calls.append(("meta", addon.id, type_name, item_id))
        if addon.id in self.failing:
            raise UpstreamError(f"{addon.id} is down")
        return self.table.get(("meta", addon.id, type_name, item_id))

    def streams(self, addon, type_name, item_id):
        self.calls.append(("stream", addon.id, type_name, item_id))
        if addon.id in self.failing:
            raise UpstreamError(f"{addon.id} is down")
        return self.table.get(("stream", addon.id, type_name, item_id), [])

    def subtitles(self, addon, type_name, item_id, extra=None):
        self.calls.append(("subtitles", addon.id, type_name, item_id))
        if addon.id in self.failing:
            raise UpstreamError(f"{addon.id} is down")
        return self.table.get(("subtitles", addon.id, type_name, item_id), [])


class AdapterTests(unittest.TestCase):
    """The adapter's own behaviour: merging, scoping and failure isolation."""

    def _adapter(self, manifests, table, failing=None) -> HeadlessStremio:
        adapter = HeadlessStremio()
        adapter._addons = tuple(
            parse_manifest(f"https://addon{index}/manifest.json", manifest)
            for index, manifest in enumerate(manifests)
        )
        adapter._addons_fetched_at = adapter._clock()
        adapter.addons_client = _FakeAddonClient(table, failing)
        return adapter

    def test_streams_are_merged_across_addons_and_deduplicated(self):
        a = {"id": "a", "name": "A", "resources": ["stream"], "types": ["movie"], "catalogs": []}
        b = {"id": "b", "name": "B", "resources": ["stream"], "types": ["movie"], "catalogs": []}
        shared = parse_stream({"url": "https://cdn/x.mkv"}, None)
        only_b = parse_stream({"infoHash": "f" * 40}, None)
        adapter = self._adapter(
            [a, b],
            {
                ("stream", "a", "movie", "tt1"): [shared],
                ("stream", "b", "movie", "tt1"): [shared, only_b],
            },
        )
        streams = adapter.streams("movie", "tt1")
        self.assertEqual(len(streams), 2)

    def test_one_failing_addon_does_not_lose_the_others(self):
        a = {"id": "a", "name": "A", "resources": ["stream"], "types": ["movie"], "catalogs": []}
        b = {"id": "b", "name": "B", "resources": ["stream"], "types": ["movie"], "catalogs": []}
        adapter = self._adapter(
            [a, b],
            {("stream", "b", "movie", "tt1"): [parse_stream({"url": "https://cdn/x.mkv"}, None)]},
            failing={"a"},
        )
        self.assertEqual(len(adapter.streams("movie", "tt1")), 1)

    def test_an_episode_is_asked_for_by_its_video_id(self):
        a = {"id": "a", "name": "A", "resources": ["stream"], "types": ["series"], "catalogs": []}
        adapter = self._adapter([a], {})
        adapter.streams("series", "tt1", video_id_for("tt1", 2, 5))
        self.assertIn(("stream", "a", "series", "tt1:2:5"), adapter.addons_client.calls)

    def test_video_ids_have_the_documented_shape(self):
        self.assertEqual(video_id_for("tt1160419", 1, 2), "tt1160419:1:2")

    def test_meta_falls_through_to_the_next_addon_when_one_fails(self):
        a = {"id": "a", "name": "A", "resources": ["meta"], "types": ["movie"], "catalogs": []}
        b = {"id": "b", "name": "B", "resources": ["meta"], "types": ["movie"], "catalogs": []}
        sentinel = object()
        adapter = self._adapter([a, b], {("meta", "b", "movie", "tt1"): sentinel}, failing={"a"})
        self.assertIs(adapter.meta("movie", "tt1"), sentinel)

    def test_meta_that_nobody_has_is_not_found(self):
        a = {"id": "a", "name": "A", "resources": ["meta"], "types": ["movie"], "catalogs": []}
        adapter = self._adapter([a], {})
        with self.assertRaises(NotFound):
            adapter.meta("movie", "tt404")

    def test_a_catalogue_no_addon_offers_is_not_found(self):
        a = {"id": "a", "name": "A", "resources": ["catalog"], "types": ["movie"], "catalogs": []}
        adapter = self._adapter([a], {})
        with self.assertRaises(NotFound):
            adapter.catalog("movie", "nonexistent")

    def test_search_only_asks_catalogues_that_support_it(self):
        manifest = {
            "id": "a",
            "name": "A",
            "resources": ["catalog"],
            "types": ["movie"],
            "catalogs": [
                {"type": "movie", "id": "searchable", "extra": [{"name": "search"}]},
                {"type": "movie", "id": "plain"},
            ],
        }
        adapter = self._adapter([manifest], {})
        adapter.search("dune")
        asked = [call[3] for call in adapter.addons_client.calls]
        self.assertEqual(asked, ["searchable"])

    def test_search_needs_a_query(self):
        adapter = self._adapter([], {})
        for value in ("", "   ", None):
            with self.subTest(value=value):
                with self.assertRaises(InvalidRequest):
                    adapter.search(value)

    def test_home_skips_catalogues_that_require_an_extra(self):
        manifest = {
            "id": "a",
            "name": "A",
            "resources": ["catalog"],
            "types": ["movie"],
            "catalogs": [
                {"type": "movie", "id": "top", "name": "Popular"},
                {"type": "movie", "id": "byGenre", "extra": [{"name": "genre", "isRequired": True}]},
            ],
        }
        adapter = self._adapter([manifest], {("catalog", "a", "movie", "top"): [object()]})
        rows = adapter.home()
        self.assertEqual([row.catalog_id for row in rows], ["top"])

    def test_home_keeps_the_order_the_shelves_are_declared_in(self):
        """The order is the manifest's, never the order the answers arrive in.

        The catalogues are fetched at the same time now, and a home screen whose
        shelves rearranged themselves depending on which host was quickest that
        morning would be worse than one that was slow.
        """
        manifests = [
            {
                "id": chr(ord("a") + i),
                "name": chr(ord("A") + i),
                "resources": ["catalog"],
                "types": ["movie"],
                "catalogs": [{"type": "movie", "id": f"c{i}", "name": f"Row {i}"}],
            }
            for i in range(4)
        ]
        table = {
            ("catalog", chr(ord("a") + i), "movie", f"c{i}"): [object()] for i in range(4)
        }
        adapter = self._adapter(manifests, table)

        # The last addon answers first and the first answers last.
        delays = {"a": 0.08, "b": 0.06, "c": 0.04, "d": 0.02}
        plain = adapter.addons_client.catalog

        def slow(addon, type_name, catalog_id, extra=None):
            time.sleep(delays[addon.id])
            return plain(addon, type_name, catalog_id, extra)

        adapter.addons_client.catalog = slow

        began = time.monotonic()
        rows = adapter.home()
        took = time.monotonic() - began

        self.assertEqual([row.catalog_id for row in rows], ["c0", "c1", "c2", "c3"])
        # And they really were asked at the same time: run one after another
        # this is 0.20s, and the slowest single one is 0.08s.
        self.assertLess(took, 0.18)

    def test_home_loses_only_the_shelf_of_an_addon_that_is_down(self):
        a = {
            "id": "a",
            "name": "A",
            "resources": ["catalog"],
            "types": ["movie"],
            "catalogs": [{"type": "movie", "id": "top", "name": "Popular"}],
        }
        b = {
            "id": "b",
            "name": "B",
            "resources": ["catalog"],
            "types": ["movie"],
            "catalogs": [{"type": "movie", "id": "new", "name": "New"}],
        }
        adapter = self._adapter(
            [a, b],
            {("catalog", "b", "movie", "new"): [object()]},
            failing={"a"},
        )
        rows = adapter.home()
        self.assertEqual([row.catalog_id for row in rows], ["new"])

    def test_a_catalogue_that_refuses_quickly_is_asked_again(self):
        """An error is not a reason to stop asking; hanging is.

        Penalising a fast refusal took a shelf off the home screen for five
        minutes because one catalogue answered 404 once. What costs the home
        screen is a host that does not answer at all, and that is caught at the
        deadline rather than here.
        """
        a = {
            "id": "a",
            "name": "A",
            "resources": ["catalog", "stream"],
            "types": ["movie"],
            "catalogs": [{"type": "movie", "id": "top", "name": "Popular"}],
        }
        adapter = self._adapter([a], {}, failing={"a"})
        adapter.home()
        self.assertFalse(adapter._recently_failed("a", adapter._home_failures))

    def test_a_hanging_catalogue_is_penalised_only_on_the_home_surface(self):
        """The two surfaces keep their own record of who is down.

        They are different endpoints and often different hosts. One table would
        mean a catalogue that hangs costing the same addon its sources, for a
        failure the stream path never saw.
        """
        a = {
            "id": "a",
            "name": "A",
            "resources": ["catalog", "stream"],
            "types": ["movie"],
            "catalogs": [{"type": "movie", "id": "top", "name": "Popular"}],
        }
        adapter = self._adapter([a], {})

        def hangs(addon, type_name, catalog_id, extra=None):
            time.sleep(0.3)
            return []

        adapter.addons_client.catalog = hangs

        from ..stremio import adapter as adapter_module

        previous = adapter_module.HOME_DEADLINE
        adapter_module.HOME_DEADLINE = 0.05
        try:
            rows = adapter.home()
        finally:
            adapter_module.HOME_DEADLINE = previous

        self.assertEqual(rows, [])
        self.assertTrue(adapter._recently_failed("a", adapter._home_failures))
        self.assertFalse(adapter._recently_failed("a"))

    def test_resolving_a_descriptor_refuses_an_unplayable_one(self):
        adapter = self._adapter([], {})
        with self.assertRaises(InvalidRequest):
            adapter.resolve_descriptor({"externalUrl": "https://site/watch"})
        with self.assertRaises(InvalidRequest):
            adapter.resolve_descriptor({})
        with self.assertRaises(InvalidRequest):
            adapter.resolve_descriptor("not an object")


if __name__ == "__main__":
    unittest.main()
