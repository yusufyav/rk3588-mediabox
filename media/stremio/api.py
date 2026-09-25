"""The Stremio account API (`api.strem.io`).

This is where session, the installed addon collection and the user's library
come from. It is a small JSON-RPC-ish surface:

    POST /api/login                 {email, password}        -> authKey
    POST /api/logout                {authKey}
    POST /api/getUser               {authKey}                -> user
    POST /api/addonCollectionGet    {authKey|null, update}   -> addons
    POST /api/datastoreGet          {authKey, collection, all} -> library items

`authKey: null` is a supported, documented call: it returns the default addon
collection, which is why the media core works out of the box without anybody
signing in. Signing in is offered, never required.

Credentials are never stored. The auth key that a login returns is, with
`0600` permissions, because that is the thing the appliance actually needs and
the password is not.
"""

from __future__ import annotations

import json
import logging
import os
import stat
import threading
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from ..errors import InvalidRequest, UpstreamError
from ..http import post_json


LOG = logging.getLogger(__name__)

DEFAULT_API_URL = "https://api.strem.io/api"

#: The Stremio API answers `{"result": …}` or `{"error": {…}}` with HTTP 200,
#: so the HTTP status alone never says whether a call worked.
def _result(payload: Any, call: str) -> Any:
    if not isinstance(payload, dict):
        raise UpstreamError(f"{call} did not return an object")
    error = payload.get("error")
    if error:
        message = error.get("message") if isinstance(error, dict) else str(error)
        raise UpstreamError(f"{call} failed: {message}", {"call": call})
    if "result" not in payload:
        raise UpstreamError(f"{call} returned neither a result nor an error")
    return payload["result"]


@dataclass(slots=True)
class StoredSession:
    auth_key: str | None = None
    user_id: str | None = None
    email: str | None = None

    def as_dict(self) -> dict[str, Any]:
        return {"authKey": self.auth_key, "userId": self.user_id, "email": self.email}


class SessionStore:
    """Persists the auth key, and only the auth key."""

    def __init__(self, path: str | os.PathLike[str] | None) -> None:
        self._path = Path(path) if path else None
        self._lock = threading.Lock()
        self._session = StoredSession()
        if self._path is not None:
            self._load()

    def _load(self) -> None:
        assert self._path is not None
        try:
            raw = json.loads(self._path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            return
        if not isinstance(raw, dict):
            return
        self._session = StoredSession(
            auth_key=raw.get("authKey") if isinstance(raw.get("authKey"), str) else None,
            user_id=raw.get("userId") if isinstance(raw.get("userId"), str) else None,
            email=raw.get("email") if isinstance(raw.get("email"), str) else None,
        )

    def get(self) -> StoredSession:
        with self._lock:
            return StoredSession(self._session.auth_key, self._session.user_id, self._session.email)

    def set(self, session: StoredSession) -> None:
        with self._lock:
            self._session = session
            if self._path is None:
                return
            try:
                self._path.parent.mkdir(parents=True, exist_ok=True)
                temporary = self._path.with_suffix(".tmp")
                temporary.write_text(json.dumps(session.as_dict()), encoding="utf-8")
                os.chmod(temporary, stat.S_IRUSR | stat.S_IWUSR)
                temporary.replace(self._path)
            except OSError as exc:
                LOG.warning("Stremio session could not be persisted: %s", exc)

    def clear(self) -> None:
        self.set(StoredSession())
        if self._path is not None:
            try:
                self._path.unlink(missing_ok=True)
            except OSError:
                pass


class StremioAPI:
    def __init__(
        self,
        *,
        api_url: str = DEFAULT_API_URL,
        store: SessionStore | None = None,
        timeout: float = 15.0,
    ) -> None:
        self.api_url = api_url.rstrip("/")
        self.store = store or SessionStore(None)
        self._timeout = timeout

    def _call(self, method: str, payload: dict[str, Any]) -> Any:
        url = f"{self.api_url}/{method}"
        return _result(post_json(url, payload, timeout=self._timeout), method)

    # ------------------------------------------------------------------ session

    def login(self, email: str, password: str) -> StoredSession:
        if not isinstance(email, str) or not isinstance(password, str) or not email or not password:
            raise InvalidRequest("email and password are required")
        result = self._call(
            "login", {"type": "Login", "email": email, "password": password, "facebook": False}
        )
        auth_key = result.get("authKey") if isinstance(result, dict) else None
        if not isinstance(auth_key, str) or not auth_key:
            raise UpstreamError("login returned no auth key")
        user = result.get("user") if isinstance(result, dict) else None
        session = StoredSession(
            auth_key=auth_key,
            user_id=(user or {}).get("_id") if isinstance(user, dict) else None,
            email=(user or {}).get("email") if isinstance(user, dict) else email,
        )
        self.store.set(session)
        return session

    def logout(self) -> None:
        session = self.store.get()
        if session.auth_key:
            try:
                self._call("logout", {"type": "Logout", "authKey": session.auth_key})
            except UpstreamError as exc:
                LOG.info("Stremio logout was refused upstream: %s", exc.message)
        self.store.clear()

    def get_user(self) -> dict[str, Any] | None:
        session = self.store.get()
        if not session.auth_key:
            return None
        result = self._call("getUser", {"type": "GetUser", "authKey": session.auth_key})
        return result if isinstance(result, dict) else None

    # ------------------------------------------------------------------- addons

    def addon_collection(self) -> list[dict[str, Any]]:
        """The installed addons, or Stremio's defaults when nobody is signed in."""
        session = self.store.get()
        result = self._call(
            "addonCollectionGet",
            {"type": "AddonCollectionGet", "authKey": session.auth_key, "update": True},
        )
        addons = result.get("addons") if isinstance(result, dict) else None
        if not isinstance(addons, list):
            raise UpstreamError("addonCollectionGet returned no addons")
        return [entry for entry in addons if isinstance(entry, dict)]

    # ------------------------------------------------------------------ library

    def library(self, collection: str = "libraryItem") -> list[dict[str, Any]]:
        """Every record in the account's library, the removed ones included.

        All of it, because the official clients work from all of it: this used
        to ask for the first 500 ids datastoreMeta listed, in whatever order it
        listed them, and an account with 893 records lost titles it was still
        watching. Removed records are kept too; which of them count as what is
        the adapter's decision, and "Devam Et" does count some of them.
        """
        session = self.store.get()
        if not session.auth_key:
            return []
        result = self._call(
            "datastoreGet",
            {
                "type": "DatastoreGet",
                "authKey": session.auth_key,
                "collection": collection,
                "all": True,
                "ids": [],
            },
        )
        return [entry for entry in result if isinstance(entry, dict)] if isinstance(result, list) else []
