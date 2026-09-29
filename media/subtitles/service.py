"""The subtitle system as the rest of the media core sees it.

For one playing film (one media session) it answers four questions:

    prepare   which external subtitles there are for it, best first
    load      one of them, fetched, checked and served on the loopback
    sync      where that one's lines belong in this film, worked out in the
              background and kept
    file      the bytes of a kept subtitle, for the player

The network stays here. The player MediaBox owns is built against a Rockchip
ffmpeg with no TLS, and every subtitle addon is HTTPS; so the player is given
`http://127.0.0.1:.../media/subtitles/file/<key>.srt`, never the addon's URL,
exactly as it is given the media session's address rather than the film's.

The control plane decides what is selected and when; this module never talks
to the player.
"""

from __future__ import annotations

import logging
import os
import re
import threading
import time
from dataclasses import dataclass, field
from typing import Any, Callable
from urllib.parse import urlsplit

from ..errors import InvalidRequest, MediaError, NotFound, UpstreamError
from ..http import request
from ..proxy.security import SourcePolicy, validate_source_url
from ..stremio.models import Stream, Subtitle, SubtitleSource
from . import sync as engine
from .audio import ListenConfig, ListenError, has_centre, listen
from .formats import SubtitleFormatError, parse
from .runner import align_isolated
from .languages import canonical
from .store import SubtitleStore, digest, valid_key


LOG = logging.getLogger("media.subtitles")

#: The most a sync may make this box download of a film it is streaming over
#: HTTP. A torrent's and a local file's evidence is read from data already on
#: the box and has no such limit. The number is a ceiling on harm, not a
#: target: a WEB-DL at 8 Mbit/s spends a tenth of it on the whole analysis; a
#: 4K remux cannot be analysed within it at all and is left alone.
DEFAULT_MAX_BYTES = 2 * 1024 * 1024 * 1024

#: Fewer affordable windows than this and an HTTP source is not listened to:
#: the calibration found nothing trustworthy below it.
MIN_AFFORDABLE_WINDOWS = 12

WINDOW_SECONDS = 30.0

#: Candidates offered per language: the menu is a television menu.
PER_LANGUAGE = 3


@dataclass(slots=True)
class VideoContext:
    session_id: str
    source_url: str
    kind: str  # "torrent" | "http" | "file"
    duration: float | None = None
    size: int | None = None
    filename: str | None = None
    video_hash: str | None = None
    identity: str = ""
    stream: Stream | None = None
    type_name: str | None = None
    item_id: str | None = None
    video_id: str | None = None
    candidates: dict[str, Subtitle] = field(default_factory=dict)
    order: list[str] = field(default_factory=list)
    loaded: dict[str, str] = field(default_factory=dict)
    #: (channels, layout) of each audio track, in the order `0:a:N` counts
    #: them; from the probe the session was created with.
    audio: tuple[tuple[int | None, str | None], ...] = ()

    def as_dict(self) -> dict[str, Any]:
        return {
            "sessionId": self.session_id,
            "kind": self.kind,
            "identity": self.identity,
            "videoHash": self.video_hash,
            "videoSize": self.size,
            "filename": self.filename,
            "duration": self.duration,
        }


@dataclass(slots=True)
class SyncJob:
    id: str
    session_id: str
    key: str
    identity: str
    state: str = "running"  # running | waiting | done | failed | cancelled
    cache: str = "miss"
    result: dict[str, Any] | None = None
    apply: dict[str, Any] | None = None
    error: str | None = None
    started: float = field(default_factory=time.monotonic)
    finished: float | None = None
    heard: int = 0
    position: float | None = None
    duration: float | None = None
    cancel: threading.Event = field(default_factory=threading.Event)

    def as_dict(self) -> dict[str, Any]:
        return {
            "job": self.id,
            "key": self.key,
            "state": self.state,
            "cache": self.cache,
            "result": self.result,
            "apply": self.apply,
            "error": self.error,
            "windows": self.heard,
            "seconds": round((self.finished or time.monotonic()) - self.started, 2),
        }


def opensubtitles_hash(first: bytes, last: bytes, size: int) -> str:
    """OpenSubtitles' file hash: the size plus the 64-bit little-endian sums
    of the first and last 64 KiB, modulo 2**64."""
    total = size
    for block in (first[:65536], last[-65536:]):
        padded = block + b"\0" * (-len(block) % 8)
        for at in range(0, len(padded), 8):
            total = (total + int.from_bytes(padded[at : at + 8], "little")) & 0xFFFFFFFFFFFFFFFF
    return f"{total:016x}"


class SubtitleService:
    def __init__(
        self,
        stremio: Any,
        source_policy: SourcePolicy,
        *,
        root: str | None = None,
        base_url: str = "",
        streaming_server_url: str = "",
        ffmpeg: str = "ffmpeg",
        max_bytes: int = DEFAULT_MAX_BYTES,
        alive: Callable[[str], bool] = lambda _: True,
        listener: Callable[..., tuple[engine.Window, int]] | None = None,
        aligner: Callable[..., engine.SyncResult] | None = None,
    ) -> None:
        self.stremio = stremio
        self.policy = source_policy
        self.store = SubtitleStore(root)
        self.base_url = base_url.rstrip("/")
        self.streaming_server = streaming_server_url.rstrip("/")
        self.listen_config = ListenConfig(ffmpeg=ffmpeg)
        self.max_bytes = max_bytes
        self.alive = alive
        self._listen = listener or listen
        self._align = aligner or align_isolated
        self._contexts: dict[str, VideoContext] = {}
        self._jobs: dict[str, SyncJob] = {}
        self._lock = threading.Lock()
        # One analysis at a time: the film is playing on the same cores and
        # the same connection.
        self._analysis = threading.Semaphore(1)

    # ------------------------------------------------------------ sessions

    def register(self, session_id: str, source_url: str, info: Any = None, stream: dict[str, Any] | None = None) -> None:
        """A media session was created: remember what the film is."""
        kind = "http"
        if source_url.startswith("file:"):
            kind = "file"
        elif self.streaming_server and source_url.startswith(self.streaming_server + "/"):
            kind = "torrent"
        container = getattr(info, "container", None)
        context = VideoContext(
            session_id=session_id,
            source_url=source_url,
            kind=kind,
            duration=getattr(container, "duration_seconds", None),
            size=getattr(container, "size_bytes", None),
            audio=tuple(
                (getattr(track, "channels", None), getattr(track, "channel_layout", None))
                for track in getattr(info, "audio", ()) or ()
            ),
        )
        if isinstance(stream, dict):
            from ..stremio.addons import parse_stream

            context.stream = parse_stream(stream, stream.get("addonId"))
        with self._lock:
            self._contexts[session_id] = context

    def forget(self, session_id: str) -> None:
        with self._lock:
            self._contexts.pop(session_id, None)
            jobs = [job for job in self._jobs.values() if job.session_id == session_id]
        for job in jobs:
            job.cancel.set()

    def shutdown(self) -> None:
        """Every sync stops: the worker is going."""
        with self._lock:
            jobs = list(self._jobs.values())
        for job in jobs:
            job.cancel.set()

    def context(self, session_id: str) -> VideoContext:
        with self._lock:
            found = self._contexts.get(session_id)
        if found is None:
            raise NotFound("no such media session for subtitles")
        return found

    # -------------------------------------------------------------- prepare

    def prepare(self, body: dict[str, Any]) -> dict[str, Any]:
        session_id = _text(body.get("sessionId"))
        if not session_id:
            raise InvalidRequest("sessionId is required")
        context = self.context(session_id)
        context.type_name = _text(body.get("type"))
        context.item_id = _text(body.get("id"))
        context.video_id = _text(body.get("videoId"))
        if isinstance(body.get("duration"), (int, float)) and body["duration"] > 0:
            context.duration = float(body["duration"])
        self._identify(context)

        found: list[Subtitle] = list(context.stream.subtitles) if context.stream else []
        if context.type_name and context.item_id:
            extra: dict[str, Any] = {}
            if context.video_hash:
                extra["videoHash"] = context.video_hash
            if context.size:
                extra["videoSize"] = context.size
            if context.filename:
                extra["filename"] = context.filename
            try:
                found.extend(
                    self.stremio.subtitles(
                        context.type_name, context.item_id, video_id=context.video_id, extra=extra or None
                    )
                )
            except MediaError as exc:
                LOG.info("subtitle addons unavailable for %s: %s", context.item_id, exc.message)

        languages = [code for code in (canonical(v) for v in body.get("languages") or []) if code]
        ranked = rank(found, languages, context.filename)
        offered: list[Subtitle] = []
        per_language: dict[str | None, int] = {}
        seen_urls: set[str] = set()
        for subtitle in ranked:
            if subtitle.url in seen_urls:
                continue
            seen_urls.add(subtitle.url)
            code = canonical(subtitle.language)
            if per_language.get(code, 0) >= PER_LANGUAGE:
                continue
            per_language[code] = per_language.get(code, 0) + 1
            offered.append(subtitle)
        with self._lock:
            context.candidates = {candidate_id(s): s for s in offered}
            context.order = [candidate_id(s) for s in offered]
        return {
            "video": context.as_dict(),
            "candidates": [describe(s, rank_index) for rank_index, s in enumerate(offered)],
        }

    def _identify(self, context: VideoContext) -> None:
        """Hash, size and name of the file, from what is known without guessing."""
        hints = context.stream.behavior_hints if context.stream else {}
        if isinstance(hints.get("videoHash"), str) and hints["videoHash"].strip():
            context.video_hash = hints["videoHash"].strip().lower()
        if isinstance(hints.get("videoSize"), int) and not isinstance(hints.get("videoSize"), bool):
            context.size = context.size or hints["videoSize"]
        if isinstance(hints.get("filename"), str) and hints["filename"].strip():
            context.filename = hints["filename"].strip()
        if not context.filename:
            name = os.path.basename(urlsplit(context.source_url).path)
            # A torrent's address ends in a file index, not a name.
            if "." in name and context.kind != "torrent":
                context.filename = name
        if not context.video_hash:
            try:
                self._hash(context)
            except (MediaError, OSError) as exc:
                LOG.info("no video hash for %s: %s", context.session_id, exc)
        stream = context.stream
        if context.video_hash and context.size:
            context.identity = f"osh:{context.video_hash}:{context.size}"
        elif stream is not None and stream.info_hash:
            context.identity = f"bt:{stream.info_hash}:{stream.file_idx if stream.file_idx is not None else '-'}"
        elif context.filename and context.size:
            context.identity = f"file:{context.filename}:{context.size}"
        else:
            parts = urlsplit(context.source_url)
            context.identity = "url:" + digest(parts.scheme, parts.netloc, parts.path)

    def _hash(self, context: VideoContext) -> None:
        if context.kind == "torrent":
            answer = self.stremio.server.opensub_hash(context.source_url)
            if answer:
                context.video_hash, size = answer
                context.size = context.size or size
            return
        if context.kind == "file":
            path = urlsplit(context.source_url).path
            size = os.path.getsize(path)
            with open(path, "rb") as handle:
                first = handle.read(65536)
                handle.seek(max(0, size - 65536))
                last = handle.read(65536)
            context.size, context.video_hash = size, opensubtitles_hash(first, last, size)
            return
        head = request(
            context.source_url, headers={"Range": "bytes=0-65535"}, timeout=6.0, max_bytes=70000
        )
        total = _total_size(head.headers.get("content-range"))
        if head.status != 206 or not total:
            return
        tail = request(
            context.source_url,
            headers={"Range": f"bytes={max(0, total - 65536)}-{total - 1}"},
            timeout=6.0,
            max_bytes=70000,
        )
        if tail.status != 206:
            return
        context.size = total
        context.video_hash = opensubtitles_hash(head.body, tail.body, total)

    # ----------------------------------------------------------------- load

    def load(self, body: dict[str, Any]) -> dict[str, Any]:
        context = self.context(_text(body.get("sessionId")) or "")
        wanted = _text(body.get("candidate"))
        with self._lock:
            subtitle = context.candidates.get(wanted or "")
            already = context.loaded.get(wanted or "")
        if subtitle is None:
            raise NotFound("no such subtitle candidate")
        if already:
            document = self.store.get(already)
            if document is not None:
                return self._loaded(already, document, subtitle)
        url = validate_source_url(subtitle.url, self.policy)
        response = request(url, timeout=15.0, max_bytes=8 * 1024 * 1024)
        if response.status >= 400:
            raise UpstreamError(f"the subtitle answered HTTP {response.status}")
        try:
            document = parse(response.body, subtitle.language)
        except SubtitleFormatError as exc:
            raise MediaError("SUBTITLE_UNREADABLE", str(exc), 422) from exc
        key = self.store.put(document)
        with self._lock:
            context.loaded[wanted or ""] = key
        return self._loaded(key, document, subtitle)

    def _loaded(self, key: str, document: Any, subtitle: Subtitle) -> dict[str, Any]:
        return {
            "key": key,
            "url": self.file_url(key, document.extension),
            "format": document.format,
            "cues": len(document.cues),
            "language": canonical(subtitle.language),
        }

    def file_url(self, key: str, extension: str) -> str:
        return f"{self.base_url}/media/subtitles/file/{key}.{extension}"

    def file(self, name: str) -> tuple[bytes, str]:
        key, _, _ = name.partition(".")
        if not valid_key(key):
            raise NotFound("no such subtitle")
        found = self.store.read(key)
        if found is None:
            raise NotFound("no such subtitle")
        return found

    # ----------------------------------------------------------------- sync

    def sync(self, body: dict[str, Any]) -> dict[str, Any]:
        context = self.context(_text(body.get("sessionId")) or "")
        key = _text(body.get("key")) or ""
        if not valid_key(key) or self.store.get(key) is None:
            raise NotFound("no such loaded subtitle")
        if isinstance(body.get("duration"), (int, float)) and body["duration"] > 0:
            context.duration = float(body["duration"])
        position = body.get("position")
        position = float(position) if isinstance(position, (int, float)) and position >= 0 else None
        audio_index = body.get("audioIndex")
        audio_index = audio_index if isinstance(audio_index, int) and audio_index >= 0 else None
        job_id = digest("job", context.session_id, key)[:16]
        with self._lock:
            job = self._jobs.get(job_id)
            if job is not None and job.state == "running":
                job.position = position if position is not None else job.position
                return job.as_dict()
            if job is not None and job.state in ("done", "failed", "cancelled"):
                return job.as_dict()
            job = SyncJob(job_id, context.session_id, key, context.identity, position=position)
            job.duration = context.duration
            self._jobs[job_id] = job
            # Forget finished jobs of films long gone.
            if len(self._jobs) > 64:
                for stale in [j for j in self._jobs.values() if j.state != "running"][:16]:
                    self._jobs.pop(stale.id, None)
        hash_match = any(
            context.loaded.get(cid) == key and subtitle.hash_match
            for cid, subtitle in context.candidates.items()
        )
        threading.Thread(
            target=self._run,
            args=(job, context, audio_index, hash_match),
            name=f"subtitle-sync-{job_id}",
            daemon=True,
        ).start()
        return job.as_dict()

    def job(self, job_id: str) -> dict[str, Any]:
        with self._lock:
            job = self._jobs.get(job_id)
        if job is None:
            raise NotFound("no such sync job")
        return job.as_dict()

    def _run(self, job: SyncJob, context: VideoContext, audio_index: int | None, hash_match: bool) -> None:
        try:
            self._analyse(job, context, audio_index, hash_match)
        except Exception as exc:  # noqa: BLE001 -- a sync never takes the worker down
            if job.cancel.is_set():
                # Stopped under it -- the film ended, or the worker is
                # shutting down and took the engine's process with it.
                job.state = "cancelled"
            else:
                LOG.exception("subtitle sync %s failed", job.id)
                job.state, job.error = "failed", str(exc)
        finally:
            job.finished = time.monotonic()

    def _analyse(self, job: SyncJob, context: VideoContext, audio_index: int | None, hash_match: bool) -> None:
        cached = self.store.result(context.identity, job.key, engine.ALGORITHM)
        if cached and cached.get("result", {}).get("decision") in ("apply", "reject"):
            job.cache = "hit"
            job.result = cached["result"]
            job.apply = self._apply(cached["result"], context, job.key, cached.get("corrected"))
            job.heard = int(cached["result"].get("windows", 0))
            job.state = "done"
            self._log(job, context, "hit")
            return

        document = self.store.get(job.key)
        if document is None:
            job.state, job.error = "failed", "the subtitle is no longer kept"
            return
        cues = [(cue.start, cue.end) for cue in document.cues]
        duration = context.duration or (max(end for _, end in cues) + 60.0)
        seekable = context.kind != "torrent"

        if context.duration and not engine.covers(cues, context.duration):
            # Somebody else's subtitle -- a trailer's, most often. Said at
            # once, so the next candidate is tried now and not after every
            # window has been heard for nothing.
            job.result = engine.SyncResult(
                "rejected", "reject", 0.0, reason="does-not-cover-film"
            ).as_dict()
            job.state = "done"
            self._log(job, context, "miss")
            return

        most = 40
        if context.kind == "http":
            if context.size and context.duration:
                per_window = context.size / context.duration * WINDOW_SECONDS
                most = min(most, int(self.max_bytes / max(1.0, per_window)))
            if most < MIN_AFFORDABLE_WINDOWS:
                job.result = engine.SyncResult(
                    "rejected", "reject", 0.0, reason="source-too-heavy"
                ).as_dict()
                job.state = "done"
                self._log(job, context, "miss")
                return

        track = audio_index or 0
        centre = track < len(context.audio) and has_centre(*context.audio[track])
        # Heard from the centre is not heard from a downmix: kept apart.
        evidence = "centre" if centre else ""
        heard = self.store.speech(context.identity, evidence)
        result: engine.SyncResult | None = self._align(cues, heard, duration) if heard else None
        with self._analysis:
            while not job.cancel.is_set():
                if not self.alive(context.session_id):
                    job.state = "cancelled"
                    return
                spans = engine.next_windows(
                    result,
                    duration,
                    [(w.start, w.end) for w in heard],
                    seekable=seekable,
                    available_until=job.position,
                    length=WINDOW_SECONDS,
                    first=8 if hash_match else 16,
                    most=most,
                )
                if not spans:
                    break
                for start, end in spans:
                    if job.cancel.is_set():
                        break
                    try:
                        window, _ = self._listen(
                            context.source_url,
                            start,
                            end - start,
                            config=self.listen_config,
                            audio_index=audio_index,
                            centre=centre,
                            cancelled=job.cancel.is_set,
                        )
                    except ListenError as exc:
                        LOG.info("subtitle sync %s: nothing heard at %.0f s: %s", job.id, start, exc)
                        # Kept as heard-and-silent, so it is not asked for again.
                        window = engine.Window(start, end, ())
                    heard.append(window)
                    job.heard = len(heard)
                self.store.keep_speech(context.identity, heard, evidence)
                result = self._align(cues, heard, duration)
        if job.cancel.is_set():
            job.state = "cancelled"
            return
        if result is None:
            result = engine.SyncResult("rejected", "gather", 0.0, reason="nothing-to-listen-to")

        answer = result.as_dict()
        waiting = (
            not seekable
            and result.decision == "gather"
            and (job.position or 0.0) < duration * 0.97
        )
        if result.decision == "gather" and not waiting:
            # Everything affordable has been listened to and it is still not
            # sure: that is a no.
            answer["decision"] = "reject"
            answer["model"] = "rejected"
            answer["reason"] = f"unresolved-{result.reason}"
        corrected = None
        if answer["decision"] == "apply" and result.model == "piecewise":
            corrected = self._correct(document, result, context, job.key)
        job.result = answer
        job.apply = self._apply(answer, context, job.key, corrected)
        job.state = "waiting" if waiting else "done"
        if not waiting:
            self.store.keep_result(
                context.identity, job.key, engine.ALGORITHM, {"result": answer, "corrected": corrected}
            )
        self._log(job, context, "miss", result)

    def _correct(self, document: Any, result: engine.SyncResult, context: VideoContext, key: str) -> str:
        """A new timeline for a different cut, kept beside the original."""
        corrected_key = digest("corrected", context.identity, key, engine.ALGORITHM)
        if self.store.path(corrected_key) is None:
            mapper = result.mapping.cue_mapper([cue.start for cue in document.cues])
            self.store.put_as(corrected_key, document.remap(mapper))
        return corrected_key

    def _apply(self, answer: dict[str, Any], context: VideoContext, key: str, corrected: str | None) -> dict[str, Any] | None:
        """What the player has to be told, if anything."""
        if answer.get("decision") != "apply":
            return None
        if answer.get("model") == "piecewise" and corrected:
            document = self.store.get(corrected)
            if document is None:
                return None
            return {"kind": "file", "key": corrected, "url": self.file_url(corrected, document.extension)}
        return {"kind": "properties", "subDelay": answer["offset"], "subSpeed": answer["scale"]}

    def _log(self, job: SyncJob, context: VideoContext, cache: str, result: engine.SyncResult | None = None) -> None:
        answer = job.result or {}
        LOG.info(
            "subtitle sync video=%s subtitle=%s source=%s model=%s offset=%+.3f scale=%.6f "
            "windows=%s inliers=%s confidence=%.2f decision=%s seconds=%.1f cache=%s reason=%s",
            context.identity,
            job.key,
            context.kind,
            answer.get("model"),
            float(answer.get("offset") or 0.0),
            float(answer.get("scale") or 1.0),
            answer.get("windows"),
            answer.get("inliers"),
            float(answer.get("confidence") or 0.0),
            answer.get("decision"),
            time.monotonic() - job.started,
            cache,
            answer.get("reason"),
        )
        if result is not None and result.evidence:
            LOG.debug("subtitle sync %s evidence %s", job.id, result.evidence)


# ------------------------------------------------------------------ helpers

def candidate_id(subtitle: Subtitle) -> str:
    return "ext:" + digest("candidate", subtitle.url)[:16]


def describe(subtitle: Subtitle, rank_index: int) -> dict[str, Any]:
    return {
        "id": candidate_id(subtitle),
        "source": subtitle.source.value,
        "language": canonical(subtitle.language),
        "rawLanguage": subtitle.language,
        "label": subtitle.label,
        "addonId": subtitle.addon_id,
        "hashMatch": subtitle.hash_match,
        "rank": rank_index,
    }


def rank(subtitles: list[Subtitle], languages: list[str], filename: str | None) -> list[Subtitle]:
    """Best candidates first, cheapest evidence first.

    The file's own subtitles lead; then the viewer's languages in their
    order; within a language a hash match (the addon matched this exact
    file) first, then a release name that shares words with the file's, then
    the addon's own order.
    """
    tokens = _tokens(filename)

    def key(item: tuple[int, Subtitle]) -> tuple[Any, ...]:
        index, subtitle = item
        code = canonical(subtitle.language)
        language_rank = languages.index(code) if code in languages else len(languages)
        overlap = len(tokens & _tokens(subtitle.label)) if tokens else 0
        return (
            subtitle.source is not SubtitleSource.STREAM,
            language_rank,
            not subtitle.hash_match,
            -overlap,
            index,
        )

    return [subtitle for _, subtitle in sorted(enumerate(subtitles), key=key)]


def _tokens(name: str | None) -> set[str]:
    if not name:
        return set()
    words = re.split(r"[^0-9a-z]+", name.lower())
    return {word for word in words if len(word) >= 3 and word not in {"mkv", "mp4", "avi", "srt"}}


def _text(value: Any) -> str | None:
    if isinstance(value, str) and value.strip():
        return value.strip()
    return None


def _total_size(content_range: str | None) -> int | None:
    if not content_range or "/" not in content_range:
        return None
    total = content_range.rsplit("/", 1)[1].strip()
    return int(total) if total.isdigit() else None
