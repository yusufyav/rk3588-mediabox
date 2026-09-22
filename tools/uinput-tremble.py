#!/usr/bin/env python3
"""A mouse that is not being moved, made out of a kernel device.

The question this exists for is what sway does with pointer focus while a hand
rests on a mouse. Warping the cursor with `swaymsg seat ... cursor set` looked
like an answer and is not one: it moves the pointer without going through
libinput, so whatever the compositor counts as input activity does not count
it, and a measurement made that way says more about the instrument than about
the setting under test.

So this makes a real one. /dev/uinput creates a relative pointing device the
kernel publishes in /dev/input like any other; libinput picks it up, sway sees
motion the way it sees a mouse, and its idle bookkeeping is exercised for real.
The motion is one pixel back and forth, which is what an optical mouse under a
still hand reports.

    uinput-tremble.py 10           tremble for ten seconds
    uinput-tremble.py 10 --period 0.1
"""

from __future__ import annotations

import argparse
import fcntl
import struct
import time

UINPUT = "/dev/uinput"

UI_DEV_CREATE = 0x5501
UI_DEV_DESTROY = 0x5502
UI_SET_EVBIT = 0x40045564
UI_SET_KEYBIT = 0x40045565
UI_SET_RELBIT = 0x40045566

EV_SYN, EV_KEY, EV_REL = 0x00, 0x01, 0x02
SYN_REPORT = 0
REL_X, REL_Y = 0x00, 0x01
BTN_LEFT = 0x110

# name[80] + input_id(4 x u16) + ff_effects_max + four 64-long __s32 arrays
USER_DEV = struct.Struct("<80s4HI" + "64i" * 4)
EVENT = struct.Struct("<qqHHi")


def event(handle, kind: int, code: int, value: int) -> None:
    handle.write(EVENT.pack(0, 0, kind, code, value))
    handle.flush()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("seconds", type=float)
    parser.add_argument("--period", type=float, default=0.1,
                        help="seconds between one-pixel moves")
    parser.add_argument("--name", default="mediabox tremble")
    args = parser.parse_args()

    with open(UINPUT, "wb", buffering=0) as handle:
        for code in (EV_KEY, EV_REL):
            fcntl.ioctl(handle, UI_SET_EVBIT, code)
        fcntl.ioctl(handle, UI_SET_KEYBIT, BTN_LEFT)
        for code in (REL_X, REL_Y):
            fcntl.ioctl(handle, UI_SET_RELBIT, code)
        handle.write(USER_DEV.pack(args.name.encode()[:79], 0x03, 0x1234, 0x5678, 1,
                                   0, *([0] * 256)))
        fcntl.ioctl(handle, UI_DEV_CREATE)
        # libinput has to notice the device and the compositor has to add it
        # before anything sent through it means anything.
        time.sleep(1.2)
        print(f"a pointer called {args.name!r} exists; trembling for {args.seconds}s",
              flush=True)

        direction = 1
        deadline = time.monotonic() + args.seconds
        while time.monotonic() < deadline:
            event(handle, EV_REL, REL_X, direction)
            event(handle, EV_REL, REL_Y, direction)
            event(handle, EV_SYN, SYN_REPORT, 0)
            direction = -direction
            time.sleep(args.period)

        fcntl.ioctl(handle, UI_DEV_DESTROY)
    print("gone", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
