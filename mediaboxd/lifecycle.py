"""Kodi and host lifecycle operations using fixed argv vectors."""

from __future__ import annotations

import ipaddress
import os
import signal
import subprocess
import threading
import time
from pathlib import Path
from typing import Any

from .config import Config, KodiConfig
from .errors import APIError
from .processes import ProcessIdentity, find_kodi_process, same_process


class KodiLifecycle:
    def __init__(self, config: KodiConfig, kodi: Any) -> None:
        self.config = config
        self.kodi = kodi
        self._lock = threading.RLock()

    def _validate_paths(self) -> None:
        for name, value in (
            ("executable", self.config.executable),
            ("working_directory", self.config.working_directory),
            ("home", self.config.home),
            ("stdout_path", self.config.stdout_path),
        ):
            if not Path(value).is_absolute():
                raise APIError("LIFECYCLE_ERROR", f"Kodi {name} must be an absolute path", 500)

    def start(self) -> dict[str, Any]:
        with self._lock:
            existing = find_kodi_process()
            if existing:
                return {"changed": False, "running": True, "pid": existing.pid}
            self._validate_paths()
            executable = Path(self.config.executable)
            if not executable.is_file() or not os.access(executable, os.X_OK):
                raise APIError("LIFECYCLE_ERROR", "Configured Kodi executable is unavailable", 503)
            working_directory = Path(self.config.working_directory)
            if not working_directory.is_dir():
                raise APIError("LIFECYCLE_ERROR", "Configured Kodi working directory is unavailable", 503)
            stdout_path = Path(self.config.stdout_path)
            stdout_path.parent.mkdir(parents=True, exist_ok=True)
            environment = {
                "PATH": "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
                "HOME": self.config.home,
                "LANG": os.environ.get("LANG", "C.UTF-8"),
                **self.config.environment,
            }
            argv = [self.config.executable, *self.config.arguments]
            try:
                with stdout_path.open("ab", buffering=0) as output:
                    child = subprocess.Popen(
                        argv,
                        cwd=self.config.working_directory,
                        env=environment,
                        stdin=subprocess.DEVNULL,
                        stdout=output,
                        stderr=subprocess.STDOUT,
                        start_new_session=True,
                        close_fds=True,
                    )
                # Reap a normally exiting child without coupling Kodi's
                # lifecycle to the daemon request thread.
                threading.Thread(target=child.wait, name="kodi-reaper", daemon=True).start()
            except OSError as exc:
                raise APIError("LIFECYCLE_ERROR", "Kodi process could not be started", 503) from exc
            deadline = time.monotonic() + self.config.start_timeout_seconds
            while time.monotonic() < deadline:
                process = find_kodi_process()
                if process:
                    try:
                        if self.kodi.ping():
                            return {"changed": True, "running": True, "pid": process.pid}
                    except APIError:
                        pass
                time.sleep(0.1)
            raise APIError(
                "LIFECYCLE_ERROR", "Kodi did not become JSON-RPC ready before timeout", 504
            )

    def stop(self) -> dict[str, Any]:
        with self._lock:
            process = find_kodi_process()
            if process is None:
                return {"changed": False, "running": False, "pid": None}
            try:
                self.kodi.quit()
            except APIError:
                pass
            if self._wait_gone(process, self.config.stop_timeout_seconds):
                return {"changed": True, "running": False, "pid": None}
            self._signal(process, signal.SIGTERM)
            if self._wait_gone(process, 3.0):
                return {"changed": True, "running": False, "pid": None}
            self._signal(process, signal.SIGKILL)
            if self._wait_gone(process, 2.0):
                return {"changed": True, "running": False, "pid": None}
            raise APIError("LIFECYCLE_ERROR", "Kodi process did not stop", 504)

    def restart(self) -> dict[str, Any]:
        self.stop()
        return self.start()

    @staticmethod
    def _wait_gone(process: ProcessIdentity, timeout: float) -> bool:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if not same_process(process):
                return True
            time.sleep(0.1)
        return not same_process(process)

    @staticmethod
    def _signal(process: ProcessIdentity, signum: signal.Signals) -> None:
        if same_process(process):
            try:
                os.kill(process.pid, signum)
            except ProcessLookupError:
                pass


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
