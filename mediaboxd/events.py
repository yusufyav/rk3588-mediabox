"""In-process event fan-out and Kodi state polling."""

from __future__ import annotations

import json
import queue
import threading
import time
from contextlib import contextmanager
from datetime import datetime, timezone
from typing import Any, Iterator


class EventBroker:
    def __init__(self) -> None:
        self._subscribers: set[queue.Queue[dict[str, Any]]] = set()
        self._lock = threading.Lock()
        self._sequence = 0

    def publish(self, event_type: str, data: Any) -> None:
        with self._lock:
            self._sequence += 1
            event = {
                "id": self._sequence,
                "type": event_type,
                "timestamp": datetime.now(timezone.utc).isoformat(),
                "data": data,
            }
            subscribers = list(self._subscribers)
        for subscriber in subscribers:
            try:
                subscriber.put_nowait(event)
            except queue.Full:
                try:
                    subscriber.get_nowait()
                    subscriber.put_nowait(event)
                except (queue.Empty, queue.Full):
                    pass

    @contextmanager
    def subscribe(self) -> Iterator[queue.Queue[dict[str, Any]]]:
        subscriber: queue.Queue[dict[str, Any]] = queue.Queue(maxsize=64)
        with self._lock:
            self._subscribers.add(subscriber)
        try:
            yield subscriber
        finally:
            with self._lock:
                self._subscribers.discard(subscriber)

    @staticmethod
    def encode(event: dict[str, Any]) -> bytes:
        data = json.dumps(event["data"], separators=(",", ":"), ensure_ascii=False)
        return (
            f"id: {event['id']}\nevent: {event['type']}\ndata: {data}\n\n"
        ).encode("utf-8")


class KodiEventMonitor:
    def __init__(self, kodi: Any, broker: EventBroker, interval: float) -> None:
        self.kodi = kodi
        self.broker = broker
        self.interval = interval
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None

    def start(self) -> None:
        if self._thread is not None:
            return
        self._thread = threading.Thread(target=self._run, name="kodi-event-monitor", daemon=True)
        self._thread.start()

    def stop(self) -> None:
        self._stop.set()
        if self._thread:
            self._thread.join(timeout=max(2.0, self.interval + 0.5))

    def _run(self) -> None:
        previous = None
        while not self._stop.is_set():
            state = self.kodi.status()
            serialized = json.dumps(state, sort_keys=True, separators=(",", ":"))
            if serialized != previous:
                self.broker.publish("kodi.state", state)
                previous = serialized
            self._stop.wait(self.interval)
