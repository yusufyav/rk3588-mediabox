"""The sync engine in a process of its own.

The worker is one Python process, and it is also the process that relays a
film's bytes to the player when the source needs a TLS the player lacks.
Alignment is a second or two of pure-Python arithmetic per round on this
board, and inside the worker it would hold the interpreter lock through all
of it while the relay waits its turn. Out here it holds nothing of the
worker's, runs below the player's priority, and can be killed.

    stdin   {"cues": [[start, end], ...], "windows": [...], "duration": s|null}
    stdout  {"result": SyncResult.as_dict(), "evidence": [...]}
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path
from typing import Sequence

from .sync import Interval, SyncResult, Window, align


def main() -> int:
    try:
        os.nice(10)
    except OSError:
        pass
    request = json.load(sys.stdin)
    windows = [
        Window(float(w["start"]), float(w["end"]), tuple((float(a), float(b)) for a, b in w["speech"]))
        for w in request["windows"]
    ]
    cues = [(float(a), float(b)) for a, b in request["cues"]]
    result = align(cues, windows, duration=request.get("duration"))
    json.dump({"result": result.as_dict(), "evidence": result.evidence}, sys.stdout)
    return 0


def align_isolated(
    cues: Sequence[Interval],
    windows: Sequence[Window],
    duration: float | None,
    *,
    timeout: float = 180.0,
) -> SyncResult:
    """`align`, run in a child process. Raises RuntimeError if it fails."""
    payload = json.dumps(
        {
            "cues": [[a, b] for a, b in cues],
            "windows": [{"start": w.start, "end": w.end, "speech": [[a, b] for a, b in w.speech]} for w in windows],
            "duration": duration,
        }
    )
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
    answer = json.loads(completed.stdout)
    result = SyncResult.from_dict(answer["result"])
    result.evidence = answer.get("evidence", [])
    return result


if __name__ == "__main__":
    sys.exit(main())
