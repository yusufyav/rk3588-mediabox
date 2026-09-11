"""TOML configuration and validation."""

from __future__ import annotations

import ipaddress
import logging
import os
import tomllib
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit


@dataclass(frozen=True, slots=True)
class KodiConfig:
    endpoint: str | None = None
    settings_paths: tuple[str, ...] = (
        "/var/tmp/kodi-home/.kodi/userdata/guisettings.xml",
    )
    request_timeout_seconds: float = 2.0
    executable: str = "/opt/rk3588-mediabox/kodi/lib/kodi/kodi-gbm"
    arguments: tuple[str, ...] = ("--standalone", "--debug")
    working_directory: str = "/var/tmp/kodi-home"
    home: str = "/var/tmp/kodi-home"
    stdout_path: str = "/var/tmp/kodi-home/kodi-stdout.log"
    start_timeout_seconds: float = 20.0
    stop_timeout_seconds: float = 12.0
    environment: dict[str, str] = field(
        default_factory=lambda: {
            "AE_SINK": "ALSA",
            "MEDIABOX_GPU": "mali",
            "LD_LIBRARY_PATH": (
                "/opt/rk3588-mediabox/mali-g24p0-runtime/lib:"
                "/opt/rk3588-screenbridge/lib"
            ),
        }
    )


@dataclass(frozen=True, slots=True)
class Config:
    bind_address: str = "127.0.0.1"
    port: int = 8787
    allow_lan: bool = False
    log_level: str = "INFO"
    system_actions_enabled: bool = False
    system_actions_allow_cidrs: tuple[str, ...] = (
        "127.0.0.0/8",
        "::1/128",
    )
    event_poll_seconds: float = 2.0
    webui_root: str | None = "/opt/rk3588-mediabox/webui/dist"
    kodi: KodiConfig = field(default_factory=KodiConfig)


def _number(data: dict[str, Any], key: str, default: float) -> float:
    value = data.get(key, default)
    if isinstance(value, bool) or not isinstance(value, (int, float)) or value <= 0:
        raise ValueError(f"{key} must be a positive number")
    return float(value)


def _strings(data: dict[str, Any], key: str, default: tuple[str, ...]) -> tuple[str, ...]:
    value = data.get(key, list(default))
    if not isinstance(value, list) or not all(isinstance(item, str) for item in value):
        raise ValueError(f"{key} must be an array of strings")
    return tuple(value)


def _validate_endpoint(endpoint: str | None) -> str | None:
    if endpoint is None or endpoint == "":
        return None
    parsed = urlsplit(endpoint)
    if parsed.scheme not in {"http", "https"} or not parsed.hostname:
        raise ValueError("kodi.endpoint must be an http(s) URL")
    if parsed.path in {"", "/"}:
        endpoint = endpoint.rstrip("/") + "/jsonrpc"
    return endpoint


def load_config(path: str | os.PathLike[str] | None) -> Config:
    raw: dict[str, Any] = {}
    if path is not None:
        with Path(path).open("rb") as handle:
            raw = tomllib.load(handle)

    kodi_raw = raw.get("kodi", {})
    if not isinstance(kodi_raw, dict):
        raise ValueError("kodi must be a TOML table")
    environment = kodi_raw.get("environment", KodiConfig().environment)
    if not isinstance(environment, dict) or not all(
        isinstance(key, str) and isinstance(value, str)
        for key, value in environment.items()
    ):
        raise ValueError("kodi.environment values must be strings")

    kodi = KodiConfig(
        endpoint=_validate_endpoint(kodi_raw.get("endpoint")),
        settings_paths=_strings(kodi_raw, "settings_paths", KodiConfig().settings_paths),
        request_timeout_seconds=_number(
            kodi_raw, "request_timeout_seconds", KodiConfig().request_timeout_seconds
        ),
        executable=str(kodi_raw.get("executable", KodiConfig().executable)),
        arguments=_strings(kodi_raw, "arguments", KodiConfig().arguments),
        working_directory=str(
            kodi_raw.get("working_directory", KodiConfig().working_directory)
        ),
        home=str(kodi_raw.get("home", KodiConfig().home)),
        stdout_path=str(kodi_raw.get("stdout_path", KodiConfig().stdout_path)),
        start_timeout_seconds=_number(
            kodi_raw, "start_timeout_seconds", KodiConfig().start_timeout_seconds
        ),
        stop_timeout_seconds=_number(
            kodi_raw, "stop_timeout_seconds", KodiConfig().stop_timeout_seconds
        ),
        environment=dict(environment),
    )

    bind_address = str(raw.get("bind_address", Config().bind_address))
    try:
        bind_ip = ipaddress.ip_address(bind_address)
    except ValueError as exc:
        raise ValueError("bind_address must be a literal IPv4 or IPv6 address") from exc
    allow_lan = raw.get("allow_lan", False)
    if not isinstance(allow_lan, bool):
        raise ValueError("allow_lan must be true or false")
    if not bind_ip.is_loopback and not allow_lan:
        raise ValueError("non-loopback bind_address requires allow_lan = true")

    port = raw.get("port", Config().port)
    if isinstance(port, bool) or not isinstance(port, int) or not 1 <= port <= 65535:
        raise ValueError("port must be between 1 and 65535")
    log_level = str(raw.get("log_level", Config().log_level)).upper()
    if log_level not in logging.getLevelNamesMapping():
        raise ValueError("log_level is not valid")
    actions_enabled = raw.get("system_actions_enabled", False)
    if not isinstance(actions_enabled, bool):
        raise ValueError("system_actions_enabled must be true or false")
    cidrs = _strings(
        raw, "system_actions_allow_cidrs", Config().system_actions_allow_cidrs
    )
    for cidr in cidrs:
        ipaddress.ip_network(cidr, strict=False)
    webui_root_value = raw.get("webui_root", Config().webui_root)
    if webui_root_value is not None and not isinstance(webui_root_value, str):
        raise ValueError("webui_root must be a string or omitted")
    webui_root = webui_root_value or None

    return Config(
        bind_address=bind_address,
        port=port,
        allow_lan=allow_lan,
        log_level=log_level,
        system_actions_enabled=actions_enabled,
        system_actions_allow_cidrs=cidrs,
        event_poll_seconds=_number(raw, "event_poll_seconds", Config().event_poll_seconds),
        webui_root=webui_root,
        kodi=kodi,
    )
