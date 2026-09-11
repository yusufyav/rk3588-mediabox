"""Bounded, read-only Linux telemetry collectors."""

from __future__ import annotations

import json
import os
import platform
import re
import shutil
import socket
import subprocess
from pathlib import Path
from typing import Any, Callable


Runner = Callable[..., subprocess.CompletedProcess[str]]


def _read(path: Path, limit: int = 1024 * 1024) -> str | None:
    try:
        with path.open("r", encoding="utf-8", errors="replace") as handle:
            return handle.read(limit)
    except OSError:
        return None


def _bytes(total_kib: int) -> int:
    return total_kib * 1024


class Telemetry:
    def __init__(
        self,
        proc_root: Path = Path("/proc"),
        sys_root: Path = Path("/sys"),
        root_path: Path = Path("/"),
        runner: Runner = subprocess.run,
    ) -> None:
        self.proc_root = proc_root
        self.sys_root = sys_root
        self.root_path = root_path
        self.runner = runner

    def system(self) -> dict[str, Any]:
        uptime_text = _read(self.proc_root / "uptime") or "0"
        load_text = _read(self.proc_root / "loadavg") or "0 0 0"
        try:
            uptime = float(uptime_text.split()[0])
        except (ValueError, IndexError):
            uptime = 0.0
        try:
            load = [float(item) for item in load_text.split()[:3]]
        except ValueError:
            load = [0.0, 0.0, 0.0]

        memory_values: dict[str, int] = {}
        for line in (_read(self.proc_root / "meminfo") or "").splitlines():
            match = re.fullmatch(r"([A-Za-z_()]+):\s+(\d+)\s+kB", line)
            if match:
                memory_values[match.group(1)] = int(match.group(2))
        total_memory = _bytes(memory_values.get("MemTotal", 0))
        available_memory = _bytes(
            memory_values.get(
                "MemAvailable",
                memory_values.get("MemFree", 0)
                + memory_values.get("Buffers", 0)
                + memory_values.get("Cached", 0),
            )
        )
        try:
            filesystem = os.statvfs(self.root_path)
            filesystem_total = filesystem.f_frsize * filesystem.f_blocks
            filesystem_available = filesystem.f_frsize * filesystem.f_bavail
            filesystem_used = filesystem_total - filesystem.f_frsize * filesystem.f_bfree
        except OSError:
            filesystem_total = filesystem_available = filesystem_used = 0

        return {
            "hostname": socket.gethostname(),
            "kernel": platform.release(),
            "architecture": platform.machine(),
            "uptime_seconds": uptime,
            "load": {"1m": load[0], "5m": load[1], "15m": load[2]},
            "memory": {
                "total_bytes": total_memory,
                "used_bytes": max(0, total_memory - available_memory),
                "available_bytes": available_memory,
            },
            "root_filesystem": {
                "total_bytes": filesystem_total,
                "used_bytes": filesystem_used,
                "available_bytes": filesystem_available,
            },
            "cpu_temperature_celsius": self._cpu_temperature(),
        }

    def _cpu_temperature(self) -> float | None:
        thermal_root = self.sys_root / "class/thermal"
        candidates: list[tuple[int, float]] = []
        try:
            zones = list(thermal_root.glob("thermal_zone*"))
        except OSError:
            return None
        for zone in zones:
            type_name = (_read(zone / "type") or "").strip().lower()
            value_text = (_read(zone / "temp") or "").strip()
            try:
                value = float(value_text)
            except ValueError:
                continue
            if abs(value) > 1000:
                value /= 1000.0
            priority = 0 if any(name in type_name for name in ("soc", "cpu", "package")) else 1
            candidates.append((priority, value))
        return min(candidates, default=(0, None))[1]

    def _run_json(self, argv: list[str]) -> list[dict[str, Any]]:
        try:
            result = self.runner(
                argv,
                check=True,
                text=True,
                capture_output=True,
                timeout=2,
            )
            document = json.loads(result.stdout)
            return document if isinstance(document, list) else []
        except (OSError, subprocess.SubprocessError, json.JSONDecodeError):
            return []

    def network(self) -> dict[str, Any]:
        addresses = self._run_json(["/usr/sbin/ip", "-j", "address", "show"])
        if not addresses:
            addresses = self._run_json(["/usr/bin/ip", "-j", "address", "show"])
        routes = self._run_json(["/usr/sbin/ip", "-j", "route", "show", "default"])
        if not routes:
            routes = self._run_json(["/usr/bin/ip", "-j", "route", "show", "default"])
        by_name = {
            item.get("ifname"): item for item in addresses if isinstance(item.get("ifname"), str)
        }
        network_root = self.sys_root / "class/net"
        names = set(by_name)
        try:
            names.update(path.name for path in network_root.iterdir())
        except OSError:
            pass
        interfaces: list[dict[str, Any]] = []
        wifi_ssid: str | None = None
        for name in sorted(names):
            document = by_name.get(name, {})
            ipv4 = [
                info["local"]
                for info in document.get("addr_info", [])
                if info.get("family") == "inet" and isinstance(info.get("local"), str)
            ]
            operstate = (_read(network_root / name / "operstate") or "unknown").strip()
            is_wireless = (network_root / name / "wireless").exists()
            if is_wireless and wifi_ssid is None:
                wifi_ssid = self._wifi_ssid(name)
            interfaces.append(
                {
                    "name": name,
                    "link_state": operstate,
                    "ipv4": ipv4,
                    "wireless": is_wireless,
                }
            )
        default_route = None
        if routes:
            route = routes[0]
            default_route = {
                "interface": route.get("dev"),
                "gateway": route.get("gateway"),
                "source": route.get("prefsrc"),
                "metric": route.get("metric"),
            }
        return {
            "interfaces": interfaces,
            "default_route": default_route,
            "active_wifi_ssid": wifi_ssid,
        }

    def _wifi_ssid(self, interface: str) -> str | None:
        executable = shutil.which("iwgetid")
        if executable is None:
            return None
        try:
            result = self.runner(
                [executable, interface, "--raw"],
                check=True,
                text=True,
                capture_output=True,
                timeout=2,
            )
            return result.stdout.strip() or None
        except (OSError, subprocess.SubprocessError):
            return None

    def display(self) -> dict[str, Any]:
        drm_root = self.sys_root / "class/drm"
        connectors = sorted(drm_root.glob("card*-HDMI-A-*"))
        connector = next(
            (path for path in connectors if (_read(path / "status") or "").strip() == "connected"),
            connectors[0] if connectors else None,
        )
        if connector is None:
            return {
                "drm_card": None,
                "connector": None,
                "connected": False,
                "mode": None,
                "refresh_hz": None,
                "colour": None,
                "depth_bits": None,
                "hdr": None,
            }
        card_name = connector.name.split("-HDMI-A-", 1)[0]
        details = self._debugfs_display(card_name)
        modes = (_read(connector / "modes") or "").splitlines()
        mode = details.get("mode") or (modes[0] if modes else None)
        refresh = details.get("refresh_hz")
        if refresh is None and mode:
            match = re.search(r"(?:p|i)(\d+(?:\.\d+)?)$", mode)
            refresh = float(match.group(1)) if match else None
        return {
            "drm_card": f"/dev/dri/{card_name}",
            "connector": connector.name,
            "connected": (_read(connector / "status") or "").strip() == "connected",
            "mode": mode,
            "refresh_hz": refresh,
            "colour": details.get("colour"),
            "depth_bits": details.get("depth_bits"),
            "hdr": details.get("hdr"),
        }

    def _debugfs_display(self, card_name: str) -> dict[str, Any]:
        card_index = card_name.removeprefix("card")
        state = _read(self.sys_root / "kernel/debug/dri" / card_index / "state") or ""
        summary = _read(self.sys_root / "kernel/debug/dri" / card_index / "summary") or ""
        text = state + "\n" + summary
        details: dict[str, Any] = {}
        mode_match = re.search(r"Display mode:\s*([^\s]+)", text)
        if mode_match:
            details["mode"] = mode_match.group(1)
            refresh_match = re.search(r"(?:p|i)(\d+(?:\.\d+)?)$", mode_match.group(1))
            if refresh_match:
                details["refresh_hz"] = float(refresh_match.group(1))
        colour_parts = []
        for label in ("bus_format", "output_mode", "color-encoding", "color-range"):
            match = re.search(rf"{re.escape(label)}(?:\[[^]]*\])?[:=]\s*([^\s]+)", text, re.I)
            if match:
                colour_parts.append(f"{label}={match.group(1)}")
        details["colour"] = ", ".join(colour_parts) or None
        depth_match = re.search(r"(?:color_depth|depth)(?:\[[^]]*\])?[:=]\s*(\d+)(?:bit)?", text, re.I)
        if depth_match:
            value = int(depth_match.group(1))
            details["depth_bits"] = 24 if value == 8 else 30 if value == 10 else value
        hdr_match = re.search(r"(?:HDR_OUTPUT_METADATA|EOTF|HDR10|SDR2HDR)[^\n]*", text, re.I)
        details["hdr"] = hdr_match.group(0).strip() if hdr_match else None
        return details
