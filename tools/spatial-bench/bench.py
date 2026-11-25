#!/usr/bin/env python3
"""QuietOS grafik ölçümü: measure.py'nin senaryoları, yanında Mali donanım
sayaçları, DDR, sistem geneli komut ve arayüzün GL sayımı (gl-count).

    python3 bench.py <çıktı dizini> [senaryo...]

Yanında `measure.py`, `idle.py` ve `mali-counters` olmalı.
"""
import json, os, re, signal, struct, subprocess, sys, threading, time
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import measure as m

def ddr(): return tuple(int(x) for x in re.findall(r"\d+", open("/sys/class/devfreq/dmc/load").read()))[:2]
class Ddr(threading.Thread):
    def run(self):
        self.stop = False; self.l = []
        while not self.stop:
            self.l.append(ddr()); time.sleep(.05)
def allcpu():
    fds = []
    for cpu in range(os.cpu_count()):
        attr = struct.pack("IIQQQQQIIQQQQIiQI", 0, 112, 1, *([0] * 14)).ljust(112, b"\0")
        fds.append(m.libc.syscall(241, attr, -1, cpu, -1, 0))
    return [f for f in fds if f >= 0]
def dmabuf():
    t = open("/sys/kernel/debug/dma_buf/bufinfo").read()
    k = re.search(r"Total (\d+) objects, (\d+) bytes", t); return int(k[1]), round(int(k[2]) / 1048576, 1)
def journal(t0):
    return subprocess.run(["journalctl", "-u", "mediabox-tv-ui", "--since", "@%d" % t0, "-o", "cat", "--no-pager"], capture_output=True, text=True).stdout.splitlines()
def kv(lines, tag):
    """`tag`lı satırların sayıları, satırdaki kare sayısıyla ağırlıklı ortalama."""
    rows = []
    for l in lines:
        if tag in l:
            d = {k: float(v) for k, v in re.findall(r"(\w+)=(-?[\d.]+)(?=\s|$)", l)}
            if d.get("frames"): rows.append(d)
    n = sum(r["frames"] for r in rows)
    return {k: round(sum(r.get(k, 0) * r["frames"] for r in rows) / n, 2) for k in rows[0] if k != "frames"} if n else None
def counters_start():
    return subprocess.Popen([HERE + "/mali-counters"], stdout=subprocess.PIPE, text=True)
def counters_stop(p):
    p.send_signal(signal.SIGTERM); return json.loads(p.communicate(timeout=10)[0])
def wrap(name, go):
    d = Ddr(); d.start(); f = allcpu(); c = counters_start(); time.sleep(.2)
    a0 = m.instr_read(f); t0 = time.time(); go(); wall = time.time() - t0
    a1 = m.instr_read(f); [os.close(x) for x in f]; g = counters_stop(c); d.stop = True; d.join()
    r0 = json.loads(open(f"{m.OUT}/results.jsonl").read().splitlines()[-1])
    n = max(1, r0.get("commits") or r0.get("ui_commits") or 1)
    time.sleep(.3); log = journal(t0)
    r = dict(extra=name, ddr_load=round(sum(x[0] for x in d.l) / max(1, len(d.l)), 1), ddr_mhz=round(sum(x[1] for x in d.l) / max(1, len(d.l)) / 1e6),
             sys_minstr_s=round((a1 - a0) / 1e6 / wall, 1), commits=n, wall=round(wall, 1),
             hw_per_frame={k: round(v / n, 1) for k, v in g.items() if k != "seconds"}, hw_total=g,
             gl=kv(log, "tv.glcount"), skia=kv(log, "tv.skia frames"), spatial=kv(log, "tv.spatial"), dmabuf=dmabuf(), gpu_mem=round(m.gmem(), 1), rss=round(m.rss()))
    print(json.dumps(r), flush=True); open(f"{m.OUT}/extra.jsonl", "a").write(json.dumps(r) + "\n")
def w(n, go): return lambda: wrap(n, go)
def nav(n, col, keys, dur=880):
    def f(): m.home(col); wrap(n, lambda: m.run(n, keys, dur))
    return f
S = {
    "film-live": nav("film-live", 0, [("down", 1100), ("up", 1100)] * 8),
    "browser": nav("browser", 1, [("down", 1100), ("up", 1100)] * 8),
    "repeat": nav("repeat", 0, ([("down", 80)] * 4 + [("up", 80)] * 4) * 6),
    "reverse": nav("reverse", 0, [("down", 150), ("up", 150)] * 14),
    # Küre sürekli hareket halinde.
    "globe-repeat": nav("globe-repeat", 1, ([("down", 80)] * 2 + [("up", 80)] * 2) * 12),
    "globe-reverse": nav("globe-reverse", 1, [("down", 150), ("up", 150)] * 14),
    "home-settings": lambda: (m.settings(), m.home(3), wrap("home-settings", lambda: m.run("home-settings", [("ok", 1200), ("back", 1200)] * 6, 540))),
    "category": lambda: (m.column(), wrap("category", lambda: m.run("category", [("down", 800), ("up", 800)] * 10, 540))),
    "idle-home": lambda: (m.home(0), time.sleep(3), wrap("idle-home", lambda: m.idle("idle-home", 20))),
    "idle-globe": lambda: (m.home(2), time.sleep(3), wrap("idle-globe", lambda: m.idle("idle-globe", 20))),
}
if __name__ == "__main__":
    print("period_ms", m.PERIOD / 1e6, "hz", m.real)
    try:
        for n in (m.ONLY or S): S[n]()
    finally:
        for d in ("/run/mediabox-trace", "/run/mediabox-trace.done"): m.shutil.rmtree(d, ignore_errors=True)
        m.home(0)
