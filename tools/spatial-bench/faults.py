#!/usr/bin/env python3
"""Hata enjeksiyonu, gerçek sürücüde: her hatada sahne arayüzün kendi çizimidir.

    python3 faults.py <çıktı>       (laboratuvar ikilisi, MEDIABOX_TV_SPATIAL_CONTROL)

Her hata için Tarayıcı satırının anlık görüntüsü alınır; karşılaştırma görüntülerdedir.
"""
import os, shutil, signal, sys, time, subprocess
HERE = os.path.dirname(os.path.abspath(__file__)); sys.path.insert(0, HERE)
out = sys.argv[1]; sys.argv = [sys.argv[0], out]
import measure as m
from idle import busy
C = "/run/mediabox-spatial"; D = "/var/lib/mediabox-ui/snapshots/home.png"
def snap(n):
    t = os.path.getmtime(D) if os.path.exists(D) else 0
    os.kill(m.PID, signal.SIGUSR1)
    for _ in range(40):
        time.sleep(.25)
        if os.path.exists(D) and os.path.getmtime(D) != t: break
    time.sleep(1.5); shutil.copy(D, f"{out}/{n}.png")
try:
    open(C, "w").write("globe"); time.sleep(1); m.home(2)
    for scene in ("globe", "advanced"):
        for fault in ("none", "shader", "buffer", "texture", "framebuffer", "draw", "context"):
            if busy(): sys.exit("ABORT")
            t = time.time(); open(C, "w").write(scene + (f" fault={fault}" if fault != "none" else "")); time.sleep(1.5)
            # Sahne hareket ederken de: arada kareler çizilir.
            m.key("up"); time.sleep(.35); m.key("down"); time.sleep(1.6); snap(f"{scene}-{fault}")
            log = subprocess.run(["journalctl", "-u", "mediabox-tv-ui", "--since", "@%d" % t, "-o", "cat", "--no-pager"], capture_output=True, text=True).stdout
            print(scene, fault, [l.split("spatial ")[1][:90] for l in log.splitlines() if "given back" in l][:2], "NRestarts", subprocess.run(["systemctl", "show", "-p", "NRestarts", "--value", "mediabox-tv-ui"], capture_output=True, text=True).stdout.strip(), flush=True)
finally:
    os.unlink(C); m.home(0)
