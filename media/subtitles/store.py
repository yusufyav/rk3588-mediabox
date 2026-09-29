"""What the subtitle system keeps on disk, and for how long.

Three things, all under one directory in the worker's state:

    files/   subtitles as the player is given them: fetched, decoded to UTF-8,
             checked to be a subtitle, and named by the hash of what they say.
             A corrected timeline is one more file here; the original it was
             made from is never touched.
    sync/    one answer per (video, subtitle, algorithm): what the engine
             concluded, so the same film with the same subtitle is not
             listened to twice.
    speech/  what was heard in each sampled stretch of a video. It depends on
             the video alone, so trying a second subtitle for the same film
             costs no listening at all.

The directory has a ceiling and the oldest entries go first. Nothing here is
precious: every entry can be made again from the network and the film.
"""

from __future__ import annotations

import hashlib
import json
import logging
import os
import re
import tempfile
import threading
import time
from pathlib import Path
from typing import Any

from .formats import Document, parse_text
from .sync import Window


LOG = logging.getLogger(__name__)

DEFAULT_CEILING_BYTES = 64 * 1024 * 1024

_KEY = re.compile(r"^[0-9a-f]{32}$")
EXTENSIONS = ("srt", "vtt", "ass", "ssa")


def digest(*parts: str) -> str:
    return hashlib.sha256("\x1f".join(parts).encode("utf-8")).hexdigest()[:32]


def valid_key(key: str) -> bool:
    return bool(_KEY.match(key or ""))


class SubtitleStore:
    def __init__(self, root: str | os.PathLike[str] | None = None, *, ceiling: int = DEFAULT_CEILING_BYTES) -> None:
        self.root = Path(root) if root else Path(tempfile.mkdtemp(prefix="mediabox-subtitles-"))
        self.ceiling = ceiling
        self._lock = threading.Lock()
        self._documents: dict[str, Document] = {}
        for name in ("files", "sync", "speech"):
            (self.root / name).mkdir(parents=True, exist_ok=True, mode=0o750)

    # ------------------------------------------------------------ subtitles

    def put(self, document: Document) -> str:
        """Keep a subtitle; its key is the hash of its text."""
        text = document.text()
        key = digest("subtitle", text)
        path = self._file(key, document.extension)
        if not path.exists():
            self._write(path, text.encode("utf-8"))
        with self._lock:
            self._documents[key] = document
            if len(self._documents) > 32:
                self._documents.pop(next(iter(self._documents)))
        return key

    def put_as(self, key: str, document: Document) -> str:
        """Keep a subtitle under a key the caller derived (a corrected one)."""
        self._write(self._file(key, document.extension), document.text().encode("utf-8"))
        with self._lock:
            self._documents[key] = document
        return key

    def get(self, key: str) -> Document | None:
        if not valid_key(key):
            return None
        with self._lock:
            found = self._documents.get(key)
        if found is not None:
            return found
        path = self.path(key)
        if path is None:
            return None
        try:
            document = parse_text(path.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            return None
        with self._lock:
            self._documents[key] = document
        return document

    def path(self, key: str) -> Path | None:
        if not valid_key(key):
            return None
        for extension in EXTENSIONS:
            candidate = self._file(key, extension)
            if candidate.exists():
                return candidate
        return None

    def read(self, key: str) -> tuple[bytes, str] | None:
        """The bytes of a kept subtitle and its format, for serving."""
        path = self.path(key)
        if path is None:
            return None
        try:
            os.utime(path)
            return path.read_bytes(), path.suffix[1:]
        except OSError:
            return None

    # --------------------------------------------------------- sync answers

    def result(self, video: str, subtitle: str, algorithm: str) -> dict[str, Any] | None:
        return self._load_json(self.root / "sync" / f"{digest(video, subtitle, algorithm)}.json")

    def keep_result(self, video: str, subtitle: str, algorithm: str, answer: dict[str, Any]) -> None:
        self._save_json(self.root / "sync" / f"{digest(video, subtitle, algorithm)}.json", answer)

    # --------------------------------------------------------------- speech

    def _speech_path(self, video: str, evidence: str) -> Path:
        """`evidence` names how it was heard; "" is the downmix, as it always was."""
        parts = ("speech", video, evidence) if evidence else ("speech", video)
        return self.root / "speech" / f"{digest(*parts)}.json"

    def speech(self, video: str, evidence: str = "") -> list[Window]:
        payload = self._load_json(self._speech_path(video, evidence))
        if not isinstance(payload, dict):
            return []
        windows = []
        for entry in payload.get("windows", []):
            try:
                windows.append(
                    Window(
                        float(entry["start"]),
                        float(entry["end"]),
                        tuple((float(a), float(b)) for a, b in entry["speech"]),
                    )
                )
            except (KeyError, TypeError, ValueError):
                continue
        return windows

    def keep_speech(self, video: str, windows: list[Window], evidence: str = "") -> None:
        self._save_json(
            self._speech_path(video, evidence),
            {
                "video": video,
                "windows": [
                    {
                        "start": round(w.start, 3),
                        "end": round(w.end, 3),
                        "speech": [[round(a, 3), round(b, 3)] for a, b in w.speech],
                    }
                    for w in windows
                ],
            },
        )

    # ------------------------------------------------------------- plumbing

    def _file(self, key: str, extension: str) -> Path:
        return self.root / "files" / f"{key}.{extension}"

    def _write(self, path: Path, data: bytes) -> None:
        # One temporary name per writer: two syncs finishing together must
        # not rename each other's half-written file.
        temporary = path.with_name(f"{path.name}.{os.getpid()}.{threading.get_ident()}.part")
        try:
            temporary.write_bytes(data)
            os.replace(temporary, path)
        except FileNotFoundError:
            # The directory went away under a sync that was being cancelled.
            return
        self._prune()

    def _load_json(self, path: Path) -> Any:
        try:
            payload = json.loads(path.read_text(encoding="utf-8"))
            os.utime(path)
            return payload
        except (OSError, ValueError):
            return None

    def _save_json(self, path: Path, payload: Any) -> None:
        self._write(path, json.dumps(payload, ensure_ascii=False).encode("utf-8"))

    def _prune(self) -> None:
        """Oldest first, until the directory is under its ceiling."""
        entries = []
        total = 0
        for folder in ("files", "sync", "speech"):
            for path in (self.root / folder).iterdir():
                try:
                    stat = path.stat()
                except OSError:
                    continue
                entries.append((stat.st_mtime, stat.st_size, path))
                total += stat.st_size
        if total <= self.ceiling:
            return
        entries.sort()
        now = time.time()
        for mtime, size, path in entries:
            if total <= self.ceiling * 0.8:
                break
            # Anything touched in the last minute is in use.
            if now - mtime < 60:
                continue
            try:
                path.unlink()
                total -= size
            except OSError:
                pass
