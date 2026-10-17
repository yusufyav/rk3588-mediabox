"""The relay that hands a remote film to the player: what a seek does when the
source stumbles once."""

from __future__ import annotations

import socket
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from unittest import mock

from media.errors import MediaError
from media.proxy import relay as relay_module
from media.proxy.relay import relay


class Source(BaseHTTPRequestHandler):
    #: Status codes to answer before answering properly; then 206.
    failures: list[int] = []
    requests: list[str] = []

    def log_message(self, *args):  # noqa: D401
        pass

    def do_GET(self):  # noqa: N802
        Source.requests.append(self.headers.get("Range", ""))
        if Source.failures:
            code = Source.failures.pop(0)
            self.send_response(code)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        body = b"film"
        self.send_response(206)
        self.send_header("Content-Range", "bytes 100-103/1000")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


FILM = bytes(range(256)) * 4000  # a 1 MB "film"


class Cutting(BaseHTTPRequestHandler):
    """Serves FILM by range; the first answer promises the whole range and
    drops the connection after `cut` bytes of it."""

    cut: int | None = None
    requests: list[str] = []
    refuse_resume = False

    def log_message(self, *args):  # noqa: D401
        pass

    def do_GET(self):  # noqa: N802
        wanted = self.headers.get("Range", "")
        Cutting.requests.append(wanted)
        start = int(wanted.removeprefix("bytes=").partition("-")[0] or 0)
        if len(Cutting.requests) > 1 and Cutting.refuse_resume:
            self.send_response(200)  # ignores the range: no good for resuming
            self.send_header("Content-Length", str(len(FILM)))
            self.end_headers()
            try:
                self.wfile.write(FILM)
            except (BrokenPipeError, ConnectionResetError):
                pass  # the relay stops reading a reply it cannot use
            return
        body = FILM[start:]
        self.send_response(206)
        self.send_header("Content-Range", f"bytes {start}-{len(FILM) - 1}/{len(FILM)}")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if Cutting.cut is not None and len(Cutting.requests) == 1:
            self.wfile.write(body[: Cutting.cut])
            self.wfile.flush()
            self.connection.close()
            return
        self.wfile.write(body)


class Resume(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = ThreadingHTTPServer(("127.0.0.1", 0), Cutting)
        cls.url = f"http://127.0.0.1:{cls.server.server_address[1]}/film.mkv"
        threading.Thread(target=cls.server.serve_forever, daemon=True).start()

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()

    def setUp(self):
        Cutting.requests = []
        Cutting.refuse_resume = False

    def test_a_stream_cut_on_the_way_is_picked_up_where_it_stopped(self):
        Cutting.cut = 300_000
        with self.assertLogs("media.proxy.relay", "WARNING") as logged:
            status, headers, chunks = relay(self.url, "bytes=100000-")
            body = b"".join(chunks)
        self.assertEqual(status, 206)
        # The player gets every byte it was promised, in order, from one reply.
        self.assertEqual(body, FILM[100_000:])
        self.assertEqual(Cutting.requests, ["bytes=100000-", "bytes=400000-"])
        self.assertIn("resuming", logged.output[0])

    def test_a_whole_file_reply_cut_on_the_way_is_picked_up_too(self):
        Cutting.cut = 500_000
        with self.assertLogs("media.proxy.relay", "WARNING"):
            status, _, chunks = relay(self.url, None)
            body = b"".join(chunks)
        self.assertEqual(body, FILM)
        self.assertEqual(Cutting.requests, ["", "bytes=500000-"])

    def test_a_resume_the_source_answers_wrongly_ends_the_reply_as_before(self):
        Cutting.cut = 300_000
        Cutting.refuse_resume = True
        with self.assertLogs("media.proxy.relay", "WARNING"):
            _, _, chunks = relay(self.url, "bytes=0-")
            body = b"".join(chunks)
        # What arrived before the cut, and nothing made up after it.
        self.assertEqual(body, FILM[:300_000])
        self.assertEqual(len(Cutting.requests), 2)

    def test_an_uncut_stream_is_one_request(self):
        Cutting.cut = None
        _, _, chunks = relay(self.url, "bytes=0-")
        self.assertEqual(b"".join(chunks), FILM)
        self.assertEqual(len(Cutting.requests), 1)


class Bogus(BaseHTTPRequestHandler):
    """A CDN's answer to a one-byte probe, as it was seen on the appliance:
    the range is one byte, the length claims the whole 29 GB file, and the
    connection is kept open after the byte."""

    content_range = "bytes 0-0/29159331995"
    requests: list[str] = []
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):  # noqa: D401
        pass

    def do_GET(self):  # noqa: N802
        Bogus.requests.append(self.headers.get("Range", ""))
        self.send_response(206)
        self.send_header("Content-Range", Bogus.content_range)
        self.send_header("Content-Length", "29159331995")
        self.end_headers()
        self.wfile.write(b"\x1a")
        self.wfile.flush()
        # Held open, the way a keep-alive CDN connection is.
        time.sleep(3)


class ContentRange(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = ThreadingHTTPServer(("127.0.0.1", 0), Bogus)
        cls.url = f"http://127.0.0.1:{cls.server.server_address[1]}/film.mkv"
        threading.Thread(target=cls.server.serve_forever, daemon=True).start()

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()

    def setUp(self):
        Bogus.requests = []
        Bogus.content_range = "bytes 0-0/29159331995"

    def test_a_206_is_as_long_as_its_range_whatever_its_length_says(self):
        started = time.monotonic()
        with self.assertLogs("media.proxy.relay", "WARNING") as logged:
            status, headers, chunks = relay(self.url, "bytes=0-0", timeout=10.0)
            body = b"".join(chunks)
        # One byte, complete: no resume, no waiting on 29 GB that never come.
        self.assertEqual(status, 206)
        self.assertEqual(body, b"\x1a")
        self.assertEqual(Bogus.requests, ["bytes=0-0"])
        self.assertLess(time.monotonic() - started, 2.0)
        self.assertNotIn("resuming", "".join(logged.output))
        # The player is told the length the range has.
        passed = {name.lower(): value for name, value in headers}
        self.assertEqual(passed["content-length"], "1")
        self.assertEqual(passed["content-range"], "bytes 0-0/29159331995")

    def test_a_range_that_cannot_be_read_is_never_resumed(self):
        Bogus.content_range = "bytes nonsense"
        with self.assertLogs("media.proxy.relay", "WARNING"):
            status, headers, chunks = relay(self.url, "bytes=0-0", timeout=1.0)
            body = b"".join(chunks)
        self.assertEqual(status, 206)
        # Nothing made up -- at most the byte that came -- and no second
        # request for more.
        self.assertIn(body, (b"", b"\x1a"))
        self.assertEqual(Bogus.requests, ["bytes=0-0"])
        self.assertNotIn("content-length", {name.lower() for name, _ in headers})


class Stalling(BaseHTTPRequestHandler):
    """Answers nothing at all to its first request for a while -- a seek on
    2026-10-17 went unanswered for 17 s -- and properly afterwards."""

    requests = 0

    def log_message(self, *args):  # noqa: D401
        pass

    def do_GET(self):  # noqa: N802
        Stalling.requests += 1
        if Stalling.requests == 1:
            time.sleep(1.5)
            return
        body = b"film"
        self.send_response(206)
        self.send_header("Content-Range", "bytes 0-3/1000")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


class Stall(unittest.TestCase):
    def test_an_open_the_source_does_not_answer_is_tried_again_within_the_wait(self):
        server = ThreadingHTTPServer(("127.0.0.1", 0), Stalling)
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        url = f"http://127.0.0.1:{server.server_address[1]}/film.mkv?token=secret"
        Stalling.requests = 0
        with mock.patch.object(relay_module, "OPEN_TIMEOUT", 0.3), mock.patch.object(
            relay_module, "RETRY_DELAYS", (0.0, 0.0)
        ):
            began = time.monotonic()
            with self.assertLogs("media.proxy.relay", "WARNING") as logged:
                status, _, chunks = relay(url, "bytes=0-", timeout=30.0)
            self.assertEqual((status, b"".join(chunks)), (206, b"film"))
        # Tried again at the open's own timeout, not the reading one.
        self.assertLess(time.monotonic() - began, 1.2)
        self.assertEqual(Stalling.requests, 2)
        self.assertIn("timed out", "".join(logged.output))
        self.assertNotIn("secret", "".join(logged.output))

    def test_the_tries_fit_in_the_televisions_wait_for_a_first_frame(self):
        # No try starts past the budget, and the last one cannot outlast the
        # wait for a first frame.
        self.assertLess(relay_module.OPEN_BUDGET + relay_module.OPEN_TIMEOUT - relay_module.CONNECT_TIMEOUT, 25.0)

    def test_every_address_is_given_a_short_connect_and_each_is_said(self):
        # 2026-10-18: torrentio's two IPv4 addresses did not take the
        # connection and the IPv6 one could not be reached; only the last was
        # said. Here a refusing address comes first and a listening one second.
        listening = socket.socket()
        listening.bind(("127.0.0.1", 0))
        listening.listen()
        self.addCleanup(listening.close)
        closed = socket.socket()
        closed.bind(("127.0.0.1", 0))
        refusing = closed.getsockname()[1]
        closed.close()
        open_port = listening.getsockname()[1]

        def addresses(host, port, **_):
            return [
                (socket.AF_INET, socket.SOCK_STREAM, 6, "", ("127.0.0.1", refusing)),
                (socket.AF_INET, socket.SOCK_STREAM, 6, "", ("127.0.0.1", open_port)),
            ]

        with mock.patch.object(relay_module.socket, "getaddrinfo", addresses):
            relay_module._connect("film.example", 443, 0.5).close()
            with mock.patch.object(relay_module.socket, "getaddrinfo", lambda *a, **k: addresses(*a)[:1]):
                with self.assertRaises(OSError) as caught:
                    relay_module._connect("film.example", 443, 0.5)
        self.assertIn(f"127.0.0.1: ", str(caught.exception))
        self.assertIn("refused", str(caught.exception).lower())


class Relay(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = ThreadingHTTPServer(("127.0.0.1", 0), Source)
        cls.url = f"http://127.0.0.1:{cls.server.server_address[1]}/film.mkv?token=secret"
        threading.Thread(target=cls.server.serve_forever, daemon=True).start()

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()

    def setUp(self):
        Source.requests = []
        patcher = mock.patch.object(relay_module, "RETRY_DELAYS", (0.0, 0.0))
        patcher.start()
        self.addCleanup(patcher.stop)

    def test_a_seek_the_source_fails_once_is_answered_by_the_next_try(self):
        Source.failures = [503]
        with self.assertLogs("media.proxy.relay", "WARNING") as logged:
            status, headers, chunks = relay(self.url, "bytes=100-")
        self.assertEqual(status, 206)
        self.assertEqual(b"".join(chunks), b"film")
        self.assertEqual(Source.requests, ["bytes=100-", "bytes=100-"])
        # Why it stumbled is said, and the account token is not.
        self.assertIn("503", logged.output[0])
        self.assertNotIn("secret", "".join(logged.output))

    def test_a_source_that_stays_down_is_a_502_after_the_retries(self):
        Source.failures = [503, 502, 504]
        with self.assertLogs("media.proxy.relay", "WARNING"):
            with self.assertRaises(MediaError) as caught:
                relay(self.url, "bytes=100-")
        self.assertEqual(caught.exception.status, 502)
        self.assertEqual(len(Source.requests), 3)

    def test_the_sources_own_refusal_is_passed_on_without_retrying(self):
        Source.failures = [416]
        with self.assertRaises(MediaError) as caught:
            relay(self.url, "bytes=5000-")
        self.assertEqual(caught.exception.status, 416)
        self.assertEqual(len(Source.requests), 1)

    def test_a_wrong_request_is_not_retried(self):
        Source.failures = [403]
        with self.assertLogs("media.proxy.relay", "WARNING"):
            with self.assertRaises(MediaError) as caught:
                relay(self.url, "bytes=100-")
        self.assertEqual(caught.exception.status, 502)
        self.assertEqual(len(Source.requests), 1)


if __name__ == "__main__":
    unittest.main()
