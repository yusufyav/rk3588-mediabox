#!/usr/bin/env python3
"""Küre deneyinin senaryoları: measure.py'nin (final-gate) birimleriyle, yanında
VPU kesmeleri, DDR, sistem geneli komut ve küre karelerinin izi.

    python3 measure-globe.py <çıktı dizini> [senaryo...]

Yanında `measure.py` ve `idle.py` olmalı (PR #5 ölçüm araçları).
"""
import glob, json, os, re, subprocess, sys, threading, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import measure as m

# Küre dinlenirken ikinci plane onundur; başka her şey yine durdurur.
def quiet(): return [w for w in m_busy() if w != "2 planes"]
m_busy = m.busy
m.busy = quiet

def irqs():
    return sum(int(x) for line in open("/proc/interrupts") if "rkvdec-core" in line for x in line.split()[1:1 + os.cpu_count()])
def ddr(): return tuple(int(x) for x in re.findall(r"\d+", open("/sys/class/devfreq/dmc/load").read()))[:2]
class Ddr(threading.Thread):
    def run(self):
        self.stop = False; self.l = []
        while not self.stop:
            self.l.append(ddr()); time.sleep(.05)
def allcpu():
    import ctypes, struct
    fds = []
    for cpu in range(os.cpu_count()):
        attr = struct.pack("IIQQQQQIIQQQQIiQI", 0, 112, 1, *([0] * 14)).ljust(112, b"\0")
        fds.append(m.libc.syscall(241, attr, -1, cpu, -1, 0))
    return [f for f in fds if f >= 0]
def dmabuf():
    t = open("/sys/kernel/debug/dma_buf/bufinfo").read()
    k = re.search(r"Total (\d+) objects, (\d+) bytes", t); return int(k[1]), round(int(k[2]) / 1048576, 1)
def wrap(name, go):
    i0 = irqs(); d = Ddr(); d.start(); f = allcpu(); a0 = m.instr_read(f); t0 = time.time(); go(); wall = time.time() - t0
    a1 = m.instr_read(f); [os.close(x) for x in f]; d.stop = True; d.join()
    time.sleep(1.2)
    log = subprocess.run(["journalctl", "-u", "mediabox-tv-ui", "--since", "@%d" % t0, "-o", "cat", "--no-pager"], capture_output=True, text=True).stdout
    g = [tuple(int(x) for x in re.search(r"frames=(\d+) distinct=(\d+) late_at_begin=(\d+) decoded=(\d+) decode_us p50=(\d+) p95=(\d+) max=(\d+)", l).groups()) for l in log.splitlines() if "globe frames=" in l]
    late = shown = 0
    for p in glob.glob(f"{m.OUT}/{name}.trace.d/tv-*.trace"):
        for l in open(p):
            w = re.search(r" globe .*want=(\d+) shown=(\d+)", l)
            if w: shown += 1; late += w[1] != w[2]
    r = dict(extra=name, vdec_irqs=irqs() - i0, ddr_load=round(sum(x[0] for x in d.l) / len(d.l), 1), ddr_mhz=round(sum(x[1] for x in d.l) / len(d.l) / 1e6),
             sys_minstr_s=round((a1 - a0) / 1e6 / wall, 1), moves=len(g), globe_frames=sum(x[0] for x in g), distinct=sum(x[1] for x in g), decoded=sum(x[3] for x in g),
             decode_us=(sorted(x[4] for x in g)[len(g) // 2], max(x[5] for x in g), max(x[6] for x in g)) if g else None, asked=shown, asked_late=late,
             dmabuf=dmabuf(), gpu_mem=round(m.gmem(), 1), rss=round(m.rss()))
    print(json.dumps(r), flush=True); open(f"{m.OUT}/extra.jsonl", "a").write(json.dumps(r) + "\n")
def step(): m.home(1); wrap("globe-step", lambda: m.run("globe-step", [("down", 1100), ("up", 1100)] * 8, 880))
def repeat(): m.home(1); wrap("globe-repeat", lambda: m.run("globe-repeat", ([("down", 80)] * 2 + [("up", 80)] * 2) * 12, 880))
def reverse(): m.home(1); wrap("globe-reverse", lambda: m.run("globe-reverse", [("down", 150), ("up", 150)] * 14, 880))
def idle(): m.home(2); time.sleep(3); wrap("globe-idle", lambda: m.idle("globe-idle", 20)); m.home(1)
S = dict(step=step, repeat=repeat, reverse=reverse, idle=idle)
try:
    for n in (m.ONLY or S): S[n]()
finally:
    for d in ("/run/mediabox-trace", "/run/mediabox-trace.done"): m.shutil.rmtree(d, ignore_errors=True)
    m.home(0)
