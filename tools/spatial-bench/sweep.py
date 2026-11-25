#!/usr/bin/env python3
"""GPU-resident sahnelerin sınırı: sahne sürekli hareket hâlinde, her nokta aynı süre.

    python3 sweep.py <çıktı> <saniye> "<config>" ["<config>" ...]

Arayüz MEDIABOX_TV_SPATIAL=... ve MEDIABOX_TV_SPATIAL_CONTROL=/run/mediabox-spatial
ile başlatılmış olmalı (variant.sh). Sahne Tarayıcı satırındadır.
"""
import glob, json, os, sys, time
HERE = os.path.dirname(os.path.abspath(__file__)); sys.path.insert(0, HERE)
out, secs, points = sys.argv[1], float(sys.argv[2]), sys.argv[3:]
sys.argv = [sys.argv[0], out]
import measure as m, bench as b
CONTROL = "/run/mediabox-spatial"
def put(t): open(CONTROL, "w").write(t)
def point(config):
    put(config); time.sleep(2.5)
    if m.busy(): sys.exit("ABORT %s" % m.busy())
    os.mkdir("/run/mediabox-trace"); time.sleep(.6)
    c = b.counters_start(); d = b.Ddr(); d.start(); time.sleep(.2)
    fds = m.instr_open(); i0 = m.instr_read(fds); c0 = m.cpu_ticks(); g = m.Gpu(); g.start(); t0 = m.now(); w0 = time.time()
    time.sleep(secs)
    t1 = m.now(); g.stop = True; g.join(); i1 = m.instr_read(fds); c1 = m.cpu_ticks(); h = b.counters_stop(c); d.stop = True; d.join()
    [os.close(f) for f in fds]
    dst = f"{out}/trace"; m.shutil.rmtree(dst, ignore_errors=True)
    os.rename("/run/mediabox-trace", "/run/mediabox-trace.done"); time.sleep(.7); m.shutil.move("/run/mediabox-trace.done", dst)
    flips = []; commits = []
    for f in glob.glob(dst + "/tv-*.trace"):
        for l in open(f):
            p = l.split()
            if len(p) < 2 or not p[0].isdigit(): continue
            if p[1] == "commit" and "ui=1" in p and t0 <= int(p[0]) <= t1: commits.append(int(p[0]))
            elif p[1] == "flip":
                kv = dict(x.split("=") for x in p[2:])
                if t0 <= int(kv["at"]) <= t1: flips.append((int(kv["seq"]), int(kv["at"])))
    flips.sort(); n = max(1, len(commits)); T = (t1 - t0) / 1e9
    iv = [(y[1] - x[1]) / 1e6 for x, y in zip(flips, flips[1:])]
    missed = sum(y[0] - x[0] - 1 for x, y in zip(flips, flips[1:])); vb = flips[-1][0] - flips[0][0] if len(flips) > 1 else 0
    unit = (t1 - t0) / max(1, g.tot); log = b.journal(w0)
    r = dict(config=config, fps=round(len(flips) / T, 1), missed_pct=round(100 * missed / max(1, vb), 2), present_ms=[m.pct(iv, p) for p in (50, 95, 99)],
             minstr_frame=round((i1 - i0) / 1e6 / n, 2), cpu_pct=round((c1 - c0) / os.sysconf("SC_CLK_TCK") / T * 100, 1),
             gpu_busy_pct=round(100 * g.busy / max(1, g.tot), 1), gpu_ms_frame=round(g.busy * unit / 1e6 / n, 2), mcyc_frame=round(g.cyc * unit / 1e15 / n, 2),
             gpu_mhz=round(sum(f * x for f, x in g.f) / max(1, sum(x for f, x in g.f)) / 1e6),
             hw={k: round(v / n) for k, v in h.items() if k != "seconds"}, ext_gb_s=round((h.get("ext_read_bytes", 0) + h.get("ext_write_bytes", 0)) / h["seconds"] / 1e9, 2),
             ddr_load=round(sum(x[0] for x in d.l) / max(1, len(d.l)), 1), spatial=b.kv(log, "tv.spatial"), gl=b.kv(log, "tv.glcount"), gpu_mem=round(m.gmem(), 1), rss=round(m.rss()),
             temp=max(int(open(f).read()) for f in glob.glob("/sys/class/thermal/thermal_zone*/temp")) // 1000)
    print(json.dumps(r), flush=True); open(f"{out}/sweep.jsonl", "a").write(json.dumps(r) + "\n")
put("nothing"); m.home(2)
try:
    for p in points: point(p)
finally:
    put("nothing"); time.sleep(2)
    for x in ("/run/mediabox-trace", "/run/mediabox-trace.done"): m.shutil.rmtree(x, ignore_errors=True)
    m.home(0); os.unlink(CONTROL)
