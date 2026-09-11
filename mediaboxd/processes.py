"""Small procfs helpers; no process-list shell parsing."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True, slots=True)
class ProcessIdentity:
    pid: int
    start_ticks: int
    command: str


def _identity(pid_dir: Path) -> ProcessIdentity | None:
    try:
        command = (pid_dir / "comm").read_text(encoding="utf-8").strip()
        stat = (pid_dir / "stat").read_text(encoding="utf-8")
        # The command may contain spaces/parentheses. Everything after the last
        # ')' begins at stat field 3; starttime is field 22, therefore index 19.
        after_comm = stat.rsplit(")", 1)[1].strip().split()
        return ProcessIdentity(int(pid_dir.name), int(after_comm[19]), command)
    except (OSError, ValueError, IndexError):
        return None


def find_kodi_process(proc_root: Path = Path("/proc")) -> ProcessIdentity | None:
    candidates: list[ProcessIdentity] = []
    try:
        pid_dirs = proc_root.iterdir()
    except OSError:
        return None
    for pid_dir in pid_dirs:
        if not pid_dir.name.isdigit():
            continue
        identity = _identity(pid_dir)
        if identity is not None and identity.command in {"kodi-gbm", "kodi.bin"}:
            candidates.append(identity)
    return max(candidates, key=lambda item: item.pid, default=None)


def same_process(identity: ProcessIdentity, proc_root: Path = Path("/proc")) -> bool:
    current = _identity(proc_root / str(identity.pid))
    return current is not None and current.start_ticks == identity.start_ticks


def kodi_runtime_settings_paths(proc_root: Path = Path("/proc")) -> list[Path]:
    identity = find_kodi_process(proc_root)
    if identity is None:
        return []
    try:
        environ = (proc_root / str(identity.pid) / "environ").read_bytes()
    except OSError:
        return []
    for entry in environ.split(b"\0"):
        if entry.startswith(b"HOME="):
            home = entry[5:].decode("utf-8", "surrogateescape")
            if home.startswith("/"):
                return [Path(home) / ".kodi/userdata/guisettings.xml"]
    return []
