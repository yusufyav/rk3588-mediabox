"""OpenSubtitles.com as the worker asks it: which requests, in which order,
and what is never sent, spent or said. A stand-in API answers; nothing here
touches the network."""

from __future__ import annotations

import base64
import json
import os
import tempfile
import unittest
from urllib.parse import parse_qsl, urlsplit

from media.http import HttpResponse
from media.subtitles import eligibility
from media.subtitles.opensubtitles import (
    Credentials,
    OpenSubtitlesCom,
    QuotaExhausted,
    parse_result,
    target_for,
)

KEY = "app-key-not-a-real-one"
PASSWORD = "hunter2-not-a-real-one"


def token(expires: float) -> str:
    claims = base64.urlsafe_b64encode(json.dumps({"exp": expires}).encode()).decode().rstrip("=")
    return f"eyJhbGciOiJIUzI1NiJ9.{claims}.signature"


def result(file_id: int, language: str, *, result_id: str = "9000001", hash_match: bool | None = None, **attributes):
    body = {
        "subtitle_id": result_id,
        "language": language,
        "release": attributes.pop("release", "Film.2012.1080p.BluRay.x264-GRP"),
        "fps": attributes.pop("fps", 23.976),
        "nb_cd": attributes.pop("nb_cd", 1),
        "files": attributes.pop("files", [{"file_id": file_id, "cd_number": 1, "file_name": "film.srt"}]),
        **attributes,
    }
    if hash_match is not None:
        body["moviehash_match"] = hash_match
    return {"id": result_id, "type": "subtitle", "attributes": body}


class Api:
    """The stand-in API: answers by path, keeps every request."""

    def __init__(self):
        self.calls: list[dict] = []
        self.search: list[list[dict]] = []
        self.download = {"link": "http://127.0.0.1:1/link/secret-link", "remaining": 17, "requests": 3,
                         "reset_time_utc": "2099-01-01T00:00:00.000Z", "file_name": "f.srt",
                         "message": "", "reset_time": ""}
        self.download_status = 200
        self.login_status = 200
        self.clock = 1_000_000.0

    def __call__(self, url, *, method="GET", body=None, headers=None, **_):
        parts = urlsplit(url)
        call = {"method": method, "host": parts.hostname, "path": parts.path, "query": parts.query,
                "params": dict(parse_qsl(parts.query)), "body": json.loads(body) if body else None,
                "headers": dict(headers or {})}
        self.calls.append(call)
        if parts.path.endswith("/login"):
            if self.login_status != 200:
                return self.answer(self.login_status, {"message": "no"})
            return self.answer(200, {"token": token(self.clock + 86400), "base_url": "api.opensubtitles.com",
                                     "status": 200, "user": {"allowed_downloads": 20, "vip": False,
                                                             "level": "Sub leecher", "user_id": 1}})
        if parts.path.endswith("/infos/user"):
            return self.answer(200, {"data": {"allowed_downloads": 20, "remaining_downloads": 18, "vip": False,
                                              "level": "Sub leecher", "user_id": 1, "downloads_count": 2}})
        if parts.path.endswith("/subtitles"):
            data = self.search.pop(0) if self.search else []
            return self.answer(200, {"total_count": len(data), "data": data})
        if parts.path.endswith("/download"):
            return self.answer(self.download_status, self.download)
        return self.answer(404, {})

    @staticmethod
    def answer(status, payload):
        return HttpResponse(status, {"content-type": "application/json"}, json.dumps(payload).encode(), "")

    def paths(self):
        return [call["path"].rsplit("/", 1)[1] for call in self.calls]


def provider(api, *, account=False, state=None):
    credentials = Credentials(KEY, "someone" if account else None, PASSWORD if account else None)
    return OpenSubtitlesCom(credentials, state_dir=state, transport=api, clock=lambda: api.clock, sleep=lambda _: None)


class Targets(unittest.TestCase):
    def test_a_film_is_its_imdb_number(self):
        self.assertEqual(target_for("movie", "tt0133093", None).ids(), {"imdb_id": 133093})
        self.assertEqual(target_for("movie", "tmdb:603", None).ids(), {"tmdb_id": 603})

    def test_an_episode_is_its_series_season_and_number(self):
        target = target_for("series", "tt10986410", "tt10986410:1:1")
        self.assertEqual(target.ids(), {"parent_imdb_id": 10986410, "season_number": 1, "episode_number": 1})

    def test_no_identity_leaves_the_name(self):
        target = target_for("movie", "local:abc", None, filename="Film.2012.mkv")
        self.assertEqual(target.ids(), {})
        self.assertEqual(target.text(), {"query": "film.2012.mkv"})

    def test_a_malformed_hash_is_not_sent(self):
        self.assertIsNone(target_for("movie", "tt1", None, moviehash="xyz").moviehash)


class Search(unittest.TestCase):
    def test_the_hash_is_asked_first_and_alone(self):
        api = Api()
        api.search = [[result(11, "tr", hash_match=True), result(12, "en", hash_match=True)]]
        found = provider(api).search(target_for("movie", "tt1859650", None, moviehash="00ff00ff00ff00ff",
                                                title="Some Film"), ["tr", "en"])
        self.assertEqual(api.paths(), ["subtitles"])
        # Sorted, lowercase, the hash and the languages and nothing else.
        self.assertEqual(api.calls[0]["query"], "languages=en%2Ctr&moviehash=00ff00ff00ff00ff")
        self.assertEqual(api.calls[0]["headers"]["Api-Key"], KEY)
        self.assertEqual(api.calls[0]["headers"]["User-Agent"], "MediaBox v2.0")
        self.assertTrue(all(s.hash_match for s in found))
        self.assertEqual([s.provider for s in found], ["opensubtitles_com"] * 2)

    def test_without_a_hash_answer_the_imdb_identity_is_asked(self):
        api = Api()
        api.search = [[], [result(21, "tr")]]
        found = provider(api).search(target_for("movie", "tt1859650", None, moviehash="00ff00ff00ff00ff"), ["tr"])
        self.assertEqual([c["params"] for c in api.calls], [
            {"languages": "tr", "moviehash": "00ff00ff00ff00ff"},
            {"imdb_id": "1859650", "languages": "tr"},
        ])
        self.assertEqual(len(found), 1)
        self.assertFalse(found[0].hash_match)

    def test_the_identity_is_asked_only_for_languages_the_hash_left_empty(self):
        api = Api()
        api.search = [[result(31, "tr", hash_match=True)], [result(32, "en")]]
        provider(api).search(target_for("movie", "tt1859650", None, moviehash="00ff00ff00ff00ff"), ["tr", "en"])
        self.assertEqual(api.calls[1]["params"], {"imdb_id": "1859650", "languages": "en"})

    def test_an_episode_is_asked_by_its_series_and_numbers(self):
        api = Api()
        api.search = [[result(41, "tr")]]
        provider(api).search(target_for("series", "tt10986410", "tt10986410:1:1", title="A Series"), ["tr"])
        self.assertEqual(api.calls[0]["query"],
                         "episode_number=1&languages=tr&parent_imdb_id=10986410&season_number=1")
        self.assertNotIn("query", api.calls[0]["params"])

    def test_a_name_is_the_last_resort_only(self):
        api = Api()
        provider(api).search(target_for("movie", "local:x", None, filename="Film.2012.1080p.mkv"), ["tr"])
        self.assertEqual(api.calls[0]["params"], {"languages": "tr", "query": "film.2012.1080p.mkv"})
        api = Api()
        provider(api).search(target_for("movie", "tt5", None, filename="Film.2012.1080p.mkv"), ["tr"])
        self.assertTrue(all("query" not in call["params"] for call in api.calls))

    def test_searching_downloads_nothing(self):
        api = Api()
        api.search = [[result(n, "tr") for n in range(1, 9)]]
        provider(api).search(target_for("movie", "tt1", None), ["tr"])
        self.assertNotIn("download", api.paths())

    def test_without_a_key_nothing_is_asked(self):
        api = Api()
        quiet = OpenSubtitlesCom(Credentials(), transport=api)
        self.assertEqual(quiet.search(target_for("movie", "tt1", None), ["tr"]), [])
        self.assertEqual(api.calls, [])
        self.assertEqual(quiet.status()["state"], "not-configured")


class Results(unittest.TestCase):
    def test_everything_the_result_says_is_kept(self):
        subtitle = parse_result(result(777, "pt-BR", result_id="555", hash_match=True, fps=25.0,
                                       hearing_impaired=True, from_trusted=True, ratings=8.5,
                                       download_count=1234, ai_translated=False))
        details = subtitle.details
        self.assertEqual((details.subtitle_id, details.file_id), ("555", 777))
        self.assertEqual((details.fps, details.nb_cd, details.files), (25.0, 1, 1))
        self.assertTrue(details.hearing_impaired and details.from_trusted)
        self.assertEqual((details.ratings, details.download_count), (8.5, 1234))
        self.assertTrue(subtitle.hash_match)
        self.assertEqual(subtitle.label, "Film.2012.1080p.BluRay.x264-GRP")

    def test_a_result_without_a_file_is_nothing(self):
        self.assertIsNone(parse_result(result(1, "tr", files=[])))


class Download(unittest.TestCase):
    def test_the_file_id_is_downloaded_not_the_subtitle_id(self):
        api = Api()
        api.search = [[result(4242, "tr", result_id="9999")]]
        service = provider(api)
        (found,) = service.search(target_for("movie", "tt1", None), ["tr"])
        link = service.download(found.details.file_id)
        self.assertEqual(link, api.download["link"])
        download = api.calls[-1]
        self.assertEqual(download["method"], "POST")
        self.assertEqual(download["body"], {"file_id": 4242})
        self.assertNotIn("9999", json.dumps(download["body"]))
        self.assertEqual(service.status()["remainingDownloads"], 17)

    def test_a_used_up_quota_is_said_and_not_asked_again(self):
        api = Api()
        api.download_status = 406
        api.download = {"requests": 21, "remaining": -1, "message": "quota",
                        "reset_time_utc": "2099-01-01T00:00:00.000Z"}
        service = provider(api)
        with self.assertRaises(QuotaExhausted) as caught:
            service.download(1)
        self.assertEqual(caught.exception.code, "SUBTITLE_QUOTA_EXHAUSTED")
        with self.assertRaises(QuotaExhausted):
            service.download(2)
        self.assertEqual(api.paths().count("download"), 1)
        status = service.status()
        self.assertEqual((status["state"], status["remainingDownloads"]), ("quota-exhausted", 0))
        self.assertEqual(status["resetTimeUtc"], "2099-01-01T00:00:00.000Z")
        # Searching goes on: it costs no quota.
        service.search(target_for("movie", "tt1", None), ["tr"])
        self.assertEqual(api.paths()[-1], "subtitles")

    def test_the_last_download_of_the_day_stops_the_next(self):
        api = Api()
        api.download["remaining"] = 0
        service = provider(api)
        service.download(1)
        with self.assertRaises(QuotaExhausted):
            service.download(2)
        self.assertEqual(api.paths().count("download"), 1)


class Account(unittest.TestCase):
    def test_one_login_serves_every_request_and_the_quota_is_the_servers(self):
        api = Api()
        service = provider(api, account=True)
        service.search(target_for("movie", "tt1", None), ["tr"])
        service.search(target_for("movie", "tt2", None), ["tr"])
        service.download(5)
        self.assertEqual(api.paths().count("login"), 1)
        self.assertTrue(all(c["headers"].get("Authorization", "").startswith("Bearer ")
                            for c in api.calls if c["path"].endswith(("/subtitles", "/download"))))
        status = service.status()
        self.assertEqual((status["allowedDownloads"], status["vip"]), (20, False))
        self.assertEqual(status["remainingDownloads"], 17)
        self.assertTrue(status["authenticated"])

    def test_the_token_outlives_a_restart(self):
        api = Api()
        with tempfile.TemporaryDirectory() as state:
            provider(api, account=True, state=state).search(target_for("movie", "tt1", None), ["tr"])
            kept = os.path.join(state, "opensubtitles-session.json")
            self.assertEqual(os.stat(kept).st_mode & 0o777, 0o600)
            with open(kept) as handle:
                self.assertNotIn(PASSWORD, handle.read())
            again = provider(api, account=True, state=state)
            again.search(target_for("movie", "tt2", None), ["tr"])
        self.assertEqual(api.paths().count("login"), 1)

    def test_refused_credentials_are_not_sent_again(self):
        api = Api()
        api.login_status = 401
        service = provider(api, account=True)
        service.search(target_for("movie", "tt1", None), ["tr"])
        service.search(target_for("movie", "tt2", None), ["tr"])
        service.download(3)
        self.assertEqual(api.paths().count("login"), 1)
        self.assertEqual(service.status()["state"], "login-refused")
        # Downloads go on, on the application's own quota.
        self.assertNotIn("Authorization", api.calls[-1]["headers"])

    def test_no_secret_is_said(self):
        api = Api()
        service = provider(api, account=True)
        service.download(3)
        said = json.dumps(service.status()) + repr(service.credentials)
        for secret in (KEY, PASSWORD, "someone", "Bearer", api.download["link"]):
            self.assertNotIn(secret, said)

    def test_the_environment_is_read_and_emptied(self):
        environment = {"OPENSUBTITLES_API_KEY": KEY, "OPENSUBTITLES_USERNAME": "u",
                       "OPENSUBTITLES_PASSWORD": PASSWORD, "PATH": "/bin"}
        credentials = Credentials.from_environment(environment, consume=True)
        self.assertTrue(credentials.configured and credentials.account)
        self.assertEqual(environment, {"PATH": "/bin"})

    def test_systemd_credentials_are_read_too(self):
        with tempfile.TemporaryDirectory() as directory:
            with open(os.path.join(directory, "opensubtitles_api_key"), "w") as handle:
                handle.write(KEY + "\n")
            credentials = Credentials.from_environment({"CREDENTIALS_DIRECTORY": directory})
        self.assertEqual(credentials.api_key, KEY)
        self.assertFalse(credentials.account)


class Metadata(unittest.TestCase):
    def test_what_the_metadata_alone_refuses(self):
        def verdict(**attributes):
            subtitle = parse_result(result(1, "tr", **attributes))
            found = eligibility.from_metadata(subtitle.details, 24000 / 1001, hash_match=subtitle.hash_match)
            return found.result.value if found else None

        self.assertEqual(verdict(nb_cd=2), "REJECT_PARTIAL")
        self.assertEqual(verdict(files=[{"file_id": 1, "cd_number": 1}, {"file_id": 2, "cd_number": 2}]),
                         "REJECT_PARTIAL")
        self.assertEqual(verdict(foreign_parts_only=True), "REJECT_PARTIAL")
        self.assertEqual(verdict(fps=25.0), "REJECT_TIMEBASE_MISMATCH")
        # Not certain enough to refuse: left to the timeline.
        self.assertIsNone(verdict(fps=24.0))
        self.assertIsNone(verdict(fps=30.0 + 0.4))
        self.assertIsNone(verdict(fps=0))
        self.assertIsNone(verdict(release="Film.Part.1.2012", fps=23.976))
        # This exact file: a rate label does not overrule the hash.
        self.assertIsNone(verdict(fps=25.0, hash_match=True))


if __name__ == "__main__":
    unittest.main()
