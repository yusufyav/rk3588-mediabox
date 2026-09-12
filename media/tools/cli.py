"""`media-core` — the media core without a user interface.

Every decision the appliance makes about a source can be reproduced from a
terminal, which is what makes the policy reviewable:

    media-core inspect  URL
    media-core policy   URL
    media-core rank     URL [URL ...]
    media-core preview  URL
    media-core stremio  status | search Q | catalog TYPE ID | meta TYPE ID |
                        streams TYPE ID | resolve TYPE ID
    media-core session  start URL | status [ID] | stop ID | list
    media-core capabilities

`--json` prints the same structure the HTTP API returns. Without it the output
is the human summary the brief asks for.
"""

from __future__ import annotations

import argparse
import json
import logging
import sys
from typing import Any

from ..api import MediaCore, MediaCoreConfig
from ..errors import MediaError
from ..inspector.model import MediaInfo
from ..policy import decide, decide_preview, rank_sources
from ..policy.audio import AudioAction
from ..policy.decide import PlaybackDecision


# ---------------------------------------------------------------- presentation


def _fps(value: float | None) -> str:
    return "—" if value is None else f"{value:.3f}".rstrip("0").rstrip(".")


def format_media(info: MediaInfo) -> str:
    lines: list[str] = []
    container = info.container
    lines.append("Container")
    lines.append(f"  {container.format_name or 'unknown'}")
    if container.duration_seconds:
        minutes, seconds = divmod(int(container.duration_seconds), 60)
        hours, minutes = divmod(minutes, 60)
        lines.append(f"  {hours:d}:{minutes:02d}:{seconds:02d}")
    if container.bit_rate:
        lines.append(f"  {container.bit_rate // 1000} kbit/s")

    for track in info.video:
        lines.append("")
        lines.append("Video")
        lines.append(f"  {track.codec or '?'} {track.profile or ''}".rstrip())
        lines.append(f"  {track.width or '?'}x{track.height or '?'}  {_fps(track.fps)} fps")
        lines.append(
            f"  {track.pixel_format or '?'}  {track.bit_depth or '?'}-bit  {track.chroma.value}"
        )
        colour = " / ".join(
            part
            for part in (track.color_primaries, track.color_transfer, track.color_matrix)
            if part
        )
        lines.append(f"  {colour or 'no colour metadata'}  ({track.color_range.value} range)")
        lines.append(f"  {track.hdr.value}")
        if track.mastering_display and track.mastering_display.max_luminance:
            lines.append(
                f"  mastering display {track.mastering_display.min_luminance}–"
                f"{track.mastering_display.max_luminance} nits"
            )
        if track.max_cll is not None:
            lines.append(f"  MaxCLL {track.max_cll}  MaxFALL {track.max_fall}")
        if track.dolby_vision:
            dv = track.dolby_vision
            lines.append(
                f"  Dolby Vision profile {dv.profile} level {dv.level} "
                f"bl_compat={dv.bl_signal_compatibility_id}"
            )

    if info.audio:
        lines.append("")
        lines.append("Audio")
        for track in info.audio:
            marks = []
            if track.is_default:
                marks.append("default")
            if track.object_audio:
                marks.append("object audio")
            lines.append(
                f"  [{track.stream_index}] {track.codec or '?'} "
                f"{track.channel_layout or (str(track.channels) + 'ch' if track.channels else '')} "
                f"{track.language or ''} "
                f"{(str(track.bit_rate // 1000) + ' kbit/s') if track.bit_rate else ''}"
                f"{'  (' + ', '.join(marks) + ')' if marks else ''}".rstrip()
            )

    if info.subtitles:
        lines.append("")
        lines.append("Subtitles")
        for track in info.subtitles:
            marks = [m for m in ("default" if track.is_default else "", "forced" if track.is_forced else "") if m]
            lines.append(
                f"  [{track.stream_index}] {track.codec or '?'} {track.language or '??'}"
                f"{'  (' + ', '.join(marks) + ')' if marks else ''}"
            )

    if info.warnings:
        lines.append("")
        lines.append("Notes")
        for warning in info.warnings:
            lines.append(f"  ! {warning}")
    return "\n".join(lines)


_SEVERITY_MARK = {"info": " ", "warning": "~", "risk": "!", "blocking": "x"}


def format_decision(info: MediaInfo, decision: PlaybackDecision) -> str:
    lines: list[str] = []
    video = decision.video.track
    lines.append("Video")
    if video is not None:
        lines.append(f"  {video.codec or '?'} {video.profile or ''}".rstrip())
        lines.append(f"  {video.width}x{video.height}")
        colour = " / ".join(p for p in (video.color_primaries, video.color_transfer) if p)
        if colour:
            lines.append(f"  {colour}")
        lines.append(f"  {decision.video.hdr.value}")
    lines.append(f"  {decision.video.verdict.value.upper()}")

    lines.append("")
    lines.append("Audio")
    audio = decision.audio.track
    if audio is None:
        lines.append("  (no audio track)")
    else:
        label = f"{audio.codec or '?'} {audio.channel_layout or ''}".strip()
        if audio.object_audio:
            label += " (object audio)"
        lines.append(f"  {label}")
        if decision.audio.action is AudioAction.TRANSCODE_AC3:
            lines.append(
                f"  -> AC3 {decision.audio.target_channels}ch "
                f"{(decision.audio.target_bitrate or 0) // 1000} kbit/s"
            )
        else:
            lines.append(f"  {decision.audio.action.value}")

    lines.append("")
    lines.append("Playback")
    lines.append(f"  {decision.mode.value}")
    lines.append("  VIDEO COPY" if decision.video_is_copied else "  UNPLAYABLE")
    if decision.audio.action is AudioAction.TRANSCODE_AC3:
        lines.append("  AUDIO TRANSCODE")
    elif decision.audio.action is AudioAction.PASSTHROUGH:
        lines.append("  AUDIO PASSTHROUGH")
    elif decision.audio.action is AudioAction.DECODE_PCM:
        lines.append("  AUDIO DECODE")

    reasons = (*decision.reasons, *decision.video.reasons, *decision.audio.reasons)
    if reasons:
        lines.append("")
        lines.append("Reasons")
        for reason in reasons:
            mark = _SEVERITY_MARK.get(reason.severity.value, " ")
            lines.append(f"  {mark} {reason.code}: {reason.message}")
    lines.append("")
    lines.append(f"Capability profile: {decision.profile_name}")
    return "\n".join(lines)


# ---------------------------------------------------------------------- output


def emit(payload: Any, text: str, as_json: bool) -> None:
    if as_json:
        print(json.dumps(payload, ensure_ascii=False, indent=2))
    else:
        print(text)


# -------------------------------------------------------------------- commands


def _core(arguments: argparse.Namespace) -> MediaCore:
    return MediaCore(
        MediaCoreConfig(
            streaming_server_url=arguments.streaming_server,
            capability_profile=arguments.profile,
            session_state_path=arguments.state,
            base_url=arguments.base_url,
            allowed_file_prefixes=tuple(arguments.allow_file_prefix or ()),
        )
    )


def cmd_inspect(core: MediaCore, arguments: argparse.Namespace) -> int:
    info = core.inspect_url(arguments.url)
    emit({"media": info.as_dict()}, format_media(info), arguments.json)
    return 0


def cmd_policy(core: MediaCore, arguments: argparse.Namespace) -> int:
    info = core.inspect_url(arguments.url)
    decision = decide(info, core.profile, preferred_language=arguments.language)
    emit(
        {"media": info.as_dict(), "playback": decision.as_dict()},
        format_decision(info, decision),
        arguments.json,
    )
    return 0 if decision.mode.value != "Unsupported" else 3


def cmd_preview(core: MediaCore, arguments: argparse.Namespace) -> int:
    info = core.inspect_url(arguments.url)
    preview = decide_preview(info, core.profile)
    text = [f"Preview: {preview.mode.value}"]
    for reason in preview.reasons:
        text.append(f"  {_SEVERITY_MARK.get(reason.severity.value, ' ')} {reason.code}: {reason.message}")
    emit(preview.as_dict(), "\n".join(text), arguments.json)
    return 0 if preview.mode.value != "Unsupported" else 3


def cmd_rank(core: MediaCore, arguments: argparse.Namespace) -> int:
    probed = []
    failures = []
    for url in arguments.urls:
        try:
            probed.append((url, core.inspect_url(url)))
        except MediaError as exc:
            failures.append({"source": url, "error": exc.as_dict()["error"]})
    ranked = rank_sources(probed, core.profile, preferred_language=arguments.language)
    lines = []
    for position, item in enumerate(ranked, start=1):
        video = item.info.primary_video
        lines.append(
            f"{position}. [{item.tier.name}] {item.decision.mode.value}  "
            f"{video.width if video else '?'}x{video.height if video else '?'} "
            f"{item.decision.video.hdr.value} / {item.decision.audio.action.value}"
        )
        lines.append(f"     {item.identity}")
        for reason in item.decision.video.reasons:
            if reason.severity.value in ("risk", "blocking"):
                lines.append(f"     ! {reason.code}: {reason.message}")
    for failure in failures:
        lines.append(f"?. unprobed: {failure['source']} ({failure['error']['message']})")
    emit(
        {"ranked": [item.as_dict() for item in ranked], "unprobed": failures},
        "\n".join(lines) or "(nothing to rank)",
        arguments.json,
    )
    return 0


def cmd_capabilities(core: MediaCore, arguments: argparse.Namespace) -> int:
    payload = core.profile.as_dict()
    lines = [f"{core.profile.name}", f"  {core.profile.description}", ""]
    lines.append(f"  video codecs        {', '.join(payload['video']['codecs'])}")
    lines.append(f"  bit depths          {payload['video']['bitDepths']}")
    lines.append(f"  chroma              {', '.join(payload['video']['chroma'])}")
    lines.append(f"  hdr                 {', '.join(payload['video']['hdrFormats'])}")
    lines.append(f"  dolby vision        {payload['video']['dolbyVisionPipeline']}")
    lines.append(f"  audio passthrough   {', '.join(payload['audio']['passthroughCodecs'])}")
    lines.append(f"  audio decode        {', '.join(payload['audio']['decodeCodecs'])}")
    lines.append(f"  max PCM channels    {payload['audio']['maxPcmChannels']}")
    lines.append(f"  transcode target    {payload['audio']['transcodeTarget']}")
    lines.append("")
    lines.append("  evidence:")
    for item in payload["evidence"]:
        lines.append(f"    {item}")
    emit(payload, "\n".join(lines), arguments.json)
    return 0


def cmd_stremio(core: MediaCore, arguments: argparse.Namespace) -> int:
    action = arguments.action
    stremio = core.stremio

    if action == "status":
        status = stremio.session_status()
        lines = [
            f"authenticated       {status.authenticated}",
            f"addons              {status.addon_count}",
            f"api reachable       {status.api_reachable}",
            f"streaming server    {status.streaming_server_reachable} "
            f"({status.streaming_server_version or 'n/a'})",
        ]
        lines += [f"  ! {note}" for note in status.notes]
        emit(status.as_dict(), "\n".join(lines), arguments.json)
        return 0

    if action == "addons":
        addons = stremio.addons()
        lines = [f"{a.id:34s} {','.join(a.resources):24s} {a.name}" for a in addons]
        emit({"addons": [a.as_dict() for a in addons]}, "\n".join(lines), arguments.json)
        return 0

    if action == "home":
        rows = stremio.home()
        lines = []
        for row in rows:
            lines.append(f"{row.addon_name} / {row.name} ({row.type}/{row.catalog_id})")
            for item in row.items[:5]:
                lines.append(f"    {item.id:16s} {item.name}")
        emit({"rows": [row.as_dict() for row in rows]}, "\n".join(lines), arguments.json)
        return 0

    if action == "search":
        rows = stremio.search(arguments.query)
        lines = []
        for row in rows:
            lines.append(f"{row.addon_name} / {row.name}")
            for item in row.items[:10]:
                lines.append(f"    {item.type:7s} {item.id:16s} {item.name} ({item.release_info or '?'})")
        emit({"rows": [row.as_dict() for row in rows]}, "\n".join(lines) or "(no results)", arguments.json)
        return 0

    if action == "catalog":
        items = stremio.catalog(arguments.type, arguments.id, limit=arguments.limit)
        lines = [f"{i.id:16s} {i.name} ({i.release_info or '?'})" for i in items]
        emit({"items": [i.as_dict() for i in items]}, "\n".join(lines), arguments.json)
        return 0

    if action == "meta":
        meta = stremio.meta(arguments.type, arguments.id)
        lines = [
            f"{meta.name} ({meta.release_info or '?'})",
            f"  type      {meta.type}",
            f"  rating    {meta.imdb_rating or '?'}",
            f"  runtime   {meta.runtime or '?'}",
            f"  genres    {', '.join(meta.genres)}",
            f"  cast      {', '.join(meta.cast[:6])}",
            f"  videos    {len(meta.videos)}",
        ]
        emit({"meta": meta.as_dict()}, "\n".join(lines), arguments.json)
        return 0

    if action == "streams":
        streams = stremio.streams(arguments.type, arguments.id, arguments.video_id)
        lines = []
        for stream in streams:
            lines.append(
                f"{stream.kind.value:9s} {'playable' if stream.is_playable else 'external':9s} "
                f"{(stream.name or '')[:24]:24s} {stream.identity}"
            )
        emit(
            {"streams": [s.as_dict() for s in streams]},
            "\n".join(lines) or "(no streams)",
            arguments.json,
        )
        return 0

    if action == "resolve":
        streams = [s for s in stremio.streams(arguments.type, arguments.id, arguments.video_id) if s.is_playable]
        if not streams:
            print("no playable stream for this item", file=sys.stderr)
            return 4
        resolved = stremio.resolve(streams[arguments.index])
        lines = [f"{resolved.url}", f"  via {resolved.via}"]
        lines += [f"  ! {note}" for note in resolved.notes]
        emit(resolved.as_dict(), "\n".join(lines), arguments.json)
        return 0

    raise SystemExit(f"unknown stremio action: {action}")


def cmd_session(core: MediaCore, arguments: argparse.Namespace) -> int:
    if arguments.action == "start":
        payload = core._create_session({"url": arguments.url, "startSeconds": arguments.start})
        lines = [
            f"session   {payload['sessionId']}",
            f"mode      {payload['mode']}",
            f"url       {payload['playbackUrl']}",
            f"argv      {' '.join(core.sessions.get(payload['sessionId']).argv)}",
        ]
        emit(payload, "\n".join(lines), arguments.json)
        return 0
    if arguments.action == "list":
        sessions = core.sessions.list()
        lines = [
            f"{s['sessionId']}  {s['mode']:16s} {s['state']:9s} pid={s['pid']} clients={s['clients']}"
            for s in sessions
        ]
        emit({"sessions": sessions}, "\n".join(lines) or "(no sessions)", arguments.json)
        return 0
    if arguments.action == "status":
        payload = core.sessions.get(arguments.id).as_dict()
        emit(payload, json.dumps(payload, indent=2), arguments.json)
        return 0
    if arguments.action == "stop":
        payload = core.sessions.stop(arguments.id, "cli")
        emit(payload, json.dumps(payload), arguments.json)
        return 0
    raise SystemExit(f"unknown session action: {arguments.action}")


# ---------------------------------------------------------------------- parser


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="media-core", description=__doc__)
    parser.add_argument("--json", action="store_true", help="print the API structure")
    parser.add_argument("--streaming-server", default="http://127.0.0.1:11470")
    parser.add_argument("--profile", default=None)
    parser.add_argument("--state", default=None)
    parser.add_argument("--base-url", default="")
    parser.add_argument("--allow-file-prefix", action="append", default=[])
    parser.add_argument("--log-level", default="WARNING")
    sub = parser.add_subparsers(dest="command", required=True)

    inspect_parser = sub.add_parser("inspect", help="normalized MediaInfo for a source")
    inspect_parser.add_argument("url")

    policy_parser = sub.add_parser("policy", help="the playback decision for a source")
    policy_parser.add_argument("url")
    policy_parser.add_argument("--language", default=None)

    preview_parser = sub.add_parser("preview", help="browser preview eligibility")
    preview_parser.add_argument("url")

    rank_parser = sub.add_parser("rank", help="rank several renditions of one title")
    rank_parser.add_argument("urls", nargs="+")
    rank_parser.add_argument("--language", default=None)

    sub.add_parser("capabilities", help="the capability profile in force")

    stremio_parser = sub.add_parser("stremio", help="the headless Stremio adapter")
    stremio_sub = stremio_parser.add_subparsers(dest="action", required=True)
    stremio_sub.add_parser("status")
    stremio_sub.add_parser("addons")
    stremio_sub.add_parser("home")
    search_parser = stremio_sub.add_parser("search")
    search_parser.add_argument("query")
    catalog_parser = stremio_sub.add_parser("catalog")
    catalog_parser.add_argument("type")
    catalog_parser.add_argument("id")
    catalog_parser.add_argument("--limit", type=int, default=None)
    meta_parser = stremio_sub.add_parser("meta")
    meta_parser.add_argument("type")
    meta_parser.add_argument("id")
    streams_parser = stremio_sub.add_parser("streams")
    streams_parser.add_argument("type")
    streams_parser.add_argument("id")
    streams_parser.add_argument("--video-id", default=None)
    resolve_parser = stremio_sub.add_parser("resolve")
    resolve_parser.add_argument("type")
    resolve_parser.add_argument("id")
    resolve_parser.add_argument("--video-id", default=None)
    resolve_parser.add_argument("--index", type=int, default=0)

    session_parser = sub.add_parser("session", help="media sessions")
    session_sub = session_parser.add_subparsers(dest="action", required=True)
    start_parser = session_sub.add_parser("start")
    start_parser.add_argument("url")
    start_parser.add_argument("--start", type=float, default=None)
    session_sub.add_parser("list")
    status_parser = session_sub.add_parser("status")
    status_parser.add_argument("id")
    stop_parser = session_sub.add_parser("stop")
    stop_parser.add_argument("id")
    return parser


_COMMANDS = {
    "inspect": cmd_inspect,
    "policy": cmd_policy,
    "preview": cmd_preview,
    "rank": cmd_rank,
    "capabilities": cmd_capabilities,
    "stremio": cmd_stremio,
    "session": cmd_session,
}


def main(argv: list[str] | None = None) -> int:
    arguments = build_parser().parse_args(argv)
    logging.basicConfig(
        level=getattr(logging, arguments.log_level.upper(), logging.WARNING),
        format="%(levelname)s %(name)s: %(message)s",
    )
    core = _core(arguments)
    try:
        return _COMMANDS[arguments.command](core, arguments)
    except MediaError as exc:
        if arguments.json:
            print(json.dumps(exc.as_dict(), indent=2))
        else:
            print(f"{exc.code}: {exc.message}", file=sys.stderr)
        return 2
    finally:
        core.shutdown()


if __name__ == "__main__":
    sys.exit(main())
