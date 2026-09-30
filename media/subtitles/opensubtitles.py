"""OpenSubtitles.com, asked directly over its REST API.

The subtitle addon every Stremio account carries is one provider; this is a
second one, asked by the worker itself and merged into the same list. Its
answers carry what the addon's do not: whether a subtitle was matched to
this exact file by its hash, how many files it is split into, the rate the
uploader made it for, whether it is only the foreign-language parts.

Two things cost different amounts and are kept apart:

    search    GET /subtitles -- free: no download quota, 5 requests a second
              per address. The list a film offers is only ever this.
    download  POST /download -- this is what the quota counts. Asked only
              for a subtitle somebody actually needs (chosen from the menu,
              or tried by the automatic choice), one at a time, and never
              again for a file already kept.

What the rules are, from the official API reference
(opensubtitles.stoplight.io, `open_api.json`, checked 2026-09-30):

  * every request carries the application's `Api-Key` and a `User-Agent` of
    the form "App v1.2";
  * `/login` is limited to 1 request a second, 10 a minute, 30 an hour; a
    401 means stop trying those credentials. Its token lives 24 hours, and
    every later request goes to the `base_url` it names;
  * without a login, downloads are counted per address; with one, per the
    account's rank. Either way the server says how many are left, in each
    `/download` answer (`remaining`, `reset_time_utc`) and in
    `/infos/user` (`allowed_downloads`, `remaining_downloads`, `vip`). Those
    numbers are read, never assumed;
  * search parameters go sorted, lowercase, without defaults, and IMDb and
    TMDb ids as plain integers; `moviehash` is 16 hex digits. There is no
    `moviebytesize` parameter in this API (the old XML-RPC one had it);
  * a download is asked for by `files[].file_id`, not by the subtitle's id;
    the link it answers is temporary (hours) and is never kept -- the file
    it leads to is, in the subtitle store.

Credentials come from the worker's environment or its systemd credentials,
and are never logged, never written to a settings file, never answered in a
diagnostic, and never passed to a child process.
"""

from __future__ import annotations

import base64
import json
import logging
import os
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable, Iterable
from urllib.parse import quote_plus, urlencode, urljoin, urlsplit

from ..errors import MediaError, UpstreamError
from ..http import HttpResponse, request
from ..stremio.models import ProviderDetails, Subtitle, SubtitleSource
from .languages import canonical


LOG = logging.getLogger("media.subtitles.opensubtitles")

PROVIDER = "opensubtitles_com"
PROVIDER_NAME = "OpenSubtitles.com"

DEFAULT_HOST = "api.opensubtitles.com"
API_PATH = "/api/v1"
DEFAULT_USER_AGENT = "MediaBox v2.0"

#: A candidate's address inside the media core. Not fetchable: a download is
#: asked for by the file id it carries, and only when it is needed.
URL_PREFIX = "opensubtitles-com:file/"

#: A login's token lives this long by the documentation; used only when the
#: token itself does not say (`exp`), and given back early by `TOKEN_MARGIN`.
TOKEN_LIFETIME = 24 * 3600.0
TOKEN_MARGIN = 600.0
#: After a login the server limited, it is not tried again for this long.
LOGIN_BACKOFF = 300.0
#: After the API said "too many requests", nothing is asked for this long.
RATE_BACKOFF = 10.0
#: 5 requests a second per address: one every quarter of a second is under it.
REQUEST_SPACING = 0.25
#: The API redirects a request whose parameters are not in its own form;
#: followed only within its own hosts, so the key never leaves them.
MAX_REDIRECTS = 3

ENVIRONMENT = {
    "api_key": "OPENSUBTITLES_API_KEY",
    "username": "OPENSUBTITLES_USERNAME",
    "password": "OPENSUBTITLES_PASSWORD",
    "user_agent": "OPENSUBTITLES_USER_AGENT",
}
#: The same values as systemd credentials (`LoadCredential=`), by file name.
CREDENTIAL_FILES = {
    "api_key": "opensubtitles_api_key",
    "username": "opensubtitles_username",
    "password": "opensubtitles_password",
}


class QuotaExhausted(MediaError):
    def __init__(self, reset: str | None) -> None:
        super().__init__(
            "SUBTITLE_QUOTA_EXHAUSTED",
            "the subtitle provider's download quota is used up",
            429,
            {"provider": PROVIDER, "resetTimeUtc": reset},
        )


class ProviderUnavailable(MediaError):
    def __init__(self, reason: str) -> None:
        super().__init__("SUBTITLE_PROVIDER_UNAVAILABLE", f"{PROVIDER_NAME}: {reason}", 502, {"provider": PROVIDER})


@dataclass(frozen=True, slots=True)
class Credentials:
    """The application's key and, optionally, an account. Never printed."""

    api_key: str | None = field(default=None, repr=False)
    username: str | None = field(default=None, repr=False)
    password: str | None = field(default=None, repr=False)
    user_agent: str = DEFAULT_USER_AGENT

    @property
    def configured(self) -> bool:
        return bool(self.api_key)

    @property
    def account(self) -> bool:
        return bool(self.username and self.password)

    @classmethod
    def from_environment(cls, environ: dict[str, str] | None = None, *, consume: bool = False) -> "Credentials":
        """From `OPENSUBTITLES_*`, else from `$CREDENTIALS_DIRECTORY`.

        With `consume`, the variables are taken out of the environment once
        read, so that no process the worker starts (the sync engine, ffmpeg)
        inherits them.
        """
        env = os.environ if environ is None else environ
        directory = env.get("CREDENTIALS_DIRECTORY")
        values: dict[str, str | None] = {}
        for name, variable in ENVIRONMENT.items():
            value = (env.pop(variable, None) if consume else env.get(variable)) or None
            if value is None and directory and name in CREDENTIAL_FILES:
                try:
                    value = (Path(directory) / CREDENTIAL_FILES[name]).read_text(encoding="utf-8").strip() or None
                except OSError:
                    value = None
            values[name] = value.strip() if isinstance(value, str) else None
        return cls(
            api_key=values["api_key"],
            username=values["username"],
            password=values["password"],
            user_agent=values["user_agent"] or DEFAULT_USER_AGENT,
        )


@dataclass(frozen=True, slots=True)
class Target:
    """What is known of the video, in the API's terms."""

    moviehash: str | None = None
    imdb_id: int | None = None
    tmdb_id: int | None = None
    parent_imdb_id: int | None = None
    parent_tmdb_id: int | None = None
    season: int | None = None
    episode: int | None = None
    filename: str | None = None
    title: str | None = None
    year: int | None = None

    def ids(self) -> dict[str, Any]:
        """The most exact identity there is, as search parameters."""
        if self.season is not None and self.episode is not None and (self.parent_imdb_id or self.parent_tmdb_id):
            found: dict[str, Any] = {"season_number": self.season, "episode_number": self.episode}
            if self.parent_imdb_id:
                found["parent_imdb_id"] = self.parent_imdb_id
            else:
                found["parent_tmdb_id"] = self.parent_tmdb_id
            return found
        if self.imdb_id:
            return {"imdb_id": self.imdb_id}
        if self.tmdb_id:
            return {"tmdb_id": self.tmdb_id}
        return {}

    def text(self) -> dict[str, Any]:
        """The last resort: a name, narrowed by what else is known."""
        name = self.filename or self.title
        if not name:
            return {}
        found: dict[str, Any] = {"query": name.lower()}
        if self.season is not None and self.episode is not None:
            found["season_number"] = self.season
            found["episode_number"] = self.episode
        if self.year:
            found["year"] = self.year
        return found


def target_for(
    type_name: str | None,
    item_id: str | None,
    video_id: str | None,
    *,
    moviehash: str | None = None,
    filename: str | None = None,
    title: str | None = None,
) -> Target:
    """A Stremio id (`tt1859650`, `tt10986410:1:1`, `tmdb:603`) as a Target.

    An episode is its series plus its season and number -- never the
    series' name searched and guessed at.
    """
    moviehash = (moviehash or "").strip().lower()
    if len(moviehash) != 16 or any(c not in "0123456789abcdef" for c in moviehash):
        moviehash = ""
    fields: dict[str, Any] = {"moviehash": moviehash or None, "filename": filename, "title": title}
    ident = video_id or item_id or ""
    parts = ident.split(":")
    if parts and parts[0] == "tmdb":
        parts = ["tmdb:" + parts[1]] + parts[2:] if len(parts) > 1 else parts
    head = parts[0] if parts else ""
    number = _external_id(head)
    if len(parts) == 3 and parts[1].isdigit() and parts[2].isdigit() and number:
        fields["season"], fields["episode"] = int(parts[1]), int(parts[2])
        fields["parent_tmdb_id" if head.startswith("tmdb:") else "parent_imdb_id"] = number
    elif len(parts) == 1 and number and type_name != "series":
        fields["tmdb_id" if head.startswith("tmdb:") else "imdb_id"] = number
    return Target(**fields)


def _external_id(value: str) -> int | None:
    """`tt0133093` -> 133093, `tmdb:603` -> 603: no prefix, no leading zero."""
    digits = value[2:] if value.startswith("tt") else value[5:] if value.startswith("tmdb:") else ""
    return int(digits) if digits.isdigit() and int(digits) > 0 else None


def api_language(code: str) -> str:
    """The API's code for a canonical one: its Portuguese and Chinese are regional."""
    return {"pt": "pt-pt", "zh": "zh-cn"}.get(code, code)


class OpenSubtitlesCom:
    """The provider. One per worker; thread-safe."""

    def __init__(
        self,
        credentials: Credentials,
        *,
        state_dir: str | os.PathLike[str] | None = None,
        transport: Callable[..., HttpResponse] = request,
        clock: Callable[[], float] = time.time,
        sleep: Callable[[float], None] = time.sleep,
    ) -> None:
        self.credentials = credentials
        self._transport = transport
        self._clock = clock
        self._sleep = sleep
        self._session_file = Path(state_dir) / "opensubtitles-session.json" if state_dir else None
        self._lock = threading.Lock()
        # Re-entrant: a login reads the account's quota with the new token.
        self._login_lock = threading.RLock()
        self._host = DEFAULT_HOST
        self._token: str | None = None
        self._expires = 0.0
        self._last_request = 0.0
        self._quiet_until = 0.0
        self._login_after = 0.0
        self._login_refused = False
        self._state = "ready" if credentials.configured else "not-configured"
        self._quota: dict[str, Any] = {
            "allowedDownloads": None,
            "remainingDownloads": None,
            "vip": None,
            "resetTimeUtc": None,
            "level": None,
        }
        self._exhausted_until = 0.0
        self.counts = {"search": 0, "download": 0, "login": 0}

    @property
    def configured(self) -> bool:
        return self.credentials.configured

    # ------------------------------------------------------------- status

    def status(self) -> dict[str, Any]:
        """What the diagnostics show. No credential, token or link in it."""
        with self._lock:
            state = self._state
            if state == "quota-exhausted" and self._clock() >= self._exhausted_until:
                state = "ready"
            return {
                "provider": PROVIDER,
                "name": PROVIDER_NAME,
                "configured": self.credentials.configured,
                "account": self.credentials.account,
                "authenticated": self._token is not None and self._clock() < self._expires,
                "state": state,
                **self._quota,
                "requests": dict(self.counts),
            }

    def exhausted(self) -> bool:
        with self._lock:
            return self._clock() < self._exhausted_until

    # ------------------------------------------------------------- search

    def search(self, target: Target, languages: Iterable[str]) -> list[Subtitle]:
        """Candidates for the video, most exact identity first.

        The file's hash; then its IMDb/TMDb identity (an episode as series,
        season and number) for the languages the hash left without a
        subtitle; a name only when there is no identity at all. Nothing is
        downloaded.
        """
        if not self.credentials.configured:
            return []
        wanted: dict[str, str] = {}
        for code in languages:
            if code and code not in wanted.values():
                wanted[api_language(code)] = code
        steps: list[tuple[str, dict[str, Any]]] = []
        if target.moviehash:
            steps.append(("hash", {"moviehash": target.moviehash}))
        ids = target.ids()
        if ids:
            steps.append(("id", ids))
        elif target.text():
            steps.append(("text", target.text()))
        found: list[Subtitle] = []
        seen: set[int] = set()
        missing = dict(wanted)
        for stage, params in steps:
            if wanted and not missing:
                break
            if not wanted and found:
                break
            if missing:
                params = {**params, "languages": ",".join(sorted(missing))}
            try:
                results = self._search(params)
            except MediaError as exc:
                LOG.info("%s search (%s) unavailable: %s", PROVIDER_NAME, stage, exc.message)
                break
            added = [s for s in results if s.details and s.details.file_id not in seen]
            for subtitle in added:
                seen.add(subtitle.details.file_id)  # type: ignore[union-attr]
            found.extend(added)
            covered = {canonical(s.language) for s in added}
            missing = {api: code for api, code in missing.items() if code not in covered}
            LOG.info("%s search %s: %d result(s)", PROVIDER_NAME, stage, len(added))
        return found

    def _search(self, params: dict[str, Any]) -> list[Subtitle]:
        with self._lock:
            self.counts["search"] += 1
        answer = self._call("GET", "/subtitles", params=params)
        payload = answer.json()
        data = payload.get("data") if isinstance(payload, dict) else None
        if not isinstance(data, list):
            return []
        found = [parse_result(item) for item in data]
        return [subtitle for subtitle in found if subtitle is not None]

    # ----------------------------------------------------------- download

    def download(self, file_id: int) -> str:
        """The temporary link for one file. This is what the quota counts."""
        if not self.credentials.configured:
            raise ProviderUnavailable("not configured")
        with self._lock:
            if self._clock() < self._exhausted_until:
                raise QuotaExhausted(self._quota.get("resetTimeUtc"))
            self.counts["download"] += 1
        answer = self._call("POST", "/download", body={"file_id": int(file_id)}, expected=(200, 406))
        try:
            payload = answer.json()
        except UpstreamError:
            payload = {}
        payload = payload if isinstance(payload, dict) else {}
        self._read_quota(payload)
        remaining = payload.get("remaining")
        link = payload.get("link")
        if answer.status == 406 or (isinstance(remaining, (int, float)) and remaining < 0) or not link:
            if answer.status == 406 or (isinstance(remaining, (int, float)) and remaining < 0):
                self._exhaust(payload)
                raise QuotaExhausted(self._quota.get("resetTimeUtc"))
            raise ProviderUnavailable("the download answered no link")
        if isinstance(remaining, (int, float)) and remaining <= 0:
            # This one was the last: the next is refused before it is asked.
            self._exhaust(payload)
        return str(link)

    def _read_quota(self, payload: dict[str, Any]) -> None:
        with self._lock:
            if isinstance(payload.get("remaining"), (int, float)):
                self._quota["remainingDownloads"] = max(0, int(payload["remaining"]))
            if isinstance(payload.get("reset_time_utc"), str):
                self._quota["resetTimeUtc"] = payload["reset_time_utc"]

    def _exhaust(self, payload: dict[str, Any]) -> None:
        until = _utc_seconds(payload.get("reset_time_utc"))
        with self._lock:
            # Without a time from the server, it is asked again in an hour.
            self._exhausted_until = until if until and until > self._clock() else self._clock() + 3600.0
            self._state = "quota-exhausted"
            self._quota["remainingDownloads"] = 0
        LOG.warning("%s download quota used up; resets %s", PROVIDER_NAME, self._quota.get("resetTimeUtc") or "later")

    # -------------------------------------------------------------- login

    def _bearer(self) -> str | None:
        """A user token, when there is an account and it lets us in; None is
        the application's own (anonymous) quota."""
        if not self.credentials.account:
            return None
        with self._login_lock:
            now = self._clock()
            with self._lock:
                if self._token and now < self._expires:
                    return self._token
                if self._login_refused or now < self._login_after:
                    return None
            if self._restore(now):
                return self._token
            return self._login(now)

    def _login(self, now: float) -> str | None:
        with self._lock:
            self.counts["login"] += 1
        try:
            answer = self._call(
                "POST",
                "/login",
                body={"username": self.credentials.username, "password": self.credentials.password},
                expected=(200, 401, 429),
                authenticated=False,
            )
        except MediaError as exc:
            with self._lock:
                self._login_after = now + LOGIN_BACKOFF
            LOG.info("%s login unavailable: %s", PROVIDER_NAME, exc.message)
            return None
        if answer.status == 401:
            # The documentation's rule: stop sending those credentials.
            with self._lock:
                self._login_refused = True
                self._state = "login-refused"
            LOG.warning("%s refused the account; downloads continue on the application's quota", PROVIDER_NAME)
            return None
        if answer.status == 429:
            with self._lock:
                self._login_after = now + LOGIN_BACKOFF
            return None
        payload = answer.json()
        token = payload.get("token") if isinstance(payload, dict) else None
        if not isinstance(token, str) or not token:
            with self._lock:
                self._login_after = now + LOGIN_BACKOFF
            return None
        host = payload.get("base_url") if isinstance(payload.get("base_url"), str) else DEFAULT_HOST
        user = payload.get("user") if isinstance(payload.get("user"), dict) else {}
        with self._lock:
            self._token = token
            self._host = _own_host(host) or DEFAULT_HOST
            self._expires = _token_expiry(token, now)
            self._read_user(user)
            if self._state == "login-refused":
                self._state = "ready"
        self._keep_session()
        # What is left of today's downloads is not in the login's answer.
        try:
            info = self._call("GET", "/infos/user").json()
            if isinstance(info, dict) and isinstance(info.get("data"), dict):
                with self._lock:
                    self._read_user(info["data"])
        except MediaError:
            pass
        return token

    def _read_user(self, user: dict[str, Any]) -> None:
        for source, target in (
            ("allowed_downloads", "allowedDownloads"),
            ("remaining_downloads", "remainingDownloads"),
            ("vip", "vip"),
            ("level", "level"),
        ):
            if source in user and user[source] is not None:
                self._quota[target] = user[source]

    def _restore(self, now: float) -> bool:
        """A token kept from an earlier run of the worker, for this account."""
        if self._session_file is None:
            return False
        try:
            kept = json.loads(self._session_file.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            return False
        if not isinstance(kept, dict) or kept.get("account") != self._account_key():
            return False
        token, expires = kept.get("token"), kept.get("expires")
        if not isinstance(token, str) or not isinstance(expires, (int, float)) or now >= expires:
            return False
        with self._lock:
            self._token, self._expires = token, float(expires)
            self._host = _own_host(str(kept.get("host") or "")) or DEFAULT_HOST
        return True

    def _keep_session(self) -> None:
        if self._session_file is None:
            return
        payload = json.dumps(
            {"account": self._account_key(), "token": self._token, "expires": self._expires, "host": self._host}
        ).encode("utf-8")
        temporary = self._session_file.with_name(self._session_file.name + ".part")
        try:
            descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
            with os.fdopen(descriptor, "wb") as handle:
                handle.write(payload)
            os.replace(temporary, self._session_file)
        except OSError as exc:
            LOG.info("%s session not kept: %s", PROVIDER_NAME, type(exc).__name__)

    def _account_key(self) -> str:
        import hashlib

        return hashlib.sha256(f"{self.credentials.username}\x1f{self.credentials.password}".encode()).hexdigest()[:24]

    def _forget_token(self) -> None:
        with self._lock:
            self._token, self._expires = None, 0.0
        if self._session_file is not None:
            try:
                self._session_file.unlink()
            except OSError:
                pass

    # ------------------------------------------------------------ plumbing

    def _call(
        self,
        method: str,
        path: str,
        *,
        params: dict[str, Any] | None = None,
        body: dict[str, Any] | None = None,
        expected: tuple[int, ...] = (200,),
        authenticated: bool = True,
        retried: bool = False,
    ) -> HttpResponse:
        with self._lock:
            if self._clock() < self._quiet_until:
                raise ProviderUnavailable("rate limited")
        token = self._bearer() if authenticated else None
        headers = {
            "Api-Key": self.credentials.api_key or "",
            "User-Agent": self.credentials.user_agent,
            "Accept": "application/json",
        }
        if token:
            headers["Authorization"] = f"Bearer {token}"
        data = None
        if body is not None:
            data = json.dumps(body).encode("utf-8")
            headers["Content-Type"] = "application/json"
        url = f"https://{self._host}{API_PATH}{path}"
        if params:
            url += "?" + urlencode(sorted((k, _param(v)) for k, v in params.items()), quote_via=quote_plus)
        answer = self._send(method, url, data, headers)
        if answer.status == 401 and token and not retried:
            # The token went stale before its time: once more with a new one.
            self._forget_token()
            return self._call(method, path, params=params, body=body, expected=expected, retried=True)
        if answer.status == 429 and 429 not in expected:
            with self._lock:
                self._quiet_until = self._clock() + RATE_BACKOFF
                self._state = "rate-limited"
            raise ProviderUnavailable("rate limited")
        if answer.status not in expected:
            if answer.status == 403:
                # A wrong key or User-Agent: an operator's fault, said once.
                with self._lock:
                    self._state = "refused"
            raise ProviderUnavailable(f"HTTP {answer.status}")
        with self._lock:
            if self._state in ("rate-limited", "refused", "unavailable"):
                self._state = "ready"
        return answer

    def _send(self, method: str, url: str, data: bytes | None, headers: dict[str, str]) -> HttpResponse:
        """One request, spaced from the last, redirects followed by hand and
        only within the API's own hosts."""
        for _ in range(MAX_REDIRECTS + 1):
            with self._lock:
                wait = self._last_request + REQUEST_SPACING - self._clock()
                self._last_request = max(self._clock(), self._last_request + REQUEST_SPACING)
            if wait > 0:
                self._sleep(wait)
            try:
                answer = self._transport(
                    url, method=method, body=data, headers=headers, timeout=10.0,
                    max_bytes=2 * 1024 * 1024, allow_redirects=False,
                )
            except UpstreamError as exc:
                with self._lock:
                    self._state = "unavailable"
                # The URL is the API's own, with no secret in it; still, only
                # the reason is said.
                raise ProviderUnavailable("unreachable") from exc
            if answer.status in (301, 302, 307, 308):
                location = answer.headers.get("location")
                target = urljoin(url, location) if location else ""
                if target and _own_host(urlsplit(target).hostname or "") and urlsplit(target).scheme == "https":
                    url = target
                    continue
                raise ProviderUnavailable("redirected away from the API")
            return answer
        raise ProviderUnavailable("too many redirects")


def parse_result(item: Any) -> Subtitle | None:
    """One `/subtitles` result as a candidate: its first file, and what the
    result says about all of them."""
    if not isinstance(item, dict) or not isinstance(item.get("attributes"), dict):
        return None
    attributes = item["attributes"]
    files = [
        entry for entry in attributes.get("files") or []
        if isinstance(entry, dict) and isinstance(entry.get("file_id"), int) and not isinstance(entry.get("file_id"), bool)
    ]
    if not files:
        return None
    first = min(files, key=lambda entry: entry.get("cd_number") if isinstance(entry.get("cd_number"), int) else 1)
    file_id = int(first["file_id"])
    release = _text(attributes.get("release")) or _text(first.get("file_name"))
    fps = attributes.get("fps")
    details = ProviderDetails(
        subtitle_id=_text(attributes.get("subtitle_id")) or _text(item.get("id")),
        file_id=file_id,
        release=release,
        fps=float(fps) if isinstance(fps, (int, float)) and not isinstance(fps, bool) and fps > 0 else None,
        nb_cd=attributes.get("nb_cd") if isinstance(attributes.get("nb_cd"), int) else None,
        files=len(files),
        hearing_impaired=attributes.get("hearing_impaired") is True,
        foreign_parts_only=attributes.get("foreign_parts_only") is True,
        from_trusted=attributes.get("from_trusted") is True,
        ratings=float(attributes["ratings"]) if isinstance(attributes.get("ratings"), (int, float)) else None,
        download_count=attributes.get("download_count") if isinstance(attributes.get("download_count"), int) else None,
        ai_translated=attributes.get("ai_translated") is True,
        machine_translated=attributes.get("machine_translated") is True,
    )
    return Subtitle(
        id=f"{PROVIDER}:{file_id}",
        url=f"{URL_PREFIX}{file_id}",
        language=_text(attributes.get("language")),
        addon_id=None,
        source=SubtitleSource.PROVIDER,
        label=release,
        # The server's own answer to "was this matched to the file's hash".
        hash_match=attributes.get("moviehash_match") is True,
        provider=PROVIDER,
        provider_name=PROVIDER_NAME,
        details=details,
    )


def file_id_of(subtitle: Subtitle) -> int | None:
    if subtitle.provider != PROVIDER or subtitle.details is None:
        return None
    return subtitle.details.file_id


def _param(value: Any) -> str:
    return str(value).lower()


def _text(value: Any) -> str | None:
    if isinstance(value, str) and value.strip():
        return value.strip()
    if isinstance(value, int) and not isinstance(value, bool):
        return str(value)
    return None


def _own_host(host: str) -> str | None:
    """`host` if it is one of the API's, else None."""
    host = (host or "").strip().lower().rstrip(".")
    if host == "opensubtitles.com" or host.endswith(".opensubtitles.com"):
        return host
    return None


def _token_expiry(token: str, now: float) -> float:
    """When the token stops working: its own `exp`, else the documented day."""
    try:
        claims = json.loads(base64.urlsafe_b64decode(token.split(".")[1] + "==="))
        expires = float(claims["exp"])
        if expires > now:
            return expires - TOKEN_MARGIN
    except (IndexError, KeyError, TypeError, ValueError):
        pass
    return now + TOKEN_LIFETIME - TOKEN_MARGIN


def _utc_seconds(value: Any) -> float | None:
    if not isinstance(value, str) or not value:
        return None
    from datetime import datetime

    try:
        return datetime.fromisoformat(value.replace("Z", "+00:00")).timestamp()
    except ValueError:
        return None
