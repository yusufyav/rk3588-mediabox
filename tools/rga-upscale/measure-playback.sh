#!/bin/sh
# measure-playback.sh <label> <file> <seconds> [keep]
#
# Runs on the appliance. Plays <file> through the daemon -- the production
# path -- waits until it is really playing, then records what a playback
# change is judged on: the player's argv, its open devices (/dev/rga), mpv's
# own view of the chain (vf, decoder and output params), the Esmart0 plane and
# VP0 mode from debugfs, and over <seconds>: CPU, GPU, RGA, NPU, DMC load, temperature,
# dropped frames and A/V sync. Two runs are compared by diffing their files.
#
# The file must live under the media worker's --allow-file-prefix
# (/opt/rk3588-mediabox/assets/); take it out again afterwards. Stops the film
# at the end unless [keep] is given. Writes $MEASURE_DIR/m-<label>.txt.
set -u
label=$1 file=$2 secs=$3 keep=${4:-}
SOCK=/run/mediabox/mediaboxd.sock
out=${MEASURE_DIR:-/var/tmp/rga-upscale}/m-$label.txt
mkdir -p "${out%/*}"
exec >"$out" 2>&1
rpc() { python3 - "$SOCK" "$1" <<'P'
import socket,sys
s=socket.socket(socket.AF_UNIX);s.settimeout(30);s.connect(sys.argv[1]);s.sendall(sys.argv[2].encode()+b"\n")
b=b""
while b"\n" not in b:
    c=s.recv(65536)
    if not c: break
    b+=c
print(b.decode().strip())
P
}
ipc() { python3 - "$1" "$2" <<'P'
import socket,sys,json
s=socket.socket(socket.AF_UNIX);s.settimeout(5);s.connect(sys.argv[1])
out={}
for i,p in enumerate(sys.argv[2].split()):
    s.sendall((json.dumps({"command":["get_property",p],"request_id":i})+"\n").encode())
    b=b""
    while True:
        b+=s.recv(65536)
        lines=[l for l in b.split(b"\n") if l.strip()]
        got=[json.loads(l) for l in lines]
        r=[g for g in got if g.get("request_id")==i]
        if r: out[p]=r[0].get("data", r[0].get("error")); break
for k,v in out.items(): print(f"  {k} = {json.dumps(v,sort_keys=True)}")
P
}
echo "== $label  file=$file  window=${secs}s  $(date -Is)"
rpc '{"command":"media_stop_here"}' >/dev/null 2>&1; sleep 2
rpc "{\"command\":\"media_play_here\",\"url\":\"file://$file\",\"start_seconds\":0,\"title\":\"upscale $label\"}" | cut -c1-160
i=0; while [ $i -lt 40 ]; do
  st=$(rpc '{"command":"media_status_here"}')
  case "$st" in *'"playing":true'*) break;; esac
  sleep 0.5; i=$((i+1)); done
sleep 6
pid=$(systemctl show mediabox-player.service -p MainPID --value)
echo "pid=$pid exe=$(readlink /proc/$pid/exe)"
echo "argv: $(tr '\0' ' ' </proc/$pid/cmdline)"
echo "fds: rga=$(ls -l /proc/$pid/fd | grep -c /dev/rga) mpp=$(ls -l /proc/$pid/fd | grep -c mpp_service) dma_heap=$(ls -l /proc/$pid/fd | grep -c dma_heap)"
echo "maps librga: $(grep -c librga /proc/$pid/maps)"
sock=$(tr '\0' '\n' </proc/$pid/cmdline | sed -n 's/^--input-ipc-server=//p')
P="hwdec-current vf container-fps estimated-vf-fps display-fps video-dec-params video-params video-out-params"
ipc "$sock" "$P"
D="frame-drop-count decoder-frame-drop-count vo-delayed-frame-count mistimed-frame-count total-avsync-change avsync time-pos"
echo "-- counters at start"; ipc "$sock" "$D"
echo "-- Esmart0 plane"; awk '/^plane\[73\]/{p=1} p&&/^plane\[/&&!/plane\[73\]/{p=0} p' /sys/kernel/debug/dri/0/state
echo "-- VP0"; awk '/^Video Port0/{p=1} /^Video Port1/{p=0} p' /sys/kernel/debug/dri/0/summary | grep -vE '^\s+(H|V|Fixed)'
echo "-- HDMI-A-1 connector"; awk '/^connector.*HDMI-A-1/{p=1} p&&/^connector/&&!/HDMI-A-1/{p=0} p' /sys/kernel/debug/dri/0/state | head -12
cpu() { awk '{print $14+$15}' /proc/$pid/stat; }
sys() { awk '/^cpu /{print $2+$3+$4+$5+$6+$7+$8, $5+$6}' /proc/stat; }
c0=$(cpu); s0=$(sys)
rga_max=0; gpu_sum=0; npu_max=0; dmc_sum=0; dmcf_max=0; n=0; t_max=0; rga_sum=0
while [ $n -lt "$secs" ]; do
  r=$(awk '/^[[:space:]]+load = /{v=$3; sub("%","",v); v+=0; if(v>m)m=v; s+=v} END{print m+0, s+0}' /sys/kernel/debug/rkrga/load)
  rm=${r% *}; rs=${r#* }
  [ "$rm" -gt "$rga_max" ] && rga_max=$rm; rga_sum=$((rga_sum+rs))
  g=$(cut -d@ -f1 /sys/class/devfreq/fb000000.gpu/load); gpu_sum=$((gpu_sum+g))
  np=$(grep -o '[0-9]*%' /sys/kernel/debug/rknpu/load | tr -d % | sort -n | tail -1); [ "${np:-0}" -gt "$npu_max" ] && npu_max=$np
  d=$(cut -d@ -f1 /sys/class/devfreq/dmc/load); dmc_sum=$((dmc_sum+d))
  df=$(cat /sys/class/devfreq/dmc/cur_freq); [ "$df" -gt "$dmcf_max" ] && dmcf_max=$df
  t=$(cat /sys/class/thermal/thermal_zone*/temp | sort -n | tail -1); [ "$t" -gt "$t_max" ] && t_max=$t
  [ "$n" -eq 3 ] && { echo "-- rkrga load sample (t=3s)"; cat /sys/kernel/debug/rkrga/load | grep -vE '^-+$|^=+$'; }
  sleep 1; n=$((n+1))
done
c1=$(cpu); s1=$(sys)
hz=$(getconf CLK_TCK)
echo "-- window ${secs}s"
echo "mpv_cpu_pct_of_one_core=$(awk -v a=$c0 -v b=$c1 -v s=$secs -v h=$hz 'BEGIN{printf "%.1f",(b-a)/h/s*100}')"
set -- $s0; st0=$1 si0=$2; set -- $s1; st1=$1 si1=$2
echo "system_cpu_busy_pct=$(awk -v a=$st0 -v b=$st1 -v x=$si0 -v y=$si1 'BEGIN{printf "%.1f",100-(y-x)/(b-a)*100}')"
echo "rga_max_sched_load_pct=$rga_max rga_sum_load_avg_pct=$((rga_sum/secs))"
echo "gpu_load_avg_pct=$((gpu_sum/secs)) npu_max_pct=$npu_max"
echo "dmc_load_avg_pct=$((dmc_sum/secs)) dmc_freq_max=$dmcf_max"
echo "temp_max_mC=$t_max"
echo "-- counters at end"; ipc "$sock" "$D"
echo "dmesg rga/mpp/vop errors since start:"; dmesg --since "-$((secs+15))s" 2>/dev/null | grep -iE "rga|mpp|vop|iommu|err" | tail -5
[ -z "$keep" ] && rpc '{"command":"media_stop_here"}' >/dev/null
echo "== done"
