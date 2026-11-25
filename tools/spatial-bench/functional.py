#!/usr/bin/env python3
"""İşlevsel regresyon: arayüz GPU sahnesiyle çalışırken oynatma, Kodi, mod, yüz, fare.

    python3 functional.py <çıktı> [adım...]

Her adımdan önce cihaz boş olmalıdır; değilse durur. Kullanıcı ayarı kalıcı
değiştirilmez: mod denemesi `display keep` edilmeden geri alınır, seçili yüz geri
konur. Arayüz MEDIABOX_TV_SPATIAL=globe MEDIABOX_TV_MOTION_LAB=1 ile başlatılmış
olmalı (variant.sh).
"""
import fcntl, glob, json, os, re, shutil, signal, struct, subprocess, sys, time
HERE = os.path.dirname(os.path.abspath(__file__)); sys.path.insert(0, HERE)
OUT, ONLY = sys.argv[1], sys.argv[2:]
sys.argv = [sys.argv[0], OUT]
import measure as m
from idle import busy, call
P = "/opt/rk3588-mediabox"; CTL = P + "/bin/mediaboxctl"
fails = 0
def sh(*a, t=60): r = subprocess.run(a, capture_output=True, text=True, timeout=t); return (r.stdout + r.stderr).strip()
def vop(): return open("/sys/kernel/debug/dri/0/summary").read()
def mode(): return re.search(r"Display mode: (\S+)", vop())[1]
def pid(): return int(sh("systemctl", "show", "-p", "MainPID", "--value", "mediabox-tv-ui"))
def say(ok, what, seen):
    global fails; fails += not ok
    print(f"{'PASS' if ok else 'FAIL'}  {what}: {seen}", flush=True)
def snap(name):
    d = "/var/lib/mediabox-ui/snapshots"; t = time.time()
    os.kill(pid(), signal.SIGUSR1); f = None
    for _ in range(40):
        time.sleep(.25); n = [p for p in glob.glob(d + "/*.png") if os.path.getmtime(p) > t - .1 and os.path.getsize(p) > 1000]
        if n: f = n[0]; break
    if not f: say(False, "anlık görüntü " + name, "yok"); return
    time.sleep(1); shutil.copy(f, f"{OUT}/{name}.png")
def spatial_lines(since): return [l for l in m_journal(since) if "tv.spatial" in l or "panick" in l or "GL error" in l]
def m_journal(since): return sh("journalctl", "-u", "mediabox-tv-ui", "--since", "@%d" % since, "-o", "cat", "--no-pager").splitlines()
def gmem(): return int(re.search(r"mediabox-tv\s+\d+\s+(\d+)", open("/sys/kernel/debug/mali0/gpu_memory").read())[1]) * 4096 / 1048576
def well(since, name, same=None):
    """Arayüz ayakta, sahne GPU'dan, hata yok."""
    for _ in range(30):
        if sh("systemctl", "is-active", "mediabox-tv-ui") == "active": break
        time.sleep(.5)
    p = pid(); m.PID = p
    bad = [l for l in spatial_lines(since) if "given back" in l or "panick" in l or "rror" in l]
    say(not bad and sh("systemctl", "show", "-p", "NRestarts", "--value", "mediabox-tv-ui") == "0", name + ": arayüz", bad or f"pid {p}")
    if same: say(p == same, name + ": aynı süreç, aynı bağlam", f"{same} -> {p}")
    m.home(2); time.sleep(1.5); snap(name)
    rss = int(re.search(r"VmRSS:\s+(\d+)", open(f"/proc/{p}/status").read())[1]) // 1024
    print(f"      GPU belleği {gmem():.1f} MB, RSS {rss} MB, fd {len(os.listdir(f'/proc/{p}/fd'))}", flush=True)
def play(asset, want):
    t, p = time.time(), pid()
    r = call({"command": "media_play_here", "url": f"file://{P}/assets/{asset}", "start_seconds": 0, "title": "regresyon"})
    say(bool(r.get("ok")), "oynatma istendi", asset)
    time.sleep(2.0); v = vop()
    st = call({"command": "media_status_here"}).get("result") or {}
    dec = next((x.get("decoder", "") for x in st.get("tracks") or [] if x.get("type") == "video"), "")
    say("rkmpp" in dec, "donanım çözücü", dec)
    planes = re.findall(r"(\S+win\d): ACTIVE\n.*?format: (\S+).*?\n\s*color: (\S+)", v, re.S)
    head = re.search(r"bus_format\[\w+\]: (\S+)\n.*?output_mode.*?(\S+)\[\d\] color-encoding\[(\S+)\]", v)
    say(all(re.search(w, v) for w in want), "VOP", (head.groups() if head else "?", planes))
    call({"command": "media_stop_here"}); time.sleep(5)
    return t, p
S = {}
def step(f): S[f.__name__] = f; return f
@step
def sdr():
    t, p = play("sdr-4k-2398-main.mp4", [r"format: NV12", r"SDR\[0\]"]); well(t, "sdr", p)
@step
def hdr10():
    before = re.search(r"bus_format.*", vop())[0]
    t, p = play("hdr10-4k-2398-main10.mp4", [r"format: NV15", r"HDR10\[\d\]"]); time.sleep(2)
    say(re.search(r"bus_format.*", vop())[0] == before, "HDR sonrası çıkış biçimi", before); well(t, "hdr10", p)
@step
def smoke():
    o = sh(P + "/bin/mediabox-playback-smoke", t=120); say("FAIL" not in o, "playback-smoke", "%d PASS" % o.count("PASS ")); print(o)
    time.sleep(3); well(time.time() - 60, "smoke")
@step
def churn():
    """Oynatma ile arayüz arasında on gidiş geliş: kaynak birikmesi."""
    p = pid(); t = time.time(); seen = []
    for _ in range(10):
        call({"command": "media_play_here", "url": f"file://{P}/assets/sdr-4k-2398-main.mp4", "start_seconds": 0, "title": "regresyon"}); time.sleep(2.2)
        call({"command": "media_stop_here"}); time.sleep(3)
        m.PID = p; seen.append((round(gmem(), 1), round(m.rss()), len(os.listdir(f"/proc/{p}/fd"))))
    print("      (GPU MB, RSS MB, fd):", seen)
    say(seen[-1][0] <= seen[1][0] + 4 and seen[-1][2] <= seen[1][2] + 2, "on oynatma, birikme yok", (seen[1], seen[-1])); well(t, "churn", p)
@step
def livetv():
    t, p = time.time(), pid()
    m.home(1); m.key("ok"); time.sleep(20)
    v = vop(); st = m.state()
    say(len(re.findall(r"win\d: ACTIVE", v)) >= 2, "Canlı TV düzlemi", (st["screen"], re.findall(r"(\S+win\d): ACTIVE\n.*?format: (\S+)", v, re.S)))
    for _ in range(4):
        m.key("back"); time.sleep(3)
        if m.state()["screen"] == "home" and not busy(): break
    say(m.state()["screen"] == "home", "Canlı TV'den dönüş", m.state()["screen"]); time.sleep(3); well(t, "livetv", p)
@step
def kodi():
    t = time.time(); m_before = mode()
    print("      ", sh(CTL, "surface", "switch", "kodi", t=90)); time.sleep(15)
    say(sh("systemctl", "is-active", "kodi") == "active", "Kodi ayakta", sh(CTL, "surface", "status"))
    print("      ", sh(CTL, "surface", "switch", "ui", t=90)); time.sleep(10)
    say(sh(CTL, "surface", "status").endswith("MediaBox arayüzü"), "arayüze dönüş", sh(CTL, "surface", "status"))
    say(mode() == m_before, "mod", mode()); well(t, "kodi")
@step
def cec():
    t, p = time.time(), pid()
    o = sh(CTL, "cec", "status"); d = sh(CTL, "cec", "devices")
    say("Mantıksal adresler: [4]" in o, "CEC adresi", o.splitlines()[:3]); print("      ", d.replace("\n", " | ")[:300])
    # Televizyonun kumandasının yolu: cec-gpio değil, çekirdeğin rc giriş aygıtı.
    say(any("dw_hdmi" in open(n).read() or "hdmi" in open(n).read().lower() for n in glob.glob("/sys/class/input/event*/device/name")), "CEC giriş aygıtı", [open(n).read().strip() for n in glob.glob("/sys/class/input/event*/device/name")])
    well(t, "cec", p)
@step
def modes():
    t, p, was = time.time(), pid(), mode(); kept = sh(CTL, "display", "status").splitlines()[-1]
    for label in ("1920x1080@60", "3840x2160@30", "1920x1080@50"):
        print("      ", sh(CTL, "display", "set", label).splitlines()[:1]); time.sleep(5)
        w, hz = label.split("@"); now = mode()
        say(now == f"{w}p{hz}", "mod " + label, now)
        m.home(2); m.key("up"); time.sleep(.3); m.key("down"); time.sleep(1.5); snap("mode-" + label.replace("@", "p"))
        print("      ", sh(CTL, "display", "revert").splitlines()[:1]); time.sleep(5)
        say(mode() == was, "geri", mode())
    say(sh(CTL, "display", "status").splitlines()[-1] == kept, "kullanıcı seçimi", kept); well(t, "modes", p)
def prefs(): return open("/var/lib/mediabox-ui/interface-preferences.json").read()
@step
def faces():
    """Ayarlar > Sistem > Arayüz düzeni: Klasik ve geri, kumandayla. Quiet OS seçiliyken."""
    t, p, was = time.time(), pid(), prefs()
    if '"quiet"' not in was: return say(False, "Quiet OS seçili değil", was)
    def choose(way): m.key("ok"); time.sleep(1.2); m.key(way); time.sleep(.5); m.key("ok"); time.sleep(2.5)
    for n in range(3):
        m.settings(); m.key("left"); time.sleep(.5)
        for _ in range(9): m.key("up"); time.sleep(.2)
        for _ in range(6): m.key("down"); time.sleep(.2)
        m.key("right"); time.sleep(1)
        choose("up"); other, held = prefs(), gmem(); choose("down")
        say('"classic"' in other and prefs() == was, "Klasik ve geri", f"GPU belleği Klasik'te {held:.1f} MB")
        m.home(2); time.sleep(3); print(f"      ana ekran {gmem():.1f} MB")
    well(t, "faces", p)
@step
def reduced():
    t, p = time.time(), pid()
    # Lab'ın satırı süreçte kalır: yeni başlatılmış bir arayüzde koşulur.
    m.home(2); m.key("menu"); time.sleep(1)
    for _ in range(6): m.key("down"); time.sleep(.2)
    snap("lab"); m.key("right"); time.sleep(.6); snap("lab-reduced"); m.key("back"); time.sleep(.6)
    os.makedirs("/run/mediabox-trace", exist_ok=True); time.sleep(.6); t0 = m.now()
    m.key("down"); time.sleep(1.5); m.key("up"); time.sleep(1.5)
    os.rename("/run/mediabox-trace", "/run/mediabox-trace.done"); time.sleep(.7)
    n = sum(1 for f in glob.glob("/run/mediabox-trace.done/tv-*.trace") for l in open(f) if " commit " in l and "ui=1" in l and int(l.split()[0]) > t0)
    shutil.rmtree("/run/mediabox-trace.done")
    say(n < 40, "azaltılmış hareket: iki gezinmede çizilen kare", n)
    m.key("menu"); time.sleep(.6); m.key("right"); time.sleep(.6); m.key("back"); time.sleep(.6); well(t, "reduced", p)
@step
def mouse():
    """Sanal bir fare (uinput): imleç, hover ve tıklama."""
    t, p = time.time(), pid()
    U = os.open("/dev/uinput", os.O_WRONLY | os.O_NONBLOCK)
    for req, v in ((0x40045564, 1), (0x40045564, 2), (0x40045565, 0x110), (0x40045565, 0x111), (0x40045566, 0), (0x40045566, 1)): fcntl.ioctl(U, req, v)
    os.write(U, struct.pack("80sHHHHI", b"spatial-regresyon-fare", 3, 0x46d, 0x4001, 1, 0) + b"\0" * (64 * 4 * 4)); fcntl.ioctl(U, 0x5501); time.sleep(3)
    def ev(*e):
        for ty, c, v in e: os.write(U, struct.pack("llHHi", 0, 0, ty, c, v))
        os.write(U, struct.pack("llHHi", 0, 0, 0, 0, 0))
    try:
        m.home(0)
        for _ in range(200): ev((2, 0, -60), (2, 1, -60)); time.sleep(.004)
        for _ in range(70): ev((2, 0, 3), (2, 1, 3)); time.sleep(.012)
        for _ in range(110): ev((2, 1, 3)); time.sleep(.012)
        time.sleep(.5); snap("mouse")
        ev((2, 0, 1)); time.sleep(.3); ev((1, 0x110, 1)); time.sleep(.08); ev((1, 0x110, 0)); time.sleep(2)
        st = m.state(); say(st["screen"] != "home" or st["home_col"] != 0, "tıklama seçti", st); snap("mouse-click")
    finally: fcntl.ioctl(U, 0x5502); os.close(U)
    time.sleep(1); m.key("up"); time.sleep(1); well(t, "mouse", p)
os.makedirs(OUT, exist_ok=True)
try:
    for n in ONLY or S:
        w = busy()
        if w: sys.exit("ABORT %s: %s" % (n, w))
        print("==", n, flush=True); S[n]()
finally:
    for d in ("/run/mediabox-trace", "/run/mediabox-trace.done"): shutil.rmtree(d, ignore_errors=True)
    if not busy(): m.home(0)
print("== %d FAIL" % fails); sys.exit(fails > 0)
