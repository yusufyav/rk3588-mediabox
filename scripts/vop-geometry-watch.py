#!/usr/bin/env python3
"""Read-only VOP2 plane-geometry watcher for the RK3588 display pipeline.

Why this exists: the Kodi pause/resume horizontal-shift fault has to be split
into "the source moved the picture" versus "the sink moved the picture" before
any patch is justified. The RK3588 vendor DRM driver exposes the geometry the
VOP2 hardware is *actually* programmed with, so that split can be settled by
observation alone -- no code change, no TV menu, no writeback.

The two authorities sampled here are:

  /sys/kernel/debug/dri/0/regs    the VOP2 register file. For each window,
                                  DSP_ST holds the on-screen origin and
                                  DSP_INFO/ACT_INFO the destination/source
                                  extents. A horizontal move by the source
                                  MUST appear as a DSP_ST x change (or a
                                  scaler-factor/offset change).

  /sys/kernel/debug/dri/0/state   the DRM atomic state Kodi last committed.
                                  Sampling both separates "Kodi asked for a
                                  different rectangle" from "the hardware
                                  latched a different rectangle".

Output is one record per *change*: identical consecutive samples are collapsed
to a single line carrying a repeat count and a first/last timestamp, so a
pause/resume transition stands out instead of drowning in heartbeat rows.
"""
import argparse
import re
import struct
import sys
import time
import zlib

REGS = "/sys/kernel/debug/dri/0/regs"
STATE = "/sys/kernel/debug/dri/0/state"

# RK3568/RK3588 VOP2 per-window register offsets, from the vendor driver's
# RK3568_{CLUSTER,ESMART}_* defines. The two window families agree on where
# the geometry lives -- ACT_INFO/DSP_INFO/DSP_ST at 0x20/0x24/0x28 -- but they
# disagree on the buffer pointers, so each gets its own map rather than one
# approximate map that would mislabel Cluster's YRGB_MST.
ESMART_OFFSETS = {
    "CTRL0": 0x00,
    "REGION0_CTRL": 0x10,
    "YRGB_MST": 0x14,
    "CBR_MST": 0x18,
    "VIR": 0x1C,
    "ACT_INFO": 0x20,
    "DSP_INFO": 0x24,
    "DSP_ST": 0x28,
    "SCL_CTRL": 0x2C,
    "SCL_FACTOR_YRGB": 0x30,
    "SCL_FACTOR_CBR": 0x34,
    "SCL_OFFSET": 0x38,
}

CLUSTER_OFFSETS = {
    "CTRL0": 0x00,
    "CTRL1": 0x04,
    "CTRL2": 0x08,
    "YRGB_MST": 0x10,
    "CBR_MST": 0x14,
    "VIR": 0x18,
    "ACT_INFO": 0x20,
    "DSP_INFO": 0x24,
    "DSP_ST": 0x28,
    "SCL_FACTOR_YRGB": 0x30,
    "SCL_FACTOR_CBR": 0x34,
    "SCL_OFFSET": 0x38,
}

WATCHED_WINDOWS = ("Cluster0", "Esmart0")


def offsets_for(name):
    return CLUSTER_OFFSETS if name.startswith("Cluster") else ESMART_OFFSETS

# Blocks captured whole and hashed. A change in the overlay mixer or the video
# port is exactly the kind of thing an OSD appearing on pause would cause, and
# it must be visible even though its register semantics are not decoded here.
WATCHED_BLOCKS = ("OVL", "VP0")

LINE_RE = re.compile(r"^([0-9a-f]{8}):\s+(.*)$")
BLOCK_RE = re.compile(r"^([A-Za-z0-9_]+):$")


def read_regs():
    """Parse the register file into {block: {address: word}}."""
    blocks = {}
    current = None
    with open(REGS, "r") as fh:
        for line in fh:
            line = line.rstrip("\n")
            m = BLOCK_RE.match(line)
            if m:
                current = m.group(1)
                blocks[current] = {}
                continue
            m = LINE_RE.match(line)
            if m and current is not None:
                base = int(m.group(1), 16)
                for i, word in enumerate(m.group(2).split()):
                    blocks[current][base + i * 4] = int(word, 16)
    return blocks


def window_base(block):
    """Lowest address in a block is its register base."""
    return min(block) if block else None


def decode_pair(value):
    """VOP2 packs these as (high<<16)|low with each field biased by -1."""
    return ((value & 0xFFFF) + 1, ((value >> 16) & 0xFFFF) + 1)


def decode_pos(value):
    """DSP_ST packs the on-screen origin as (y<<16)|x, unbiased."""
    return (value & 0xFFFF, (value >> 16) & 0xFFFF)


def sample_regs(blocks):
    """Reduce the register file to the geometry fields under test."""
    out = {}
    for name in WATCHED_WINDOWS:
        block = blocks.get(name)
        if not block:
            continue
        base = window_base(block)
        vals = {k: block.get(base + off, 0) for k, off in offsets_for(name).items()}
        act_w, act_h = decode_pair(vals["ACT_INFO"])
        dsp_w, dsp_h = decode_pair(vals["DSP_INFO"])
        pos_x, pos_y = decode_pos(vals["DSP_ST"])
        out[name] = {
            "raw": vals,
            "src": (act_w, act_h),
            "dst": (dsp_w, dsp_h),
            "pos": (pos_x, pos_y),
        }
    for name in WATCHED_BLOCKS:
        block = blocks.get(name)
        if not block:
            continue
        words = [block[a] for a in sorted(block)]
        # crc32, not hash(): the digest is compared across separate runs
        # (arm A versus arm B), so it has to be stable between processes.
        blob = struct.pack("<%dI" % len(words), *words)
        out[name] = {"digest": "%08x" % zlib.crc32(blob)}
    return out


PLANE_RE = re.compile(r"^plane\[(\d+)\]: (\S+)")
CRTC_POS_RE = re.compile(r"^\tcrtc-pos=(\d+)x(\d+)\+(-?\d+)\+(-?\d+)")
SRC_POS_RE = re.compile(r"^\tsrc-pos=([\d.]+)x([\d.]+)\+([\d.-]+)\+([\d.-]+)")


def sample_state():
    """Extract each plane's committed atomic rectangle from the state file."""
    out = {}
    name = None
    try:
        with open(STATE, "r") as fh:
            for line in fh:
                line = line.rstrip("\n")
                m = PLANE_RE.match(line)
                if m:
                    name = m.group(2)
                    out.setdefault(name, {})
                    continue
                if name is None:
                    continue
                m = CRTC_POS_RE.match(line)
                if m:
                    out[name]["crtc"] = tuple(int(g) for g in m.groups())
                    continue
                m = SRC_POS_RE.match(line)
                if m:
                    out[name]["src"] = tuple(float(g) for g in m.groups())
    except OSError as exc:
        out["_error"] = str(exc)
    return out


# The buffer pointers move on every displayed frame, so they are deliberately
# kept out of the geometry fingerprint: including them would emit a record per
# frame and bury the transition being looked for. They are counted separately
# instead, which doubles as the play/pause detector -- a paused pipeline keeps
# scanning out the same buffer, so the count goes flat.
BUFFER_FIELDS = ("YRGB_MST", "CBR_MST")


def fingerprint(regs, state):
    """The comparable identity of a sample; timestamps are deliberately out."""
    parts = []
    for name in WATCHED_WINDOWS:
        w = regs.get(name)
        if w:
            parts.append("%s:%s" % (name, ",".join(
                "%s=%08x" % (k, w["raw"][k]) for k in sorted(w["raw"])
                if k not in BUFFER_FIELDS)))
    for name in WATCHED_BLOCKS:
        b = regs.get(name)
        if b:
            parts.append("%s:%s" % (name, b["digest"]))
    for name in sorted(state):
        s = state[name]
        if "crtc" in s:
            parts.append("state:%s:crtc=%s:src=%s" % (name, s["crtc"], s.get("src")))
    return "|".join(parts)


def render(regs, state):
    """Human-readable body of a change record."""
    lines = []
    for name in WATCHED_WINDOWS:
        w = regs.get(name)
        if not w:
            continue
        regfields = " ".join(
            "%s=%08x" % (k, w["raw"][k]) for k in sorted(w["raw"]))
        lines.append(
            "  %-9s pos=(%d,%d) dst=%dx%d src=%dx%d | %s"
            % (name, w["pos"][0], w["pos"][1], w["dst"][0], w["dst"][1],
               w["src"][0], w["src"][1], regfields))
    for name in WATCHED_BLOCKS:
        b = regs.get(name)
        if b:
            lines.append("  %-9s digest=%s" % (name, b["digest"]))
    for name in sorted(state):
        s = state[name]
        if "crtc" in s:
            c = s["crtc"]
            sr = s.get("src")
            lines.append(
                "  atomic:%-14s crtc=%dx%d+%d+%d src=%s"
                % (name, c[0], c[1], c[2], c[3],
                   ("%gx%g+%g+%g" % sr) if sr else "?"))
    return "\n".join(lines)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--interval", type=float, default=0.04,
                    help="seconds between samples (default 0.04 = 25 Hz)")
    ap.add_argument("--duration", type=float, default=120.0,
                    help="total seconds to watch")
    ap.add_argument("--label", default="watch", help="tag written into the header")
    args = ap.parse_args()

    start = time.time()
    print("# vop-geometry-watch label=%s interval=%.3f duration=%.1f"
          % (args.label, args.interval, args.duration))
    print("# started %s" % time.strftime("%Y-%m-%dT%H:%M:%S%z", time.localtime(start)))
    print("# regs=%s state=%s" % (REGS, STATE))
    sys.stdout.flush()

    last_fp = None
    first_t = None
    last_t = None
    count = 0
    index = 0
    seen_buffers = set()

    def flush_record(regs, state, fp):
        nonlocal index
        if fp is None:
            return
        index += 1
        print("[%03d] t=%.3f..%.3f samples=%d distinct_video_buffers=%d"
              % (index, first_t - start, last_t - start, count, len(seen_buffers)))
        print(render(regs, state))
        sys.stdout.flush()

    held_regs = held_state = None
    while time.time() - start < args.duration:
        now = time.time()
        try:
            regs = sample_regs(read_regs())
        except OSError as exc:
            print("# regs read failed: %s" % exc)
            break
        state = sample_state()
        fp = fingerprint(regs, state)
        if fp != last_fp:
            flush_record(held_regs, held_state, last_fp)
            last_fp = fp
            held_regs, held_state = regs, state
            first_t = now
            count = 0
            seen_buffers = set()
        video = regs.get("Esmart0")
        if video:
            seen_buffers.add(video["raw"]["YRGB_MST"])
        count += 1
        last_t = now
        time.sleep(args.interval)

    flush_record(held_regs, held_state, last_fp)
    print("# ended %s distinct_states=%d"
          % (time.strftime("%Y-%m-%dT%H:%M:%S%z"), index))


if __name__ == "__main__":
    main()
