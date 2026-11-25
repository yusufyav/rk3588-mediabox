#!/usr/bin/env python3
"""Zorunlu önkoşul: cihazda kimse bir şey izlemiyor. Değilse nedeniyle 1 döner."""
import glob, json, re, socket, subprocess, sys, time
def call(r):
    s=socket.socket(socket.AF_UNIX); s.settimeout(10); s.connect("/run/mediabox/mediaboxd.sock")
    s.sendall((json.dumps(r)+"\n").encode()); b=b""
    while not b.endswith(b"\n"):
        d=s.recv(65536)
        if not d: break
        b+=d
    return json.loads(b)
def busy(quiet_s=0):
    why=[]
    st=call({"command":"status"}).get("result",{})
    if (st.get("surface") or {}).get("active")!="ui": why.append("surface %r"%st.get("surface"))
    if (st.get("media") or {}).get("sessions")!=0: why.append("media session")
    if (st.get("kodi") or {}).get("running"): why.append("kodi")
    for u in ("kodi","mediabox-browser","screenbridge-daemon"):
        if subprocess.run(["systemctl","is-active","-q",u]).returncode==0: why.append(u)
    for p in ("mediabox-live","mediabox-dvb","mediabox-player","mpv","chromium","kodi.bin"):
        if subprocess.run(["pgrep","-f","/%s( |$)"%p],capture_output=True).stdout: why.append("process "+p)
    for f in glob.glob("/proc/asound/card*/pcm*p/sub*/status"):
        if "state:" in open(f).read(): why.append("pcm "+f)
    planes=len(re.findall(r"win\d: ACTIVE",open("/sys/kernel/debug/dri/0/summary").read()))
    if planes!=1: why.append("%d planes"%planes)
    if not subprocess.run(["systemctl","is-active","-q","mediabox-tv-ui"]).returncode==0: why.append("no ui")
    if quiet_s:
        out=subprocess.run(["journalctl","-u","mediabox-tv-ui","--since","-%ds"%quiet_s,"-o","cat","--no-pager"],capture_output=True,text=True).stdout
        n=[float(x) for x in re.findall(r'"fps":([0-9.]+)',out)]
        if any(x>2 for x in n): why.append("interface moved in the last %d s"%quiet_s)
    return why
if __name__=="__main__":
    w=busy(int(sys.argv[1]) if len(sys.argv)>1 else 0)
    print("IDLE" if not w else "BUSY: "+"; ".join(w)); sys.exit(1 if w else 0)
