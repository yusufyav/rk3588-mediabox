"""The subtitle system through the media core's own routes.

A local HTTP server stands in for the subtitle addon's file host and for the
streaming server (its `/opensubHash`); the listener is a synthetic film, so a
sync's right answer is known. Nothing here touches the network or a player.
"""

from __future__ import annotations

import json
import tempfile
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from media.api import MediaCore, MediaCoreConfig
from media.stremio.addons import AddonClient, parse_stream
from media.stremio.models import Subtitle, SubtitleSource
from media.subtitles.service import SubtitleService, opensubtitles_hash, rank
from media.subtitles.sync import align

from .subtitle_synth import cut, film, invert, listen


def srt(cues) -> bytes:
    def stamp(t):
        ms = int(round(t * 1000))
        return f"{ms // 3600000:02d}:{ms // 60000 % 60:02d}:{ms // 1000 % 60:02d},{ms % 1000:03d}"

    blocks = [f"{i}\n{stamp(a)} --> {stamp(b)}\nsatır {i}\n" for i, (a, b) in enumerate(cues, 1)]
    return "\n".join(blocks).encode("utf-8")


class Files(BaseHTTPRequestHandler):
    files: dict[str, bytes] = {}
    hashes: list[str] = []

    def log_message(self, *args):  # noqa: D401
        pass

    def do_GET(self):  # noqa: N802
        if self.path.startswith("/opensubHash"):
            Files.hashes.append(self.path)
            body = json.dumps({"result": {"hash": "00ff00ff00ff00ff", "size": 123456789}}).encode()
        elif self.path in Files.files:
            body = Files.files[self.path]
        else:
            self.send_response(404)
            self.end_headers()
            return
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


class Harness(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = ThreadingHTTPServer(("127.0.0.1", 0), Files)
        cls.origin = f"http://127.0.0.1:{cls.server.server_address[1]}"
        threading.Thread(target=cls.server.serve_forever, daemon=True).start()

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()

    def setUp(self):
        self.cache = tempfile.TemporaryDirectory()
        self.core = MediaCore(
            MediaCoreConfig(
                # The stand-in server is "the streaming server", which is what
                # lets its loopback address through the source policy.
                streaming_server_url=self.origin,
                base_url="http://127.0.0.1:8790",
                subtitle_cache_dir=self.cache.name,
            )
        )
        self.addon_subtitles: list[Subtitle] = []
        self.addon_calls: list[dict] = []

        def subtitles(type_name, item_id, *, video_id=None, extra=None):
            self.addon_calls.append({"type": type_name, "id": item_id, "videoId": video_id, "extra": extra})
            return list(self.addon_subtitles)

        self.core.stremio.subtitles = subtitles
        self.film = film(1)
        self.heard_calls: list[tuple[float, float]] = []

        def listener(url, start, length, **_):
            self.heard_calls.append((start, start + length))
            return listen(self.film.heard, [(start, start + length)])[0], 0

        service: SubtitleService = self.core.subtitles
        service._listen = listener
        service._align = lambda cues, windows, duration: align(cues, windows, duration=duration)
        # No real media session behind these: the film is always "playing".
        service.alive = lambda _: True

    def tearDown(self):
        self.core.shutdown()
        self.cache.cleanup()

    def call(self, method, path, body=None):
        response = self.core.handle(method, path, "", json.dumps(body).encode() if body is not None else None)
        payload = json.loads(response.body) if response.headers[0][1].startswith("application/json") else response.body
        return response.status, payload

    def register(self, session, *, stream=None, source=None):
        self.core.subtitles.register(session, source or f"{self.origin}/{'a' * 40}/0", None, stream)

    def wait(self, job):
        deadline = time.monotonic() + 120
        while time.monotonic() < deadline:
            status, answer = self.call("GET", f"/media/subtitles/sync/{job}")
            self.assertEqual(status, 200)
            if answer["state"] not in ("running",):
                return answer
            time.sleep(0.05)
        self.fail("the sync did not finish")


class StreamBoundSubtitles(unittest.TestCase):
    DESCRIPTOR = {
        "name": "Torrentio",
        "infoHash": "A" * 40,
        "fileIdx": 2,
        "behaviorHints": {"filename": "Film.2012.1080p.BluRay.x264-GRP.mkv"},
        "subtitles": [
            {"id": "s1", "url": "https://example.org/tr.srt", "lang": "tur", "label": "BluRay GRP"},
            {"id": "s2", "url": "https://example.org/en.vtt", "lang": "eng"},
            {"lang": "eng"},  # no url: nothing to load
        ],
    }

    def test_they_survive_parsing(self):
        stream = parse_stream(self.DESCRIPTOR, "torrentio")
        self.assertEqual([s.id for s in stream.subtitles], ["s1", "s2"])
        self.assertEqual(stream.subtitles[0].language, "tur")
        self.assertEqual(stream.subtitles[0].label, "BluRay GRP")
        self.assertIs(stream.subtitles[0].source, SubtitleSource.STREAM)

    def test_they_survive_the_round_trip_through_a_client(self):
        stream = parse_stream(self.DESCRIPTOR, "torrentio")
        again = parse_stream(stream.as_dict(), "torrentio")
        self.assertEqual(again.subtitles, stream.subtitles)

    def test_a_stream_without_them_has_none(self):
        self.assertEqual(parse_stream({"url": "https://x/y.mkv"}, None).subtitles, ())


class AddonSubtitles(unittest.TestCase):
    def test_labels_and_hash_matches_are_read_not_guessed(self):
        from media.stremio.addons import parse_subtitles

        found = parse_subtitles(
            [
                {"id": "1", "url": "https://s/1", "lang": "tur", "m": "h"},
                {"id": "2", "url": "https://s/2", "language": "eng", "name": "WEB-DL"},
                {"id": "3", "url": "https://s/3", "lang": "eng", "m": "i"},
            ],
            "opensubtitles",
        )
        self.assertEqual([(s.hash_match, s.label) for s in found], [(True, None), (False, "WEB-DL"), (False, None)])
        self.assertTrue(all(s.source is SubtitleSource.ADDON for s in found))

    def test_the_request_carries_the_file(self):
        client = AddonClient()
        seen = []

        import media.stremio.addons as addons

        original = addons.get_json
        addons.get_json = lambda url, timeout: seen.append(url) or {"subtitles": []}
        try:
            from media.stremio.models import Addon

            addon = Addon("https://a/manifest.json", "https://a", "a", "a", resources=("subtitles",))
            client.subtitles(addon, "movie", "tt1", {"videoHash": "abc", "videoSize": 42, "filename": "F.mkv"})
        finally:
            addons.get_json = original
        self.assertEqual(seen, ["https://a/subtitles/movie/tt1/filename=F.mkv&videoHash=abc&videoSize=42.json"])


class Ranking(unittest.TestCase):
    def test_the_files_own_first_then_language_then_hash(self):
        subtitles = [
            Subtitle("a", "u1", "eng"),
            Subtitle("b", "u2", "tur"),
            Subtitle("c", "u3", "tur", hash_match=True),
            Subtitle("d", "u4", "eng", source=SubtitleSource.STREAM),
            Subtitle("e", "u5", "tur", label="Film 2012 BluRay GRP"),
        ]
        order = [s.id for s in rank(subtitles, ["tr", "en"], "Film.2012.1080p.BluRay.x264-GRP.mkv")]
        self.assertEqual(order, ["d", "c", "e", "b", "a"])

    def test_opensubtitles_hash(self):
        first = bytes(range(256)) * 256
        last = bytes(reversed(range(256))) * 256
        size = 10_000_000
        expected = size
        import struct

        for block in (first, last):
            for (value,) in struct.iter_unpack("<Q", block):
                expected = (expected + value) & 0xFFFFFFFFFFFFFFFF
        self.assertEqual(opensubtitles_hash(first, last, size), f"{expected:016x}")


class Prepare(Harness):
    def test_stream_and_addon_subtitles_in_one_ranked_list(self):
        descriptor = dict(StreamBoundSubtitles.DESCRIPTOR)
        descriptor["subtitles"] = [{"id": "s1", "url": f"{self.origin}/stream.srt", "lang": "eng"}]
        self.register("s" * 32, stream=descriptor)
        self.addon_subtitles = [
            Subtitle("o1", f"{self.origin}/en.srt", "eng", "opensubtitles"),
            Subtitle("o2", f"{self.origin}/tr.srt", "tur", "opensubtitles"),
            Subtitle("o3", f"{self.origin}/tr-hash.srt", "tur", "opensubtitles", hash_match=True),
        ]
        status, answer = self.call(
            "POST",
            "/media/subtitles/prepare",
            {"sessionId": "s" * 32, "type": "series", "id": "tt1", "videoId": "tt1:1:2", "languages": ["tur"]},
        )
        self.assertEqual(status, 200, answer)
        sources = [(c["source"], c["language"], c["hashMatch"]) for c in answer["candidates"]]
        self.assertEqual(
            sources,
            [
                ("stream_external", "en", False),
                ("addon_external", "tr", True),
                ("addon_external", "tr", False),
                ("addon_external", "en", False),
            ],
        )
        # The addon was asked about the episode, with what is known of the file.
        call = self.addon_calls[0]
        self.assertEqual(call["videoId"], "tt1:1:2")
        self.assertEqual(
            call["extra"],
            {"videoHash": "00ff00ff00ff00ff", "videoSize": 123456789, "filename": "Film.2012.1080p.BluRay.x264-GRP.mkv"},
        )
        self.assertEqual(answer["video"]["identity"], "osh:00ff00ff00ff00ff:123456789")

    def test_unknown_values_are_not_invented(self):
        self.register("t" * 32, source=f"{self.origin}/{'b' * 40}/0")
        Files.hashes.clear()
        self.core.stremio.server.opensub_hash = lambda url: None
        status, answer = self.call("POST", "/media/subtitles/prepare", {"sessionId": "t" * 32, "type": "movie", "id": "tt2"})
        self.assertEqual(status, 200)
        self.assertIsNone(self.addon_calls[0]["extra"])
        self.assertIsNone(answer["video"]["videoHash"])

    def test_an_unknown_session(self):
        status, _ = self.call("POST", "/media/subtitles/prepare", {"sessionId": "u" * 32})
        self.assertEqual(status, 404)


class LoadAndSync(Harness):
    SESSION = "d" * 32

    #: A file on the box: seekable, nothing to download, no budget.
    FILM = "file:///srv/films/Film.2012.1080p.BluRay.x264-GRP.mkv"

    def prepared(self, subtitle_bytes, *, language="tur", session=None, source=FILM):
        session = session or self.SESSION
        Files.files["/sub.srt"] = subtitle_bytes
        self.addon_subtitles = [Subtitle("x", f"{self.origin}/sub.srt", language, "opensubtitles")]
        self.register(session, source=source)
        _, answer = self.call("POST", "/media/subtitles/prepare", {"sessionId": session, "type": "movie", "id": "tt3"})
        status, loaded = self.call(
            "POST", "/media/subtitles/load", {"sessionId": session, "candidate": answer["candidates"][0]["id"]}
        )
        self.assertEqual(status, 200, loaded)
        return loaded

    def sync(self, key, session=None, position=None):
        status, job = self.call(
            "POST",
            "/media/subtitles/sync",
            {"sessionId": session or self.SESSION, "key": key, "duration": self.film.duration, "position": position},
        )
        self.assertEqual(status, 202, job)
        return self.wait(job["job"])

    def test_an_addon_subtitle_reaches_the_player_on_the_loopback(self):
        loaded = self.prepared(srt([(1, 2), (3, 4), (5, 6)] * 1).replace("satır".encode(), "ğüşİ".encode("utf-8")))
        self.assertTrue(loaded["url"].startswith("http://127.0.0.1:8790/media/subtitles/file/"))
        self.assertTrue(loaded["url"].endswith(".srt"))
        self.assertEqual(loaded["format"], "srt")
        status, body = self.call("GET", loaded["url"][len("http://127.0.0.1:8790"):])
        self.assertEqual(status, 200)
        self.assertIn("ğüşİ", body.decode("utf-8"))

    def test_a_legacy_code_page_is_given_to_the_player_as_utf8(self):
        loaded = self.prepared("1\n00:00:01,000 --> 00:00:02,000\nİyi günler\n".encode("cp1254"))
        _, body = self.call("GET", loaded["url"][len("http://127.0.0.1:8790"):])
        self.assertIn("İyi günler", body.decode("utf-8"))

    def test_something_that_is_not_a_subtitle_is_refused(self):
        Files.files["/sub.srt"] = b"<html>not found</html>"
        self.addon_subtitles = [Subtitle("x", f"{self.origin}/sub.srt", "tur")]
        self.register(self.SESSION)
        _, answer = self.call("POST", "/media/subtitles/prepare", {"sessionId": self.SESSION, "type": "movie", "id": "tt3"})
        status, error = self.call(
            "POST", "/media/subtitles/load", {"sessionId": self.SESSION, "candidate": answer["candidates"][0]["id"]}
        )
        self.assertEqual(status, 422)
        self.assertEqual(error["error"]["code"], "SUBTITLE_UNREADABLE")

    def test_an_offset_is_found_applied_as_properties_and_kept(self):
        sub = invert(self.film.reference, lambda v: v - 4.2)
        loaded = self.prepared(srt(sub))
        answer = self.sync(loaded["key"])
        self.assertEqual(answer["state"], "done")
        self.assertEqual(answer["cache"], "miss")
        self.assertEqual(answer["result"]["decision"], "apply")
        self.assertEqual(answer["apply"]["kind"], "properties")
        self.assertAlmostEqual(answer["apply"]["subDelay"], 4.2, delta=0.3)
        self.assertAlmostEqual(answer["apply"]["subSpeed"], 1.0, delta=1e-4)
        heard = len(self.heard_calls)
        self.assertGreater(heard, 0)

        # The same film and subtitle again, in a new session: nothing is
        # listened to, the answer is the kept one.
        loaded_again = self.prepared(srt(sub), session="e" * 32)
        self.assertEqual(loaded_again["key"], loaded["key"])
        again = self.sync(loaded_again["key"], session="e" * 32)
        self.assertEqual(again["cache"], "hit")
        self.assertEqual(again["apply"], answer["apply"])
        self.assertEqual(len(self.heard_calls), heard)

    def test_speech_heard_for_one_subtitle_serves_the_next(self):
        first = self.prepared(srt(invert(self.film.reference, lambda v: v - 4.2)))
        self.sync(first["key"])
        heard = len(self.heard_calls)
        second = self.prepared(srt(invert(self.film.reference, lambda v: v + 2.0)), session="f" * 32)
        answer = self.sync(second["key"], session="f" * 32)
        self.assertEqual(answer["cache"], "miss")
        self.assertAlmostEqual(answer["apply"]["subDelay"], -2.0, delta=0.3)
        # Whatever the first sync heard was not listened to again.
        repeated = set(self.heard_calls[:heard]) & set(self.heard_calls[heard:])
        self.assertEqual(repeated, set())

    def test_a_different_cut_gets_a_corrected_file_and_the_original_is_kept(self):
        sub = cut(self.film.reference, [(1837.0, 45.0)])
        loaded = self.prepared(srt(sub))
        _, original = self.call("GET", loaded["url"][len("http://127.0.0.1:8790"):])
        answer = self.sync(loaded["key"])
        self.assertEqual(answer["result"]["model"], "piecewise", answer)
        self.assertEqual(answer["apply"]["kind"], "file")
        self.assertNotEqual(answer["apply"]["key"], loaded["key"])
        status, corrected = self.call("GET", answer["apply"]["url"][len("http://127.0.0.1:8790"):])
        self.assertEqual(status, 200)
        self.assertNotEqual(corrected, original)
        _, still = self.call("GET", loaded["url"][len("http://127.0.0.1:8790"):])
        self.assertEqual(still, original)

    def test_a_subtitle_for_another_film_is_not_corrected(self):
        loaded = self.prepared(srt(film(1001).reference))
        answer = self.sync(loaded["key"])
        self.assertEqual(answer["state"], "done")
        self.assertNotEqual(answer["result"]["decision"], "apply")
        self.assertIsNone(answer["apply"])

    def test_a_torrent_is_only_listened_to_behind_the_film(self):
        sub = invert(self.film.reference, lambda v: v - 4.2)
        loaded = self.prepared(srt(sub), source=f"{self.origin}/{'c' * 40}/0")
        answer = self.sync(loaded["key"], position=400.0)
        self.assertTrue(self.heard_calls)
        self.assertTrue(all(end <= 400.0 for _, end in self.heard_calls), self.heard_calls)
        self.assertIn(answer["state"], ("waiting", "done"))

    def test_stopping_the_session_stops_the_sync(self):
        sub = invert(self.film.reference, lambda v: v - 4.2)
        loaded = self.prepared(srt(sub))
        gate = threading.Event()
        service = self.core.subtitles
        original = service._listen

        def slow(*args, **kwargs):
            gate.wait(5)
            return original(*args, **kwargs)

        service._listen = slow
        _, job = self.call(
            "POST", "/media/subtitles/sync", {"sessionId": self.SESSION, "key": loaded["key"], "duration": self.film.duration}
        )
        service.forget(self.SESSION)
        gate.set()
        answer = self.wait(job["job"])
        self.assertEqual(answer["state"], "cancelled")

    def test_the_engine_runs_in_its_own_process(self):
        from media.subtitles.runner import align_isolated

        sub = invert(self.film.reference, lambda v: v - 4.2)
        windows = listen(self.film.heard, [(300.0 + 300 * k, 330.0 + 300 * k) for k in range(16)])
        isolated = align_isolated(sub, windows, self.film.duration)
        local = align(sub, windows, duration=self.film.duration)
        self.assertEqual(isolated.decision, local.decision)
        self.assertAlmostEqual(isolated.offset, local.offset, places=2)


if __name__ == "__main__":
    unittest.main()
