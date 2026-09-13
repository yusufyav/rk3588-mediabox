#!/usr/bin/env python3
"""Measure the MediaBox product UI on the television it actually runs on.

Smoothness is not a property of the code, it is a property of this browser on
this GPU driving this panel. So this attaches to the kiosk Chromium already
showing the interface — the one started by `mediabox-tv-ui.service` with
`MEDIABOX_UI_DEBUG_PORT` set — rather than launching a headless copy that would
composite through llvmpipe and answer a different question.

What it reports, and where each number comes from:

* **key → painted focus**  A key is injected with `Input.dispatchKeyEvent`. In
  the page, a capture-phase listener stamps `performance.now()`, and a message
  posted from inside the following `requestAnimationFrame` runs only after that
  frame has been committed. The gap between the two is the time from the press
  to the frame carrying the new focus. Reported alongside the Event Timing
  API's own `keydown` duration, which the browser measures to presentation and
  rounds to 8 ms — two independent sources for the same quantity.
* **frame time**  Deltas between successive `requestAnimationFrame` callbacks
  while the recorder is running. At 60 Hz a frame the compositor kept up with
  is ~16.7 ms; anything past 33 ms means at least one frame was not presented.
* **long frames**  The Long Animation Frame observer, which reports frames the
  renderer blocked on, with the script that did it.
* **RSS / CPU**  Read from `/proc` on this machine for the kiosk's own process
  tree: the compositor, the browser and every renderer it forked.

Run it on the appliance:

    ui-perf.py --scenario all --out /var/tmp/perf.json
"""

from __future__ import annotations

import argparse
import json
import os
import statistics
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from cdp import Devtools, page_target  # noqa: E402

# ------------------------------------------------------------------- keys

KEYS = {
    "ArrowUp": (38, "ArrowUp"),
    "ArrowDown": (40, "ArrowDown"),
    "ArrowLeft": (37, "ArrowLeft"),
    "ArrowRight": (39, "ArrowRight"),
    "Enter": (13, "Enter"),
    "Escape": (27, "Escape"),
}

# The page-side instrument. Installed once per measurement run, and written so
# that re-installing it over a previous copy is harmless.
PROBE = r"""
(() => {
  const M = (window.__mbperf = window.__mbperf || {});
  if (M.installed) { M.reset(); return "reinstalled"; }
  M.installed = true;
  M.keys = [];
  M.frames = [];
  M.loaf = [];
  M.longtasks = [];
  M.evt = [];
  M.proc = [];

  M.reset = () => { M.keys = []; M.frames = []; M.loaf = []; M.longtasks = []; M.evt = []; M.proc = []; };

  document.addEventListener("keydown", (e) => {
    const t0 = performance.now();
    const slot = { key: e.key, t0, raf: null, painted: null };
    M.keys.push(slot);
    requestAnimationFrame(() => {
      slot.raf = performance.now();
      // A message posted from inside the animation-frame callback is delivered
      // after that frame has been committed, so this is the first moment at
      // which the new focus is on the panel rather than merely in the DOM.
      const channel = new MessageChannel();
      channel.port1.onmessage = () => { slot.painted = performance.now(); };
      channel.port2.postMessage(0);
    });
  }, true);

  const observe = (type, sink, extra) => {
    try {
      new PerformanceObserver((list) => { for (const entry of list.getEntries()) sink(entry); })
        .observe(Object.assign({ type, buffered: true }, extra || {}));
    } catch (_) { /* an observer this build does not have is simply absent */ }
  };
  observe("event", (e) => {
    if (e.name !== "keydown") return;
    // `duration` is the browser's own input-to-presentation figure, rounded to
    // 8 ms. `processing` is how long our own handler held the main thread,
    // which is the only part of it this project can shorten.
    M.evt.push(e.duration);
    M.proc.push(e.processingEnd - e.processingStart);
  }, { durationThreshold: 0 });
  observe("longtask", (e) => M.longtasks.push(e.duration));
  observe("long-animation-frame", (e) => M.loaf.push({
    duration: e.duration,
    blocking: e.blockingDuration,
    script: (e.scripts && e.scripts[0] && (e.scripts[0].sourceURL || e.scripts[0].invoker)) || null,
  }));

  let previous = 0;
  const tick = (now) => {
    if (previous) M.frames.push(now - previous);
    previous = now;
    if (M.recording) requestAnimationFrame(tick);
  };
  M.start = () => { M.frames = []; previous = 0; M.recording = true; requestAnimationFrame(tick); };
  M.stop = () => { M.recording = false; };
  return "installed";
})()
"""


def summary(values: list[float]) -> dict:
    if not values:
        return {"n": 0}
    ordered = sorted(values)

    def at(fraction: float) -> float:
        index = min(len(ordered) - 1, int(round(fraction * (len(ordered) - 1))))
        return round(ordered[index], 1)

    return {
        "n": len(ordered),
        "min": round(ordered[0], 1),
        "p50": at(0.50),
        "p95": at(0.95),
        "max": round(ordered[-1], 1),
        "mean": round(statistics.fmean(ordered), 1),
    }


# --------------------------------------------------------------------- box

def kiosk_processes() -> list[tuple[int, str]]:
    """Every process of the television's kiosk: the compositor and the browser."""
    found = []
    for entry in os.listdir("/proc"):
        if not entry.isdigit():
            continue
        try:
            with open(f"/proc/{entry}/cmdline", "rb") as handle:
                raw = handle.read()
        except OSError:
            continue
        if not raw:
            continue
        # Chromium rewrites its own argv to set a process title, which leaves
        # the whole command line in the first NUL-separated field. Splitting on
        # whitespace as well is what makes a renderer recognisable as chromium
        # rather than as "chromium --type=renderer --lots --of --flags".
        first = raw.split(b"\0", 1)[0].split()
        if not first:
            continue
        name = os.path.basename(first[0].decode("utf-8", "replace"))
        if name in ("sway", "swaybg", "chromium", "chrome", "chromium-browser"):
            found.append((int(entry), name))
    return found


def rss_total() -> dict:
    """Resident memory of the kiosk, counting shared pages once.

    Chromium's renderers share a great deal with the browser process, so adding
    their RSS figures together overstates the cost badly. `Pss` — each page
    divided by the number of processes mapping it — is the figure that answers
    "how much of this machine is the kiosk using", so it is the headline and
    the naive sum is kept beside it rather than instead of it.
    """
    pss = 0
    rss = 0
    counted = 0
    for pid, _ in kiosk_processes():
        try:
            with open(f"/proc/{pid}/smaps_rollup") as handle:
                for line in handle:
                    if line.startswith("Pss:"):
                        pss += int(line.split()[1])
                    elif line.startswith("Rss:"):
                        rss += int(line.split()[1])
            counted += 1
        except OSError:
            continue
    return {
        "processes": counted,
        "pss_mib": round(pss / 1024, 1),
        "rss_sum_mib": round(rss / 1024, 1),
    }


def frame_verdict(frames: list[float]) -> dict:
    """Frame times against a 60 Hz budget.

    A 16.67 ms vsync interval is never reported as exactly that, so a threshold
    set at the budget counts ordinary jitter as a miss. 20 ms is the first
    value that cannot be jitter, and 33 ms means a frame was not presented.
    """
    return {
        "summary": summary(frames),
        "count": len(frames),
        "over_20ms": sum(1 for f in frames if f > 20.0),
        "over_33ms": sum(1 for f in frames if f > 33.0),
        "dropped_percent": round(100.0 * sum(1 for f in frames if f > 33.0) / max(1, len(frames)), 2),
    }


def cpu_sample(seconds: float) -> dict:
    """Kiosk CPU over a window, as a percentage of one core."""
    ticks = os.sysconf("SC_CLK_TCK")

    def total() -> int:
        used = 0
        for pid, _ in kiosk_processes():
            try:
                with open(f"/proc/{pid}/stat") as handle:
                    fields = handle.read().rsplit(") ", 1)[1].split()
                used += int(fields[11]) + int(fields[12])  # utime + stime
            except (OSError, IndexError):
                continue
        return used

    before = total()
    start = time.monotonic()
    time.sleep(seconds)
    elapsed = time.monotonic() - start
    delta = total() - before
    return {
        "window_s": round(elapsed, 2),
        "cpu_percent_of_one_core": round(100.0 * (delta / ticks) / elapsed, 1),
    }


# ------------------------------------------------------------------ driver

class Page:
    def __init__(self, port: int) -> None:
        self.devtools = Devtools(page_target(port, time.monotonic() + 30))
        self.devtools.call("Runtime.enable")
        self.devtools.call("Page.enable")

    def js(self, expression: str):
        return self.devtools.evaluate(expression)

    def probe(self) -> None:
        self.js(PROBE)

    def key(self, name: str) -> None:
        code, key = KEYS[name]
        for kind in ("rawKeyDown", "keyUp"):
            self.devtools.call(
                "Input.dispatchKeyEvent",
                {
                    "type": kind,
                    "key": key,
                    "code": key,
                    "windowsVirtualKeyCode": code,
                    "nativeVirtualKeyCode": code,
                },
            )

    def collect(self) -> dict:
        return self.js(
            "JSON.stringify({keys: window.__mbperf.keys, frames: window.__mbperf.frames,"
            " loaf: window.__mbperf.loaf, longtasks: window.__mbperf.longtasks,"
            " evt: window.__mbperf.evt, proc: window.__mbperf.proc})"
        )


def press(page: Page, sequence: list[str], gap: float) -> None:
    for name in sequence:
        page.key(name)
        time.sleep(gap)


def measure_keys(page: Page, sequence: list[str], gap: float, record_frames: bool) -> dict:
    page.probe()
    page.js("window.__mbperf.reset()")
    if record_frames:
        page.js("window.__mbperf.start()")
    time.sleep(0.3)
    press(page, sequence, gap)
    time.sleep(0.5)
    if record_frames:
        page.js("window.__mbperf.stop()")
    raw = json.loads(page.collect())

    painted = [k["painted"] - k["t0"] for k in raw["keys"] if k.get("painted") is not None]
    to_frame = [k["raf"] - k["t0"] for k in raw["keys"] if k.get("raf") is not None]
    frames = [f for f in raw["frames"] if f > 0]
    result = {
        "presses": len(raw["keys"]),
        "key_to_painted_ms": summary(painted),
        "key_to_frame_start_ms": summary(to_frame),
        "event_timing_keydown_ms": summary(raw["evt"]),
        "handler_ms": summary(raw.get("proc", [])),
        "longtasks_over_50ms": len(raw["longtasks"]),
        "long_animation_frames": raw["loaf"][:6],
        "long_animation_frame_count": len(raw["loaf"]),
    }
    if record_frames:
        result["frames"] = frame_verdict(frames)
    return result


def image_stats(page: Page) -> dict:
    return json.loads(page.js(r"""
      JSON.stringify((() => {
        const images = performance.getEntriesByType("resource")
          .filter(r => r.initiatorType === "img" || r.initiatorType === "css");
        const bytes = images.reduce((sum, r) => sum + (r.transferSize || 0), 0);
        const decoded = images.reduce((sum, r) => sum + (r.decodedBodySize || 0), 0);
        const nodes = document.querySelectorAll(".card-art img, img.card-img").length;
        return {
          requests: images.length,
          transfer_bytes: bytes,
          decoded_bytes: decoded,
          img_elements: document.images.length,
          card_images: nodes,
          last_finish_ms: Math.round(images.reduce((m, r) => Math.max(m, r.responseEnd), 0)),
        };
      })())
    """))


def wait_for(page: Page, selector: str, timeout: float) -> bool:
    deadline = time.monotonic() + timeout
    query = json.dumps(selector)
    while time.monotonic() < deadline:
        if page.js(f"!!document.querySelector({query})"):
            return True
        time.sleep(0.2)
    return False


def scenario_load(page: Page) -> dict:
    """A cold screen: reload, then time how long until Home is usable."""
    # Stamp the document first: `Page.reload` returns before the old one is
    # gone, so without a mark to watch disappear the wait below is answered by
    # the page that is already on screen and times nothing at all.
    page.js("window.__mbgone = true")
    page.devtools.call("Page.reload", {"ignoreCache": True})
    start = time.monotonic()
    while time.monotonic() - start < 30:
        try:
            if not page.js("window.__mbgone === true"):
                break
        except RuntimeError:
            break  # the context went away with the old document, which is the point
        time.sleep(0.05)
    ready = wait_for(page, ".rail-track .card", 90)
    first_card = time.monotonic() - start
    page.probe()
    time.sleep(4.0)
    stats = image_stats(page)
    raw = json.loads(page.collect())
    return {
        "reached_first_card": ready,
        "reload_to_first_card_s": round(first_card, 2),
        "images": stats,
        "longtasks_over_50ms": len(raw["longtasks"]),
        "long_animation_frame_count": len(raw["loaf"]),
        "worst_long_animation_frames": sorted(
            raw["loaf"], key=lambda e: -e["duration"]
        )[:5],
    }


def scenario_rail(page: Page) -> dict:
    """Walking along a rail: the most common thing anybody does with a remote."""
    page.js("window.scrollTo(0, 0)")
    time.sleep(0.4)
    # Land on the first rail before measuring, so the numbers describe rail
    # movement rather than the trip down from the hero.
    press(page, ["ArrowDown", "ArrowDown"], 0.35)
    time.sleep(0.6)
    sequence = ["ArrowRight"] * 14 + ["ArrowLeft"] * 8
    return measure_keys(page, sequence, 0.18, record_frames=True)


def scenario_vertical(page: Page) -> dict:
    """Moving between rails, which scrolls the page as well as the rail."""
    sequence = ["ArrowDown"] * 5 + ["ArrowUp"] * 5
    return measure_keys(page, sequence, 0.32, record_frames=True)


def scenario_detail(page: Page) -> dict:
    """Open a title and come back, the journey that decides whether it feels quick."""
    # Start from a poster in the first rail. Opening whatever focus happened to
    # land on measures a different journey each run, and the one that matters
    # is the one everybody takes: a card on Home.
    page.js("window.scrollTo(0, 0)")
    time.sleep(0.4)
    press(page, ["ArrowDown", "ArrowDown", "ArrowRight", "ArrowRight"], 0.3)
    time.sleep(0.8)
    came_from = page.js(
        "(document.querySelector('.is-focused .card-name')||{}).textContent || null"
    )
    page.probe()
    page.js("window.__mbperf.reset()")
    page.js("window.__mbperf.start()")

    open_start = time.monotonic()
    page.key("Enter")
    opened = wait_for(page, ".detail-title", 30)
    open_seconds = time.monotonic() - open_start
    time.sleep(1.2)

    back_start = time.monotonic()
    page.key("Escape")
    returned = wait_for(page, ".rail-track .card", 20)
    back_seconds = time.monotonic() - back_start
    time.sleep(0.8)
    page.js("window.__mbperf.stop()")

    raw = json.loads(page.collect())
    frames = [f for f in raw["frames"] if f > 0]
    restored = page.js(
        "(() => { const el = document.querySelector('.is-focused');"
        " return el ? (el.querySelector('.card-name')?.textContent"
        " || el.textContent || '').trim().slice(0, 60) : null; })()"
    )
    return {
        "came_from": came_from,
        "detail_opened": opened,
        "open_seconds": round(open_seconds, 2),
        "returned_home": returned,
        "back_seconds": round(back_seconds, 2),
        "focus_after_back": restored,
        "frames": frame_verdict(frames),
        "longtasks_over_50ms": len(raw["longtasks"]),
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", type=int, default=9222)
    parser.add_argument("--out", default=None)
    parser.add_argument(
        "--scenario",
        default="all",
        choices=["all", "load", "rail", "vertical", "detail", "resources"],
    )
    arguments = parser.parse_args()

    page = Page(arguments.port)
    report: dict = {
        "when": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "url": page.js("location.href"),
        "viewport": page.js("`${innerWidth}x${innerHeight}@${devicePixelRatio}`"),
        "user_agent": page.js("navigator.userAgent"),
    }
    want = arguments.scenario

    if want in ("all", "load"):
        report["idle"] = {"cpu": cpu_sample(6.0), "memory": rss_total()}
        report["load"] = scenario_load(page)
    if want in ("all", "rail"):
        report["rail"] = scenario_rail(page)
        report["rail_cpu"] = None
    if want in ("all", "vertical"):
        report["vertical"] = scenario_vertical(page)
    if want in ("all", "detail"):
        report["detail"] = scenario_detail(page)
    if want in ("all", "resources"):
        report["images"] = image_stats(page)
        report["memory_after"] = rss_total()

    if want == "all":
        # CPU while the remote is actually being used, measured over a window
        # in which keys keep arriving rather than over an idle page.
        import threading

        sample: dict = {}
        worker = threading.Thread(target=lambda: sample.update(cpu_sample(8.0)))
        worker.start()
        deadline = time.monotonic() + 8.0
        while time.monotonic() < deadline:
            page.key("ArrowRight")
            time.sleep(0.16)
            page.key("ArrowLeft")
            time.sleep(0.16)
        worker.join()
        report["navigation_cpu"] = sample
        report["memory_after"] = rss_total()

    text = json.dumps(report, indent=2, ensure_ascii=False)
    if arguments.out:
        with open(arguments.out, "w") as handle:
            handle.write(text)
    print(text)


if __name__ == "__main__":
    main()
