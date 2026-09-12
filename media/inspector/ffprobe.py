"""Running ffprobe as a product, not as a shell one-liner.

Every property this module has exists because the alternative was seen to hurt
a real appliance:

    * **argument vector, never a shell string** — a source URL is attacker
      influenced (it comes from a third-party addon), and a shell would make
      that a command.
    * **a wall-clock deadline with a kill escalation** — a probe of a remote
      stream can block on a socket that never answers, and an ffprobe nobody
      is waiting for is a process nobody reaps.
    * **its own process group** — ffprobe is killed as a group, so a child it
      spawned cannot survive its parent.
    * **bounded stdout and stderr, drained concurrently** — a probe writing
      more than a pipe buffer while nothing reads the other pipe deadlocks
      both sides, and that deadlock looks exactly like a slow network.
    * **explicit read limits on the input** — `-probesize` / `-analyzeduration`
      keep a remote probe to a range request instead of a download.

The module never downloads a source. It reads a bounded prefix and stops.
"""

from __future__ import annotations

import json
import logging
import os
import shutil
import signal
import subprocess
import threading
import time
from dataclasses import dataclass
from typing import Any

from ..errors import ProbeError, ProbeTimeout
from .model import ProbeResult


LOG = logging.getLogger(__name__)

#: ffprobe writes one JSON document; a source with hundreds of tracks is still
#: far below this. Past it the output is a fault, not information.
MAX_STDOUT_BYTES = 8 * 1024 * 1024
MAX_STDERR_BYTES = 64 * 1024

DEFAULT_TIMEOUT_SECONDS = 25.0

#: How long a terminated probe is given to exit before it is killed outright.
TERMINATE_GRACE_SECONDS = 3.0

#: Bounded input reading. 24 MiB / 15 s is enough to see the headers, the DOVI
#: configuration record and the first frames' side data of a 4K HEVC stream,
#: and small enough that probing a remote source is a range request.
DEFAULT_PROBESIZE = "24M"
DEFAULT_ANALYZEDURATION = "15000000"

#: Frames read for side data. Mastering-display and HDR10+ metadata sit on
#: frames, not on the stream, so a header-only probe cannot see them; two
#: frames is enough to find them and cheap enough not to matter.
FRAME_INTERVAL = "%+#2"


@dataclass(frozen=True, slots=True)
class FFprobeConfig:
    binary: str = "ffprobe"
    timeout_seconds: float = DEFAULT_TIMEOUT_SECONDS
    probesize: str = DEFAULT_PROBESIZE
    analyzeduration: str = DEFAULT_ANALYZEDURATION
    #: Sent to servers that require one; some addon CDNs refuse the default.
    user_agent: str = "MediaBox/2.0"


def resolve_binary(config: FFprobeConfig) -> str:
    found = shutil.which(config.binary) if os.path.sep not in config.binary else config.binary
    if not found or not os.path.exists(found):
        raise ProbeError(f"ffprobe binary not found: {config.binary}")
    return found


def build_argv(source: str, config: FFprobeConfig, *, frames: bool) -> list[str]:
    """The exact argument vector used. Never joined into a string."""
    argv = [
        resolve_binary(config),
        "-hide_banner",
        "-loglevel",
        "error",
        "-probesize",
        config.probesize,
        "-analyzeduration",
        config.analyzeduration,
        "-user_agent",
        config.user_agent,
        "-of",
        "json",
    ]
    if frames:
        argv += [
            "-select_streams",
            "v:0",
            "-read_intervals",
            FRAME_INTERVAL,
            "-show_frames",
            "-show_entries",
            "frame=pkt_pts_time:frame_side_data_list",
        ]
    else:
        argv += ["-show_format", "-show_streams"]
    # `--` would be ideal, but ffprobe has no end-of-options marker; `-i`
    # unambiguously binds the next token as the input instead.
    argv += ["-i", source]
    return argv


class _BoundedReader(threading.Thread):
    """Drains one pipe into a capped buffer so the child can never block on it."""

    def __init__(self, stream: Any, limit: int) -> None:
        super().__init__(daemon=True)
        self._stream = stream
        self._limit = limit
        self.data = bytearray()
        self.truncated = False

    def run(self) -> None:
        try:
            while True:
                block = self._stream.read(65536)
                if not block:
                    return
                room = self._limit - len(self.data)
                if room > 0:
                    self.data.extend(block[:room])
                if len(block) > room:
                    self.truncated = True
                    # Keep draining: stopping here would block the writer.
        except (OSError, ValueError):
            return
        finally:
            try:
                self._stream.close()
            except OSError:
                pass


def _terminate(process: subprocess.Popen[bytes]) -> None:
    """End the probe and everything it started, then reap it.

    The child was given its own process group, so the signal reaches any
    helper it spawned. `wait()` without a timeout at the end is deliberate:
    after SIGKILL the kernel guarantees the exit, and not waiting is exactly
    how a zombie is created.
    """
    if process.poll() is not None:
        process.wait()
        return
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except (ProcessLookupError, PermissionError, OSError):
        process.terminate()
    try:
        process.wait(timeout=TERMINATE_GRACE_SECONDS)
        return
    except subprocess.TimeoutExpired:
        pass
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except (ProcessLookupError, PermissionError, OSError):
        process.kill()
    process.wait()


def run_ffprobe(source: str, config: FFprobeConfig, *, frames: bool) -> tuple[dict[str, Any], str]:
    """Run one probe. Returns (parsed json, stderr text).

    Raises :class:`ProbeTimeout` when the deadline passes and
    :class:`ProbeError` when ffprobe fails or its output is unusable.
    """
    argv = build_argv(source, config, frames=frames)
    try:
        process = subprocess.Popen(
            argv,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=True,
        )
    except OSError as exc:
        raise ProbeError(f"ffprobe could not be started: {exc}") from exc

    out = _BoundedReader(process.stdout, MAX_STDOUT_BYTES)
    err = _BoundedReader(process.stderr, MAX_STDERR_BYTES)
    out.start()
    err.start()

    timed_out = False
    try:
        process.wait(timeout=config.timeout_seconds)
    except subprocess.TimeoutExpired:
        timed_out = True
    finally:
        # Unconditional: on the normal path this reaps an already-exited child,
        # and on the timeout path it is what stops the probe.
        _terminate(process)
        out.join(timeout=TERMINATE_GRACE_SECONDS)
        err.join(timeout=TERMINATE_GRACE_SECONDS)

    stderr_text = bytes(err.data).decode("utf-8", "replace").strip()
    if timed_out:
        raise ProbeTimeout(
            f"ffprobe exceeded {config.timeout_seconds:g}s for this source"
        )
    if process.returncode != 0:
        raise ProbeError(
            "ffprobe failed",
            {"returncode": process.returncode, "stderr": stderr_text[:2000]},
        )
    if out.truncated:
        raise ProbeError("ffprobe produced more output than the media core accepts")
    raw = bytes(out.data)
    if not raw.strip():
        raise ProbeError("ffprobe produced no output", {"stderr": stderr_text[:2000]})
    try:
        parsed = json.loads(raw)
    except (json.JSONDecodeError, UnicodeDecodeError) as exc:
        raise ProbeError(
            "ffprobe produced output that is not JSON", {"stderr": stderr_text[:2000]}
        ) from exc
    if not isinstance(parsed, dict):
        raise ProbeError("ffprobe produced a JSON value that is not an object")
    return parsed, stderr_text


def probe(source: str, config: FFprobeConfig | None = None) -> ProbeResult:
    """Probe a source twice: headers first, then a bounded look at frames.

    The frame pass is best effort. It is where HDR10+ and the mastering display
    live, but a source that refuses a second range request is still perfectly
    describable without them — the result simply says so in `warnings` rather
    than pretending the metadata is absent.
    """
    config = config or FFprobeConfig()
    started = time.monotonic()
    header, header_stderr = run_ffprobe(source, config, frames=False)

    result = ProbeResult(
        format=header.get("format") if isinstance(header.get("format"), dict) else {},
        streams=[s for s in header.get("streams", []) if isinstance(s, dict)],
    )
    if header_stderr:
        result.warnings.append(f"ffprobe: {header_stderr[:300]}")
    if not result.streams:
        raise ProbeError("the source declares no streams")

    has_video = any(s.get("codec_type") == "video" for s in result.streams)
    if has_video:
        try:
            frames, _ = run_ffprobe(source, config, frames=True)
            result.frames = [f for f in frames.get("frames", []) if isinstance(f, dict)]
        except (ProbeError, ProbeTimeout) as exc:
            LOG.info("Frame-level probe unavailable for this source: %s", exc.message)
            result.warnings.append(
                "frame-level side data unavailable; dynamic HDR metadata was not inspected"
            )
    result.duration_seconds = round(time.monotonic() - started, 3)
    return result
