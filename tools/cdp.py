#!/usr/bin/env python3
"""A DevTools client small enough to carry, for the tools that drive Chromium.

Only the standard library is used — including the WebSocket client below —
because the appliance has no package manager state worth disturbing for a test
tool, and the same file has to run on the developer's machine and on the box.

Two consumers: `ui-screenshot.py`, which launches a headless browser to capture
a screen, and `ui-perf.py`, which attaches to the television's own kiosk
browser to measure it.
"""

from __future__ import annotations

import base64
import json
import secrets
import shutil
import socket
import struct
import time
import urllib.error
import urllib.request
from urllib.parse import urlsplit


# --------------------------------------------------------------- websocket

class WebSocket:
    """Enough of RFC 6455 to speak to Chromium: text frames, client side."""

    def __init__(self, url: str, timeout: float = 30.0) -> None:
        parts = urlsplit(url)
        host = parts.hostname or "127.0.0.1"
        port = parts.port or 80
        path = parts.path or "/"
        if parts.query:
            path = f"{path}?{parts.query}"
        self._socket = socket.create_connection((host, port), timeout=timeout)
        self._socket.settimeout(timeout)
        self._buffer = b""
        key = base64.b64encode(secrets.token_bytes(16)).decode()
        request = (
            f"GET {path} HTTP/1.1\r\n"
            f"Host: {host}:{port}\r\n"
            "Upgrade: websocket\r\n"
            "Connection: Upgrade\r\n"
            f"Sec-WebSocket-Key: {key}\r\n"
            "Sec-WebSocket-Version: 13\r\n\r\n"
        )
        self._socket.sendall(request.encode())
        while b"\r\n\r\n" not in self._buffer:
            chunk = self._socket.recv(4096)
            if not chunk:
                raise ConnectionError("the browser closed the upgrade")
            self._buffer += chunk
        head, _, rest = self._buffer.partition(b"\r\n\r\n")
        if b"101" not in head.split(b"\r\n")[0]:
            raise ConnectionError(f"upgrade refused: {head.splitlines()[0]!r}")
        self._buffer = rest

    def _recv_exact(self, count: int) -> bytes:
        while len(self._buffer) < count:
            chunk = self._socket.recv(65536)
            if not chunk:
                raise ConnectionError("the browser closed the connection")
            self._buffer += chunk
        taken, self._buffer = self._buffer[:count], self._buffer[count:]
        return taken

    def send(self, text: str) -> None:
        payload = text.encode()
        header = bytearray([0x81])
        length = len(payload)
        if length < 126:
            header.append(0x80 | length)
        elif length < 65536:
            header.append(0x80 | 126)
            header += struct.pack(">H", length)
        else:
            header.append(0x80 | 127)
            header += struct.pack(">Q", length)
        mask = secrets.token_bytes(4)
        header += mask
        masked = bytes(byte ^ mask[index % 4] for index, byte in enumerate(payload))
        self._socket.sendall(bytes(header) + masked)

    def recv(self) -> str:
        """One complete text message, reassembled across continuation frames."""
        chunks: list[bytes] = []
        while True:
            first, second = self._recv_exact(2)
            final = bool(first & 0x80)
            opcode = first & 0x0F
            length = second & 0x7F
            if length == 126:
                length = struct.unpack(">H", self._recv_exact(2))[0]
            elif length == 127:
                length = struct.unpack(">Q", self._recv_exact(8))[0]
            payload = self._recv_exact(length) if length else b""
            if opcode == 0x8:
                raise ConnectionError("the browser closed the connection")
            if opcode == 0x9:  # ping
                self._socket.sendall(b"\x8a\x80" + secrets.token_bytes(4))
                continue
            if opcode == 0xA:  # pong
                continue
            chunks.append(payload)
            if final:
                return b"".join(chunks).decode("utf-8", "replace")

    def close(self) -> None:
        try:
            self._socket.close()
        except OSError:
            pass


# --------------------------------------------------------------------- CDP

class Devtools:
    def __init__(self, websocket_url: str) -> None:
        self._socket = WebSocket(websocket_url)
        self._id = 0

    def call(self, method: str, params: dict | None = None, timeout: float = 60.0) -> dict:
        self._id += 1
        message_id = self._id
        self._socket.send(json.dumps({"id": message_id, "method": method, "params": params or {}}))
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            message = json.loads(self._socket.recv())
            if message.get("id") != message_id:
                continue  # an event, or an older reply
            if "error" in message:
                raise RuntimeError(f"{method}: {message['error'].get('message')}")
            return message.get("result", {})
        raise TimeoutError(f"{method} did not answer in {timeout}s")

    def evaluate(self, expression: str) -> object:
        result = self.call(
            "Runtime.evaluate",
            {"expression": expression, "returnByValue": True, "awaitPromise": True},
        )
        return result.get("result", {}).get("value")

    def close(self) -> None:
        self._socket.close()


# ----------------------------------------------------------------- browser

def browser_binary() -> str:
    for name in ("chromium", "chromium-browser", "google-chrome", "chrome"):
        found = shutil.which(name)
        if found:
            return found
    raise SystemExit("no Chromium binary found")


def page_target(port: int, deadline: float) -> str:
    """The debugging endpoint of the first page, once the browser is up."""
    while time.monotonic() < deadline:
        try:
            with urllib.request.urlopen(f"http://127.0.0.1:{port}/json/list", timeout=2) as body:
                targets = json.load(body)
            for target in targets:
                if target.get("type") == "page" and target.get("webSocketDebuggerUrl"):
                    return target["webSocketDebuggerUrl"]
        except (urllib.error.URLError, OSError, json.JSONDecodeError):
            pass
        time.sleep(0.3)
    raise SystemExit("the browser never exposed a debuggable page")
