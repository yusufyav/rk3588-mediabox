"""Reusable Kodi JSON-RPC client and playback validation."""

from __future__ import annotations

import base64
import json
import math
import threading
import urllib.error
import urllib.request
import xml.etree.ElementTree as ET
from pathlib import Path
from typing import Any
from urllib.parse import unquote, urlsplit, urlunsplit

from .config import KodiConfig
from .errors import InvalidRequest, KodiRPCError, KodiUnavailable
from .processes import find_kodi_process, kodi_runtime_settings_paths


def _settings_endpoint(path: Path) -> str | None:
    try:
        root = ET.parse(path).getroot()
    except (OSError, ET.ParseError):
        return None
    settings = {
        node.get("id"): (node.text or "").strip()
        for node in root.iter("setting")
        if node.get("id")
    }
    if settings.get("services.webserver", "false").lower() != "true":
        return None
    port_text = settings.get("services.webserverport", "")
    try:
        port = int(port_text)
    except ValueError:
        return None
    if not 1 <= port <= 65535:
        return None
    scheme = "https" if settings.get("services.webserverssl", "false").lower() == "true" else "http"
    username = settings.get("services.webserverusername", "")
    password = settings.get("services.webserverpassword", "")
    auth = ""
    if username and password:
        from urllib.parse import quote

        auth = f"{quote(username, safe='')}:{quote(password, safe='')}@"
    return f"{scheme}://{auth}127.0.0.1:{port}/jsonrpc"


class KodiEndpointResolver:
    def __init__(self, config: KodiConfig) -> None:
        self._config = config

    def resolve(self) -> str:
        if self._config.endpoint:
            return self._config.endpoint
        candidates = kodi_runtime_settings_paths()
        candidates.extend(Path(item) for item in self._config.settings_paths)
        seen: set[Path] = set()
        for path in candidates:
            if path in seen:
                continue
            seen.add(path)
            endpoint = _settings_endpoint(path)
            if endpoint:
                return endpoint
        raise KodiUnavailable("Kodi endpoint could not be discovered from runtime settings")


def _transport_url(endpoint: str) -> tuple[str, str | None]:
    parsed = urlsplit(endpoint)
    auth = None
    if parsed.username is not None:
        password = parsed.password or ""
        token = f"{unquote(parsed.username)}:{unquote(password)}".encode()
        auth = "Basic " + base64.b64encode(token).decode("ascii")
        host = parsed.hostname or ""
        if parsed.port is not None:
            host += f":{parsed.port}"
        endpoint = urlunsplit((parsed.scheme, host, parsed.path, parsed.query, ""))
    return endpoint, auth


class KodiClient:
    def __init__(self, config: KodiConfig) -> None:
        self.config = config
        self.resolver = KodiEndpointResolver(config)
        self._id = 0
        self._lock = threading.Lock()

    def call(self, method: str, params: dict[str, Any] | None = None) -> Any:
        with self._lock:
            self._id += 1
            request_id = self._id
        endpoint, authorization = _transport_url(self.resolver.resolve())
        payload: dict[str, Any] = {
            "jsonrpc": "2.0",
            "id": request_id,
            "method": method,
        }
        if params is not None:
            payload["params"] = params
        headers = {"Content-Type": "application/json", "Accept": "application/json"}
        if authorization:
            headers["Authorization"] = authorization
        request = urllib.request.Request(
            endpoint,
            data=json.dumps(payload, separators=(",", ":")).encode(),
            headers=headers,
            method="POST",
        )
        try:
            with urllib.request.urlopen(
                request, timeout=self.config.request_timeout_seconds
            ) as response:
                document = json.load(response)
        except (urllib.error.URLError, TimeoutError, OSError, json.JSONDecodeError) as exc:
            raise KodiUnavailable() from exc
        if not isinstance(document, dict):
            raise KodiRPCError("Kodi returned an invalid JSON-RPC document")
        if "error" in document:
            error = document["error"]
            message = error.get("message", "Kodi JSON-RPC error") if isinstance(error, dict) else str(error)
            raise KodiRPCError(message, error)
        if "result" not in document:
            raise KodiRPCError("Kodi JSON-RPC response has no result")
        return document["result"]

    def ping(self) -> bool:
        return self.call("JSONRPC.Ping") == "pong"

    def active_players(self) -> list[dict[str, Any]]:
        result = self.call("Player.GetActivePlayers")
        if not isinstance(result, list):
            raise KodiRPCError("Player.GetActivePlayers returned an invalid result")
        return result

    def active_player_id(self) -> int:
        players = self.active_players()
        if not players:
            raise InvalidRequest("Kodi has no active player")
        video = next((item for item in players if item.get("type") == "video"), players[0])
        player_id = video.get("playerid")
        if not isinstance(player_id, int):
            raise KodiRPCError("Kodi returned an invalid player id")
        return player_id

    def status(self) -> dict[str, Any]:
        process = find_kodi_process()
        state: dict[str, Any] = {
            "running": process is not None,
            "pid": process.pid if process else None,
            "jsonrpc_reachable": False,
            "active_players": [],
            "current_item": None,
            "speed": None,
            "time": None,
            "total_time": None,
        }
        try:
            state["jsonrpc_reachable"] = self.ping()
            players = self.active_players()
            state["active_players"] = players
            if players:
                player_id = next(
                    (item["playerid"] for item in players if item.get("type") == "video"),
                    players[0]["playerid"],
                )
                properties = self.call(
                    "Player.GetProperties",
                    {"playerid": player_id, "properties": ["speed", "time", "totaltime"]},
                )
                item = self.call(
                    "Player.GetItem",
                    {"playerid": player_id, "properties": ["title", "file"]},
                )
                state["speed"] = properties.get("speed")
                state["time"] = properties.get("time")
                state["total_time"] = properties.get("totaltime")
                state["current_item"] = item.get("item")
        except (KodiUnavailable, KodiRPCError):
            pass
        return state

    def play_pause(self) -> Any:
        return self.call("Player.PlayPause", {"playerid": self.active_player_id()})

    def stop(self) -> Any:
        return self.call("Player.Stop", {"playerid": self.active_player_id()})

    def seek(self, seconds: Any) -> Any:
        seconds_value = validate_seconds(seconds, "seconds")
        whole = int(seconds_value)
        milliseconds = int(round((seconds_value - whole) * 1000))
        if milliseconds == 1000:
            whole += 1
            milliseconds = 0
        hours, remainder = divmod(whole, 3600)
        minutes, secs = divmod(remainder, 60)
        value = {
            "hours": hours,
            "minutes": minutes,
            "seconds": secs,
            "milliseconds": milliseconds,
        }
        return self.call(
            "Player.Seek",
            {"playerid": self.active_player_id(), "value": {"time": value}},
        )

    def open(self, url: Any, resume_seconds: Any = 0) -> Any:
        safe_url = validate_media_url(url)
        resume = validate_seconds(resume_seconds, "resume_seconds")
        whole = int(resume)
        milliseconds = int(round((resume - whole) * 1000))
        if milliseconds == 1000:
            whole += 1
            milliseconds = 0
        hours, remainder = divmod(whole, 3600)
        minutes, seconds = divmod(remainder, 60)
        return self.call(
            "Player.Open",
            {
                "item": {"file": safe_url},
                "options": {
                    "resume": {
                        "hours": hours,
                        "minutes": minutes,
                        "seconds": seconds,
                        "milliseconds": milliseconds,
                    }
                },
            },
        )

    def quit(self) -> Any:
        return self.call("Application.Quit")


def validate_seconds(value: Any, field: str) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise InvalidRequest(f"{field} must be a number")
    value = float(value)
    if not math.isfinite(value) or value < 0 or value > 604800:
        raise InvalidRequest(f"{field} must be between 0 and 604800")
    return value


def validate_media_url(value: Any) -> str:
    if not isinstance(value, str) or not value:
        raise InvalidRequest("url must be a non-empty string")
    if any(character in value for character in ("\r", "\n", "\x00")):
        raise InvalidRequest("url contains forbidden control characters")
    parsed = urlsplit(value)
    if parsed.scheme not in {"http", "https", "file"}:
        raise InvalidRequest("url scheme must be http, https, or file")
    if parsed.scheme in {"http", "https"} and not parsed.netloc:
        raise InvalidRequest("http(s) url must include a host")
    if parsed.scheme == "file" and (parsed.netloc not in {"", "localhost"} or not parsed.path.startswith("/")):
        raise InvalidRequest("file url must contain an absolute local path")
    return value
