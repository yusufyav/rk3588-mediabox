"""API-visible error types."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any


@dataclass(slots=True)
class APIError(Exception):
    code: str
    message: str
    status: int = 500
    details: Any | None = None

    def as_dict(self) -> dict[str, Any]:
        error: dict[str, Any] = {"code": self.code, "message": self.message}
        if self.details is not None:
            error["details"] = self.details
        return {"error": error}


class InvalidRequest(APIError):
    def __init__(self, message: str, details: Any | None = None) -> None:
        super().__init__("INVALID_REQUEST", message, 400, details)


class KodiUnavailable(APIError):
    def __init__(self, message: str = "Kodi JSON-RPC endpoint is unreachable") -> None:
        super().__init__("KODI_UNREACHABLE", message, 503)


class KodiRPCError(APIError):
    def __init__(self, message: str, details: Any | None = None) -> None:
        super().__init__("KODI_RPC_ERROR", message, 502, details)
