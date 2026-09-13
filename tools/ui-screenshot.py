#!/usr/bin/env python3
"""Capture the MediaBox product UI exactly as a browser renders it.

Acceptance for an interface is visual, and a screenshot taken before the
catalogue has arrived proves nothing. So this drives Chromium over the
DevTools protocol: it navigates, waits for an element the screen only produces
once real data is on it, and only then captures.

Chromium is the one the appliance itself runs, and the URL is the one the
television's kiosk loads, so what comes out is the product as shipped rather
than a mock-up of it.

Only the standard library is used, through the small DevTools client in
`cdp.py`, because the appliance has no package manager state worth disturbing
for a test tool.

    ui-screenshot.py --url http://127.0.0.1:8787/ --out home.png \
        --wait '.rail-track .card' --width 1920 --height 1080
"""

from __future__ import annotations

import argparse
import base64
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from cdp import Devtools, browser_binary, page_target  # noqa: E402


def capture(arguments: argparse.Namespace) -> None:
    profile = tempfile.mkdtemp(prefix="mediabox-shot-")
    command = [
        browser_binary(),
        "--headless",
        "--no-sandbox",
        "--disable-gpu",
        "--disable-background-networking",
        "--disable-component-update",
        "--disable-sync",
        "--no-first-run",
        "--no-default-browser-check",
        "--hide-scrollbars",
        f"--user-data-dir={profile}",
        f"--remote-debugging-port={arguments.port}",
        f"--window-size={arguments.width},{arguments.height}",
        "about:blank",
    ]
    browser = subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    devtools = None
    try:
        deadline = time.monotonic() + 40
        devtools = Devtools(page_target(arguments.port, deadline))
        devtools.call("Page.enable")
        devtools.call("Runtime.enable")
        # An exact device size matters: this is a 1920x1080 television, and a
        # layout captured at some other width is not the one people will see.
        devtools.call(
            "Emulation.setDeviceMetricsOverride",
            {
                "width": arguments.width,
                "height": arguments.height,
                "deviceScaleFactor": 1,
                "mobile": arguments.mobile,
            },
        )
        devtools.call("Page.navigate", {"url": arguments.url})

        if arguments.wait:
            selector = json.dumps(arguments.wait)
            deadline = time.monotonic() + arguments.wait_timeout
            while time.monotonic() < deadline:
                if devtools.evaluate(f"!!document.querySelector({selector})"):
                    break
                time.sleep(0.25)
            else:
                raise SystemExit(f"timed out waiting for {arguments.wait!r} on {arguments.url}")
        else:
            time.sleep(arguments.settle)

        for step in arguments.script or []:
            devtools.evaluate(step)
            time.sleep(arguments.settle)

        # Artwork is fetched after the markup exists; capturing before it has
        # decoded would show the layout without the thing the layout is for.
        devtools.evaluate(
            "Promise.all(Array.from(document.images)"
            ".filter(i => !i.complete)"
            ".map(i => new Promise(r => { i.onload = i.onerror = r; })))"
        )
        time.sleep(arguments.settle)

        shot = devtools.call(
            "Page.captureScreenshot", {"format": "png", "captureBeyondViewport": False}
        )
        data = base64.b64decode(shot["data"])
        os.makedirs(os.path.dirname(os.path.abspath(arguments.out)) or ".", exist_ok=True)
        with open(arguments.out, "wb") as handle:
            handle.write(data)
        print(f"{arguments.out}  {len(data)} bytes  {arguments.width}x{arguments.height}")
    finally:
        if devtools is not None:
            devtools.close()
        browser.terminate()
        try:
            browser.wait(timeout=10)
        except subprocess.TimeoutExpired:
            browser.kill()
        shutil.rmtree(profile, ignore_errors=True)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--width", type=int, default=1920)
    parser.add_argument("--height", type=int, default=1080)
    parser.add_argument("--mobile", action="store_true")
    parser.add_argument("--port", type=int, default=9333)
    parser.add_argument("--wait", default=None, help="CSS selector that means the screen is ready")
    parser.add_argument("--wait-timeout", type=float, default=90.0)
    parser.add_argument("--settle", type=float, default=1.2)
    parser.add_argument(
        "--script",
        action="append",
        help="JavaScript to run before capturing (repeatable, in order)",
    )
    capture(parser.parse_args())


if __name__ == "__main__":
    main()
