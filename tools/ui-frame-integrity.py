#!/usr/bin/env python3
"""Does a piece of the browser's picture stay put once it has changed?

The fault this answers to is a small region that flickers while nothing is
moving: the last letter typed into the address bar, a seek-bar thumbnail under
a still pointer, a dropdown held open by a hover. All three are the same shape
of complaint -- something changes, and then alternates between its new and its
old contents.

A browser cannot be asked about this, because the question is what reached the
screen and the browser's own answer comes from the same bookkeeping that is
under suspicion. So the measurement is taken from outside it. `grim` asks the
compositor for the output as it stands, and this samples one small rectangle of
that, over and over, while the region under test is meant to be perfectly
still.

A sample is not free, and it is worth being exact about where the cost is.
`grim -g` reads back only the rectangle asked for; what costs is the screencopy
request itself, which makes the compositor produce a frame -- and this unit runs
with WLR_SCENE_DEBUG_DAMAGE=rerender, so that frame is the whole output redrawn.
Everything below is therefore arranged to take as few samples as will answer the
question, and to get rectangles by asking the page for them rather than by
searching the screen for them. The screen is searched exactly once, for a
magenta box the page draws, and that one search settles where the page sits
under the browser's own furniture; after it, every rectangle comes from
getBoundingClientRect.

    ui-frame-integrity.py --trace 8              which swap the GPU process makes
    ui-frame-integrity.py --trials 40            does a changed region stay changed
    ui-frame-integrity.py --pointer 6            is the pointer taken away again
    ui-frame-integrity.py --hover --watch 10     does a hover popover blink
    ui-frame-integrity.py --youtube <url>        the same, on the real site
    ui-frame-integrity.py --cost 20              what a frame costs

Run it on the box; it talks to 127.0.0.1 and shells out to the compositor.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import statistics
import subprocess
import sys
import threading
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import cdp  # noqa: E402

ENV: dict = {}


# --------------------------------------------------------------- compositor

def wayland_env() -> dict:
    """The compositor's socket, found rather than assumed.

    The browser's sway runs under a unit with a RuntimeDirectory of its own, so
    the socket is not where a logged-in user's would be.
    """
    env = dict(os.environ)
    for base in ("/run/mediabox-browser", "/run/user/0", "/run/mediabox"):
        directory = Path(base)
        if not directory.is_dir():
            continue
        sockets = [e for e in sorted(directory.glob("wayland-*")) if e.suffix != ".lock"]
        if not sockets:
            continue
        env["XDG_RUNTIME_DIR"] = base
        env["WAYLAND_DISPLAY"] = sockets[0].name
        ipc = sorted(directory.glob("sway-ipc.*.sock"))
        if ipc:
            env["SWAYSOCK"] = str(ipc[-1])
        return env
    if env.get("WAYLAND_DISPLAY") and env.get("XDG_RUNTIME_DIR"):
        return env
    raise SystemExit("no Wayland socket found -- is the browser holding the display?")


def capture(geometry: str | None = None) -> tuple[int, int, bytes]:
    """One screenshot, as raw RGB, straight out of the compositor."""
    command = ["grim", "-t", "ppm"]
    if geometry:
        command += ["-g", geometry]
    command += ["-"]
    blob = subprocess.run(command, env=ENV, capture_output=True, check=True).stdout
    return parse_ppm(blob)


def parse_ppm(blob: bytes) -> tuple[int, int, bytes]:
    if not blob.startswith(b"P6"):
        raise RuntimeError("grim did not return a binary PPM")
    fields, offset = [], 2
    while len(fields) < 3:
        while offset < len(blob) and blob[offset:offset + 1].isspace():
            offset += 1
        if blob[offset:offset + 1] == b"#":
            while blob[offset:offset + 1] not in (b"\n", b""):
                offset += 1
            continue
        start = offset
        while offset < len(blob) and not blob[offset:offset + 1].isspace():
            offset += 1
        fields.append(int(blob[start:offset]))
    width, height, _maxval = fields
    return width, height, blob[offset + 1:]


def swaymsg(*args: str) -> None:
    subprocess.run(["swaymsg", *args], env=ENV, capture_output=True, check=True)


def wtype(*args: str) -> None:
    subprocess.run(["wtype", *args], env=ENV, check=True)


# ------------------------------------------------------------- coordinates

RUN = b"\xff\x00\xff" * 32          # thirty-two magenta pixels in a row
BLOCK = 64                          # pixels, the grain the screen is read at


def find_locator() -> tuple[int, int, int, int]:
    """Where the test page's magenta box landed on the screen.

    The only search of the screen this tool does. Between the page's own
    coordinates and the screen's sit a device scale factor, the window's
    position and whatever the browser draws above the page; one element whose
    place is known in both settles all of it, once, and everything after that
    comes from getBoundingClientRect.

    Neither a magenta pixel nor a magenta row is enough to go on. Film has pure
    magenta in it, and the 4K clip this is often run beside has whole runs of
    it: looking for a long run and taking the bounding box of every row that
    had one put the box at 2184 pixels wide, spanning the test and the film
    together. So the screen is read as a grid of blocks instead, blocks that
    contain a run are grouped by where they touch, and the group that is square
    is the box the page drew. Substring searches over slices, so the whole thing
    is a few thousand C-speed tests rather than a walk over four million pixels.
    """
    width, height, pixels = capture()
    stride = width * 3
    columns = (width + BLOCK - 1) // BLOCK
    hits: set[tuple[int, int]] = set()
    for y in range(0, height, 4):
        row = pixels[y * stride:(y + 1) * stride]
        if RUN not in row:
            continue
        for column in range(columns):
            # One block wider than the block itself, so a run lying across a
            # boundary is seen by the block it starts in.
            piece = row[column * BLOCK * 3:(column + 2) * BLOCK * 3]
            if RUN in piece:
                hits.add((column, y))
    if not hits:
        raise SystemExit("the locator box is not on the screen -- is the test page up?")

    # Group the blocks that touch, four-connected on the grid the screen was
    # read at.
    remaining = set(hits)
    groups = []
    while remaining:
        seed = remaining.pop()
        group, edge = {seed}, [seed]
        while edge:
            column, y = edge.pop()
            for neighbour in ((column + 1, y), (column - 1, y),
                              (column, y + 4), (column, y - 4)):
                if neighbour in remaining:
                    remaining.remove(neighbour)
                    group.add(neighbour)
                    edge.append(neighbour)
        groups.append(group)

    for group in sorted(groups, key=len, reverse=True):
        rows = sorted({y for _c, y in group})
        top, bottom = rows[0], rows[-1]
        middle = pixels[(top + bottom) // 2 * stride:((top + bottom) // 2 + 1) * stride]
        first_column = min(c for c, _y in group)
        last_column = max(c for c, _y in group)
        window = middle[first_column * BLOCK * 3:(last_column + 2) * BLOCK * 3]
        if RUN not in window:
            continue
        left = first_column * BLOCK + window.find(RUN) // 3
        right = first_column * BLOCK + (window.rfind(RUN) + len(RUN)) // 3
        box = (left, top, right - left, bottom - top + 1)
        if box[2] >= 64 and 0.85 < box[2] / max(box[3], 1) < 1.18:
            return box
    raise SystemExit("nothing on the screen has the shape of the locator box")


def page_to_screen(page) -> tuple[float, float, float]:
    """scale, x offset, y offset -- from the page's coordinates to the screen's.

    The page is held still for the one screenshot this needs: a clip playing in
    the corner is the one thing on the screen that can be mistaken for the box.
    """
    page.evaluate("window.mbxAnim(false)")
    page.evaluate("window.mbxVideo(false)")
    time.sleep(0.4)
    try:
        x, y, w, _h = find_locator()
    finally:
        page.evaluate("window.mbxAnim(true)")
        page.evaluate("window.mbxVideo(true)")
    css = page.evaluate("window.mbxRect('target')")
    scale = w / css["w"]
    return scale, x - css["x"] * scale, y - css["y"] * scale


def element_on_screen(page, element, mapping) -> tuple[int, int, int, int]:
    scale, dx, dy = mapping
    r = page.evaluate(f"window.mbxRect({element!r})")
    return (round(r["x"] * scale + dx), round(r["y"] * scale + dy),
            round(r["w"] * scale), round(r["h"] * scale))


def geometry_of(box: tuple[int, int, int, int], inset: int = 0) -> str:
    x, y, w, h = box
    return f"{x + inset},{y + inset} {max(1, w - 2 * inset)}x{max(1, h - 2 * inset)}"


# --------------------------------------------------------------- arithmetic

def mean_luma(pixels: bytes) -> float:
    """Brightness over a rectangle, in whole-array steps rather than per pixel."""
    count = len(pixels) // 3
    if not count:
        return 0.0
    return (sum(pixels[0::3]) * 299 + sum(pixels[1::3]) * 587
            + sum(pixels[2::3]) * 114) / (count * 1000.0)


def _edge(a: bytes, b: bytes, low: int, high: int, first: bool) -> int:
    """The first or last index at which two blobs disagree, by bisection.

    Whole-slice comparisons, so the interpreter does not walk the bytes: a
    byte-at-a-time loop over a region costs more than the screenshot of it.
    """
    while high - low > 1:
        middle = (low + high) // 2
        if first:
            low, high = (low, middle) if a[low:middle] != b[low:middle] else (middle, high)
        else:
            low, high = (middle, high) if a[middle:high] != b[middle:high] else (low, middle)
    return low


def diff_box(a: bytes, b: bytes, width: int) -> tuple[int, int, int, int, int]:
    """Where two captures of the same region disagree, and how wide that is.

    The count of distinct pictures alone cannot tell a blinking caret from a
    letter that keeps leaving: both are "two pictures". The shape can -- a caret
    is a column a pixel or two wide, and a letter is not.
    """
    size = min(len(a), len(b))
    if a[:size] == b[:size]:
        return (0, 0, 0, 0, 0)
    first, last = _edge(a, b, 0, size, True), _edge(a, b, 0, size, False)
    top, bottom = (first // 3) // width, (last // 3) // width
    left, right = width, -1
    stride = width * 3
    for row in range(top, min(bottom + 1, size // stride + 1)):
        start, end = row * stride, min((row + 1) * stride, size)
        if start >= end or a[start:end] == b[start:end]:
            continue
        left = min(left, (_edge(a, b, start, end, True) - start) // 3)
        right = max(right, (_edge(a, b, start, end, False) - start) // 3)
    if right < left:
        left, right = 0, width - 1
    # The last number is how far apart the first and last disagreeing bytes are,
    # not how many disagree: bisection finds the ends, not the middle. It is
    # reported as a span for that reason and must not be read as a pixel count.
    return (left, top, right - left + 1, bottom - top + 1, (last - first) // 3 + 1)


# -------------------------------------------------------------------- input

class Jitter:
    """A mouse sitting on a desk under somebody's hand.

    Not synthetic mischief: an optical mouse that is not being moved still
    reports motion, and the complaint was made with one. A pointer warped once
    and then left alone is a pointer nobody has, and it hides the difference
    between a compositor that keeps pointer focus and one that gives it up the
    moment the hand stops.
    """

    def __init__(self, position: str, step: int = 1, period: float = 0.12) -> None:
        self.x, self.y = (int(v) for v in position.split(","))
        self.step, self.period = step, period
        self.stop = threading.Event()
        self.thread = threading.Thread(target=self._run, daemon=True)

    def _run(self) -> None:
        offset = 0
        while not self.stop.is_set():
            offset = self.step - offset
            try:
                swaymsg("seat", "seat0", "cursor", "set",
                        str(self.x + offset), str(self.y + offset))
            except subprocess.CalledProcessError:
                return
            self.stop.wait(self.period)

    def start(self) -> "Jitter":
        self.thread.start()
        return self

    def finish(self) -> None:
        self.stop.set()
        self.thread.join(timeout=2)


# ------------------------------------------------------------------- trials

STATES = (0x20, 0xE0)   # the two colours tools/ui-frame-integrity.html holds


def run_trials(page, geometry, trials, samples, interval, settle) -> dict:
    """Change a region once, then watch it hold.

    If a changed region is not being drawn into every buffer of the window's
    swap chain, the screen shows whichever buffer comes round next -- new
    contents, old contents, new contents -- and it keeps doing so for as long as
    the window produces frames for some other reason. So the test is: change the
    box, leave it alone, and see whether the colour it used to be ever comes
    back after the new one has already been seen. Nothing about pipeline latency
    produces that.
    """
    stale_total = sample_total = pingpong_trials = late_trials = 0
    pictures = []
    previous = None

    for _ in range(trials):
        answer = page.evaluate("window.mbxToggle()")
        want, was = int(answer["colour"][1:3], 16), previous
        previous = want
        time.sleep(settle)

        seen_new, stale, seen = False, 0, set()
        for _ in range(samples):
            _w, _h, pixels = capture(geometry)
            seen.add(hashlib.md5(pixels).hexdigest()[:8])
            nearest = min(STATES, key=lambda s: abs(mean_luma(pixels) - s))
            sample_total += 1
            if nearest == want:
                seen_new = True
            elif was is not None and nearest == was:
                stale += 1
                if seen_new:
                    stale_total += 1
            time.sleep(interval)

        if was is not None and stale and seen_new:
            pingpong_trials += 1
        if was is not None and not seen_new:
            late_trials += 1
        pictures.append(len(seen))

    return {
        "trials": trials,
        "samples": sample_total,
        "stale_samples": stale_total,
        "pingpong_trials": pingpong_trials,
        "never_arrived_trials": late_trials,
        "max_pictures_in_a_trial": max(pictures),
    }


def run_watch(geometry, seconds, interval) -> dict:
    """Count how many different pictures a region that should be still gives."""
    seen: dict[str, int] = {}
    frames: dict[str, bytes] = {}
    width = 0
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        width, _h, pixels = capture(geometry)
        digest = hashlib.md5(pixels).hexdigest()[:8]
        if digest not in seen:
            seen[digest], frames[digest] = 0, pixels
        seen[digest] += 1
        time.sleep(interval)
    order = sorted(seen, key=lambda d: -seen[d])
    differences = []
    for digest in order[1:6]:
        x, y, w, h, n = diff_box(frames[order[0]], frames[digest], width)
        differences.append({"seen": seen[digest], "box": f"{x},{y} {w}x{h}", "span": n})
    return {
        "region": geometry,
        "samples": sum(seen.values()),
        "distinct_pictures": len(seen),
        "counts": [seen[d] for d in order],
        "differences_from_the_commonest": differences,
    }


# -------------------------------------------------------------------- typing

def type_and_locate(where: str, page, mapping) -> str:
    """Type into a text field and say which pixels that field owns.

    The page's own field is measured through the page. The address bar belongs
    to no page and nothing can be asked where it is, so it is found the only way
    something outside a page can be: by typing one more character and looking at
    what changed -- but only within the band of screen above where the page
    starts, and with the comparison done by bisection rather than by walking.
    """
    page.evaluate("window.mbxAnim(false)")
    page.evaluate("window.mbxVideo(false)")
    time.sleep(0.4)

    if where != "omnibox":
        page.evaluate("document.getElementById('field').focus()")
        time.sleep(0.2)
        wtype("mediabox partial swap")
        time.sleep(0.6)
        geometry = geometry_of(element_on_screen(page, "field", mapping))
        page.evaluate("window.mbxAnim(true)")
        page.evaluate("window.mbxVideo(true)")
        return geometry

    wtype("-M", "ctrl", "l", "-m", "ctrl")
    time.sleep(0.4)
    wtype("mediabox partial swap")
    time.sleep(0.6)

    # The browser's own furniture is everything above where the page starts. The
    # suggestion list the address bar drops open is below the field and inside
    # the page's rows, so it stays out of this by construction.
    band = max(1, int(mapping[2]))
    width = capture()[0]
    strip = f"0,0 {width}x{band}"
    _w, _h, before = capture(strip)
    wtype("W")
    time.sleep(0.6)
    _w, _h, after = capture(strip)
    page.evaluate("window.mbxAnim(true)")
    page.evaluate("window.mbxVideo(true)")

    left, top, w, h, changed = diff_box(before, after, width)
    if not changed:
        raise SystemExit("typing into the address bar changed nothing on the screen")
    # Widened a little, so the caret sitting just past the last letter is inside
    # the sample rather than on its edge.
    left, top = max(0, left - 6), max(0, top - 4)
    return f"{left},{top} {min(width - left, w + 13)}x{min(band - top, h + 9)}"


def ink_width(pixels: bytes, width: int, height: int) -> int:
    """How far to the right the text in a field reaches.

    The complaint is a letter that is there and then is not while the ones
    before it stay. Counting letters is not needed to see that: the rightmost
    column holding anything but the field's own background moves left when a
    letter goes missing, and never moves left on its own while somebody is still
    typing.
    """
    background = pixels[0:3]
    flat = bytes(background * width)
    rightmost = -1
    stride = width * 3
    for y in range(height):
        row = pixels[y * stride:(y + 1) * stride]
        if row == flat[:len(row)]:
            continue
        index = _edge(row, flat[:len(row)], 0, len(row), False)
        rightmost = max(rightmost, index // 3)
    return rightmost


def run_live_typing(page, where, mapping, letters, gap, interval) -> dict:
    """Type, and watch the field while it is still being typed into."""
    x, y, w, h = _parts(type_and_locate(where, page, mapping))
    # The field goes on growing to the right as letters arrive, so the strip
    # watched has to be wider than the one the locator found.
    geometry = f"{x},{y} {min(w * 3, 1200)}x{h}"

    samples: list[int] = []
    regressions: list[dict] = []
    done = threading.Event()

    def typist() -> None:
        for letter in letters:
            wtype(letter)
            time.sleep(gap)
        time.sleep(0.6)
        done.set()

    thread = threading.Thread(target=typist, daemon=True)
    thread.start()
    high = -1
    while not done.is_set():
        cw, ch, pixels = capture(geometry)
        ink = ink_width(pixels, cw, ch)
        if high > 0 and ink < high - 3:
            regressions.append({"from": high, "to": ink, "at": len(samples)})
        high = max(high, ink)
        samples.append(ink)
        time.sleep(interval)
    thread.join()
    return {
        "region": geometry,
        "typed_into": where,
        "letters": len(letters),
        "samples": len(samples),
        "ink_regressions": len(regressions),
        "regressions": regressions[:12],
    }


# ------------------------------------------------------------- pointer focus

def run_pointer(page, mapping, seconds, tremble=False) -> dict:
    """Does the pointer stay where it was put?

    A popover held open by a hover closes when the browser is told the pointer
    has left. Whether anything told it that is not a matter of opinion: the page
    counts the events it is given, with the pointer standing still throughout.
    """
    x, y, w, _h = element_on_screen(page, "hover", mapping)
    # Cleared before the pointer is moved, not after: the leave this is looking
    # for arrives a millisecond behind the enter. And away first, because
    # warping to where the pointer already is produces no motion, hence no
    # enter, which looks exactly like a compositor that refused to send one.
    page.evaluate("window.mbxPointerLog()")
    swaymsg("seat", "seat0", "cursor", "set", str(x + 40), str(y + 260))
    time.sleep(0.4)
    swaymsg("seat", "seat0", "cursor", "set", str(x + w // 2), str(y + 14))
    shake = Jitter(f"{x + w // 2},{y + 14}").start() if tremble else None
    time.sleep(seconds)
    if shake:
        shake.finish()
    events = page.evaluate("window.mbxPointerLog()") or []
    return {
        "pointer_parked_on": f"{x + w // 2},{y + 14}",
        "seconds": seconds,
        "pointer_held": "trembling, as a hand holds one" if tremble
                        else "moved once, then left completely alone",
        "pointer_events_while_still": [e[0] for e in events],
        "popover_open_at_the_end":
            bool(page.evaluate("document.querySelectorAll('#hover:hover').length")),
    }


def hover_and_locate(page, mapping) -> str:
    """Open the popover with the pointer, and take its rectangle from the page."""
    x, y, w, _h = element_on_screen(page, "hover", mapping)
    swaymsg("seat", "seat0", "cursor", "set", str(x + 40), str(y + 260))
    time.sleep(0.4)
    swaymsg("seat", "seat0", "cursor", "set", str(x + w // 2), str(y + 14))
    time.sleep(0.8)
    if not page.evaluate("document.querySelectorAll('#hover:hover').length"):
        raise SystemExit("the popover did not open -- the pointer never reached the trigger")
    return geometry_of(element_on_screen(page, "pop", mapping), inset=4)


# ------------------------------------------------------------------ youtube

def run_youtube(page, url, seconds, mapping, tremble=False) -> dict:
    """The seek-bar preview, on the site the complaint was made about.

    The mapping from page coordinates to screen ones is measured on the test
    page first and carried here, because it is a property of the window rather
    than of the page in it. Deriving it from innerHeight instead was tried and
    was wrong by about ten pixels -- which does not matter for a popover and
    matters entirely for a progress bar twelve pixels tall.
    """
    page.call("Page.navigate", {"url": url})
    for _ in range(120):
        if page.evaluate("!!document.querySelector('video') && "
                         "document.querySelector('video').readyState >= 2"):
            break
        time.sleep(0.5)
    else:
        raise SystemExit(f"{url} never got as far as a video")
    time.sleep(6)

    bar = page.evaluate("""(() => {
      const bar = document.querySelector('.ytp-progress-bar-container') ||
                  document.querySelector('.ytp-progress-bar');
      if (!bar) return null;
      const r = bar.getBoundingClientRect();
      return {x: r.x, y: r.y, w: r.width, h: r.height,
              dpr: devicePixelRatio, inner: innerHeight};
    })()""")
    if not bar:
        raise SystemExit("this page has no progress bar")

    scale, dx, dy = mapping
    x = int(bar["x"] * scale + dx + bar["w"] * scale * 0.55)
    y = int(bar["y"] * scale + dy + bar["h"] * scale / 2)

    # And checked, because a point computed is not a point that lands: the page
    # is asked what is under it before anything is measured there.
    under = page.evaluate(
        f"(() => {{const e = document.elementFromPoint({(x - dx) / scale:.1f}, "
        f"{(y - dy) / scale:.1f}); return e ? (e.closest('.ytp-progress-bar-container') "
        "? 'progress bar' : e.className.toString().slice(0, 60)) : 'nothing'; })()")

    page.evaluate("""
      window.__pointerLog = [];
      const bar = document.querySelector('.ytp-progress-bar-container') ||
                  document.querySelector('.ytp-progress-bar');
      for (const kind of ['pointerenter', 'pointerleave']) {
        bar.addEventListener(kind, () => window.__pointerLog.push(kind));
      }
      true;
    """)

    # Into the picture first: the player keeps its controls hidden until the
    # pointer is somewhere in the video at all.
    swaymsg("seat", "seat0", "cursor", "set", str(x), str(max(0, y - 300)))
    time.sleep(1.5)
    page.evaluate("window.__pointerLog.length = 0; true")
    swaymsg("seat", "seat0", "cursor", "set", str(x), str(y))
    # A perfectly motionless pointer is not the case the complaint was made in,
    # and on YouTube it cannot even be measured: the player puts its own
    # controls away after a few seconds without a mousemove, whoever is or is
    # not holding pointer focus. With a hand on the mouse the controls stay, and
    # then the only thing that can take the preview away is the pointer leaving.
    shake = Jitter(f"{x},{y}").start() if tremble else None
    time.sleep(seconds)
    if shake:
        shake.finish()

    return {
        "video": page.evaluate(
            "(()=>{const v=document.querySelector('video');"
            "return v.videoWidth+'x'+v.videoHeight+' t='+Math.round(v.currentTime)})()"),
        "pointer_parked_on": f"{x},{y}",
        "what_is_under_the_pointer": under,
        "seconds": seconds,
        "pointer_held": "trembling, as a hand holds one" if tremble
                        else "moved once, then left completely alone",
        "pointer_events_while_still": page.evaluate("window.__pointerLog"),
        # YouTube puts its own controls away when the pointer leaves, so the
        # class it hangs on the player says whether there was anything left to
        # hover over at all. Without controls there is no seek bar, and without
        # a seek bar there is no preview -- which is the complaint, one step
        # earlier than where it was noticed.
        "player_controls_hidden": page.evaluate(
            "document.querySelector('.html5-video-player')"
            ".classList.contains('ytp-autohide')"),
        "seek_preview_still_up": page.evaluate("""(() => {
          const t = document.querySelector('.ytp-tooltip');
          if (!t) return false;
          const s = getComputedStyle(t);
          return s.display !== 'none' && s.visibility !== 'hidden' &&
                 parseFloat(s.opacity || '1') > 0.1;
        })()"""),
    }


# -------------------------------------------------------------------- trace

SWAP_WORDS = ("SwapBuffers", "PostSubBuffer", "SubBuffer", "DrawAndSwap",
              "SwapBuffersWithDamage", "SwapBuffersSkipped", "Present")


def run_trace(port, seconds) -> dict:
    """Which swap the GPU process actually performs, in its own trace.

    A flag on a command line is a request. This is the answer: viz names the
    call it makes, and a window that swaps a sub-rectangle and a window that
    swaps all of itself do not make the same one.
    """
    import urllib.request

    with urllib.request.urlopen(f"http://127.0.0.1:{port}/json/version", timeout=5) as body:
        socket = cdp.WebSocket(json.load(body)["webSocketDebuggerUrl"], timeout=120.0)

    def send(identifier, method, params):
        socket.send(json.dumps({"id": identifier, "method": method, "params": params}))

    send(1, "Tracing.start", {
        "transferMode": "ReturnAsStream",
        "traceConfig": {"includedCategories": ["viz", "gpu", "gpu.service", "cc"]},
    })
    time.sleep(seconds)
    send(2, "Tracing.end", {})

    handle, deadline = None, time.monotonic() + 60
    while time.monotonic() < deadline and handle is None:
        message = json.loads(socket.recv())
        if message.get("method") == "Tracing.tracingComplete":
            handle = message["params"].get("stream")

    names: dict[str, int] = {}
    if handle:
        identifier = 10
        while True:
            identifier += 1
            send(identifier, "IO.read", {"handle": handle, "size": 1 << 20})
            chunk = None
            while chunk is None:
                message = json.loads(socket.recv())
                if message.get("id") == identifier:
                    chunk = message.get("result", {})
            for piece in chunk.get("data", "").split('"name":"')[1:]:
                name = piece.split('"', 1)[0]
                if any(word in name for word in SWAP_WORDS):
                    names[name] = names.get(name, 0) + 1
            if chunk.get("eof"):
                break
        send(99, "IO.close", {"handle": handle})
    socket.close()
    return {"traced_seconds": seconds,
            "swap_events": dict(sorted(names.items(), key=lambda kv: -kv[1]))}


# ---------------------------------------------------------------------- cost

def gpu_load() -> tuple[float, int]:
    try:
        with open("/sys/class/devfreq/fb000000.gpu/load") as handle:
            load, _, clock = handle.read().strip().partition("@")
            return float(load), int(clock.rstrip("Hz")) // 1000000
    except (OSError, ValueError):
        return 0.0, 0


def cpu_jiffies() -> tuple[int, int]:
    """Processor time, browser and compositor counted apart.

    Disabling partial swap moves work about between the two, so one number for
    both would hide exactly what the measurement is for.
    """
    browser = compositor = 0
    for task in Path("/proc").iterdir():
        if not task.name.isdigit():
            continue
        try:
            comm = (task / "comm").read_text().strip()
            if comm not in ("chrome", "chromium", "sway"):
                continue
            fields = (task / "stat").read_text().rsplit(") ", 1)[1].split()
            spent = int(fields[11]) + int(fields[12])
        except (OSError, IndexError, ValueError):
            continue
        if comm == "sway":
            compositor += spent
        else:
            browser += spent
    return browser, compositor


def run_cost(page, seconds, interval) -> dict:
    """What a frame costs, with nothing on the page but one small square.

    The other half of the partial-swap question. A window that redraws only what
    changed does almost no work for a 64-pixel square; a window that redraws all
    of itself does 2560x1440 of work for the same square.
    """
    page.evaluate("window.mbxFrames()")          # discard what is already there
    ticks = os.sysconf("SC_CLK_TCK")
    loads, clocks = [], []
    was_browser, was_compositor = cpu_jiffies()
    start = time.monotonic()
    while time.monotonic() - start < seconds:
        load, clock = gpu_load()
        loads.append(load)
        clocks.append(clock)
        time.sleep(interval)
    elapsed = time.monotonic() - start
    now_browser, now_compositor = cpu_jiffies()
    return {
        "seconds": round(elapsed, 1),
        "gpu_load_avg": round(statistics.mean(loads), 1),
        "gpu_load_max": max(loads),
        "gpu_clock_mhz": sorted({c for c in clocks if c}),
        "browser_cpu_percent": round((now_browser - was_browser) / ticks / elapsed * 100, 1),
        "compositor_cpu_percent":
            round((now_compositor - was_compositor) / ticks / elapsed * 100, 1),
        "frame_ms": page.evaluate("window.mbxFrames()") or {},
    }


# ---------------------------------------------------------------------- main

def _parts(geometry: str) -> tuple[int, int, int, int]:
    position, size = geometry.split(" ")
    x, y = (int(v) for v in position.split(","))
    w, h = (int(v) for v in size.split("x"))
    return x, y, w, h


def open_page(args):
    page = cdp.Devtools(cdp.page_target(args.port, time.monotonic() + 20))
    page.call("Page.enable")
    page.call("Page.navigate", {"url": args.page})
    for _ in range(120):
        if page.evaluate("window.mbxReady === true"):
            break
        time.sleep(0.2)
    else:
        raise SystemExit(f"{args.page} never became ready")
    time.sleep(1.2)
    return page


def summarise_events(events: list) -> str:
    """A tally and a shape, rather than a hundred and twenty-four words.

    What matters about the list is how many times the pointer was taken away
    and given back, which is the rate the thing on screen blinks at.
    """
    if not events:
        return "none"
    counts: dict[str, int] = {}
    for event in events:
        counts[event] = counts.get(event, 0) + 1
    tally = ", ".join(f"{name} x{count}" for name, count in counts.items())
    return f"{tally}  [{' '.join(e.replace('pointer', '') for e in events[:6])} ...]" \
        if len(events) > 6 else tally


PRINT_ORDER = (
    "output", "locator", "sampled", "region", "popover", "typed_into", "jitter",
    "video", "traced_seconds", "letters", "trials", "samples", "stale_samples",
    "pingpong_trials", "never_arrived_trials", "max_pictures_in_a_trial",
    "distinct_pictures", "ink_regressions", "seconds", "pointer_parked_on", "what_is_under_the_pointer",
    "pointer_held", "pointer_events_while_still",
    "popover_open_at_the_end", "player_controls_hidden", "seek_preview_still_up", "gpu_load_avg",
    "gpu_load_max", "gpu_clock_mhz", "browser_cpu_percent",
    "compositor_cpu_percent", "frame_ms",
)


def main() -> int:
    global ENV
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", type=int, default=9222)
    parser.add_argument("--page", default="http://127.0.0.1:8099/ui-frame-integrity.html")
    parser.add_argument("--label", default="")
    parser.add_argument("--trials", type=int, default=0)
    parser.add_argument("--samples", type=int, default=16)
    parser.add_argument("--interval", type=float, default=0.01)
    parser.add_argument("--settle", type=float, default=0.30)
    parser.add_argument("--watch", type=float, default=0.0)
    parser.add_argument("--region", default="", help="x,y,w,h in output coordinates")
    parser.add_argument("--typing", choices=("page", "omnibox"), default="")
    parser.add_argument("--live-typing", choices=("page", "omnibox"), default="")
    parser.add_argument("--letters", default="abcdefghijkl")
    parser.add_argument("--gap", type=float, default=0.35)
    parser.add_argument("--hover", action="store_true")
    parser.add_argument("--pointer", type=float, default=0.0)
    parser.add_argument("--youtube", default="")
    parser.add_argument("--trace", type=float, default=0.0)
    parser.add_argument("--cost", type=float, default=0.0)
    parser.add_argument("--park", default="", help="x,y to leave the pointer on")
    parser.add_argument("--jitter", default="",
                        help="x,y to keep a mouse-like tremble going on, or 'auto' "
                             "to tremble wherever the measurement puts the pointer")
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args()

    ENV = wayland_env()
    if args.park:
        swaymsg("seat", "seat0", "cursor", "set", *args.park.split(","))
        time.sleep(0.4)
    # "auto" means: tremble wherever the measurement decides to put the pointer,
    # which is not known until it has looked. Anything else is a fixed point.
    shaking_where_it_lands = args.jitter == "auto"
    tremble = Jitter(args.jitter).start() if args.jitter and not shaking_where_it_lands else None

    page = None
    try:
        if args.trace:
            result = run_trace(args.port, args.trace)
        elif args.youtube:
            page = cdp.Devtools(cdp.page_target(args.port, time.monotonic() + 20))
            page.call("Page.enable")
            # The window's geometry is measured on the test page first, then
            # carried to the site.
            page.call("Page.navigate", {"url": args.page})
            for _ in range(120):
                if page.evaluate("window.mbxReady === true"):
                    break
                time.sleep(0.2)
            time.sleep(1.0)
            mapping = page_to_screen(page)
            result = run_youtube(page, args.youtube, args.pointer or 8.0, mapping,
                                 shaking_where_it_lands)
        elif args.region and args.watch:
            x, y, w, h = (int(v) for v in args.region.split(","))
            result = run_watch(f"{x},{y} {w}x{h}", args.watch, args.interval)
        elif args.cost:
            page = open_page(args)
            result = run_cost(page, args.cost, max(args.interval, 0.1))
        elif args.pointer:
            page = open_page(args)
            result = run_pointer(page, page_to_screen(page), args.pointer,
                                 shaking_where_it_lands)
        elif args.hover:
            page = open_page(args)
            geometry = hover_and_locate(page, page_to_screen(page))
            result = run_watch(geometry, args.watch or 10.0, args.interval)
            result["popover"] = geometry
        elif args.live_typing:
            page = open_page(args)
            result = run_live_typing(page, args.live_typing, page_to_screen(page),
                                     args.letters, args.gap, args.interval)
        elif args.typing:
            page = open_page(args)
            geometry = type_and_locate(args.typing, page, page_to_screen(page))
            result = run_watch(geometry, args.watch or 8.0, args.interval)
            result["typed_into"] = args.typing
        else:
            page = open_page(args)
            page.evaluate("window.mbxVideo(false)")
            time.sleep(0.4)
            box = find_locator()
            page.evaluate("window.mbxVideo(true)")
            geometry = geometry_of(box, inset=max(8, min(box[2], box[3]) // 4))
            result = run_trials(page, geometry, args.trials or 20, args.samples,
                                args.interval, args.settle)
            result["locator"] = geometry_of(box)
            result["sampled"] = geometry
    finally:
        if tremble:
            tremble.finish()
        if page:
            page.close()

    if args.jitter:
        result["jitter"] = args.jitter
    result["label"] = args.label
    if args.json:
        print(json.dumps(result))
        return 0

    print(f"  {'label':24s} {args.label or '-'}")
    for key in PRINT_ORDER:
        if key not in result:
            continue
        value = result[key]
        if key == "pointer_events_while_still":
            rate = ""
            leaves = value.count("pointerleave")
            if leaves and result.get("seconds"):
                rate = f"   -- taken away {leaves / result['seconds']:.1f} times a second"
            print(f"  {key:24s} {summarise_events(value)}{rate}")
            continue
        print(f"  {key:24s} {value}")
    for name, count in result.get("swap_events", {}).items():
        print(f"  {'  ' + name:24s} {count}")
    if "counts" in result:
        print(f"  {'picture counts':24s} {result['counts']}")
    for r in result.get("regressions", []):
        print(f"  {'  ink went back':24s} {r['from']} -> {r['to']} px at sample {r['at']}")
    for d in result.get("differences_from_the_commonest", []):
        print(f"  {'  differs in':24s} {d['box']}  (seen {d['seen']}x, "
              f"{d['span']} px between the outermost changes)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
