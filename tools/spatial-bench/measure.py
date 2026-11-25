#!/usr/bin/env python3
"""QuietOS ölçümü: mediabox-trace (commit/flip), kbase meşgul süresi, PMU komutu."""
import ctypes, glob, json, os, re, shutil, struct, subprocess, sys, threading, time
from idle import busy, call
OUT=sys.argv[1]; ONLY=sys.argv[2:]
os.makedirs(OUT,exist_ok=True)
PID=int(subprocess.run(["pgrep","-o","-x","mediabox-tv"],capture_output=True,text=True).stdout)
now=lambda: time.clock_gettime_ns(time.CLOCK_MONOTONIC)
mode=re.search(r"Display mode: \d+x\d+p([\d.]+)\n\s*dclk\[(\d+).*?real_dclk\[(\d+).*?\n\s*H: \d+ \d+ \d+ (\d+)\n\s*V: \d+ \d+ \d+ (\d+)",open("/sys/kernel/debug/dri/0/summary").read(),re.S)
real=int(mode[3])*1000/(int(mode[4])*int(mode[5])); PERIOD=1e9/real
libc=ctypes.CDLL(None,use_errno=True)
def perf(tid):
    # PERF_TYPE_HARDWARE, INSTRUCTIONS; inherit off, all modes
    attr=struct.pack("IIQQQQQIIQQQQIiQI",0,112,1,0,0,0,0,0,0,0,0,0,0,0,0,0,0).ljust(112,b"\0")
    return libc.syscall(241,attr,tid,-1,-1,0)
def instr_open(): return [f for f in (perf(int(t)) for t in os.listdir(f"/proc/{PID}/task")) if f>=0]
def instr_read(fds): return sum(struct.unpack("Q",os.read(f,8))[0] for f in fds)
def gpu():
    b=open("/sys/kernel/debug/mali0/dvfs_utilization").read(); m=re.findall(r"\d+",b); return int(m[0]),int(m[1])
def freq(): return int(open("/sys/class/devfreq/fb000000.gpu/cur_freq").read())
class Gpu(threading.Thread):
    def run(self):
        self.stop=False; self.cyc=0.0; self.busy=0; self.tot=0; self.f=[]; p=gpu()
        while not self.stop:
            f=freq(); time.sleep(.004); q=gpu()
            db=q[0]-p[0]; dt=db+q[1]-p[1]
            if db>=0 and dt>=0:
                self.busy+=db; self.tot+=dt; self.cyc+=db*f
                if db: self.f.append((f,db))
            p=q
def cpu_ticks():
    s=open(f"/proc/{PID}/stat").read().rsplit(")",1)[1].split(); return int(s[11])+int(s[12])
def rss(): return int(re.search(r"VmRSS:\s+(\d+)",open(f"/proc/{PID}/status").read())[1])/1024
def gmem(): return int(re.search(r"mediabox-tv\s+\d+\s+(\d+)",open("/sys/kernel/debug/mali0/gpu_memory").read())[1])*4096/1048576
def key(a):
    t=now(); r=call({"command":"input_inject","action":a}); assert r.get("ok"),r
    return t
def state(): return json.load(open("/var/lib/mediabox-ui/tv-state.json"))
def wait(ms): time.sleep(ms/1000)
def home(col):
    for _ in range(6):
        if state()["screen"]=="home": break
        key("back"); wait(1300)
    assert state()["screen"]=="home",state()
    for _ in range(30):
        c=state()["home_col"]
        if c==col: break
        key("down" if c<col else "up"); wait(400)
    assert state()["home_col"]==col,state(); wait(1500)
def settings():
    home(3); key("ok"); wait(2000); assert state()["screen"]=="settings",state()
def column():
    settings(); key("left"); wait(600)
    for _ in range(8): key("up"); wait(300)
    wait(1000)
def pct(v,p):
    v=sorted(v); return round(v[min(len(v)-1,int(len(v)*p/100))],2) if v else None
def run(name,keys,dur):
    """keys: [(action, gap to the next press ms)]; dur: a move's drawn length."""
    w=busy()
    if w: print("ABORT",w); sys.exit(1)
    D="/run/mediabox-trace"; assert not os.path.exists(D)
    os.mkdir(D); wait(600)
    fds=instr_open(); i0=instr_read(fds); c0=cpu_ticks(); g=Gpu(); g.start(); t0=now(); sent=[]
    for a,gap in keys:
        t=time.monotonic(); sent.append(key(a)); rest=gap/1000-(time.monotonic()-t)
        if rest>0: time.sleep(rest)
    wait(200); t1=now(); g.stop=True; g.join(); i1=instr_read(fds); c1=cpu_ticks()
    for f in fds: os.close(f)
    dst=f"{OUT}/{name}.trace.d"; shutil.rmtree(dst,ignore_errors=True)
    os.rename(D,"/run/mediabox-trace.done"); wait(700); shutil.move("/run/mediabox-trace.done",dst)
    com=[];fl=[]
    for f in glob.glob(dst+"/tv-*.trace"):
        for l in open(f):
            p=l.split()
            if len(p)<2 or not p[0].isdigit(): continue
            if p[1]=="commit" and "ui=1" in p: com.append(int(p[0]))
            elif p[1]=="flip":
                kv=dict(x.split("=") for x in p[2:]); fl.append((int(kv["seq"]),int(kv["at"])))
    com.sort(); fl.sort()
    import bisect
    missed=vb=frames=0; fi=[]; ci=[]; lc=[]; lf=[]; nolat=0
    at=[x[1] for x in fl]
    for n,s in enumerate(sent):
        end=min(s+dur*1e6, sent[n+1] if n+1<len(sent) else 1e30)
        # Bir sonraki kareye kadarki aralık da bu hareketindir: tuş tekrarında sürekli.
        i=bisect.bisect_right(com,s)
        nxt=sent[n+1] if n+1<len(sent) else 1<<62
        if i<len(com) and com[i]<nxt+0 and com[i]-s<dur*1e6:
            lc.append((com[i]-s)/1e6); j=bisect.bisect_right(at,com[i])
            if j<len(at): lf.append((at[j]-s)/1e6)
        else: nolat+=1
        j=bisect.bisect_right(at,com[i]) if i<len(com) else len(at)
        seg=[x for x in fl[j:] if x[1]<=end]
        # Sürekli basışta önceki parçanın sonuyla bağla.
        if n and gapless(keys[n-1][1],dur) and j>0 and seg: seg=[fl[j-1]]+seg
        for a,b in zip(seg,seg[1:]):
            missed+=b[0]-a[0]-1; vb+=b[0]-a[0]; fi.append((b[1]-a[1])/1e6)
        frames+=len(seg)
        cs=[x for x in com[i:] if x<=end]; ci+=[(b-a)/1e6 for a,b in zip(cs,cs[1:])]
    unit=(t1-t0)/max(1,g.tot); fs=sum(f*b for f,b in g.f)/max(1,sum(b for f,b in g.f))/1e6
    r=dict(name=name,keys=len(keys),frames=frames,present_ms=[pct(fi,p) for p in (50,95,99)],commit_iv_ms=[pct(ci,p) for p in (50,95,99)],
      missed=missed,vblanks=vb,missed_pct=round(100*missed/max(1,vb),2),
      in_commit=[pct(lc,p) for p in (50,95,99)],in_flip=[pct(lf,p) for p in (50,95,99)],no_latency=nolat,
      cpu_pct=round((c1-c0)*1e9/os.sysconf("SC_CLK_TCK")/(t1-t0)*100,1),minstr_key=round((i1-i0)/1e6/max(1,len(keys)),1),
      minstr_frame=round((i1-i0)/1e6/max(1,len(com)),2),gpu_busy_pct=round(100*g.busy/max(1,g.tot),1),gpu_ms_frame=round(g.busy*unit/1e6/max(1,len(com)),2),
      gpu_mhz=round(fs),mcyc_key=round(g.cyc*unit/1e15*1e0/max(1,len(keys))*1e0,1) if False else round(g.cyc*unit/1e9/1e6/max(1,len(keys)),1),
      mcyc_frame=round(g.cyc*unit/1e9/1e6/max(1,len(com)),2),
      gpu_mem_mb=round(gmem(),1),rss_mb=round(rss()),commits=len(com),wall_s=round((t1-t0)/1e9,1))
    print(json.dumps(r),flush=True); open(f"{OUT}/results.jsonl","a").write(json.dumps(r)+"\n")
def gapless(gap,dur): return gap<dur
def idle(name,secs):
    if busy(): sys.exit("ABORT")
    os.mkdir("/run/mediabox-trace"); wait(600); fds=instr_open(); i0=instr_read(fds);c0=cpu_ticks();g=Gpu();g.start();t0=now();time.sleep(secs);t1=now();g.stop=True;g.join()
    i1=instr_read(fds);c1=cpu_ticks();dst=f"{OUT}/{name}.trace.d";shutil.rmtree(dst,ignore_errors=True)
    os.rename("/run/mediabox-trace","/run/mediabox-trace.done");wait(700);shutil.move("/run/mediabox-trace.done",dst)
    n=sum(1 for f in glob.glob(dst+"/tv-*.trace") for l in open(f) if " commit " in l and "ui=1" in l and t0<=int(l.split()[0])<=t1)
    r=dict(name=name,secs=secs,ui_commits=n,cpu_pct=round((c1-c0)/os.sysconf("SC_CLK_TCK")/secs*100,2),minstr_s=round((i1-i0)/1e6/secs,1),gpu_busy_pct=round(100*g.busy/max(1,g.tot),2),gpu_mem_mb=round(gmem(),1),rss_mb=round(rss()))
    print(json.dumps(r),flush=True);open(f"{OUT}/results.jsonl","a").write(json.dumps(r)+"\n")
S={}
def sc(f): S[f.__name__.replace("_","-")]=f; return f
# 900 ms ışık: pencere 880, sondaki dinlenme sayılmaz. Ayarlarda amblem 560: 540.
@sc
def film_live(): home(0); run("film-live",[("down",1100),("up",1100)]*10,880)
@sc
def browser(): home(1); run("browser",[("down",1100),("up",1100)]*8,880)
@sc
def settings_scene(): home(2); run("settings-scene",[("down",1100),("up",1100)]*8,880)
@sc
def repeat(): home(0); run("repeat",([("down",80)]*4+[("up",80)]*4)*6,880)
@sc
def reverse(): home(0); run("reverse",[("down",150),("up",150)]*14,880)
@sc
def home_settings():
    settings(); home(3); run("home-settings",[("ok",1200),("back",1200)]*8,540)
@sc
def home_settings_cut(): home(3); run("home-settings-cut",[("ok",220),("back",220)]*8+[("ok",220),("back",1200)],540)
@sc
def category(): column(); run("category",[("down",800),("up",800)]*12,540)
@sc
def category_repeat(): column(); run("category-repeat",([("down",80)]*4+[("up",80)]*4)*5,540)
@sc
def idle_home(): home(0); wait(3000); idle("idle-home",60)
@sc
def idle_settings(): settings(); wait(3000); idle("idle-settings",60)
if __name__=="__main__":
    print("period_ms",PERIOD/1e6,"hz",real)
    try:
        for n in (ONLY or S): S[n]()
    finally:
        for d in ("/run/mediabox-trace","/run/mediabox-trace.done"): shutil.rmtree(d,ignore_errors=True)
        home(0)
