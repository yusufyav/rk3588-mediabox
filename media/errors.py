"""Errors the media core reports across its HTTP surface."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any


@dataclass(slots=True)
class MediaError(Exception):
    """An error with a stable machine-readable code."""

    code: str
    message: str
    status: int = 500
    details: Any | None = None

    def as_dict(self) -> dict[str, Any]:
        error: dict[str, Any] = {"code": self.code, "message": self.message}
        if self.details is not None:
            error["details"] = self.details
        return {"error": error}


class InvalidRequest(MediaError):
    def __init__(self, message: str, details: Any | None = None) -> None:
        super().__init__("INVALID_REQUEST", message, 400, details)


class NotFound(MediaError):
    def __init__(self, message: str = "Not found") -> None:
        super().__init__("NOT_FOUND", message, 404)


class ProbeError(MediaError):
    """ffprobe could not produce usable information about a source."""

    def __init__(self, message: str, details: Any | None = None) -> None:
        super().__init__("PROBE_FAILED", message, 502, details)


class ProbeTimeout(MediaError):
    def __init__(self, message: str = "ffprobe exceeded its time budget") -> None:
        super().__init__("PROBE_TIMEOUT", message, 504)


class UpstreamError(MediaError):
    """A provider (Stremio API, an addon, the streaming server) failed."""

    def __init__(self, message: str, details: Any | None = None) -> None:
        super().__init__("UPSTREAM_FAILED", message, 502, details)


class SessionError(MediaError):
    def __init__(self, code: str, message: str, status: int = 409) -> None:
        super().__init__(code, message, status)
