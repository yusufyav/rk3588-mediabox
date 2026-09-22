#!/usr/bin/env python3
"""What the television's browser is actually doing with a video.

"The video played" is not an answer. This asks the four questions that are,
at the same moment, once a second, and prints them together:

  which decoder   Chromium's own name for the decoder it built for this
                  stream, taken from the DevTools Media domain. This is the
                  only source that distinguishes a hardware decoder from
                  libgav1 running on the CPU -- both play the video.
  which silicon   /proc/mpp_service/load, per block. The RK3588 AV1 decoder is
                  fdc70000.av1d and is a different device from the rkvdec
                  cores that carry H.264, HEVC and VP9, so this says which one
                  woke up. Read only while something is decoding: the file
                  holds its last computed value when no session is open, so a
                  reading taken at idle is the previous run's, not zero.
  what it cost    CPU across every process of the browser, the GPU's load and
                  clock, and the panel's refresh.
  what was lost   totalVideoFrames / droppedVideoFrames from the page itself,
                  plus the presentation cadence: how many display refreshes
                  each decoded frame was held for, which is what judder is.

Runs on the appliance, against the browser that is already on the television:

    browser-video-probe.py --url http://127.0.0.1:8099/av1-8bit --seconds 600 \
                           --label "AV1 4K60 8-bit"

It only needs a debugging port on that browser, which is what
MEDIABOX_BROWSER_DEBUG_PORT in a drop-in gives.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, "/var/tmp")

from cdp import Devtools, WebSocket  # noqa: E402


# ------------------------------------------------------------------ device

def mpp_load() -> dict[str, float]:
    """Per-block VPU load. Absent file means no Rockchip BSP kernel."""
    out: dict[str, float] = {}
    try:
        with open("/proc/mpp_service/load") as f:
            for line in f:
                parts = line.split()
                if len(parts) >= 3 and parts[1] == "load:":
                    out[parts[0]] = float(parts[2].rstrip("%"))
    except OSError:
        pass
    return out


def gpu_load() -> tuple[float, int]:
    try:
        with open("/sys/class/devfreq/fb000000.gpu/load") as f:
            load, _, freq = f.read().strip().partition("@")
        return float(load), int(freq.rstrip("Hz"))
    except (OSError, ValueError):
        return (float("nan"), 0)


def browser_jiffies() -> int:
    """utime+stime across every process of the browser, from /proc."""
    total = 0
    for pid in os.listdir("/proc"):
        if not pid.isdigit():
            continue
        try:
            with open(f"/proc/{pid}/cmdline", "rb") as f:
                cmd = f.read()
            if b"chromium" not in cmd and b"sway" not in cmd:
                continue
            with open(f"/proc/{pid}/stat") as f:
                fields = f.read().rsplit(") ", 1)[1].split()
            total += int(fields[11]) + int(fields[12])
        except (OSError, IndexError, ValueError):
            continue
    return total


def mpp_is_open() -> int:
    """How many processes hold the decoder device open, right now."""
    n = 0
    for pid in os.listdir("/proc"):
        if not pid.isdigit():
            continue
        fd_dir = f"/proc/{pid}/fd"
        try:
            for fd in os.listdir(fd_dir):
                try:
                    if os.readlink(f"{fd_dir}/{fd}") == "/dev/mpp_service":
                        n += 1
                        break
                except OSError:
                    continue
        except OSError:
            continue
    return n


# ------------------------------------------------------------------ browser

def targets(port: int) -> list[dict]:
    with urllib.request.urlopen(f"http://127.0.0.1:{port}/json/list", timeout=8) as b:
        return json.load(b)


class Page:
    """A DevTools page connection that also keeps the events it is sent."""

    def __init__(self, url: str) -> None:
        self._socket = WebSocket(url)
        self._id = 0
        self.events: list[dict] = []

    def call(self, method: str, params: dict | None = None, timeout: float = 30.0) -> dict:
        self._id += 1
        mid = self._id
        self._socket.send(json.dumps({"id": mid, "method": method, "params": params or {}}))
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            msg = json.loads(self._socket.recv())
            if msg.get("id") != mid:
                self.events.append(msg)
                continue
            if "error" in msg:
                raise RuntimeError(f"{method}: {msg['error'].get('message')}")
            return msg.get("result", {})
        raise TimeoutError(f"{method} did not answer in {timeout}s")

    def evaluate(self, expression: str, timeout: float = 30.0):
        r = self.call("Runtime.evaluate",
                      {"expression": expression, "returnByValue": True, "awaitPromise": True},
                      timeout=timeout)
        return r.get("result", {}).get("value")

    def media_messages(self) -> list[str]:
        """What the media stack said about this stream, in order.

        The reason a hardware decoder was refused is only ever here: when
        VideoDecoderPipeline cannot initialise, Blink quietly selects the
        software decoder and the page plays, so the only difference visible
        anywhere else is the fan.
        """
        out: list[str] = []
        for e in self.events:
            method = e.get("method")
            params = e.get("params", {})
            if method == "Media.playerMessagesLogged":
                for m in params.get("messages", []):
                    out.append(f"{m.get('level', '?')}: {m.get('message', '')}")
            elif method == "Media.playerErrorsRaised":
                for m in params.get("errors", []):
                    out.append(f"error: {json.dumps(m)[:400]}")
        return out

    def decoder_names(self) -> dict[str, str]:
        """Every decoder name the Media domain has reported."""
        found: dict[str, str] = {}
        for e in self.events:
            if e.get("method") != "Media.playerPropertiesChanged":
                continue
            for prop in e.get("params", {}).get("properties", []):
                name, value = prop.get("name"), prop.get("value")
                if name in ("kVideoDecoderName", "kIsPlatformVideoDecoder",
                            "kVideoTracks", "kIsVideoDecryptingDemuxerStream",
                            "kFrameUrl", "kResolution"):
                    found[name] = str(value)
        return found

    def close(self) -> None:
        self._socket.close()


# ------------------------------------------------------ the page-side probe
#
# The cadence instrument. requestVideoFrameCallback reports, per decoded frame,
# the display time the compositor gave it; the gap between consecutive frames
# in units of one display refresh is how long that frame stood on the panel.
# A 60 fps video on a 120 Hz panel should give a solid run of 2. On a 144 Hz
# panel the same video can only alternate 2, 3, 2, 3 -- no frame is dropped and
# the motion still stutters, which is the thing this measures and dropped-frame
# counters cannot see.
PROBE = r"""
(() => {
  const v = document.querySelector("video");
  if (!v) return "no video element";
  const M = (window.__mbvideo = window.__mbvideo || {});
  if (M.installed) { M.reset(); return "reinstalled"; }
  M.installed = true;
  M.gaps = [];
  M.last = null;
  M.reset = () => { M.gaps = []; M.last = null; };
  const tick = (now, meta) => {
    if (M.last !== null) M.gaps.push(meta.expectedDisplayTime - M.last);
    M.last = meta.expectedDisplayTime;
    if (v.requestVideoFrameCallback) v.requestVideoFrameCallback(tick);
  };
  if (v.requestVideoFrameCallback) v.requestVideoFrameCallback(tick);
  return "installed";
})()
"""

READ = r"""
(() => {
  const v = document.querySelector("video");
  if (!v) return JSON.stringify({error: "no video"});
  const q = v.getVideoPlaybackQuality ? v.getVideoPlaybackQuality() : {};
  const M = window.__mbvideo || {gaps: []};
  return JSON.stringify({
    t: v.currentTime, paused: v.paused, w: v.videoWidth, h: v.videoHeight,
    total: q.totalVideoFrames || 0, dropped: q.droppedVideoFrames || 0,
    corrupted: q.corruptedVideoFrames || 0,
    gaps: M.gaps.splice(0, M.gaps.length)
  });
})()
"""


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=9222)
    ap.add_argument("--url", required=True)
    ap.add_argument("--seconds", type=float, default=60.0)
    ap.add_argument("--label", default="")
    ap.add_argument("--settle", type=float, default=8.0,
                    help="seconds to let playback start before counting")
    ap.add_argument("--refresh", type=float, default=0.0,
                    help="panel refresh in Hz, for the cadence histogram")
    ap.add_argument("--youtube-quality", default="",
                    help="pin YouTube to one quality, e.g. hd2160. A player "
                         "sized by the page picks its own otherwise, and on a "
                         "television that is not the question being asked.")
    args = ap.parse_args()

    ws = None
    for t in targets(args.port):
        if t.get("type") == "page" and t.get("webSocketDebuggerUrl"):
            ws = t["webSocketDebuggerUrl"]
            break
    if not ws:
        print("no debuggable page", file=sys.stderr)
        return 2

    page = Page(ws)
    page.call("Media.enable")
    page.call("Page.enable")
    page.call("Page.navigate", {"url": args.url})
    time.sleep(args.settle)
    if args.youtube_quality:
        page.evaluate("""(() => {
          const pl = document.querySelector('#movie_player')
                  || document.querySelector('.html5-video-player');
          if (!pl || !pl.setPlaybackQualityRange) return 'no player';
          pl.setPlaybackQualityRange('%s', '%s');
          return 'pinned';
        })()""" % (args.youtube_quality, args.youtube_quality))
        time.sleep(6.0)

    page.evaluate(PROBE)
    page.evaluate("const v=document.querySelector('video'); if(v&&v.paused) v.play();")

    first = json.loads(page.evaluate(READ))
    if "error" in first:
        print(f"page has no video: {args.url}", file=sys.stderr)
        return 2

    j0 = browser_jiffies()
    t0 = time.monotonic()
    gaps: list[float] = []
    cpu_samples: list[float] = []
    gpu_samples: list[tuple[float, int]] = []
    mpp_samples: list[dict[str, float]] = []
    mpp_open: list[int] = []
    last_j = j0
    last_t = t0
    hz = os.sysconf("SC_CLK_TCK")
    last = first

    while time.monotonic() - t0 < args.seconds:
        # Events the browser sent meanwhile are collected by the next call();
        # there is no separate drain, because a timed read on a framed socket
        # can stop in the middle of a frame and lose the stream.
        time.sleep(1.0)
        now = time.monotonic()
        j = browser_jiffies()
        # A renderer that exits between samples takes its jiffies with it, so
        # the running total can go *down*. That is a process leaving, not the
        # browser using negative CPU; the sample is dropped rather than
        # averaged in. Measured once as min -17451%.
        if now > last_t and j >= last_j:
            cpu_samples.append(100.0 * (j - last_j) / hz / (now - last_t))
        last_j, last_t = j, now
        gpu_samples.append(gpu_load())
        mpp_samples.append(mpp_load())
        mpp_open.append(mpp_is_open())
        last = json.loads(page.evaluate(READ))
        gaps.extend(last.get("gaps", []))

    elapsed = time.monotonic() - t0
    decoded = last["total"] - first["total"]
    dropped = last["dropped"] - first["dropped"]
    corrupted = last["corrupted"] - first["corrupted"]
    played = last["t"] - first["t"]

    def stat(xs):
        xs = [x for x in xs if x == x]
        if not xs:
            return (0.0, 0.0, 0.0)
        s = sorted(xs)
        return (s[0], sum(s) / len(s), s[-1])

    print("=" * 70)
    print(f" {args.label or args.url}")
    print("=" * 70)
    print(f"  url                {args.url}")
    print(f"  measured           {elapsed:.0f} s   video advanced {played:.1f} s")
    print(f"  resolution         {last['w']}x{last['h']}")
    yt = page.evaluate("""(() => {
      const pl = document.querySelector('#movie_player')
              || document.querySelector('.html5-video-player');
      if (!pl || !pl.getStatsForNerds) return '';
      const s = pl.getStatsForNerds();
      return [s.codecs || '', s.resolution || '', s.dims_and_frames || ''].join(' | ');
    })()""")
    if yt:
        print(f"  youtube reports    {yt}")
    names = page.decoder_names()
    for k in ("kVideoDecoderName", "kIsPlatformVideoDecoder", "kVideoTracks"):
        if k in names:
            print(f"  {k:<18} {names[k][:150]}")
    print(f"  decoded frames     {decoded}")
    pct = (100.0 * dropped / decoded) if decoded else 0.0
    print(f"  dropped frames     {dropped}  ({pct:.3f}%)")
    print(f"  corrupted frames   {corrupted}")
    if decoded and played > 0:
        print(f"  decode rate        {decoded / elapsed:.2f} fps")

    lo, avg, hi = stat(cpu_samples)
    print(f"  browser CPU        min {lo:.1f}%  avg {avg:.1f}%  max {hi:.1f}%  (of one core)")
    lo, avg, hi = stat([g[0] for g in gpu_samples])
    freqs = sorted({g[1] for g in gpu_samples if g[1]})
    print(f"  GPU                min {lo:.0f}%  avg {avg:.0f}%  max {hi:.0f}%   "
          f"clock {'/'.join(str(f // 1000000) + 'MHz' for f in freqs)}")
    print(f"  holds /dev/mpp_service  {max(mpp_open) if mpp_open else 0} process(es)")

    blocks: dict[str, list[float]] = {}
    for s in mpp_samples:
        for k, v in s.items():
            blocks.setdefault(k, []).append(v)
    for k in sorted(blocks):
        lo, avg, hi = stat(blocks[k])
        if hi > 0.5:
            # /proc/mpp_service/load keeps its last computed value when no
            # session is open, so a figure that never moves across a whole run
            # is the previous run's, not this one's. Say so rather than let it
            # be read as evidence.
            stale = " (unchanged -- stale, no session)" if lo == hi else ""
            print(f"  VPU {k:<22} min {lo:.1f}%  avg {avg:.1f}%  max {hi:.1f}%{stale}")

    msgs = page.media_messages()
    if msgs:
        keep = [m for m in msgs
                if any(k in m for k in ("ecoder", "V4L2", "v4l2", "rror", "allback",
                                        "ardware", "nsupported", "ccelerat"))]
        for m in (keep or msgs)[-12:]:
            print(f"  media log          {m[:180]}")

    if gaps:
        refresh = args.refresh
        if refresh <= 0:
            print(f"  frame interval     avg {sum(gaps) / len(gaps):.2f} ms over {len(gaps)} frames")
        else:
            period = 1000.0 / refresh
            counts: dict[int, int] = {}
            for g in gaps:
                counts[round(g / period)] = counts.get(round(g / period), 0) + 1
            total = sum(counts.values())
            spread = "  ".join(f"{k}x:{100.0 * v / total:.1f}%"
                               for k, v in sorted(counts.items()) if v)
            print(f"  cadence @{refresh:g}Hz    {spread}   ({total} frames)")
            print(f"  frame interval     avg {sum(gaps) / len(gaps):.2f} ms")
    page.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
