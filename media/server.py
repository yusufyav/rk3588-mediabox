"""A standalone HTTP server for the media core.

Its reason to exist is acceptance: the media core must be testable on the
appliance without touching the production control plane, so this runs it on its
own port out of a staging directory. `mediaboxd` embeds :class:`MediaCore`
directly and does not use this module.
"""

from __future__ import annotations

import argparse
import ipaddress
import logging
import os
import signal
import socket
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any
from urllib.parse import urlsplit

from .api import MAX_BODY_BYTES, MediaCore, MediaCoreConfig


LOG = logging.getLogger("media.server")


class MediaHTTPServer(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True


def handler_factory(core: MediaCore) -> type[BaseHTTPRequestHandler]:
    class Handler(BaseHTTPRequestHandler):
        server_version = "mediabox-media-core/2.0"
        sys_version = ""
        protocol_version = "HTTP/1.1"

        def log_message(self, message: str, *args: Any) -> None:
            LOG.info("%s %s", self.address_string(), message % args)

        def _dispatch(self, method: str) -> None:
            split = urlsplit(self.path)
            body = None
            length = self.headers.get("Content-Length")
            if length is not None:
                try:
                    size = int(length)
                except ValueError:
                    size = 0
                if size > MAX_BODY_BYTES:
                    self._send(413, [("Content-Type", "application/json")], b'{"error":{"code":"BODY_TOO_LARGE"}}')
                    return
                body = self.rfile.read(max(0, size))
            response = core.handle(method, split.path, split.query, body)
            if response.stream is None:
                self._send(response.status, response.headers, response.body or b"")
                return
            self._send_stream(response.status, response.headers, response.stream)

        def do_GET(self) -> None:  # noqa: N802
            self._dispatch("GET")

        def do_POST(self) -> None:  # noqa: N802
            self._dispatch("POST")

        def do_DELETE(self) -> None:  # noqa: N802
            self._dispatch("DELETE")

        def _send(self, status: int, headers: list[tuple[str, str]], body: bytes) -> None:
            self.send_response(status)
            for name, value in headers:
                self.send_header(name, value)
            if not any(name.lower() == "content-length" for name, _ in headers):
                self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            if self.command != "HEAD":
                self.wfile.write(body)

        def _send_stream(self, status: int, headers: list[tuple[str, str]], stream) -> None:
            self.send_response(status)
            for name, value in headers:
                self.send_header(name, value)
            self.end_headers()
            try:
                for block in stream:
                    self.wfile.write(block)
            except (BrokenPipeError, ConnectionResetError):
                # The client went away. The session's generator closes in its
                # own `finally`, which is what detaches and lets the reaper end
                # the child; nothing is leaked by returning here.
                LOG.info("client disconnected from a media session")
            finally:
                close = getattr(stream, "close", None)
                if close is not None:
                    close()

    return Handler


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="MediaBox media core (standalone)")
    parser.add_argument("--bind", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=8790)
    parser.add_argument("--streaming-server", default="http://127.0.0.1:11470")
    parser.add_argument("--profile", default=None, help="capability profile name")
    parser.add_argument("--state", default=None, help="path for the Stremio session file")
    parser.add_argument("--base-url", default="", help="public base URL for session links")
    parser.add_argument("--loopback-base-url", default="", help="loopback base URL for Kodi")
    parser.add_argument(
        "--allow-file-prefix",
        action="append",
        default=[],
        help="absolute directory a file:// source may live under (repeatable)",
    )
    parser.add_argument(
        "--library",
        default=None,
        help="path to the appliance library manifest (JSON)",
    )
    parser.add_argument("--log-level", default="INFO")
    parser.add_argument("--idle-timeout", type=float, default=45.0)
    parser.add_argument(
        "--torrent-network-status",
        default=os.environ.get("MEDIABOX_TORRENT_NETWORK_STATUS", "UNKNOWN"),
    )
    arguments = parser.parse_args(argv)

    try:
        bind_address = ipaddress.ip_address(arguments.bind)
    except ValueError:
        parser.error("--bind must be a literal loopback address")
    if not bind_address.is_loopback:
        parser.error("media worker is local-only; --bind must be loopback")
    if arguments.idle_timeout <= 0:
        parser.error("--idle-timeout must be positive")

    logging.basicConfig(
        level=getattr(logging, arguments.log_level.upper(), logging.INFO),
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
    )

    base_url = arguments.base_url or f"http://{arguments.bind}:{arguments.port}"
    core = MediaCore(
        MediaCoreConfig(
            streaming_server_url=arguments.streaming_server,
            capability_profile=arguments.profile,
            session_state_path=arguments.state,
            base_url=base_url,
            loopback_base_url=arguments.loopback_base_url,
            allowed_file_prefixes=tuple(arguments.allow_file_prefix),
            idle_timeout_seconds=arguments.idle_timeout,
            torrent_network_status=arguments.torrent_network_status,
            library_path=arguments.library,
        )
    )

    server_type = MediaHTTPServer
    if ":" in arguments.bind:
        class IPv6MediaHTTPServer(MediaHTTPServer):
            address_family = socket.AF_INET6

        server_type = IPv6MediaHTTPServer

    server = server_type((arguments.bind, arguments.port), handler_factory(core))
    stopping = threading.Event()

    def stop(_signum: int, _frame: object) -> None:
        if stopping.is_set():
            return
        stopping.set()
        threading.Thread(target=server.shutdown, daemon=True).start()

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    LOG.info("media core listening on %s:%d", arguments.bind, arguments.port)
    try:
        server.serve_forever(poll_interval=0.5)
    finally:
        core.shutdown()
        server.server_close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
