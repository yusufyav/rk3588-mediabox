"""The boundary between mediaboxd and the V2 media core.

mediaboxd owns the origin; the media core owns `/media/`. These tests are
about that seam and nothing else — what each side is handed, and what neither
is allowed to swallow from the other.
"""

from __future__ import annotations

import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from mediaboxd.api import APIContext, health_response  # noqa: E402
from mediaboxd.config import MediaCoreConfig, load_config  # noqa: E402
from mediaboxd.stremio import RESERVED_EXACT, RESERVED_PREFIXES, StremioBridge  # noqa: E402
from mediaboxd.__main__ import _media_core  # noqa: E402


class _NoSystemActions:
    enabled = False


class MountBoundaryTests(unittest.TestCase):
    def setUp(self):
        self.config = load_config(None)
        self.bridge = StremioBridge(self.config.stremio, kodi=None, events=None)
        self.core = _media_core(self.config)
        self.assertIsNotNone(self.core, "the media core should be importable from the repo")
        self.addCleanup(self.core.shutdown)

    def test_the_streaming_server_proxy_does_not_swallow_media_paths(self):
        for path in ("/media", "/media/status", "/media/session/" + "a" * 32):
            with self.subTest(path=path):
                self.assertFalse(self.bridge.owns(path))
                self.assertTrue(self.core.owns(path))

    def test_media_is_reserved_alongside_api_and_ui(self):
        self.assertIn("/media/", RESERVED_PREFIXES)
        self.assertIn("/media", RESERVED_EXACT)

    def test_the_proxy_still_owns_everything_else_at_the_root_mount(self):
        for path in ("/hlsv2/abc/master.m3u8", "/settings", "/casting"):
            with self.subTest(path=path):
                self.assertTrue(self.bridge.owns(path))
                self.assertFalse(self.core.owns(path))

    def test_a_path_that_merely_starts_with_media_is_not_the_media_core(self):
        self.assertFalse(self.core.owns("/mediafoo"))
        self.assertTrue(self.bridge.owns("/mediafoo"))

    def test_the_media_core_is_pointed_at_the_configured_streaming_server(self):
        self.assertEqual(self.core.config.streaming_server_url, self.config.stremio.upstream)
        self.assertIn(
            self.config.stremio.upstream, self.core.source_policy.allowed_local_origins
        )

    def test_kodi_is_given_a_loopback_session_url(self):
        self.assertEqual(
            self.core.config.loopback_base_url, f"http://127.0.0.1:{self.config.port}"
        )

    def test_the_media_core_can_be_turned_off(self):
        import dataclasses

        disabled = dataclasses.replace(
            self.config, media=MediaCoreConfig(enabled=False)
        )
        self.assertIsNone(_media_core(disabled))

    def test_health_reports_the_capability_profile_in_force(self):
        context = APIContext(
            kodi=None,
            lifecycle=None,
            telemetry=None,
            events=None,
            system_actions=_NoSystemActions(),
            stremio=self.bridge,
            media=self.core,
        )
        payload = health_response(context)
        self.assertEqual(payload["media"]["mediaCore"]["mount"], "/media")
        self.assertEqual(
            payload["media"]["mediaCore"]["capabilityProfile"], "rk3588_orangepi5_production"
        )

    def test_health_still_works_without_a_media_core(self):
        context = APIContext(
            kodi=None,
            lifecycle=None,
            telemetry=None,
            events=None,
            system_actions=_NoSystemActions(),
            stremio=self.bridge,
            media=None,
        )
        self.assertNotIn("mediaCore", health_response(context)["media"])

    def test_the_media_core_answers_its_own_capability_route(self):
        response = self.core.handle("GET", "/media/capabilities")
        self.assertEqual(response.status, 200)
        payload = json.loads(response.body)
        self.assertEqual(payload["name"], "rk3588_orangepi5_production")

    def test_local_files_are_off_by_default(self):
        self.assertEqual(self.config.media.allowed_file_prefixes, ())
        self.assertEqual(self.core.source_policy.allowed_file_prefixes, ())


class MediaConfigTests(unittest.TestCase):
    def _load(self, text: str):
        import tempfile

        with tempfile.NamedTemporaryFile("w", suffix=".toml", delete=False) as handle:
            handle.write(text)
            path = handle.name
        self.addCleanup(lambda: Path(path).unlink(missing_ok=True))
        return load_config(path)

    def test_defaults_enable_the_media_core(self):
        self.assertTrue(load_config(None).media.enabled)

    def test_the_capability_profile_can_be_named(self):
        config = self._load('[media]\ncapability_profile = "rk3588_orangepi5_production"\n')
        self.assertEqual(config.media.capability_profile, "rk3588_orangepi5_production")

    def test_an_empty_state_path_turns_persistence_off(self):
        self.assertIsNone(self._load('[media]\nstate_path = ""\n').media.state_path)

    def test_a_relative_file_prefix_is_refused(self):
        with self.assertRaises(ValueError):
            self._load('[media]\nallowed_file_prefixes = ["var/tmp"]\n')

    def test_a_relative_state_path_is_refused(self):
        with self.assertRaises(ValueError):
            self._load('[media]\nstate_path = "media.json"\n')

    def test_media_must_be_a_table(self):
        with self.assertRaises(ValueError):
            self._load('media = 1\n')

    def test_enabled_must_be_a_boolean(self):
        with self.assertRaises(ValueError):
            self._load('[media]\nenabled = "yes"\n')


if __name__ == "__main__":
    unittest.main()
