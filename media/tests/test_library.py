"""The appliance's own library: what it accepts, and what it refuses to show."""

from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

from ..library import LIBRARY_ADDON_ID, Library


def manifest(*items) -> dict:
    return {"items": list(items)}


def entry(**overrides) -> dict:
    base = {
        "id": "ntld-1968",
        "type": "movie",
        "name": "Night of the Living Dead",
        "releaseInfo": "1968",
        "genres": ["Horror"],
        "sources": [{"name": "720p", "url": "https://example.org/film.mp4"}],
    }
    base.update(overrides)
    return base


class LibraryTests(unittest.TestCase):
    def setUp(self) -> None:
        self._dir = tempfile.TemporaryDirectory()
        self.addCleanup(self._dir.cleanup)
        self.path = Path(self._dir.name) / "library.json"

    def write(self, document) -> Library:
        self.path.write_text(json.dumps(document), encoding="utf-8")
        return Library(str(self.path))

    def test_an_unconfigured_library_is_empty_rather_than_an_error(self) -> None:
        library = Library(None)
        self.assertFalse(library.configured)
        self.assertEqual(library.items(), ())
        self.assertEqual(library.as_row()["items"], [])

    def test_a_missing_manifest_is_empty_rather_than_an_error(self) -> None:
        library = Library(str(self.path))
        self.assertTrue(library.configured)
        self.assertEqual(library.items(), ())

    def test_an_entry_becomes_a_preview_and_a_stream(self) -> None:
        library = self.write(manifest(entry()))
        (item,) = library.items()
        preview = item.as_preview()
        self.assertEqual(preview["id"], "ntld-1968")
        self.assertEqual(preview["name"], "Night of the Living Dead")
        self.assertEqual(preview["addonId"], LIBRARY_ADDON_ID)
        stream = item.sources[0].as_stream()
        self.assertTrue(stream["playable"])
        self.assertEqual(stream["url"], "https://example.org/film.mp4")
        self.assertEqual(stream["identity"], "library:ntld-1968:0")

    def test_an_entry_with_no_playable_source_is_dropped(self) -> None:
        library = self.write(manifest(entry(sources=[])))
        self.assertEqual(library.items(), ())

    def test_a_source_scheme_the_appliance_cannot_play_is_refused(self) -> None:
        library = self.write(
            manifest(entry(sources=[{"name": "x", "url": "magnet:?xt=urn:btih:abc"}]))
        )
        self.assertEqual(library.items(), ())

    def test_an_entry_without_an_id_or_a_name_is_dropped(self) -> None:
        library = self.write(manifest(entry(id=""), entry(name=""), entry()))
        self.assertEqual([item.id for item in library.items()], ["ntld-1968"])

    def test_a_duplicate_id_keeps_the_first_declaration(self) -> None:
        library = self.write(
            manifest(
                entry(name="First"),
                entry(name="Second"),
            )
        )
        self.assertEqual([item.name for item in library.items()], ["First"])

    def test_a_manifest_that_is_not_json_is_empty_rather_than_fatal(self) -> None:
        self.path.write_text("{not json", encoding="utf-8")
        self.assertEqual(Library(str(self.path)).items(), ())

    def test_a_changed_manifest_is_picked_up_without_a_restart(self) -> None:
        library = self.write(manifest(entry()))
        self.assertEqual(len(library.items()), 1)
        self.path.write_text(
            json.dumps(manifest(entry(), entry(id="other", name="Other"))), encoding="utf-8"
        )
        # The stamp is (mtime_ns, size) and the second document is longer, so
        # the change is visible even if the clock granularity hides the mtime.
        self.assertEqual(len(library.items()), 2)

    def test_the_row_shape_matches_a_catalogue_row(self) -> None:
        row = self.write(manifest(entry())).as_row()
        self.assertEqual(
            sorted(row), ["addonId", "addonName", "catalogId", "items", "name", "type"]
        )


class LibraryRoutesTests(unittest.TestCase):
    def setUp(self) -> None:
        self._dir = tempfile.TemporaryDirectory()
        self.addCleanup(self._dir.cleanup)
        self.path = Path(self._dir.name) / "library.json"
        self.path.write_text(json.dumps(manifest(entry())), encoding="utf-8")

    def core(self):
        from ..api import MediaCore, MediaCoreConfig

        return MediaCore(MediaCoreConfig(library_path=str(self.path)))

    def test_library_listing_is_served(self) -> None:
        response = self.core().handle("GET", "/media/library")
        self.assertEqual(response.status, 200)
        payload = json.loads(response.body.decode("utf-8"))
        self.assertTrue(payload["configured"])
        self.assertEqual(payload["items"][0]["id"], "ntld-1968")

    def test_one_library_item_carries_its_sources(self) -> None:
        response = self.core().handle("GET", "/media/library/ntld-1968")
        self.assertEqual(response.status, 200)
        payload = json.loads(response.body.decode("utf-8"))
        self.assertEqual(payload["meta"]["name"], "Night of the Living Dead")
        self.assertEqual(payload["playable"], 1)
        self.assertEqual(payload["streams"][0]["kind"], "http")

    def test_an_unknown_library_item_is_a_404(self) -> None:
        response = self.core().handle("GET", "/media/library/nope")
        self.assertEqual(response.status, 404)

    def test_status_reports_the_library(self) -> None:
        response = self.core().handle("GET", "/media/status")
        payload = json.loads(response.body.decode("utf-8"))
        self.assertEqual(payload["library"], {"configured": True, "items": 1})


if __name__ == "__main__":
    unittest.main()
