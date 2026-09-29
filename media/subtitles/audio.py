"""When somebody is speaking in one stretch of a film.

One ffmpeg per window: seek, decode the audio of `length` seconds, fold it to
8 kHz mono in the speech band -- from the centre channel alone when the track
has one, which is where a film's dialogue is mixed -- and hand back 16-bit
samples. The detection itself is energy with an adaptive floor and
hysteresis -- the same family as auditok, which ffsubsync falls back to -- in
plain Python over 20 ms frames.

It is deliberately not a neural detector and not speech recognition. It has
to run beside a 4K film on the same board, and what the sync engine needs is
the rhythm of dialogue, not its words: a detector that also hears a door slam
costs a little correlation, while one that costs a core costs the film.

The process is the same kind of citizen `media.inspector.ffprobe` is: an
argument vector, its own process group, a deadline with a kill, bounded
output, and a lower priority than the player.
"""

from __future__ import annotations

import logging
import math
import os
import signal
import subprocess
import threading
import time
from array import array
from dataclasses import dataclass
from typing import Callable

from .sync import Window, merge


LOG = logging.getLogger(__name__)

RATE = 8000
FRAME = 160  # 20 ms
MAX_SECONDS = 120.0

#: Layouts with a front-centre channel beside others. A downmix of 5.1 lays
#: the score and the effects of four more channels over the dialogue; the
#: centre channel alone does not. Measured on *Avengers: Endgame* (DTS 5.1),
#: the same 40 windows: downmixed, 1 of them agreed with the subtitle; from
#: the centre, 7 did, on the line an independent transcript gives
#: (scale 1.001, -6.6 s).
CENTRE_LAYOUTS = frozenset({
    "3.0", "4.0", "5.0", "5.0(side)", "5.1", "5.1(side)", "6.0", "6.1", "6.1(back)",
    "7.0", "7.1", "7.1(wide)", "7.1(wide-side)", "hexagonal", "octagonal",
})


def has_centre(channels: int | None, layout: str | None) -> bool:
    """Whether a track's dialogue can be heard from its centre channel.

    Asked of the probe's answer, never guessed from the file name. Without a
    layout, six and eight channels are ffmpeg's 5.1 and 7.1. A stereo track
    has no centre: taking one would hear silence.
    """
    if layout:
        return layout in CENTRE_LAYOUTS
    return channels in (6, 8)


@dataclass(frozen=True, slots=True)
class ListenConfig:
    ffmpeg: str = "ffmpeg"
    #: A window is 30 s of audio; over a slow link that can take a while, and
    #: past this it is abandoned rather than waited for.
    timeout_seconds: float = 90.0
    user_agent: str = "MediaBox/2.0"
    #: Below the player, always.
    niceness: int = 10


class ListenError(RuntimeError):
    pass


def ffmpeg_argv(
    url: str, start: float, length: float, config: ListenConfig, audio_index: int | None, centre: bool = False
) -> list[str]:
    argv = [config.ffmpeg, "-nostdin", "-hide_banner", "-loglevel", "error", "-threads", "1"]
    if url.startswith(("http://", "https://")):
        argv += ["-user_agent", config.user_agent, "-rw_timeout", "20000000"]
    argv += [
        "-ss", f"{max(0.0, start):.3f}",
        "-i", url,
        "-t", f"{min(length, MAX_SECONDS):.3f}",
        "-map", f"0:a:{audio_index}" if audio_index is not None else "0:a:0?",
        "-vn", "-sn", "-dn",
        *(["-af", "pan=mono|c0=FC,highpass=f=200,lowpass=f=3400"] if centre
          else ["-ac", "1", "-af", "highpass=f=200,lowpass=f=3400"]),
        "-ar", str(RATE),
        "-f", "s16le",
        "-",
    ]
    return argv


def listen(
    url: str,
    start: float,
    length: float,
    *,
    config: ListenConfig | None = None,
    audio_index: int | None = None,
    centre: bool = False,
    cancelled: Callable[[], bool] = lambda: False,
) -> tuple[Window, int]:
    """The speech in [start, start + length), and how many bytes were read.

    The byte count is the audio's, not the network's -- ffmpeg does not say
    how much of the container it read -- and is only used for the log.
    """
    config = config or ListenConfig()
    argv = ffmpeg_argv(url, start, length, config, audio_index, centre)
    try:
        process = subprocess.Popen(
            argv,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=True,
        )
    except OSError as exc:
        raise ListenError(f"ffmpeg could not be started: {exc}") from exc
    try:
        os.setpriority(os.PRIO_PROCESS, process.pid, config.niceness)
    except OSError:
        pass

    limit = int(RATE * 2 * (min(length, MAX_SECONDS) + 1))
    chunks: list[bytes] = []
    errors: list[bytes] = []

    def drain_errors() -> None:
        assert process.stderr is not None
        errors.append(process.stderr.read(16 * 1024))

    reader = threading.Thread(target=drain_errors, daemon=True)
    reader.start()
    deadline = time.monotonic() + config.timeout_seconds
    received = 0
    try:
        assert process.stdout is not None
        while received < limit:
            if cancelled() or time.monotonic() > deadline:
                raise ListenError("cancelled" if cancelled() else "timed out")
            block = process.stdout.read1(65536) if hasattr(process.stdout, "read1") else process.stdout.read(65536)
            if not block:
                break
            chunks.append(block)
            received += len(block)
    finally:
        _stop(process)
        reader.join(timeout=1.0)
    raw = b"".join(chunks)[:limit]
    if not raw:
        message = b"".join(errors).decode("utf-8", "replace").strip().splitlines()
        raise ListenError(message[-1] if message else "no audio in that stretch")
    samples = array("h")
    samples.frombytes(raw[: len(raw) - len(raw) % 2])
    if samples.itemsize == 2 and array("h", [1]).tobytes() != b"\x01\x00":
        samples.byteswap()
    heard = len(samples) / RATE
    speech = detect(samples)
    return Window(start, start + heard, tuple((start + a, start + b) for a, b in speech)), len(raw)


def _stop(process: subprocess.Popen[bytes]) -> None:
    if process.poll() is None:
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            process.wait(timeout=2.0)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()
    for stream in (process.stdout, process.stderr):
        if stream is not None:
            try:
                stream.close()
            except OSError:
                pass


_sumprod = getattr(math, "sumprod", None)


def frame_levels(samples: array) -> list[float]:
    """Energy of each 20 ms frame, in dB."""
    levels = []
    for at in range(0, len(samples) - FRAME + 1, FRAME):
        chunk = samples[at : at + FRAME]
        energy = _sumprod(chunk, chunk) if _sumprod else sum(x * x for x in chunk)
        levels.append(10.0 * math.log10(energy / FRAME + 1.0))
    return levels


def detect(samples: array) -> list[tuple[float, float]]:
    """Speech as intervals in seconds from the start of `samples`.

    The threshold follows the window: a whisper in a quiet scene and a shout
    over an explosion are both relative to what else is in the window. On
    above 45 % of the way from the floor to the peak, off below 30 %, and a
    stretch has to last a quarter of a second to count; pauses shorter than
    that between words do not end it.
    """
    levels = frame_levels(samples)
    if len(levels) < 10:
        return []
    ordered = sorted(levels)
    floor = ordered[int(len(ordered) * 0.15)]
    peak = ordered[int(len(ordered) * 0.97)]
    if peak - floor < 6.0:
        # Nothing stands out from the floor: silence, or a constant bed of
        # music with nothing over it. Neither is speech anybody can place.
        return []
    on = floor + 0.45 * (peak - floor)
    off = floor + 0.30 * (peak - floor)
    step = FRAME / RATE
    found: list[tuple[float, float]] = []
    active: float | None = None
    for index, level in enumerate(levels):
        t = index * step
        if active is None and level >= on:
            active = t
        elif active is not None and level < off:
            found.append((active, t))
            active = None
    if active is not None:
        found.append((active, len(levels) * step))
    return [(a, b) for a, b in merge(found, gap=0.25) if b - a >= 0.25]
