#!/usr/bin/env python3
"""Capture the MediaBox product UI exactly as a browser renders it.

Acceptance for an interface is visual, and a screenshot taken before the
catalogue has arrived proves nothing. So this drives Chromium over the
DevTools protocol: it navigates, waits for an element the screen only produces
once real data is on it, and only then captures.

Chromium is the one the appliance itself runs, and the URL is the one the
television's kiosk loads, so what comes out is the product as shipped rather
than a mock-up of it.

Only the standard library is used — including a small WebSocket client —
because the appliance has no package manager state worth disturbing for a test
tool.

    ui-screenshot.py --url http://127.0.0.1:8787/ --out home.png \
        --wait '.rail-track .card' --width 1920 --height 1080
"""

from __future__ import annotations

import argparse
import base64
import json
import os
import secrets
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
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


def capture(arguments: argparse.Namespace) -> None:
    profile = tempfile.mkdtemp(prefix="mediabox-shot-")
    command = [
        browser_binary(),
        "--headless",
        "--no-sandbox",
        "--disable-gpu",
        "--disable-background-networking",
        "--disable-component-update",
        "--disable-sync",
        "--no-first-run",
        "--no-default-browser-check",
        "--hide-scrollbars",
        f"--user-data-dir={profile}",
        f"--remote-debugging-port={arguments.port}",
        f"--window-size={arguments.width},{arguments.height}",
        "about:blank",
    ]
    browser = subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    devtools = None
    try:
        deadline = time.monotonic() + 40
        devtools = Devtools(page_target(arguments.port, deadline))
        devtools.call("Page.enable")
        devtools.call("Runtime.enable")
        # An exact device size matters: this is a 1920x1080 television, and a
        # layout captured at some other width is not the one people will see.
        devtools.call(
            "Emulation.setDeviceMetricsOverride",
            {
                "width": arguments.width,
                "height": arguments.height,
                "deviceScaleFactor": 1,
                "mobile": arguments.mobile,
            },
        )
        devtools.call("Page.navigate", {"url": arguments.url})

        if arguments.wait:
            selector = json.dumps(arguments.wait)
            deadline = time.monotonic() + arguments.wait_timeout
            while time.monotonic() < deadline:
                if devtools.evaluate(f"!!document.querySelector({selector})"):
                    break
                time.sleep(0.25)
            else:
                raise SystemExit(f"timed out waiting for {arguments.wait!r} on {arguments.url}")
        else:
            time.sleep(arguments.settle)

        for step in arguments.script or []:
            devtools.evaluate(step)
            time.sleep(arguments.settle)

        # Artwork is fetched after the markup exists; capturing before it has
        # decoded would show the layout without the thing the layout is for.
        devtools.evaluate(
            "Promise.all(Array.from(document.images)"
            ".filter(i => !i.complete)"
            ".map(i => new Promise(r => { i.onload = i.onerror = r; })))"
        )
        time.sleep(arguments.settle)

        shot = devtools.call(
            "Page.captureScreenshot", {"format": "png", "captureBeyondViewport": False}
        )
        data = base64.b64decode(shot["data"])
        os.makedirs(os.path.dirname(os.path.abspath(arguments.out)) or ".", exist_ok=True)
        with open(arguments.out, "wb") as handle:
            handle.write(data)
        print(f"{arguments.out}  {len(data)} bytes  {arguments.width}x{arguments.height}")
    finally:
        if devtools is not None:
            devtools.close()
        browser.terminate()
        try:
            browser.wait(timeout=10)
        except subprocess.TimeoutExpired:
            browser.kill()
        shutil.rmtree(profile, ignore_errors=True)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--width", type=int, default=1920)
    parser.add_argument("--height", type=int, default=1080)
    parser.add_argument("--mobile", action="store_true")
    parser.add_argument("--port", type=int, default=9333)
    parser.add_argument("--wait", default=None, help="CSS selector that means the screen is ready")
    parser.add_argument("--wait-timeout", type=float, default=90.0)
    parser.add_argument("--settle", type=float, default=1.2)
    parser.add_argument(
        "--script",
        action="append",
        help="JavaScript to run before capturing (repeatable, in order)",
    )
    capture(parser.parse_args())


if __name__ == "__main__":
    main()
