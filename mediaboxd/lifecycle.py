"""Kodi and host lifecycle operations using fixed argv vectors."""

from __future__ import annotations

import ipaddress
import re
import subprocess
import threading
import time
from typing import Any

from .config import Config, KodiConfig
from .errors import APIError
from .processes import find_kodi_process


class KodiLifecycle:
    def __init__(self, config: KodiConfig, kodi: Any) -> None:
        self.config = config
        self.kodi = kodi
        self._lock = threading.RLock()

    def _unit(self) -> str:
        """The systemd unit that owns Kodi, validated as a plain unit name."""
        unit = self.config.unit
        if not re.fullmatch(r"[A-Za-z0-9@._-]{1,64}\.service", unit):
            raise APIError("LIFECYCLE_ERROR", "Kodi unit name is not valid", 500)
        return unit

    def _systemctl(self, verb: str) -> None:
        """Drive the unit with a fixed argv. No shell, no caller-supplied words.

        mediaboxd does not launch Kodi itself. A process spawned from inside
        this daemon inherits this daemon's sandbox — including its address
        family filter, which leaves Kodi unable to open the udev netlink
        monitor and therefore with no keyboard at all. Kodi has to start in its
        own unit, identically at boot and on every restart, so the display and
        input context never depends on who asked for the restart.
        """
        argv = ["/usr/bin/systemctl", verb, self._unit()]
        try:
            result = subprocess.run(
                argv,
                stdin=subprocess.DEVNULL,
                capture_output=True,
                timeout=self.config.start_timeout_seconds + 10,
                check=False,
            )
        except (OSError, subprocess.TimeoutExpired) as exc:
            raise APIError("LIFECYCLE_ERROR", f"Kodi unit could not be {verb}ed", 503) from exc
        if result.returncode != 0:
            raise APIError("LIFECYCLE_ERROR", f"Kodi unit could not be {verb}ed", 503)

    def _wait_ready(self) -> dict[str, Any]:
        deadline = time.monotonic() + self.config.start_timeout_seconds
        while time.monotonic() < deadline:
            process = find_kodi_process()
            if process:
                try:
                    if self.kodi.ping():
                        return {"changed": True, "running": True, "pid": process.pid}
                except APIError:
                    pass
            time.sleep(0.2)
        raise APIError(
            "LIFECYCLE_ERROR", "Kodi did not become JSON-RPC ready before timeout", 504
        )

    def start(self) -> dict[str, Any]:
        with self._lock:
            existing = find_kodi_process()
            if existing:
                return {"changed": False, "running": True, "pid": existing.pid}
            self._systemctl("start")
            return self._wait_ready()

    def stop(self) -> dict[str, Any]:
        with self._lock:
            if find_kodi_process() is None:
                return {"changed": False, "running": False, "pid": None}
            self._systemctl("stop")
            deadline = time.monotonic() + self.config.stop_timeout_seconds
            while time.monotonic() < deadline:
                if find_kodi_process() is None:
                    return {"changed": True, "running": False, "pid": None}
                time.sleep(0.2)
            raise APIError("LIFECYCLE_ERROR", "Kodi process did not stop", 504)

    def restart(self) -> dict[str, Any]:
        with self._lock:
            self._systemctl("restart")
            return self._wait_ready()


class SystemActions:
    def __init__(self, config: Config) -> None:
        self.enabled = config.system_actions_enabled
        self.networks = tuple(
            ipaddress.ip_network(item, strict=False)
            for item in config.system_actions_allow_cidrs
        )

    def run(self, action: str, client_address: str) -> dict[str, Any]:
        if not self.enabled:
            raise APIError("SYSTEM_ACTION_DISABLED", "System actions are disabled", 403)
        try:
            address = ipaddress.ip_address(client_address)
        except ValueError as exc:
            raise APIError("FORBIDDEN", "Client address is not permitted", 403) from exc
        if not any(address in network for network in self.networks):
            raise APIError("FORBIDDEN", "Client address is not permitted", 403)
        argv = {
            "reboot": ["/usr/bin/systemctl", "reboot"],
            "shutdown": ["/usr/bin/systemctl", "poweroff"],
        }.get(action)
        if argv is None:
            raise APIError("INVALID_REQUEST", "Unknown system action", 400)
        try:
            subprocess.Popen(argv, stdin=subprocess.DEVNULL, close_fds=True)
        except OSError as exc:
            raise APIError("SYSTEM_ACTION_ERROR", "System action could not be requested", 503) from exc
        return {"accepted": True, "action": action}
