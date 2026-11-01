#!/usr/bin/env python3
"""Live television's timeline, offline.

  live-trace.py matrix <file.ts> [--json out.json]
      What each service of a transport stream is, timed from the stream
      itself: codec, PTS cadence, random-access interval, PCR interval, how
      far a key frame's PTS leads the PCR it arrives with, audio.

  live-trace.py analyze <trace-dir> [--json out.json] [--csv frames.csv]
      Every `*.trace` file of the components (ctl, dvb, live, tv) in one
      CLOCK_MONOTONIC nanosecond timeline, cut at each channel request, with
      the phases of the opening and every frame of the first five seconds.

The trace format (mbtrace 2) is written by the components when
/run/mediabox-trace exists: one header line `#mbtrace 2 <component> pid=<n>`
and then `<CLOCK_MONOTONIC ns> <event> key=value ...` per line.
"""

import argparse
import bisect
import collections
import csv
import glob
import json
import os
import statistics
import sys

PACKET = 188
PCR_HZ = 27_000_000
PCR_WRAP = (1 << 33) * 300
PTS_WRAP = 1 << 33

STREAM_TYPES = {
    0x01: "mpeg1video", 0x02: "mpeg2video", 0x03: "mp1/mp2", 0x04: "mp2",
    0x0F: "aac", 0x11: "aac-latm", 0x1B: "h264", 0x24: "hevc",
    0x81: "ac3", 0x87: "eac3",
}


# --------------------------------------------------------------------------
# matrix


def pcr_of(p):
    if not (p[3] & 0x20) or p[4] < 7 or not (p[5] & 0x10):
        return None
    base = (p[6] << 25) | (p[7] << 17) | (p[8] << 9) | (p[9] << 1) | (p[10] >> 7)
    return base * 300 + (((p[10] & 1) << 8) | p[11])


def payload_of(p):
    afc = (p[3] >> 4) & 3
    if afc == 1:
        return p[4:]
    if afc == 3:
        return p[5 + p[4]:]
    return b""


def ts_time(b):
    return (((b[0] >> 1) & 7) << 30) | (b[1] << 22) | ((b[2] >> 1) << 15) | (b[3] << 7) | (b[4] >> 1)


def pes_times(payload):
    if len(payload) < 14 or payload[:3] != b"\x00\x00\x01":
        return None, None
    flags = payload[7] >> 6
    pts = ts_time(payload[9:14]) if flags & 2 else None
    dts = ts_time(payload[14:19]) if flags == 3 and len(payload) >= 19 else pts
    return pts, dts


def section(payload):
    if not payload:
        return None
    return payload[1 + payload[0]:]


def wrap_delta(a, b, wrap):
    return (b - a) % wrap


def h264_idr(data):
    """Whether an H.264 access unit's first bytes hold an IDR slice or a
    recovery point SEI (open-GOP broadcast)."""
    i = 0
    while True:
        i = data.find(b"\x00\x00\x01", i)
        if i < 0 or i + 3 >= len(data):
            return False
        nal = data[i + 3] & 0x1F
        if nal == 5:
            return True
        if nal == 6 and i + 4 < len(data) and data[i + 4] == 6:
            return True
        i += 3


def percentiles(values, ps=(50, 95, 99)):
    if not values:
        return {}
    s = sorted(values)
    return {f"p{p}": s[min(len(s) - 1, int(len(s) * p / 100))] for p in ps} | {"min": s[0], "max": s[-1], "n": len(s)}


def histogram(values, digits=1):
    c = collections.Counter(round(v, digits) for v in values)
    return dict(sorted(c.items(), key=lambda kv: -kv[1])[:6])


def matrix(path):
    data = open(path, "rb").read()
    start = next(k for k in range(len(data) - 5 * PACKET) if all(data[k + j * PACKET] == 0x47 for j in range(5)))
    pmt_pids = {}
    programs = {}
    pcr = collections.defaultdict(list)  # pid -> [(index, pcr)]
    pcr_disc = collections.Counter()
    pes = collections.defaultdict(list)  # pid -> [(index, pts, dts, rai, key)]
    first_bytes = {}
    n = (len(data) - start) // PACKET
    for idx in range(n):
        p = data[start + idx * PACKET:start + (idx + 1) * PACKET]
        if p[0] != 0x47:
            continue
        pid = ((p[1] & 0x1F) << 8) | p[2]
        pusi = p[1] & 0x40
        value = pcr_of(p)
        if value is not None:
            pcr[pid].append((idx, value))
            if p[5] & 0x80:
                pcr_disc[pid] += 1
        if pid == 0 and pusi:
            s = section(payload_of(p))
            if s and s[0] == 0:
                length = ((s[1] & 0xF) << 8) | s[2]
                for k in range(8, 3 + length - 4, 4):
                    number = (s[k] << 8) | s[k + 1]
                    if number:
                        pmt_pids[((s[k + 2] & 0x1F) << 8) | s[k + 3]] = number
            continue
        if pid in pmt_pids and pusi:
            s = section(payload_of(p))
            if s and s[0] == 2 and pmt_pids[pid] not in programs:
                length = ((s[1] & 0xF) << 8) | s[2]
                end = 3 + length - 4
                pcr_pid = ((s[8] & 0x1F) << 8) | s[9]
                info = ((s[10] & 0xF) << 8) | s[11]
                k = 12 + info
                streams = []
                while k + 5 <= end:
                    stype, epid = s[k], ((s[k + 1] & 0x1F) << 8) | s[k + 2]
                    es_info = ((s[k + 3] & 0xF) << 8) | s[k + 4]
                    descriptors = s[k + 5:k + 5 + es_info]
                    tags = set()
                    j = 0
                    while j + 2 <= len(descriptors):
                        tags.add(descriptors[j])
                        j += 2 + descriptors[j + 1]
                    name = STREAM_TYPES.get(stype)
                    if stype == 0x06:
                        name = "ac3" if 0x6A in tags else "eac3" if 0x7A in tags else None
                    if name:
                        streams.append((epid, name))
                    k += 5 + es_info
                programs[pmt_pids[pid]] = {"pcr_pid": pcr_pid, "streams": streams}
            continue
        if pusi:
            payload = payload_of(p)
            pts, dts = pes_times(payload)
            if pts is not None:
                rai = bool((p[3] & 0x20) and p[4] > 0 and p[5] & 0x40)
                head = payload[9 + payload[8]:]
                pes[pid].append([idx, pts, dts, rai, h264_idr(head)])
                first_bytes[pid] = head

    rows = []
    for number in sorted(programs):
        prog = programs[number]
        pcrs = pcr.get(prog["pcr_pid"], [])
        pcr_idx = [i for i, _ in pcrs]
        # The PCR at a packet, from the two around it (linear in bytes,
        # as a constant-rate multiplex is).
        def pcr_at(index):
            k = bisect.bisect_right(pcr_idx, index)
            if k == 0 or k >= len(pcrs):
                return None
            (i0, v0), (i1, v1) = pcrs[k - 1], pcrs[k]
            return v0 + wrap_delta(v0, v1, PCR_WRAP) * (index - i0) // max(1, i1 - i0)
        pcr_ms = [wrap_delta(a[1], b[1], PCR_WRAP) * 1000 / PCR_HZ for a, b in zip(pcrs, pcrs[1:])]
        row = {"program": number, "pcr_pid": prog["pcr_pid"], "pcr_interval_ms": percentiles([round(v, 2) for v in pcr_ms]),
               "pcr_discontinuities": pcr_disc.get(prog["pcr_pid"], 0)}
        audio = []
        for epid, name in prog["streams"]:
            frames = pes.get(epid, [])
            if not frames:
                continue
            if name in ("h264", "hevc", "mpeg2video", "mpeg1video"):
                if "video" in row:
                    continue
                pts = sorted(f[1] for f in frames)
                pres = [wrap_delta(a, b, PTS_WRAP) / 90 for a, b in zip(pts, pts[1:])]
                dec = [wrap_delta(a[2], b[2], PTS_WRAP) / 90 for a, b in zip(frames, frames[1:])]
                reordered = sum(1 for a, b in zip(frames, frames[1:]) if wrap_delta(a[1], b[1], PTS_WRAP) > PTS_WRAP // 2)
                keys = [f for f in frames if f[3] or f[4]]
                key_ms = [wrap_delta(a[2], b[2], PTS_WRAP) / 90 for a, b in zip(keys, keys[1:])]
                # How far ahead of the PCR a key frame's PTS is when it
                # arrives: what a decoder waits from the access point's
                # arrival to showing it, before any latency of its own.
                lead = []
                for f in keys:
                    at = pcr_at(f[0])
                    if at is not None:
                        lead.append(round(wrap_delta(at, f[1] * 300, PCR_WRAP) / PCR_HZ * 1000, 1))
                lead = [v for v in lead if v < 10_000]
                row["video"] = {
                    "codec": name, "pid": epid, "pes": len(frames),
                    "pts_step_ms": histogram(pres), "dts_step_ms": histogram(dec),
                    "out_of_order_pes": reordered,
                    "rap_interval_ms": percentiles([round(v) for v in key_ms]),
                    "rap_flagged": sum(1 for f in frames if f[3]), "idr_or_recovery": sum(1 for f in frames if f[4]),
                    "key_pts_lead_over_pcr_ms": percentiles(lead),
                }
            else:
                pts = [f[1] for f in frames]
                steps = [wrap_delta(a, b, PTS_WRAP) / 90 for a, b in zip(pts, pts[1:])]
                audio.append({"codec": name, "pid": epid, "pts_step_ms": histogram(steps)})
        row["audio"] = audio
        rows.append(row)
    return {"file": path, "packets": n, "programs": rows}


def print_matrix(result):
    print(f"{result['file']}: {result['packets']} paket")
    print(f"{'prog':>6} {'codec':6} {'PTS adımı ms':16} {'DTS adımı':12} {'reorder':>7} {'RAP p50/max ms':>15} {'PCR p50/max ms':>15} {'PTS-PCR key p50/max':>20}  ses")
    for row in result["programs"]:
        v = row.get("video")
        pcr = row["pcr_interval_ms"]
        audio = ", ".join(f"{a['codec']} {list(a['pts_step_ms'])[:1]}" for a in row["audio"])
        if not v:
            print(f"{row['program']:>6} {'-':6} {'':16} {'':12} {'':>7} {'':>15} {pcr.get('p50','-')!s:>7}/{pcr.get('max','-')!s:<7} {'':>20}  {audio}")
            continue
        rap, lead = v["rap_interval_ms"], v["key_pts_lead_over_pcr_ms"]
        print(f"{row['program']:>6} {v['codec']:6} {str(v['pts_step_ms']):16.16} {str(v['dts_step_ms']):12.12} {v['out_of_order_pes']:>7} "
              f"{rap.get('p50','-')!s:>7}/{rap.get('max','-')!s:<7} {pcr.get('p50','-')!s:>7}/{pcr.get('max','-')!s:<7} "
              f"{lead.get('p50','-')!s:>9}/{lead.get('max','-')!s:<10}  {audio}")


# --------------------------------------------------------------------------
# analyze

# A phase longer than this is no latency of an opening but a broken clock.
IMPLAUSIBLE_MS = 60_000
# A chunk stamped further ahead of its arrival than this: the PCR ran away.
RUNAWAY_NS = 3_000_000_000


class Event(tuple):
    """(ns, component, run, name, fields)"""
    ns = property(lambda self: self[0])
    comp = property(lambda self: self[1])
    run = property(lambda self: self[2])
    name = property(lambda self: self[3])
    f = property(lambda self: self[4])


def parse_line(line):
    parts = line.split()
    if len(parts) < 2:
        return None
    try:
        ns = int(parts[0])
    except ValueError:
        return None
    fields = {}
    for part in parts[2:]:
        key, _, value = part.partition("=")
        try:
            fields[key] = int(value)
        except ValueError:
            fields[key] = value
    return ns, parts[1], fields


def load(directory):
    """Every event of every file, and the runs (processes) they came from."""
    events, runs = [], {}
    for path in sorted(glob.glob(os.path.join(directory, "*.trace"))):
        run = None
        with open(path, errors="replace") as handle:
            for line in handle:
                if line.startswith("#mbtrace"):
                    head = line.split()
                    if len(head) < 3 or head[1] != "2":
                        print(f"{path}: sürüm {head[1:2]} okunmuyor", file=sys.stderr)
                        break
                    meta = dict(part.split("=", 1) for part in head[3:] if "=" in part)
                    run = meta.get("run") or f"{path}"
                    runs.setdefault(run, {"comp": head[2], "pid": int(meta.get("pid", 0)), "start": int(meta.get("start", 0)), "files": []})
                    runs[run]["files"].append(os.path.basename(path))
                    continue
                if run is None:
                    continue
                parsed = parse_line(line)
                if parsed:
                    ns, name, fields = parsed
                    events.append(Event((ns, runs[run]["comp"], run, name, fields)))
    events.sort(key=lambda e: e.ns)
    return events, runs


def correlate(events, runs):
    """Tag each event with the request it belongs to (`rid`), by what the
    processes said, not by when: the control plane names it, the receiver
    carries it in the selection it acts on, the player's branch opens the
    FIFO the control plane named for it, the interface's frames are the
    player's by (player run, frame id)."""
    rid_of = {}
    ctl_paths = [(e.ns, e.f.get("path"), e.f.get("rid")) for e in events
                 if e.comp == "ctl" and e.name in ("loadfile", "player-start")]
    cold = [(e.ns, e.f.get("rid")) for e in events if e.comp == "ctl" and e.name == "receiver-start"]
    # The receiver: its first events are the request that started it, then
    # each selection's.
    current = {}
    gen_rid = {}
    for e in events:
        if e.comp == "dvb":
            if e.run not in current:
                start = runs[e.run]["start"]
                before = [rid for ns, rid in cold if ns <= start]
                current[e.run] = before[-1] if before else None
            if e.name == "select":
                current[e.run] = e.f.get("rid") or None
            rid_of[id(e)] = current[e.run]
        elif e.comp == "ctl":
            rid_of[id(e)] = e.f.get("rid")
        elif e.comp == "live":
            if e.name == "open":
                named = [rid for ns, path, rid in ctl_paths if path == e.f.get("path") and ns <= e.ns]
                gen_rid[(e.run, e.f.get("gen"))] = named[-1] if named else None
            gen = e.f.get("gen")
            rid_of[id(e)] = gen_rid.get((e.run, gen)) if gen is not None else None
    # Frames: the player's send names (run, gen, id); the interface's events
    # name the player by its pid.
    frame_rid = {}
    for e in events:
        if e.comp == "live" and e.name == "send":
            frame_rid[(e.run, e.f["id"])] = rid_of.get(id(e))
    live_runs = sorted((r["start"], r["pid"], run) for run, r in runs.items() if r["comp"] == "live")

    def player(pid, ns):
        found = [run for start, p, run in live_runs if p == pid and start <= ns]
        return found[-1] if found else None

    for e in events:
        if e.comp == "tv" and "id" in e.f and "pid" in e.f:
            key = (player(e.f["pid"], e.ns), e.f["id"])
            e.f["key"] = key
            rid_of[id(e)] = frame_rid.get(key)
        elif e.comp == "live" and e.name in ("send", "release"):
            e.f["key"] = (e.run, e.f["id"])
            if e.name == "release":
                rid_of[id(e)] = frame_rid.get(e.f["key"])
    return rid_of


def ms(a, b):
    if a is None or b is None:
        return None
    return round(b / 1e6 - a / 1e6, 1)


def frames_of(events, rid_of, rid):
    """The request's frames, by (player run, id), with what the interface did
    with each and the vblank it was scanned out at."""
    sent, order = {}, []
    for e in events:
        if e.comp == "live" and e.name == "send" and rid_of.get(id(e)) == rid:
            sent[e.f["key"]] = {"key": e.f["key"], "id": e.f["id"], "gen": e.f.get("gen"), "pts_us": e.f.get("pts_us"),
                                "due_ns": e.f.get("due"), "sent_ns": e.ns}
            order.append(e.f["key"])
    in_flight = {}
    for e in events:
        if e.comp != "tv":
            continue
        key = e.f.get("key")
        if e.name == "arrive" and key in sent:
            sent[key]["arrive_ns"] = e.ns
        elif e.name == "unseen" and key in sent:
            sent[key]["unseen"] = e.f.get("why", "?")
        elif e.name == "commit":
            in_flight[e.run] = key if key in sent else None
            if key in sent:
                sent[key]["commit_ns"] = e.ns
        elif e.name == "flip":
            key = in_flight.pop(e.run, None)
            if key:
                sent[key]["scan_seq"] = e.f.get("seq")
                sent[key]["scan_ns"] = e.f.get("at")
    return [sent[k] for k in order]


def nominal_step_ms(frames):
    """The channel's picture step: the commonest PTS step of its frames."""
    steps = collections.Counter(round((b["pts_us"] - a["pts_us"]) / 1000)
                                for a, b in zip(frames, frames[1:])
                                if a.get("pts_us") is not None and b.get("pts_us") is not None and b["pts_us"] > a["pts_us"])
    return steps.most_common(1)[0][0] if steps else None


def cadence(frames, period_ns, nominal_ms=None):
    """PTS step, due step and scanned vblank step of each frame, against the
    last frame that was scanned, and what is wrong with it. A PTS step half
    as long again as the channel's own is pictures missing from the stream
    as it reached the panel: the panel holds the one before (`pts-gap`)."""
    out, prev, sent_before = [], None, None
    for fr in frames:
        row = dict(fr)
        # The stream's own step: against the frame sent before, shown or not.
        stream_step = None
        if sent_before and fr.get("pts_us") is not None and sent_before.get("pts_us") is not None:
            stream_step = (fr["pts_us"] - sent_before["pts_us"]) / 1000
        sent_before = fr
        if prev:
            if fr.get("pts_us") is not None and prev.get("pts_us") is not None:
                row["pts_step_ms"] = (fr["pts_us"] - prev["pts_us"]) / 1000
            if fr.get("due_ns") and prev.get("due_ns"):
                row["due_step_ms"] = (fr["due_ns"] - prev["due_ns"]) / 1e6
            if fr.get("scan_seq") is not None and prev.get("scan_seq") is not None:
                row["vblanks"] = fr["scan_seq"] - prev["scan_seq"]
        problems = []
        if row.get("pts_step_ms") is not None and row["pts_step_ms"] <= 0:
            problems.append("pts-regression")
        if "unseen" in fr:
            problems.append("unseen:" + str(fr["unseen"]))
        elif fr.get("scan_seq") is None:
            problems.append("never-scanned")
        # DRM renders on `due`, which can deliberately be a cadence-cleaned
        # version of the demuxer's opening PTS. Judge the panel against the
        # timestamp it was actually given; keep source PTS for diagnosis.
        if row.get("vblanks") is not None and row.get("due_step_ms") and period_ns:
            want = round(row["due_step_ms"] * 1e6 / period_ns)
            if row["vblanks"] > want:
                problems.append(f"repeat+{row['vblanks'] - want}")
            elif row["vblanks"] < want:
                problems.append(f"short-{want - row['vblanks']}")
        if row.get("vblanks") == 0:
            problems.append("same-vblank")
        if (nominal_ms and stream_step is not None and stream_step > nominal_ms * 1.5
                and not str(fr.get("unseen", "")).startswith("stop")):
            problems.append("pts-gap")
        row["problems"] = problems
        out.append(row)
        if fr.get("scan_seq") is not None or prev is None:
            prev = fr
    return out


def analyze(directory):
    events, runs = load(directory)
    rid_of = correlate(events, runs)
    mine = collections.defaultdict(list)
    for e in events:
        rid = rid_of.get(id(e))
        if rid:
            mine[rid].append(e)
    requests = [e for e in events if e.comp == "ctl" and e.name == "request"]
    # A channel opened without the control plane (`mediabox-live` alone) is
    # its own request, by its player's branch.
    if not requests:
        for e in events:
            if e.comp == "live" and e.name == "open":
                rid = f"{e.run}/{e.f.get('gen')}"
                requests.append(Event((e.ns, "ctl", None, "request", {"rid": rid, "sid": None})))
                mine[rid] = [x for x in events if x.comp == "live" and x.run == e.run and x.f.get("gen") == e.f.get("gen")]
                for x in mine[rid]:
                    rid_of[id(x)] = rid
                keys = {x.f["key"] for x in mine[rid] if x.name == "send"}
                for x in events:
                    if x.comp == "tv" and x.f.get("key") in keys:
                        rid_of[id(x)] = rid
                        mine[rid].append(x)
                mine[rid].sort(key=lambda x: x.ns)
    modes = [(e.ns, e.f.get("period")) for e in events if e.comp == "tv" and e.name == "mode" and e.f.get("period")]
    mode_reqs = [e.ns for e in events if e.comp == "tv" and e.name == "mode-req"]
    results = []
    for req in requests:
        rid = req.f.get("rid")
        own = mine.get(rid, [])

        def at(comp, name, **match):
            for e in own:
                if e.comp == comp and e.name == name and all(e.f.get(k) == v for k, v in match.items()):
                    return e
            return None

        def ns(e):
            return e.ns if e else None

        s = {"rid": rid, "sid": req.f.get("sid"), "t0_ns": req.ns, "cancelled": at("ctl", "cancel") is not None,
             "status": "OK", "reasons": []}
        if s["cancelled"]:
            s["cancelled_at"] = at("ctl", "cancel").f.get("at")
        frames = frames_of(events, rid_of, rid)
        scanned = [f for f in frames if f.get("scan_ns")]
        photon = scanned[0]["scan_ns"] if scanned else None
        e = {name: ns(at(comp, name)) for comp, name in [
            ("ctl", "select"), ("ctl", "loadfile"), ("ctl", "loadfile-done"), ("ctl", "player-start"),
            ("ctl", "receiver-start"), ("ctl", "receiver-up"), ("dvb", "selected"), ("dvb", "tune"), ("dvb", "lock"),
            ("dvb", "usb-first"), ("dvb", "pat"), ("dvb", "pmt"), ("dvb", "pcr"), ("dvb", "rap"), ("dvb", "replay"),
            ("dvb", "fifo-open"), ("dvb", "fifo-write"), ("live", "open"), ("live", "built"), ("live", "src"),
            ("live", "parse-key"), ("live", "dec-in"), ("live", "dec-out"), ("live", "sink")]}
        e["dvb-select"] = ns(at("dvb", "select"))
        e["pad"] = ns(at("live", "pad", kind="video"))
        start = e["player-start"] or e["loadfile"]
        s["phases_ms"] = {
            "request->select": ms(req.ns, e["select"]), "select->dvb-seen": ms(e["select"], e["dvb-select"]),
            "request->receiver-up": ms(req.ns, e["receiver-up"]), "tune->lock": ms(e["tune"], e["lock"]),
            "lock->usb-first": ms(e["lock"], e["usb-first"]),
            "dvb-seen->pat": ms(e["dvb-select"], e["pat"]), "pat->pmt": ms(e["pat"], e["pmt"]), "pmt->rap": ms(e["pmt"], e["rap"]),
            "fifo-open->first-write": ms(e["fifo-open"], e["fifo-write"]),
            "request->player-start/loadfile": ms(req.ns, start), "loadfile->done": ms(e["loadfile"], e["loadfile-done"]),
            "player-start/loadfile->open": ms(start, e["open"]), "open->built": ms(e["open"], e["built"]),
            "open->first-src": ms(e["open"], e["src"]), "src->video-pad": ms(e["src"], e["pad"]),
            "pad->parse-key": ms(e["pad"], e["parse-key"]), "parse-key->dec-in": ms(e["parse-key"], e["dec-in"]),
            "dec-in->dec-out": ms(e["dec-in"], e["dec-out"]), "dec-out->sink": ms(e["dec-out"], e["sink"]),
            "sink->first-send": ms(e["sink"], frames[0]["sent_ns"] if frames else None),
            "send->arrive": ms(frames[0]["sent_ns"], frames[0].get("arrive_ns")) if frames else None,
            "first-scanned: send->commit": ms(scanned[0]["sent_ns"], scanned[0].get("commit_ns")) if scanned else None,
            "first-scanned: commit->flip": ms(scanned[0].get("commit_ns"), photon) if scanned else None,
            "request->first-scanout": ms(req.ns, photon),
        }
        # The receiver's start from its last access point.
        replay = at("dvb", "replay")
        if replay:
            s["replay"] = dict(replay.f)
        # Invalid runs: a PCR jump, a stamp running away from its arrival, a
        # phase no opening takes.
        srcs = [x for x in own if x.comp == "live" and x.name == "src"]
        if any(x.f.get("disc") == 1 for x in srcs):
            s["reasons"].append("pcr-discontinuity")
        runaway = [x for x in srcs if x.f.get("pts", 0) - x.f.get("arr", 0) > RUNAWAY_NS]
        if runaway:
            s["reasons"].append(f"stamp-runaway:{max(x.f['pts'] - x.f['arr'] for x in runaway) / 1e9:.0f}s")
        bad = [k for k, v in s["phases_ms"].items() if v is not None and (v < 0 or v > IMPLAUSIBLE_MS)]
        if bad:
            s["reasons"].append("implausible:" + ",".join(bad))
        if s["reasons"]:
            s["status"] = "INVALID"
            s["untrusted_phases_ms"] = s.pop("phases_ms")
            s["phases_ms"] = {}
        elif not frames and not s["cancelled"]:
            s["status"] = "INCOMPLETE"
            s["reasons"].append("no-frames")
        paced = [x.f for x in srcs if x.f.get("class") == "paced"]
        if paced:
            s["burst"] = {"chunks": len(paced), "bytes": sum(f["bytes"] for f in paced),
                          "media_ms": round((paced[-1]["pts"] - paced[0]["pts"]) / 1e6, 1),
                          "arrival_ms": round((paced[-1]["arr"] - paced[0]["arr"]) / 1e6, 1)}
        live = [x.f for x in srcs if x.f.get("class") == "live"]
        if live:
            s["kept_lead_ms"] = round((live[0]["pts"] - live[0]["arr"]) / 1e6, 1)
        fed, decode = {}, []
        for x in own:
            if x.comp == "live" and x.name == "dec-in" and x.f.get("pts", -1) >= 0:
                fed.setdefault(x.f["pts"], x.ns)
            elif x.comp == "live" and x.name == "dec-out" and x.f.get("pts") in fed:
                decode.append(round((x.ns - fed.pop(x.f["pts"])) / 1e6, 1))
        s["decode_ms"] = percentiles(decode)
        s["player_drops"] = dict(collections.Counter(x.f.get("why") for x in own if x.comp == "live" and x.name == "drop"))
        key = at("live", "parse-key")
        lat = at("live", "sink-latency")
        if key and lat and photon:
            # Hypothesis: first light = key PTS on the clock + latency
            # (+ render delay, already in the due time).
            s["key_due_minus_photon_ms"] = round((lat.f["base"] + key.f["pts"] + lat.f["ns"] - photon) / 1e6, 1)
            s["latency_ms"] = round(lat.f["ns"] / 1e6, 1)
        first_src = srcs[0].f if srcs else None
        if key and first_src:
            s["key_pts_minus_first_src_ms"] = round((key.f["pts"] - first_src["pts"]) / 1e6, 1)
        gate = at("live", "audio-gate", open=1)
        if gate and photon:
            s["audio_gate_minus_first_photon_ms"] = ms(photon, gate.ns)
        # The first sound buffer after the gate opened: one already on its
        # way to the sink when the channel changed is the channel before's,
        # though traced under the new generation (the sink's probe knows
        # only the current one).
        asink = next((e for e in own if gate and e.comp == "live" and e.name == "audio-sink" and e.ns >= gate.ns), None)
        if asink and lat and photon:
            s["audio_due_minus_first_photon_ms"] = round((asink.f["base"] + asink.f["pts"] + lat.f["ns"] - photon) / 1e6, 1)
        when = photon or (own[-1].ns if own else req.ns)
        period_ns = next((p for t, p in reversed(modes) if t <= when), modes[0][1] if modes else 20_000_000)
        s["period_ns"] = period_ns
        s["mode_changes"] = [{"req_ms": ms(req.ns, r), "done_ms": ms(req.ns, next((t for t, _ in modes if t >= r), None))}
                             for r in mode_reqs if req.ns <= r and (photon is None or r <= photon + 10_000_000_000)]
        rows = cadence(frames, period_ns, nominal_step_ms(frames))
        s["nominal_step_ms"] = nominal_step_ms(frames)
        window = [r for r in rows if photon is None or r.get("scan_ns") is None or r["scan_ns"] < photon + 5_000_000_000]
        window = [r for r in window if photon is None or r.get("sent_ns", 0) < photon + 5_000_000_000]
        s["first5s"] = {
            "frames": len(window),
            "problems": dict(collections.Counter(p for r in window for p in r["problems"])),
            "pts_step_ms": histogram([r["pts_step_ms"] for r in window if r.get("pts_step_ms") is not None]),
            "due_step_ms": histogram([r["due_step_ms"] for r in window if r.get("due_step_ms") is not None]),
            "vblanks": histogram([r["vblanks"] for r in window if r.get("vblanks") is not None], 0),
        }
        blocking = {name: count for name, count in s["first5s"]["problems"].items()
                    if name != "unseen:stop"}
        if blocking and s["status"] == "OK":
            s["status"] = "FAIL"
            s["reasons"].append("frame-integrity:" + ",".join(
                f"{name}={count}" for name, count in sorted(blocking.items())))
        # Pictures the player threw away are pictures missing from the panel,
        # but for the last channel's frames reaching the sink after a newer
        # one was asked for (`stale`).
        # `bogus-pts`: a channel's first decoded pictures timed nowhere near
        # its key frame, never shown and never meant to be.
        thrown = {why: n for why, n in s["player_drops"].items() if why not in ("stale", "bogus-pts")}
        if thrown and s["status"] in ("OK", "FAIL"):
            s["status"] = "FAIL"
            s["reasons"].append("player-drops:" + ",".join(f"{why}={n}" for why, n in sorted(thrown.items())))
        steady = [r for r in rows if photon and r.get("scan_ns") and r["scan_ns"] >= photon + 5_000_000_000]
        if steady:
            jitter = [(r["scan_ns"] - r["due_ns"]) / 1e6 for r in steady if r.get("due_ns")]
            s["steady"] = {"frames": len(steady), "seconds": round((steady[-1]["scan_ns"] - steady[0]["scan_ns"]) / 1e9, 1),
                           "scan_minus_due_ms": percentiles([round(j, 2) for j in jitter], (1, 50, 99)),
                           "problems": dict(collections.Counter(p for r in steady for p in r["problems"]))}
            steady_blocking = {name: n for name, n in s["steady"]["problems"].items() if not name.startswith("unseen:stop")}
            if steady_blocking and s["status"] in ("OK", "FAIL"):
                s["status"] = "FAIL"
                s["reasons"].append("steady-integrity:" + ",".join(f"{k}={v}" for k, v in sorted(steady_blocking.items())))
        s["frames"] = rows
        results.append(s)
    return {"dir": directory, "events": len(events), "runs": runs, "requests": results}


def print_analysis(result):
    print(f"{result['dir']}: {result['events']} olay, {len(result['runs'])} süreç")
    for s in result["requests"]:
        print(f"\n== istek {s['rid']} servis {s['sid']}: {s['status']}"
              f"{' ' + ','.join(s['reasons']) if s['reasons'] else ''}{' (iptal: ' + str(s.get('cancelled_at')) + ')' if s['cancelled'] else ''}"
              f", vblank {s['period_ns'] / 1e6:.3f} ms, mod değişimi {s['mode_changes'] or 'yok'}")
        for name, value in s["phases_ms"].items():
            if value is not None:
                print(f"   {name:34} {value:9.1f} ms")
        for key in ("replay", "burst", "kept_lead_ms", "decode_ms", "player_drops", "latency_ms", "key_pts_minus_first_src_ms",
                    "key_due_minus_photon_ms", "audio_gate_minus_first_photon_ms", "audio_due_minus_first_photon_ms"):
            if s.get(key) not in (None, {}, []):
                print(f"   {key} {s[key]}")
        f5 = s["first5s"]
        print(f"   ilk 5 s: {f5['frames']} kare, sorunlar {f5['problems'] or 'yok'}")
        print(f"     PTS adımı {f5['pts_step_ms']}  due adımı {f5['due_step_ms']}  vblank adımı {f5['vblanks']}")
        if "steady" in s:
            st = s["steady"]
            print(f"   sonrası: {st['frames']} kare / {st['seconds']} s, tarama-due {st['scan_minus_due_ms']}, sorunlar {st['problems'] or 'yok'}")


# --------------------------------------------------------------------------
# selftest: synthetic traces whose answer is known


class Synth:
    def __init__(self, directory):
        self.dir = directory
        self.files = {}

    def run(self, comp, pid, start):
        key = (comp, pid, start)
        self.files[key] = [f"#mbtrace 2 {comp} run=b:{pid}:{start} pid={pid} start={start}"]
        return key

    def ev(self, run, ns, name, **fields):
        self.files[run].append(f"{int(ns)} {name} " + " ".join(f"{'class' if k == 'klass' else k}={v}" for k, v in fields.items()))

    def write(self):
        os.makedirs(self.dir, exist_ok=True)
        for (comp, pid, start), lines in self.files.items():
            with open(os.path.join(self.dir, f"{comp}-{pid}-{start}.trace"), "w") as out:
                out.write("\n".join(lines) + "\n")


def synth_frames(sy, live, tv, pid, gen, ids, t0, period=20_000_000, unseen=(), skip_vblank=(), seq0=100, pts0=0):
    """Frames `ids` of branch `gen`, one per vblank from t0, scanned out."""
    seq = seq0
    for k, fid in enumerate(ids):
        due = t0 + k * period
        sy.ev(live, due - 40e6, "send", gen=gen, id=fid, pts_us=pts0 + k * 20000, due=int(due), held=1)
        sy.ev(tv, due - 39e6, "arrive", pid=pid, id=fid, due=int(due), pending=0)
        if fid in unseen:
            sy.ev(tv, due - 5e6, "unseen", pid=pid, id=fid, why="same-vblank")
            continue
        if fid in skip_vblank:
            seq += 1
            due += period
        sy.ev(tv, due - 4e6, "commit", pid=pid, id=fid, ui=0)
        sy.ev(tv, due, "flip", seq=seq + k, at=int(due))
    return seq + len(ids)


def selftest():
    import tempfile
    failures = []

    def check(name, ok, detail=""):
        print(f"  {'ok  ' if ok else 'FAIL'} {name}{'' if ok else ': ' + str(detail)}")
        if not ok:
            failures.append(name)

    base = tempfile.mkdtemp(prefix="live-trace-selftest-")
    S = 1_000_000_000

    # 1. Two requests one over the other; the first given up before the
    #    receiver saw it. The old channel's frames go on reaching the panel
    #    after the new request, and the next player reuses frame ids.
    sy = Synth(os.path.join(base, "overlap"))
    ctl = sy.run("ctl", 10, 0)
    dvb = sy.run("dvb", 20, 1)
    old = sy.run("live", 30, 2)
    new = sy.run("live", 31, 9 * S)
    tv = sy.run("tv", 40, 0)
    sy.ev(tv, 1, "mode", w=1920, h=1080, hz=50, period=20_000_000)
    sy.ev(ctl, 1 * S, "request", rid=100, sid=1)
    sy.ev(ctl, 1 * S + 1e6, "player-start", rid=100, path="/run/live.ts")
    sy.ev(old, 2 * S, "open", gen=1, path="/run/live.ts", base=0)
    sy.ev(tv, 2 * S, "client", pid=30)
    synth_frames(sy, old, tv, 30, 1, range(1, 400), 3 * S)          # the old channel, to 11 s
    sy.ev(ctl, 10 * S, "request", rid=200, sid=2)
    sy.ev(ctl, 10 * S + 1e6, "select", rid=200, want="a")
    sy.ev(ctl, 10.05 * S, "request", rid=300, sid=3)
    sy.ev(ctl, 10.06 * S, "cancel", rid=200, at="switch")
    sy.ev(ctl, 10.06 * S + 1e6, "select", rid=300, want="b")
    sy.ev(dvb, 10.1 * S, "select", rid=300, want="b")                # the receiver never saw 200
    sy.ev(dvb, 10.1 * S + 1e6, "selected", sid=3, retune=0)
    sy.ev(dvb, 10.1 * S + 2e6, "replay", at="select", bytes=1000, pcrs=40, pcr_span_ms="700.0")
    sy.ev(ctl, 10.3 * S, "player-start", rid=300, path="/run/live.ts")
    sy.ev(new, 10.4 * S, "open", gen=1, path="/run/live.ts", base=0)
    sy.ev(tv, 10.4 * S, "client", pid=31)
    synth_frames(sy, new, tv, 31, 1, range(1, 50), 12 * S, seq0=5000)  # ids 1.. again
    sy.write()
    r = {s["rid"]: s for s in analyze(sy.dir)["requests"]}
    check("iptal edilen istek iptal görünür", r[200]["cancelled"] and r[200]["cancelled_at"] == "switch")
    check("iptal edilen isteğe kare yazılmaz", not r[200]["frames"], len(r[200]["frames"]))
    check("iptal edilen istek yeni isteğin alıcı olayını almaz", r[200]["phases_ms"].get("select->dvb-seen") is None)
    check("yeni istek kendi select'ini alıcıda görür", r[300]["phases_ms"]["select->dvb-seen"] == 39.0, r[300]["phases_ms"].get("select->dvb-seen"))
    check("yeni isteğin ilk taraması eski kanalın karesi değil", r[300]["phases_ms"]["request->first-scanout"] == 1950.0,
          r[300]["phases_ms"].get("request->first-scanout"))
    check("aynı id'li kareler ayrı oynatıcılarda karışmaz", len(r[300]["frames"]) == 49 and len(r[100]["frames"]) == 399,
          (len(r[300]["frames"]), len(r[100]["frames"])))
    check("eski kanal 10. saniyeden sonra da kendi isteğinde", r[100]["frames"][-1]["scan_ns"] > 10 * S)
    check("eski isteğin replay'i yok (yeni isteğin)", "replay" not in r[100] and r[300]["replay"]["pcr_span_ms"] == "700.0")

    # 2. What the measurement must catch: a frame put back unseen at a
    #    vblank, a repeated vblank, a timestamp going back.
    sy = Synth(os.path.join(base, "faults"))
    ctl, live, tv = sy.run("ctl", 1, 0), sy.run("live", 2, 1), sy.run("tv", 3, 0)
    sy.ev(tv, 1, "mode", w=1920, h=1080, hz=50, period=20_000_000)
    sy.ev(ctl, 1 * S, "request", rid=7, sid=1)
    sy.ev(ctl, 1 * S + 1, "player-start", rid=7, path="/f")
    sy.ev(live, 1.1 * S, "open", gen=1, path="/f", base=0)
    sy.ev(tv, 1.1 * S, "client", pid=2)
    synth_frames(sy, live, tv, 2, 1, [1, 2, 3, 4, 5], 2 * S, unseen={3}, skip_vblank={5})
    sy.ev(live, 2.2 * S, "send", gen=1, id=6, pts_us=60000, due=int(2.2 * S), held=1)  # back in time
    sy.ev(tv, 2.2 * S + 1e6, "arrive", pid=2, id=6, due=int(2.2 * S), pending=0)
    sy.ev(tv, 2.21 * S, "commit", pid=2, id=6, ui=0)
    sy.ev(tv, 2.22 * S, "flip", seq=120, at=int(2.22 * S))
    sy.write()
    s = analyze(sy.dir)["requests"][0]
    p = s["first5s"]["problems"]
    check("görünmeden iade yakalanır", p.get("unseen:same-vblank") == 1, p)
    check("tekrarlanan vblank yakalanır", p.get("repeat+1") == 1, p)
    check("geri giden PTS yakalanır", p.get("pts-regression") == 1, p)
    check("iadeden sonraki kare yanlış alarm vermez", s["frames"][3]["problems"] == [], s["frames"][3])
    check("kare kusuru koşuyu FAIL yapar", s["status"] == "FAIL", s["status"])

    # 2b. Pictures missing from the stream itself: PTS and due 80 ms apart
    #     and the panel holding the picture before for four vblanks, as it
    #     should -- the panel is right and the viewer still sees a jump.
    #     Measured 2026-10-31 on NTV HD and TRT 4K, all of it "OK" before.
    sy = Synth(os.path.join(base, "gap"))
    ctl, live, tv = sy.run("ctl", 1, 0), sy.run("live", 2, 1), sy.run("tv", 3, 0)
    sy.ev(tv, 1, "mode", w=1920, h=1080, hz=50, period=20_000_000)
    sy.ev(ctl, 1 * S, "request", rid=9, sid=1)
    sy.ev(ctl, 1 * S + 1, "player-start", rid=9, path="/g")
    sy.ev(live, 1.1 * S, "open", gen=1, path="/g", base=0)
    sy.ev(tv, 1.1 * S, "client", pid=2)
    seq = 100
    for k, slot in enumerate([0, 1, 2, 3, 7, 8, 9, 10, 11, 12]):
        due = int(2 * S + slot * 20_000_000)
        sy.ev(live, due - 40e6, "send", gen=1, id=k + 1, pts_us=slot * 20000, due=due, held=1)
        sy.ev(tv, due - 39e6, "arrive", pid=2, id=k + 1, due=due, pending=0)
        sy.ev(tv, due - 4e6, "commit", pid=2, id=k + 1, ui=0)
        sy.ev(tv, due, "flip", seq=seq + slot, at=due)
    sy.ev(live, 2.5 * S, "drop", gen=1, pts_us=999, why="order", last_us=999)
    sy.write()
    s = analyze(sy.dir)["requests"][0]
    check("akıştaki eksik resimler pts-gap olarak yakalanır", s["first5s"]["problems"].get("pts-gap") == 1, s["first5s"]["problems"])
    check("panel doğru tutsa da boşluk koşuyu FAIL yapar", s["status"] == "FAIL" and "pts-gap" in s["reasons"][0], s["reasons"])
    check("oynatıcının attığı kare FAIL'e yazılır", any(r.startswith("player-drops:order=1") for r in s["reasons"]), s["reasons"])

    # 3. A PCR discontinuity in the opening: no latency is reported.
    sy = Synth(os.path.join(base, "discontinuity"))
    ctl, live, tv = sy.run("ctl", 1, 0), sy.run("live", 2, 1), sy.run("tv", 3, 0)
    sy.ev(ctl, 1 * S, "request", rid=8, sid=1)
    sy.ev(ctl, 1 * S + 1, "player-start", rid=8, path="/f")
    sy.ev(live, 1.1 * S, "open", gen=1, path="/f", base=0)
    sy.ev(live, 1.2 * S, "src", gen=1, n=1, bytes=100, arr=int(0.2 * S), pts=int(0.2 * S), pcr0=0, pcr1=0, klass="paced", disc=0)
    sy.ev(live, 1.3 * S, "src", gen=1, n=2, bytes=100, arr=int(0.3 * S), pts=int(0.3 * S + 78900 * S), pcr0=1, pcr1=1, klass="paced", disc=1)
    sy.ev(tv, 1.1 * S, "client", pid=2)
    synth_frames(sy, live, tv, 2, 1, [1, 2], 79_000 * S)
    sy.write()
    s = analyze(sy.dir)["requests"][0]
    check("PCR süreksizliği koşuyu INVALID yapar", s["status"] == "INVALID" and "pcr-discontinuity" in s["reasons"], s["reasons"])
    check("INVALID koşuda gecikme raporlanmaz", s["phases_ms"] == {} and s["untrusted_phases_ms"]["request->first-scanout"] > 60_000)
    check("kaçan damga ayrıca adlandırılır", any(x.startswith("stamp-runaway") for x in s["reasons"]), s["reasons"])
    check("burst sınıfı okunur", s.get("burst", {}).get("chunks") == 2, s.get("burst"))

    print(f"\n{len(failures)} başarısız" if failures else "\nhepsi geçti")
    return 1 if failures else 0


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    m = sub.add_parser("matrix")
    m.add_argument("ts")
    m.add_argument("--json")
    a = sub.add_parser("analyze")
    a.add_argument("dir")
    a.add_argument("--json")
    a.add_argument("--csv")
    sub.add_parser("selftest")
    args = parser.parse_args()
    if args.command == "selftest":
        sys.exit(selftest())
    if args.command == "matrix":
        result = matrix(args.ts)
        print_matrix(result)
    else:
        result = analyze(args.dir)
        print_analysis(result)
        if args.csv:
            with open(args.csv, "w", newline="") as out:
                keys = ["rid", "key", "id", "gen", "pts_us", "due_ns", "sent_ns", "arrive_ns", "commit_ns", "scan_seq", "scan_ns",
                        "pts_step_ms", "due_step_ms", "vblanks", "problems"]
                writer = csv.DictWriter(out, keys, extrasaction="ignore")
                writer.writeheader()
                for s in result["requests"]:
                    for row in s["frames"]:
                        writer.writerow({**row, "rid": s["rid"], "problems": ";".join(row["problems"])})
    if args.json:
        with open(args.json, "w") as out:
            json.dump(result, out, indent=1, default=str)


if __name__ == "__main__":
    main()
