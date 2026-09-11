"""mediaboxd command-line entry point."""

from __future__ import annotations

import argparse
import logging
import signal
import socket
import sys
import threading

from .api import APIContext, MediaBoxHTTPServer, handler_factory
from .config import load_config
from .events import EventBroker, KodiEventMonitor
from .kodi import KodiClient
from .lifecycle import KodiLifecycle, SystemActions
from .telemetry import Telemetry


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(description="RK3588 MediaBox control daemon")
    result.add_argument("--config", help="TOML config path; built-in secure defaults when omitted")
    return result


def main(argv: list[str] | None = None) -> int:
    arguments = parser().parse_args(argv)
    try:
        config = load_config(arguments.config)
    except (OSError, ValueError) as exc:
        print(f"mediaboxd: configuration error: {exc}", file=sys.stderr)
        return 2
    logging.basicConfig(
        level=getattr(logging, config.log_level),
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
    )
    kodi = KodiClient(config.kodi)
    events = EventBroker()
    monitor = KodiEventMonitor(kodi, events, config.event_poll_seconds)
    context = APIContext(
        kodi=kodi,
        lifecycle=KodiLifecycle(config.kodi, kodi),
        telemetry=Telemetry(),
        events=events,
        system_actions=SystemActions(config),
    )
    server_type = MediaBoxHTTPServer
    if ":" in config.bind_address:
        class IPv6MediaBoxHTTPServer(MediaBoxHTTPServer):
            address_family = socket.AF_INET6

        server_type = IPv6MediaBoxHTTPServer
    server = server_type(
        (config.bind_address, config.port), handler_factory(context)
    )
    stopping = threading.Event()

    def stop(_signum: int, _frame: object) -> None:
        if stopping.is_set():
            return
        stopping.set()
        threading.Thread(target=server.shutdown, daemon=True).start()

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    monitor.start()
    logging.getLogger(__name__).info(
        "listening on %s:%d (LAN=%s, system_actions=%s)",
        config.bind_address,
        config.port,
        config.allow_lan,
        config.system_actions_enabled,
    )
    try:
        server.serve_forever(poll_interval=0.5)
    finally:
        monitor.stop()
        server.server_close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
