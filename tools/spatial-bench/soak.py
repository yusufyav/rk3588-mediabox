#!/usr/bin/env python3
"""Uzun koşu: sürekli hareket eden sahne ile gezinme dönüşümlü, kaynak ve ısı.

    python3 soak.py <çıktı> <dakika> "<sahne>" "<sahne>"

Her turda ilk sahne 20 s sürekli hareket eder, ikincisi altında kumanda gezinir
(tuş tekrarı, ters yön, uygulamalar arası), sonra arayüz 8 s dinlenir. Biri bir şey
izlemeye başlarsa durur. Başlatma `sweep.py` ile aynıdır.
"""
import glob, json, os, sys, time
HERE = os.path.dirname(os.path.abspath(__file__)); sys.path.insert(0, HERE)
out, minutes, moving, shown = sys.argv[1], float(sys.argv[2]), sys.argv[3], sys.argv[4]
sys.argv = [sys.argv[0], out, "20"]
import sweep as s
m = s.m
def zones(): return {open(z + "/type").read().strip(): int(open(z + "/temp").read()) // 1000 for z in glob.glob("/sys/class/thermal/thermal_zone*")}
def sample(**more):
    r = dict(t=round(time.time() - T0), gpu_mem=round(m.gmem(), 1), rss=round(m.rss()), fds=len(os.listdir(f"/proc/{m.PID}/fd")), zones=zones(),
             gpu_mhz=m.freq() // 10**6, throttle=[open(f).read().strip() for f in sorted(glob.glob("/sys/class/thermal/cooling_device*/cur_state"))], **more)
    print(json.dumps(r), flush=True); open(f"{out}/soak.jsonl", "a").write(json.dumps(r) + "\n")
T0 = time.time()
s.put("nothing"); m.home(2)
try:
    sample(at="start")
    n = 0
    while time.time() - T0 < minutes * 60:
        n += 1
        s.point(moving); sample(at="moving", round=n)
        s.put(shown); time.sleep(1.5); m.home(0)
        m.run("soak-repeat", ([("down", 80)] * 4 + [("up", 80)] * 4) * 4, 880)
        m.run("soak-reverse", [("down", 150), ("up", 150)] * 8, 880)
        m.home(2)
        time.sleep(8); sample(at="rest", round=n)
    s.put("nothing"); time.sleep(6); sample(at="end")
finally: s.end()
