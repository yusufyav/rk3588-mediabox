"""Media sessions: one owner for every process the media core starts.

The rule that shapes this module is simple and was learned the expensive way:
*a process the appliance started must not outlive the reason it was started.*
An ffmpeg with no reader is a busy core forever, and the reader is a browser or
a television that can vanish without saying so.

So a session owns its child completely:

* one child per session, and one session per source — asking twice returns the
  session that already exists instead of starting a second encoder;
* every child is started in its own process group, so ending it ends whatever
  it started;
* stop is idempotent, and runs the same path whether it was asked for, timed
  out, or reached at shutdown;
* the reaper ends only a session nobody holds and nobody is reading, once
  its TTL has passed since the last of either (see `reap_once`);
* `stop()` does not return until the child has been waited on, so a stopped
  session leaves no zombie.

`Direct` sessions are recorded but start nothing: the player opens the source
itself and the media core stays out of the media path.
"""

from __future__ import annotations

import logging
import os
import secrets
import signal
import subprocess
import threading
import time
from dataclasses import dataclass, field
from enum import Enum
from typing import Any, Iterator

from ..errors import MediaError, NotFound, SessionError
from ..policy.decide import PlaybackDecision, PlaybackMode, mode_when_chosen
from .ffmpeg import FFmpegConfig, build_argv
from .security import SourcePolicy, validate_session_id, validate_source_url


LOG = logging.getLogger(__name__)

READ_CHUNK_BYTES = 64 * 1024

#: A session nobody has read from for this long is ended. The number is a
#: compromise: long enough to survive a player buffering or a network hiccup,
#: short enough that a closed tab costs seconds of CPU rather than hours.
DEFAULT_IDLE_TIMEOUT_SECONDS = 45.0
REAPER_INTERVAL_SECONDS = 5.0
TERMINATE_GRACE_SECONDS = 5.0
MAX_STDERR_BYTES = 32 * 1024


class SessionMode(str, Enum):
    #: No process. The player is handed the source URL.
    DIRECT = "Direct"
    #: Bytes relayed by the media core without being re-muxed.
    DIRECT_PROXY = "DirectProxy"
    REMUX = "Remux"
    AUDIO_TRANSCODE = "AudioTranscode"


class SessionState(str, Enum):
    CREATED = "created"
    RUNNING = "running"
    STOPPING = "stopping"
    STOPPED = "stopped"
    FAILED = "failed"


_MODE_FOR_DECISION = {
    PlaybackMode.DIRECT: SessionMode.DIRECT,
    PlaybackMode.DIRECT_WITH_AUDIO_TRANSCODE: SessionMode.AUDIO_TRANSCODE,
    PlaybackMode.REMUX: SessionMode.REMUX,
}


@dataclass(slots=True)
class MediaSession:
    session_id: str
    source: str
    mode: SessionMode
    decision: PlaybackDecision
    created_at: float
    last_seen: float
    state: SessionState = SessionState.CREATED
    process: subprocess.Popen[bytes] | None = None
    pid: int | None = None
    clients: int = 0
    started_at: float | None = None
    stopped_at: float | None = None
    exit_code: int | None = None
    stop_reason: str | None = None
    stderr: bytearray = field(default_factory=bytearray)
    argv: tuple[str, ...] = ()
    playback_url: str | None = None
    #: Whether anybody has read a byte out of the child yet. A stream that has
    #: been read from cannot be handed to a second reader as it stands: see
    #: `SessionRegistry._rewind_for_a_new_reader`.
    served: bool = False
    #: Who holds the session open: the player it was created for, until that
    #: player lets it go. A held session is never reaped, however long it has
    #: gone unread -- a film paused for an hour reads nothing and still needs
    #: its address when it resumes.
    owner: str | None = None

    def selected_tracks(self) -> dict[str, Any]:
        video = self.decision.video.track
        audio = self.decision.audio.track
        return {
            "video": None
            if video is None
            else {
                "streamIndex": video.stream_index,
                "codec": video.codec,
                "profile": video.profile,
                "width": video.width,
                "height": video.height,
                "hdr": self.decision.video.hdr.value,
                "action": "copy",
            },
            "audio": None
            if audio is None
            else {
                "streamIndex": audio.stream_index,
                "codec": audio.codec,
                "channels": audio.channels,
                "language": audio.language,
                "action": self.decision.audio.action.value,
                "target": None
                if not self.decision.audio.requires_encoder
                else {
                    "codec": self.decision.audio.target_codec,
                    "channels": self.decision.audio.target_channels,
                    "bitRate": self.decision.audio.target_bitrate,
                },
            },
        }

    def as_dict(self) -> dict[str, Any]:
        return {
            "sessionId": self.session_id,
            "source": self.source,
            "mode": self.mode.value,
            "state": self.state.value,
            "pid": self.pid,
            "clients": self.clients,
            "createdAt": self.created_at,
            "startedAt": self.started_at,
            "lastSeen": self.last_seen,
            "owner": self.owner,
            "stoppedAt": self.stopped_at,
            "exitCode": self.exit_code,
            "stopReason": self.stop_reason,
            "playbackUrl": self.playback_url,
            "selectedTracks": self.selected_tracks(),
            "videoCopied": self.decision.video_is_copied,
            "reasons": [
                reason.as_dict()
                for reason in (
                    *self.decision.reasons,
                    *self.decision.video.reasons,
                    *self.decision.audio.reasons,
                )
            ],
            "stderr": bytes(self.stderr).decode("utf-8", "replace")[-2000:] or None,
        }


class _StderrDrain(threading.Thread):
    """Keeps the child's stderr pipe empty so the child never blocks on it."""

    def __init__(self, session: MediaSession, stream: Any) -> None:
        super().__init__(daemon=True, name=f"media-session-stderr-{session.session_id[:8]}")
        self._session = session
        self._stream = stream

    def run(self) -> None:
        try:
            while True:
                block = self._stream.read1(8192)
                if not block:
                    return
                room = MAX_STDERR_BYTES - len(self._session.stderr)
                if room > 0:
                    self._session.stderr.extend(block[:room])
        except (OSError, ValueError):
            return
        finally:
            try:
                self._stream.close()
            except OSError:
                pass


class SessionManager:
    """Creates, serves, and ends every media session on this appliance."""

    def __init__(
        self,
        *,
        source_policy: SourcePolicy | None = None,
        ffmpeg: FFmpegConfig | None = None,
        idle_timeout_seconds: float = DEFAULT_IDLE_TIMEOUT_SECONDS,
        base_url: str = "",
        clock: Any = time.monotonic,
    ) -> None:
        self._sessions: dict[str, MediaSession] = {}
        self._by_source: dict[tuple[str, str], str] = {}
        self._lock = threading.RLock()
        self._policy = source_policy or SourcePolicy()
        self._ffmpeg = ffmpeg or FFmpegConfig()
        self._idle_timeout = idle_timeout_seconds
        self._base_url = base_url.rstrip("/")
        self._clock = clock
        self._stopping = threading.Event()
        self._reaper: threading.Thread | None = None

    # ------------------------------------------------------------------ create

    def create(
        self,
        source: str,
        decision: PlaybackDecision,
        *,
        start_seconds: float | None = None,
        container: str | None = None,
        owner: str | None = None,
    ) -> MediaSession:
        """Create a session, or return the live one for the same source and mode.

        Deduplication is the point, not an optimisation: two encoders for one
        viewer is the failure this whole module exists to prevent.
        """
        validate_source_url(source, self._policy)
        if decision.mode is PlaybackMode.UNSUPPORTED:
            raise SessionError(
                "SOURCE_UNSUPPORTED", "this source cannot be played by this appliance", 422
            )
        # The viewer's choice is played, as Stremio plays it; a risky
        # rendition is ranked last, never refused here.
        mode = _MODE_FOR_DECISION[mode_when_chosen(decision)]

        key = (source, mode.value)
        now = self._clock()
        with self._lock:
            existing_id = self._by_source.get(key)
            if existing_id is not None:
                existing = self._sessions.get(existing_id)
                if existing is not None and existing.state in (
                    SessionState.CREATED,
                    SessionState.RUNNING,
                ):
                    existing.last_seen = now
                    if owner is not None:
                        existing.owner = owner
                    return existing
                self._by_source.pop(key, None)

            session = MediaSession(
                session_id=secrets.token_hex(16),
                source=source,
                mode=mode,
                decision=decision,
                created_at=now,
                last_seen=now,
                owner=owner,
            )
            if mode is SessionMode.DIRECT:
                session.state = SessionState.RUNNING
                session.playback_url = source
            else:
                session.playback_url = (
                    f"{self._base_url}/media/session/{session.session_id}"
                    if self._base_url
                    else f"/media/session/{session.session_id}"
                )
                session.argv = tuple(
                    build_argv(
                        source,
                        decision,
                        self._ffmpeg,
                        **({"container": container} if container else {}),
                        **({"start_seconds": start_seconds} if start_seconds else {}),
                    )
                )
            self._sessions[session.session_id] = session
            self._by_source[key] = session.session_id
        self._ensure_reaper()
        return session

    # ------------------------------------------------------------------- serve

    def get(self, session_id: str) -> MediaSession:
        validate_session_id(session_id)
        with self._lock:
            session = self._sessions.get(session_id)
        if session is None:
            raise NotFound("no such media session")
        return session

    def attach(self, session_id: str) -> Iterator[bytes]:
        """Start the child if needed and stream its output to one client.

        The generator is the client's handle on the session: it counts as a
        client while it is being read, and detaches in its `finally` whether
        the client finished, gave up, or died.
        """
        session = self.get(session_id)
        if session.mode is SessionMode.DIRECT:
            raise SessionError(
                "SESSION_IS_DIRECT",
                "this session is played directly from the source; there is nothing to relay",
                409,
            )
        self._rewind_for_a_new_reader(session)
        self._start(session)
        with self._lock:
            session.clients += 1
            session.last_seen = self._clock()
        return self._relay(session)

    def read_direct(self, session_id: str, chunks: Iterator[bytes]) -> Iterator[bytes]:
        """Count a reader of a direct session for as long as it reads.

        A direct relay starts no process, and for that reason it used to
        count for nothing: the session's idleness was measured from the
        moment it was created, and a film that played on from one long
        response was reaped under the player that was reading it.
        """
        session = self.get(session_id)
        with self._lock:
            session.clients += 1
            session.last_seen = self._clock()
        return _CountedStream(self, session, chunks)

    def touch(self, session: MediaSession) -> None:
        with self._lock:
            session.last_seen = self._clock()

    def release(self, session_id: str) -> dict[str, Any]:
        """The holder is done with the session. It is kept for its TTL from
        now, for any reader still on it, and reaped after that."""
        session = self.get(session_id)
        with self._lock:
            session.owner = None
            session.last_seen = self._clock()
        return {"released": True, "sessionId": session_id}

    def _rewind_for_a_new_reader(self, session: MediaSession) -> None:
        """Give a reader that arrives on its own the stream from the start.

        A transformed session is one ffmpeg writing to one pipe, and a pipe
        has one position. Whoever reads it second does not get the container's
        header -- they get wherever the muxer has got to, which is the middle
        of a cluster.

        That is not a theoretical reader. Kodi opens a URL several times before
        it plays it: once to ask the mime type, once through CurlFile, once
        more through its file cache. Measured on the Ultra against a remux the
        television was handing over:

            first reader of a fresh session:  1a 45 df a3   (Matroska)
            a reader that arrived later:      21 49 d1 a6   (mid-stream)

            ffmpeg[...]: Input #0, ac3, from 'http://127.0.0.1:8790/media/...'

        Kodi played the audio and showed no picture, with the session's id
        where the film's name belongs and no duration -- because what it was
        handed was not a container at all.

        So a reader that arrives when nobody else is reading gets a child of
        its own. The command is unchanged, `-ss` included, so it restarts at
        the same second; only the pipe is new. A reader that arrives while
        another is mid-stream is left alone: rewinding then would break the
        one that is already watching, and two readers of one transform is a
        different problem from this one.
        """
        with self._lock:
            if session.clients > 0 or not session.served or session.process is None:
                return
            process = session.process
            session.process = None
            session.pid = None
            session.served = False
            session.state = SessionState.CREATED
        LOG.info(
            "media session %s rewound: a new reader needs the stream from the start",
            session.session_id,
        )
        _terminate(process)

    def _relay(self, session: MediaSession) -> Iterator[bytes]:
        process = session.process
        assert process is not None and process.stdout is not None
        try:
            while True:
                # `read1` returns as soon as the muxer has produced anything.
                # `read` waits for a full buffer, which turns a live stream
                # into one that starts 64 KiB late and stutters after that.
                try:
                    block = process.stdout.read1(READ_CHUNK_BYTES)
                except (ValueError, OSError):
                    # The session was stopped from another thread and its pipe
                    # was closed underneath this read. That is the end of the
                    # stream, not an error for the client to see.
                    return
                if not block:
                    return
                with self._lock:
                    session.last_seen = self._clock()
                    session.served = True
                yield block
        finally:
            self.detach(session.session_id)

    def detach(self, session_id: str) -> None:
        with self._lock:
            session = self._sessions.get(session_id)
            if session is None:
                return
            session.clients = max(0, session.clients - 1)
            session.last_seen = self._clock()

    def _start(self, session: MediaSession) -> None:
        with self._lock:
            if session.process is not None:
                return
            if session.state in (SessionState.STOPPING, SessionState.STOPPED):
                raise SessionError("SESSION_STOPPED", "this media session has ended", 409)
            try:
                process = subprocess.Popen(
                    list(session.argv),
                    stdin=subprocess.DEVNULL,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    start_new_session=True,
                )
            except OSError as exc:
                session.state = SessionState.FAILED
                session.stop_reason = str(exc)
                raise MediaError(
                    "SESSION_START_FAILED", f"the media session could not start: {exc}", 500
                ) from exc
            session.process = process
            session.pid = process.pid
            session.state = SessionState.RUNNING
            session.started_at = self._clock()
            LOG.info(
                "media session %s started pid=%d mode=%s",
                session.session_id,
                process.pid,
                session.mode.value,
            )
        _StderrDrain(session, process.stderr).start()

    # -------------------------------------------------------------------- stop

    def stop(self, session_id: str, reason: str = "requested") -> dict[str, Any]:
        """End a session. Safe to call any number of times, from any thread."""
        with self._lock:
            session = self._sessions.get(session_id)
            if session is None:
                return {"stopped": False, "sessionId": session_id}
            if session.state is SessionState.STOPPED:
                return {"stopped": False, "sessionId": session_id, "state": "stopped"}
            session.state = SessionState.STOPPING
            session.stop_reason = reason
            process = session.process
            self._by_source.pop((session.source, session.mode.value), None)

        if process is not None:
            _terminate(process)
            session.exit_code = process.returncode

        with self._lock:
            session.state = SessionState.STOPPED
            session.stopped_at = self._clock()
            session.clients = 0
            session.process = None
        LOG.info("media session %s stopped (%s)", session_id, reason)
        return {"stopped": True, "sessionId": session_id, "reason": reason}

    def list(self) -> list[dict[str, Any]]:
        with self._lock:
            return [session.as_dict() for session in self._sessions.values()]

    def active(self) -> list[MediaSession]:
        with self._lock:
            return [
                session
                for session in self._sessions.values()
                if session.state in (SessionState.CREATED, SessionState.RUNNING)
            ]

    def forget_stopped(self, older_than_seconds: float = 300.0) -> int:
        """Drop the records of sessions that ended a while ago."""
        cutoff = self._clock() - older_than_seconds
        with self._lock:
            expired = [
                session_id
                for session_id, session in self._sessions.items()
                if session.state is SessionState.STOPPED
                and (session.stopped_at or 0) < cutoff
            ]
            for session_id in expired:
                self._sessions.pop(session_id, None)
        return len(expired)

    # ------------------------------------------------------------------ reaper

    def _ensure_reaper(self) -> None:
        with self._lock:
            if self._reaper is not None and self._reaper.is_alive():
                return
            if self._stopping.is_set():
                return
            self._reaper = threading.Thread(
                target=self._reap_loop, name="media-session-reaper", daemon=True
            )
            self._reaper.start()

    def reap_once(self) -> list[str]:
        """End every session whose deadline has passed. Returns their ids.

        Idleness is counted only for a session nobody holds and nobody is
        reading. The Stremio streaming server keeps its streams the same way:
        a stream's inactivity timer starts when its last reader closes.
        """
        now = self._clock()
        doomed: list[tuple[str, str]] = []
        with self._lock:
            for session in self._sessions.values():
                if session.state not in (SessionState.CREATED, SessionState.RUNNING):
                    continue
                held = session.owner is not None or session.clients > 0
                if session.mode is SessionMode.DIRECT:
                    # Nothing is running; the record expires but costs nothing.
                    if not held and now - session.last_seen > self._idle_timeout * 4:
                        doomed.append((session.session_id, "idle"))
                    continue
                if not held and now - session.last_seen > self._idle_timeout:
                    doomed.append((session.session_id, "idle"))
                elif session.process is not None and session.process.poll() is not None:
                    doomed.append((session.session_id, "child-exited"))
        for session_id, reason in doomed:
            self.stop(session_id, reason)
        return [session_id for session_id, _ in doomed]

    def _reap_loop(self) -> None:
        while not self._stopping.wait(REAPER_INTERVAL_SECONDS):
            try:
                self.reap_once()
                self.forget_stopped()
            except Exception:  # pragma: no cover - a reaper must not die
                LOG.exception("media session reaper iteration failed")

    def shutdown(self) -> None:
        """End every session. Called on daemon shutdown; leaves nothing behind."""
        self._stopping.set()
        for session in self.active():
            self.stop(session.session_id, "shutdown")


def _terminate(process: subprocess.Popen[bytes]) -> None:
    """SIGTERM the process group, then SIGKILL, then wait. Always waits."""
    if process.poll() is not None:
        process.wait()
        _close(process)
        return
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except (ProcessLookupError, PermissionError, OSError):
        process.terminate()
    try:
        process.wait(timeout=TERMINATE_GRACE_SECONDS)
        _close(process)
        return
    except subprocess.TimeoutExpired:
        pass
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except (ProcessLookupError, PermissionError, OSError):
        process.kill()
    process.wait()
    _close(process)


def _close(process: subprocess.Popen[bytes]) -> None:
    for stream in (process.stdout, process.stderr):
        if stream is not None:
            try:
                stream.close()
            except OSError:
                pass


class _CountedStream:
    """One reader of a direct session: counted while open, let go once.

    A class rather than a generator because a generator that was never
    started does not run its `finally` when it is closed, and a reader that
    was counted must be uncounted however its response ended.
    """

    def __init__(self, manager: SessionManager, session: MediaSession, chunks: Iterator[bytes]):
        self._manager = manager
        self._session = session
        self._chunks = iter(chunks)
        self._source = chunks
        self._open = True

    def __iter__(self) -> "_CountedStream":
        return self

    def __next__(self) -> bytes:
        try:
            block = next(self._chunks)
        except BaseException:
            self.close()
            raise
        self._manager.touch(self._session)
        return block

    def close(self) -> None:
        if not self._open:
            return
        self._open = False
        close = getattr(self._source, "close", None)
        if close is not None:
            close()
        self._manager.detach(self._session.session_id)
