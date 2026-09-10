#!/usr/bin/env bash
# Samples what a running Kodi GUI costs, for whichever GL user space it was
# started with.
#
#   scripts/gui-load-sample.sh <label> [seconds]
#
# "Mali instead of llvmpipe" is worth nothing as a renderer string, so this
# measures the two things a viewer actually feels: how much CPU the GUI burns,
# and how many frames it manages to put on screen. The frame rate is Kodi's own
# System.FPS infolabel rather than anything inferred, and navigation is driven
# over JSON-RPC so both runs see the same input at the same times.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=env.sh
source "$here/scripts/env.sh"

label="${1:?usage: $0 <label> [seconds]}"
secs="${2:-30}"
: "${KODI_RPC_PORT:=8080}"

rpc() {
  mediabox_ssh "curl -s --max-time 5 -H 'Content-Type: application/json' -d '$1' \
      http://127.0.0.1:$KODI_RPC_PORT/jsonrpc"
}
fps() {
  rpc '{"jsonrpc":"2.0","id":1,"method":"XBMC.GetInfoLabels","params":{"labels":["System.FPS"]}}' \
    | sed -n 's/.*"System.FPS":"\([^"]*\)".*/\1/p'
}
nav() { rpc "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"Input.$1\"}" >/dev/null; }

echo "===== GUI load: $label ====="
echo "renderer : $(mediabox_ssh "grep -m1 -o 'GL_RENDERER = .*' /var/tmp/kodi-home/.kodi/temp/kodi.log" 2>/dev/null)"

read_cpu() { mediabox_ssh "awk '{print \$14+\$15}' /proc/\$(pgrep -x kodi-gbm | head -1)/stat"; }
read_rss() { mediabox_ssh "awk '/VmRSS/{print \$2}' /proc/\$(pgrep -x kodi-gbm | head -1)/status"; }

j0=$(read_cpu); t0=$(date +%s)
echo "-- scripted navigation for ${secs}s, sampling Kodi's own System.FPS --"
samples=""
end=$(( $(date +%s) + secs ))
while [ "$(date +%s)" -lt "$end" ]; do
  nav Down; nav Down; nav Up
  f="$(fps)"
  [ -n "$f" ] && samples="$samples $f"
done
j1=$(read_cpu); t1=$(date +%s)
r1=$(read_rss)

hz=$(mediabox_ssh "getconf CLK_TCK")
echo "  process CPU     : $(awk -v a="$j0" -v b="$j1" -v hz="$hz" -v d="$((t1-t0))" \
        'BEGIN{printf "%.1f%% of one core", ((b-a)/hz)/d*100}')"
echo "  RSS             : $((r1/1024)) MiB"
echo "  System.FPS      :$(echo "$samples" | awk '{n=0;s=0;min=1e9;max=0;
        for(i=1;i<=NF;i++){v=$i+0;s+=v;n++;if(v<min)min=v;if(v>max)max=v}
        if(n)printf " n=%d min=%.1f avg=%.1f max=%.1f", n, min, s/n, max}')"
mediabox_ssh "pid=\$(pgrep -x kodi-gbm | head -1)
  echo \"  open fds        : \$(ls /proc/\$pid/fd | wc -l) (dmabuf \$(ls -l /proc/\$pid/fd | grep -c dmabuf))\"
  echo \"  DRM IOVA arena  : \$(tail -1 /sys/kernel/debug/dri/0/mm_dump)\""
