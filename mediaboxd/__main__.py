"""mediaboxd command-line entry point."""

from __future__ import annotations

import argparse
import logging
import signal
import socket
import sys
import threading
from pathlib import Path

from .api import APIContext, MediaBoxHTTPServer, handler_factory, kodi_response
from .config import load_config
from .events import EventBroker, KodiEventMonitor
from .kodi import KodiClient
from .lifecycle import KodiLifecycle, SystemActions
from .stremio import StremioBridge
from .telemetry import Telemetry


def _media_core(config) -> object | None:
    """Build the V2 media core, or run without it if it is not installed.

    The media core lives beside mediaboxd rather than inside it, so a control
    plane that was deployed without it still starts — it simply answers 404 on
    /media/ instead of refusing to run.
    """
    if not config.media.enabled:
        return None
    try:
        from media.api import MediaCore, MediaCoreConfig
    except ImportError:
        logging.getLogger(__name__).warning(
            "the media core is not installed; /media/ will not be served"
        )
        return None
    base = f"http://{config.bind_address}:{config.port}"
    return MediaCore(
        MediaCoreConfig(
            streaming_server_url=config.stremio.upstream,
            capability_profile=config.media.capability_profile,
            session_state_path=config.media.state_path,
            base_url=base,
            loopback_base_url=f"http://127.0.0.1:{config.port}",
            allowed_file_prefixes=config.media.allowed_file_prefixes,
            idle_timeout_seconds=config.media.idle_timeout_seconds,
        )
    )


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
    monitor = KodiEventMonitor(
        kodi,
        events,
        config.event_poll_seconds,
        serializer=kodi_response,
    )
    context = APIContext(
        kodi=kodi,
        lifecycle=KodiLifecycle(config.kodi, kodi),
        telemetry=Telemetry(),
        events=events,
        system_actions=SystemActions(config),
        webui_root=Path(config.webui_root) if config.webui_root else None,
        stremio=StremioBridge(config.stremio, kodi, events),
        media=_media_core(config),
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
        "listening on %s:%d (LAN=%s, system_actions=%s, stremio=%s, media_core=%s)",
        config.bind_address,
        config.port,
        config.allow_lan,
        config.system_actions_enabled,
        config.stremio.enabled,
        context.media is not None,
    )
    try:
        server.serve_forever(poll_interval=0.5)
    finally:
        monitor.stop()
        context.stremio.shutdown()
        if context.media is not None:
            context.media.shutdown()
        server.server_close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
