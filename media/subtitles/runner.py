"""The sync engine in a process of its own.

The worker is one Python process, and it is also the process that relays a
film's bytes to the player when the source needs a TLS the player lacks.
Alignment is a second or two of pure-Python arithmetic per round on this
board, and inside the worker it would hold the interpreter lock through all
of it while the relay waits its turn. Out here it holds nothing of the
worker's, runs below the player's priority, and can be killed.

    stdin   {"cues": [[start, end], ...], "windows": [...], "duration": s|null,
             "ratios": [r, ...]|null}
    stdout  {"result": SyncResult.as_dict(), "evidence": [...]}

or, with "task": "eligibility", the pre-filter (`eligibility.evaluate`):

    stdin   {"task": "eligibility", "cues": [...], "reference": {...},
             "videoRate": f|null, "duration": s|null, "reversedCues": n,
             "names": [...]}
    stdout  {"verdict": Verdict.as_dict()}
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path
from typing import Sequence

from . import eligibility
from .sync import FRAME_RATE_RATIOS, Interval, SyncResult, Window, align


def main() -> int:
    try:
        os.nice(10)
    except OSError:
        pass
    request = json.load(sys.stdin)
    if request.get("task") == "cross_check":
        first, second = (_reference(r) for r in request["references"])
        json.dump({"quality": eligibility.cross_check(first, second, request.get("videoRate"))}, sys.stdout)
        return 0
    if request.get("task") == "eligibility":
        verdict = eligibility.evaluate(
            [(float(a), float(b)) for a, b in request["cues"]],
            _reference(request["reference"]),
            request.get("videoRate"),
            request.get("duration"),
            reversed_cues=int(request.get("reversedCues") or 0),
            names=tuple(request.get("names") or ()),
        )
        json.dump({"verdict": verdict.as_dict()}, sys.stdout)
        return 0
    windows = [
        Window(float(w["start"]), float(w["end"]), tuple((float(a), float(b)) for a, b in w["speech"]))
        for w in request["windows"]
    ]
    cues = [(float(a), float(b)) for a, b in request["cues"]]
    ratios = tuple(float(r) for r in request.get("ratios") or FRAME_RATE_RATIOS)
    result = align(cues, windows, duration=request.get("duration"), ratios=ratios)
    json.dump({"result": result.as_dict(), "evidence": result.evidence}, sys.stdout)
    return 0


def _reference(payload: dict) -> "eligibility.Reference":
    return eligibility.Reference(
        payload["kind"],
        tuple(float(t) for t in payload["times"]),
        payload.get("track"),
        payload.get("language"),
        payload.get("codec"),
        payload.get("quality", "single"),
    )


def _reference_payload(reference: "eligibility.Reference") -> dict:
    return {
        "kind": reference.kind,
        "times": list(reference.times),
        "track": reference.track,
        "language": reference.language,
        "codec": reference.codec,
        "quality": reference.quality,
    }


def align_isolated(
    cues: Sequence[Interval],
    windows: Sequence[Window],
    duration: float | None,
    ratios: Sequence[float] = FRAME_RATE_RATIOS,
    *,
    timeout: float = 180.0,
) -> SyncResult:
    """`align`, run in a child process. Raises RuntimeError if it fails."""
    answer = _run(
        {
            "cues": [[a, b] for a, b in cues],
            "windows": [{"start": w.start, "end": w.end, "speech": [[a, b] for a, b in w.speech]} for w in windows],
            "duration": duration,
            "ratios": list(ratios),
        },
        timeout,
    )
    result = SyncResult.from_dict(answer["result"])
    result.evidence = answer.get("evidence", [])
    return result


def evaluate_isolated(
    cues: Sequence[Interval],
    reference: "eligibility.Reference",
    video_rate: float | None,
    duration: float | None,
    *,
    reversed_cues: int = 0,
    names: Sequence[str | None] = (),
    timeout: float = 120.0,
) -> "eligibility.Verdict":
    """`eligibility.evaluate`, run in a child process."""
    answer = _run(
        {
            "task": "eligibility",
            "cues": [[a, b] for a, b in cues],
            "reference": _reference_payload(reference),
            "videoRate": video_rate,
            "duration": duration,
            "reversedCues": reversed_cues,
            "names": [name for name in names if name],
        },
        timeout,
    )
    return eligibility.Verdict.from_dict(answer["verdict"])


def cross_check_isolated(
    first: "eligibility.Reference",
    second: "eligibility.Reference",
    video_rate: float | None,
    *,
    timeout: float = 120.0,
) -> str:
    """`eligibility.cross_check`, run in a child process."""
    answer = _run(
        {"task": "cross_check", "references": [_reference_payload(first), _reference_payload(second)], "videoRate": video_rate},
        timeout,
    )
    return str(answer["quality"])


def _run(request: dict, timeout: float) -> dict:
    payload = json.dumps(request)
    package_root = str(Path(__file__).resolve().parents[2])
    environment = dict(os.environ)
    environment["PYTHONPATH"] = os.pathsep.join(
        part for part in (package_root, environment.get("PYTHONPATH", "")) if part
    )
    try:
        completed = subprocess.run(
            [sys.executable, "-m", "media.subtitles.runner"],
            input=payload.encode("utf-8"),
            capture_output=True,
            timeout=timeout,
            env=environment,
            start_new_session=True,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise RuntimeError(f"the sync engine did not answer: {exc}") from exc
    if completed.returncode != 0:
        tail = completed.stderr.decode("utf-8", "replace").strip().splitlines()[-1:] or ["no output"]
        raise RuntimeError(f"the sync engine failed: {tail[0]}")
    return json.loads(completed.stdout)


if __name__ == "__main__":
    sys.exit(main())
