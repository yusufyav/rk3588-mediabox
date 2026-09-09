#!/usr/bin/env python3
"""hdmi-audio-probe - Gate MA0 HDMI PCM audio bring-up and capability probe.

Gate MP0 recorded that no ELD file exists under /proc/asound and left "does
HDMI audio work at all" open. This tool answers that from the ALSA API rather
than from the presence of a file, because on this vendor BSP the ELD is not a
file: it is a 128-byte ALSA control on the HDMI card, and it is populated.

Scope is deliberately PCM only. Compressed passthrough -- AC-3, E-AC-3, DTS,
TrueHD, DTS-HD MA, Atmos, IEC61937/HBR -- belongs to Gate MA1 and is not
implemented here. What this tool reports about those formats comes from the
sink's own ELD, which is capability discovery, not playback.

It binds libasound.so.2 through ctypes instead of linking against it. The
target has the runtime but not the development headers, and this gate does not
install packages on the target. The cost is the ctypes boilerplate below; the
benefit is that the probe is programmatic ALSA -- real hw_params negotiation,
real xrun accounting -- rather than a wrapper around aplay.

    hdmi-audio-probe.py --probe [--device hw:0,0]
    hdmi-audio-probe.py --device hw:0,0 --rate 48000 --channels 2 \
                        --format S24_LE --duration 30 --tone
    hdmi-audio-probe.py --device hw:0,0 --channels 6 --chanid
"""

import argparse
import ctypes
import ctypes.util
import json
import math
import struct
import sys
import time

asound = ctypes.CDLL(ctypes.util.find_library("asound") or "libasound.so.2")

SND_PCM_STREAM_PLAYBACK = 0
SND_PCM_ACCESS_RW_INTERLEAVED = 3
# CARD=0, HWDEP=1, MIXER=2, PCM=3, RAWMIDI=4, TIMER=5, SEQUENCER=6. The ELD
# control lives on the PCM interface, which amixer prints as iface=PCM.
SND_CTL_ELEM_IFACE_PCM = 3

# Only the formats an HDMI sink can actually carry as LPCM. The value is the
# libasound enum; the second field is bytes per sample on the wire.
FORMATS = {
    "S16_LE": (2, 2),
    "S24_LE": (6, 4),   # 24 significant bits inside a 32-bit container
    "S32_LE": (10, 4),
    "S24_3LE": (32, 3),
}
RATES = [32000, 44100, 48000, 88200, 96000, 176400, 192000]
CHANNELS = [1, 2, 4, 6, 8]

asound.snd_strerror.restype = ctypes.c_char_p
# snd_pcm_sframes_t is a signed long. Left at the ctypes default of int, a
# frame count or a negative errno would be truncated on aarch64.
asound.snd_pcm_writei.restype = ctypes.c_long
asound.snd_pcm_avail_update.restype = ctypes.c_long


def err(code, what):
    raise RuntimeError(f"{what}: {asound.snd_strerror(code).decode()} ({code})")


def check(code, what):
    if code < 0:
        err(code, what)
    return code


class Pcm:
    """A playback handle plus the hw_params actually granted to it."""

    def __init__(self, device):
        self.handle = ctypes.c_void_p()
        check(asound.snd_pcm_open(ctypes.byref(self.handle), device.encode(),
                                  SND_PCM_STREAM_PLAYBACK, 0), f"snd_pcm_open({device})")
        self.device = device
        self.params = ctypes.c_void_p()
        check(asound.snd_pcm_hw_params_malloc(ctypes.byref(self.params)),
              "snd_pcm_hw_params_malloc")
        check(asound.snd_pcm_hw_params_any(self.handle, self.params),
              "snd_pcm_hw_params_any")

    def close(self):
        if self.params:
            asound.snd_pcm_hw_params_free(self.params)
            self.params = None
        if self.handle:
            asound.snd_pcm_close(self.handle)
            self.handle = None


def capability_matrix(device):
    """What the driver says it will accept, asked one dimension at a time.

    Each test re-opens the device: snd_pcm_hw_params_test_* narrows the
    parameter space as a side effect, so testing rates after formats on the
    same handle would report the intersection rather than the axis.
    """
    caps = {"formats": {}, "rates": {}, "channels": {}}
    for name, (val, _) in FORMATS.items():
        p = Pcm(device)
        caps["formats"][name] = asound.snd_pcm_hw_params_test_format(
            p.handle, p.params, val) == 0
        p.close()
    for rate in RATES:
        p = Pcm(device)
        caps["rates"][rate] = asound.snd_pcm_hw_params_test_rate(
            p.handle, p.params, rate, 0) == 0
        p.close()
    for ch in CHANNELS:
        p = Pcm(device)
        caps["channels"][ch] = asound.snd_pcm_hw_params_test_channels(
            p.handle, p.params, ch) == 0
        p.close()
    return caps


def read_eld(card, device=0):
    """The sink's ELD, read from the ALSA control rather than looked for as a file.

    Returns the raw bytes, or None if the card has no ELD control. An HDMI card
    with no ELD control and one whose ELD is all zeroes are different findings,
    so the caller is given the bytes and left to decide.
    """
    ctl = ctypes.c_void_p()
    if asound.snd_ctl_open(ctypes.byref(ctl), f"hw:{card}".encode(), 0) < 0:
        return None
    try:
        eid = ctypes.c_void_p()
        val = ctypes.c_void_p()
        if asound.snd_ctl_elem_id_malloc(ctypes.byref(eid)) < 0:
            return None
        if asound.snd_ctl_elem_value_malloc(ctypes.byref(val)) < 0:
            return None
        try:
            asound.snd_ctl_elem_id_clear(eid)
            asound.snd_ctl_elem_id_set_interface(eid, SND_CTL_ELEM_IFACE_PCM)
            asound.snd_ctl_elem_id_set_name(eid, b"ELD")
            asound.snd_ctl_elem_id_set_device(eid, device)
            asound.snd_ctl_elem_value_set_id(val, eid)
            if asound.snd_ctl_elem_read(ctl, val) < 0:
                return None
            asound.snd_ctl_elem_value_get_bytes.restype = ctypes.POINTER(ctypes.c_ubyte)
            ptr = asound.snd_ctl_elem_value_get_bytes(val)
            return bytes(ptr[i] for i in range(128))
        finally:
            asound.snd_ctl_elem_id_free(eid)
            asound.snd_ctl_elem_value_free(val)
    finally:
        asound.snd_ctl_close(ctl)


# SNDRV_CHMAP_* from the ALSA uapi. Needed because the interleave order an
# HDMI sink expects is not the order a channel list is usually written in:
# this driver reports FL, FR, LFE, FC, RL, RR for 5.1, so a probe that assumed
# FL, FR, FC, LFE, ... would mislabel four of the six channels.
CHMAP = {0: "UNKNOWN", 1: "NA", 2: "MONO", 3: "FL", 4: "FR", 5: "RL", 6: "RR",
         7: "FC", 8: "LFE", 9: "SL", 10: "SR", 11: "RC", 12: "FLC", 13: "FRC",
         14: "RLC", 15: "RRC", 16: "FLW", 17: "FRW", 18: "FLH", 19: "FCH",
         20: "FRH", 21: "TC", 22: "TFL", 23: "TFR", 24: "TFC", 25: "TRL",
         26: "TRR", 27: "TRC", 28: "TFLC", 29: "TFRC", 30: "TSL", 31: "TSR",
         32: "LLFE", 33: "RLFE", 34: "BC", 35: "BLC", 36: "BRC"}

def read_chmap(card, device=0, channels=8):
    """The driver's own channel order, read from the 'Playback Channel Map' control.

    Read while a stream is open it reports the order in force for that stream;
    read while closed it reports zeroes. Either way this is the authority on
    which interleave slot feeds which speaker -- guessing it is how channel
    identification tests end up certifying the wrong mapping.
    """
    ctl = ctypes.c_void_p()
    if asound.snd_ctl_open(ctypes.byref(ctl), f"hw:{card}".encode(), 0) < 0:
        return None
    try:
        eid = ctypes.c_void_p()
        val = ctypes.c_void_p()
        if asound.snd_ctl_elem_id_malloc(ctypes.byref(eid)) < 0:
            return None
        if asound.snd_ctl_elem_value_malloc(ctypes.byref(val)) < 0:
            asound.snd_ctl_elem_id_free(eid)
            return None
        try:
            asound.snd_ctl_elem_id_clear(eid)
            asound.snd_ctl_elem_id_set_interface(eid, SND_CTL_ELEM_IFACE_PCM)
            asound.snd_ctl_elem_id_set_name(eid, b"Playback Channel Map")
            asound.snd_ctl_elem_id_set_device(eid, device)
            asound.snd_ctl_elem_value_set_id(val, eid)
            if asound.snd_ctl_elem_read(ctl, val) < 0:
                return None
            asound.snd_ctl_elem_value_get_integer.restype = ctypes.c_long
            return [asound.snd_ctl_elem_value_get_integer(val, i)
                    for i in range(channels)]
        finally:
            asound.snd_ctl_elem_id_free(eid)
            asound.snd_ctl_elem_value_free(val)
    finally:
        asound.snd_ctl_close(ctl)


AUDIO_FORMAT = {1: "LPCM", 2: "AC-3", 3: "MPEG-1", 4: "MP3", 5: "MPEG-2",
                6: "AAC LC", 7: "DTS", 8: "ATRAC", 9: "DSD", 10: "E-AC-3",
                11: "DTS-HD", 12: "MLP (TrueHD)", 13: "DST", 14: "WMA Pro"}
SPEAKERS = ["FL/FR", "LFE", "FC", "RL/RR", "RC", "FLC/FRC", "RLC/RRC"]


def decode_eld(b):
    if b is None or not any(b):
        return None
    mnl = b[4] & 0x1F
    sad_count = b[5] >> 4
    out = {
        "eld_ver": b[0] >> 3,
        "baseline_len_bytes": b[2] * 4,
        "cea_edid_ver": b[4] >> 5,
        "sad_count": sad_count,
        "conn_type": (b[5] >> 2) & 3,
        "supports_ai": (b[5] >> 1) & 1,
        "hdcp": b[5] & 1,
        "audio_sync_delay_ms": b[6] * 2,
        "speaker_allocation": [SPEAKERS[i] for i in range(7) if b[7] >> i & 1],
        "monitor_name": bytes(b[20:20 + mnl]).decode("ascii", "replace"),
        "sads": [],
    }
    off = 20 + mnl
    for i in range(sad_count):
        d = b[off + i * 3: off + i * 3 + 3]
        if len(d) < 3:
            break
        fmt = (d[0] >> 3) & 0xF
        sad = {
            "format": AUDIO_FORMAT.get(fmt, f"reserved({fmt})"),
            "max_channels": (d[0] & 7) + 1,
            "rates_hz": [RATES[k] for k in range(7) if d[1] >> k & 1],
            "raw": d.hex(" "),
        }
        if fmt == 1:
            sad["depths_bits"] = [bits for k, bits in enumerate((16, 20, 24))
                                  if d[2] >> k & 1]
        elif fmt in (2, 3, 4, 5, 7, 8):
            sad["max_bitrate_kbps"] = d[2] * 8
        out["sads"].append(sad)
    return out


def make_tone(rate, channels, fmt, seconds, only_channel=None, amplitude=0.1):
    """One second of a click-free looping tone, or a single-channel burst.

    The frequency is rate/100 so exactly 100 samples make one cycle at every
    supported rate; the buffer therefore loops without a discontinuity and any
    click heard is the hardware's, not the signal's. Amplitude defaults to
    -20 dBFS: this runs on someone's living-room TV.
    """
    code, width = FORMATS[fmt]
    frames = int(rate * seconds)
    peak = amplitude * ((1 << 31) - 1)
    buf = bytearray()
    for n in range(frames):
        s = int(peak * math.sin(2 * math.pi * (n % 100) / 100.0))
        for ch in range(channels):
            v = s if (only_channel is None or ch == only_channel) else 0
            if fmt == "S16_LE":
                buf += struct.pack("<h", v >> 16)
            elif fmt == "S32_LE":
                buf += struct.pack("<i", v)
            elif fmt == "S24_LE":
                buf += struct.pack("<i", v >> 8)
            elif fmt == "S24_3LE":
                buf += struct.pack("<i", v >> 8)[:3]
    return bytes(buf), frames, width


def configure(p, rate, channels, fmt):
    code, width = FORMATS[fmt]
    check(asound.snd_pcm_hw_params_set_access(p.handle, p.params,
                                              SND_PCM_ACCESS_RW_INTERLEAVED),
          "set_access")
    check(asound.snd_pcm_hw_params_set_format(p.handle, p.params, code),
          f"set_format({fmt})")
    check(asound.snd_pcm_hw_params_set_channels(p.handle, p.params, channels),
          f"set_channels({channels})")
    got = ctypes.c_uint(rate)
    check(asound.snd_pcm_hw_params_set_rate_near(p.handle, p.params,
                                                 ctypes.byref(got), None),
          f"set_rate_near({rate})")
    check(asound.snd_pcm_hw_params(p.handle, p.params), "snd_pcm_hw_params")

    period = ctypes.c_ulong()
    buffer_ = ctypes.c_ulong()
    asound.snd_pcm_hw_params_get_period_size(p.params, ctypes.byref(period), None)
    asound.snd_pcm_hw_params_get_buffer_size(p.params, ctypes.byref(buffer_))
    return {
        "requested_rate": rate,
        "actual_rate": got.value,
        "channels": channels,
        "format": fmt,
        "sample_bytes": width,
        "period_size_frames": period.value,
        "buffer_size_frames": buffer_.value,
    }


def play(device, rate, channels, fmt, duration, chanid=False, card=0):
    p = Pcm(device)
    try:
        got = configure(p, rate, channels, fmt)
        rate = got["actual_rate"]
        frame_bytes = got["sample_bytes"] * channels
        check(asound.snd_pcm_prepare(p.handle), "snd_pcm_prepare")

        xruns = 0
        written = 0
        planned_frames = 0
        start = time.monotonic()

        if chanid:
            # Channel identification: one channel at a time. The labels come
            # from the driver's own chmap, read now that the stream is open, so
            # the test reports the mapping the hardware actually uses rather
            # than a plausible-looking one.
            live = read_chmap(card, channels=channels)
            if live and any(live):
                order = [CHMAP.get(v, f"?{v}") for v in live[:channels]]
                print(f"  channel map from the driver: {', '.join(order)}")
            else:
                order = [f"ch{i}" for i in range(channels)]
                print("  channel map unavailable; channels are reported by index only")
            per = max(1.0, duration / channels)
            plan = [(i, order[i], per) for i in range(channels)]
        else:
            plan = [(None, "all", duration)]

        for ch_index, label, seconds in plan:
            if chanid:
                print(f"  now playing: channel {ch_index} ({label})", flush=True)
            pcm, frames, _ = make_tone(rate, channels, fmt, 1.0, only_channel=ch_index)
            payload = ctypes.create_string_buffer(pcm, len(pcm))
            base = ctypes.addressof(payload)
            loops = max(1, int(round(seconds)))
            planned_frames += loops * frames
            for _ in range(loops):
                off = 0
                while off < frames:
                    n = asound.snd_pcm_writei(
                        p.handle,
                        ctypes.c_void_p(base + off * frame_bytes),
                        ctypes.c_ulong(frames - off))
                    if n == -32:  # -EPIPE: the buffer ran dry
                        xruns += 1
                        asound.snd_pcm_prepare(p.handle)
                        continue
                    if n < 0:
                        err(n, "snd_pcm_writei")
                    off += n
                    written += n

        asound.snd_pcm_drain(p.handle)
        wall = time.monotonic() - start
        return {
            **got,
            "frames_written": written,
            "expected_frames": planned_frames,
            "wall_seconds": round(wall, 3),
            "xruns": xruns,
        }
    finally:
        p.close()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--device", default="hw:0,0")
    ap.add_argument("--card", type=int, default=0)
    ap.add_argument("--probe", action="store_true",
                    help="report ELD and the hw_params capability matrix, play nothing")
    ap.add_argument("--rate", type=int, default=48000)
    ap.add_argument("--channels", type=int, default=2)
    ap.add_argument("--format", default="S16_LE", choices=sorted(FORMATS))
    ap.add_argument("--duration", type=float, default=10.0)
    ap.add_argument("--tone", action="store_true", help="play a test tone")
    ap.add_argument("--chanid", action="store_true",
                    help="play the tone in one channel at a time, in HDMI order")
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args()

    if args.probe:
        eld_raw = read_eld(args.card)
        report = {
            "device": args.device,
            "card": args.card,
            "eld_present": eld_raw is not None and any(eld_raw),
            "eld_raw": eld_raw.hex(" ") if eld_raw else None,
            "eld": decode_eld(eld_raw),
            "capabilities": capability_matrix(args.device),
            "chmap_idle": read_chmap(args.card),
        }
        if args.json:
            print(json.dumps(report, indent=2))
            return 0
        print(f"device: {args.device}   card: hw:{args.card}")
        print()
        e = report["eld"]
        if e:
            print("ELD (read from the ALSA control, not from a file):")
            print(f"  monitor            : {e['monitor_name']!r}")
            print(f"  eld_ver / cea_ver  : {e['eld_ver']} / {e['cea_edid_ver']}")
            print(f"  connection type    : {'HDMI' if e['conn_type'] == 0 else e['conn_type']}")
            print(f"  supports_ai / hdcp : {e['supports_ai']} / {e['hdcp']}")
            print(f"  audio sync delay   : {e['audio_sync_delay_ms']} ms")
            print(f"  speaker allocation : {', '.join(e['speaker_allocation'])}")
            print(f"  short audio descriptors ({e['sad_count']}):")
            for s in e["sads"]:
                extra = ""
                if "depths_bits" in s:
                    extra = "  depths: " + "/".join(str(d) for d in s["depths_bits"]) + "-bit"
                elif "max_bitrate_kbps" in s:
                    extra = f"  max bitrate: {s['max_bitrate_kbps']} kbps"
                rates = "/".join(f"{r // 1000}k" for r in s["rates_hz"])
                print(f"    {s['format']:<13} {s['max_channels']}ch  rates: {rates}{extra}")
        else:
            print("ELD: ABSENT or all-zero on this card")
        print()
        caps = report["capabilities"]
        print("hw_params capability matrix (what the driver accepts, per axis):")
        print("  formats  : " + "  ".join(
            f"{k}={'yes' if v else 'no'}" for k, v in caps["formats"].items()))
        print("  rates    : " + "  ".join(
            f"{k}={'yes' if v else 'no'}" for k, v in caps["rates"].items()))
        print("  channels : " + "  ".join(
            f"{k}={'yes' if v else 'no'}" for k, v in caps["channels"].items()))
        return 0

    if not (args.tone or args.chanid):
        print("nothing to do: pass --probe, --tone or --chanid", file=sys.stderr)
        return 2

    print(f"playing {args.duration}s on {args.device}: "
          f"{args.rate} Hz, {args.channels}ch, {args.format}"
          f"{', channel identification' if args.chanid else ''}", flush=True)
    try:
        r = play(args.device, args.rate, args.channels, args.format,
                 args.duration, chanid=args.chanid, card=args.card)
    except RuntimeError as exc:
        # A parameter the driver will not accept is capability information, not
        # a defect: a sweep across formats and rates is expected to find the
        # edges. It is reported as such and exits 3, so a runner can tell it
        # apart from a real playback failure without parsing a traceback.
        print(f"  rejected by the driver: {exc}")
        print("AUDIO RESULT: UNSUPPORTED")
        return 3
    if args.json:
        print(json.dumps(r, indent=2))
    else:
        print(f"  granted rate       : {r['actual_rate']} Hz "
              f"(requested {r['requested_rate']})")
        print(f"  granted format     : {r['format']} ({r['sample_bytes']} bytes/sample)")
        print(f"  granted channels   : {r['channels']}")
        print(f"  period / buffer    : {r['period_size_frames']} / "
              f"{r['buffer_size_frames']} frames")
        print(f"  frames written     : {r['frames_written']} "
              f"(expected ~{r['expected_frames']})")
        print(f"  wall time          : {r['wall_seconds']} s")
        print(f"  xruns              : {r['xruns']}")
    print(f"AUDIO RESULT: {'PASS' if r['xruns'] == 0 else 'PARTIAL'}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
