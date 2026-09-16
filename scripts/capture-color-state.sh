#!/usr/bin/env bash
# Snapshots the whole colour chain in one call, for one named state.
#
#   scripts/capture-color-state.sh <out-dir> <label>
#
# Gate MP2 stopped because three descriptions of the same signal disagreed and
# nobody could say which of them was a request and which was what the hardware
# did. So this keeps the two apart on purpose and says which is which:
#
#   REQUESTED - modetest reads the DRM properties Kodi set. It needs no DRM
#               master, so it can run while Kodi holds the display.
#   ACTUAL    - the vendor VOP2 debugfs summary: the bus format really being
#               driven, each window's CSC direction and matrix, the video port's
#               HDR tag. This is the authority. A connector property read back
#               is what was asked for, not what came out.
#   KODI      - the MP2COLOR trace lines from Kodi's own log for this state,
#               so a request can be attributed to the transition that made it.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=env.sh
source "$here/scripts/env.sh"

out="${1:?usage: $0 <out-dir> <label>}"
label="${2:?usage: $0 <out-dir> <label>}"
: "${KODI_RUN_HOME:=/var/tmp/kodi-home}"
mkdir -p "$out"

mediabox_ssh "bash -s" <<REMOTE > "$out/$label.txt" 2>&1
label="$label"
kodi_log="$KODI_RUN_HOME/.kodi/temp/kodi.log"
echo "===== colour state: \$label ====="
echo "captured: \$(date -Is)"
echo

echo "----- ACTUAL: vendor VOP2 / HDMI state (debugfs) -----"
echo "This is the authority for what is on the wire."
cat /sys/kernel/debug/dri/0/summary
echo

echo "----- ACTUAL: decoded headline -----"
sum=\$(cat /sys/kernel/debug/dri/0/summary)
bus=\$(echo "\$sum" | grep -m1 -o 'bus_format\[[0-9a-f]*\]: [A-Z0-9_]*')
echo "  bus_format      : \$bus"
case "\$bus" in
  *YUV8_1X24*)   echo "  wire            : YCbCr 4:4:4, 8-bit" ;;
  *YUV10_1X30*)  echo "  wire            : YCbCr 4:4:4, 10-bit" ;;
  *YUYV8_1X16*)  echo "  wire            : YCbCr 4:2:2, 8-bit" ;;
  *YUYV10_1X20*) echo "  wire            : YCbCr 4:2:2, 10-bit" ;;
  *RGB888_1X24*) echo "  wire            : RGB 4:4:4, 8-bit" ;;
  *RGB101010*)   echo "  wire            : RGB 4:4:4, 10-bit" ;;
  *)             echo "  wire            : (unmapped bus format)" ;;
esac
echo "  vp hdr/encoding : \$(echo "\$sum" | grep -m1 -oE 'SDR\[[0-9]\]|HDR[0-9]*\[[0-9]\]') \$(echo "\$sum" | grep -m1 -o 'color-encoding\[[^]]*\]') \$(echo "\$sum" | grep -m1 -o 'color-range\[[^]]*\]')"
echo "  display mode    : \$(echo "\$sum" | grep -m1 -o 'Display mode: .*')"
echo "  active windows  :"
echo "\$sum" | grep -E '^\s+(Cluster|Esmart)[0-9]+-win[0-9]+: ' | sed 's/^/    /'
echo

echo "----- ACTUAL: per-window CSC and colour tags -----"
echo "\$sum" | awk '
  /-win[0-9]+: /            { win=\$1; act=\$2 }
  win && /format:/          { fmt=\$2 }
  win && /^\s+color: /      { col=\$0; sub(/^[ \t]+/,"",col) }
  win && /^\s+csc: /        { csc=\$0; sub(/^[ \t]+/,"",csc) }
  win && /^\s+src: /        { src=\$0; sub(/^[ \t]+/,"",src);
                              printf "  %-18s %-7s fmt=%-8s\n    %s\n    %s\n    %s\n", win, act, fmt, col, csc, src;
                              win="" }
'
echo

echo "----- REQUESTED: connector properties (modetest) -----"
modetest -M rockchip -c 2>/dev/null | awk '/^Connectors:/{f=1} f' |
  grep -E -A3 '^\s+[0-9]+ (Colorspace|color_depth|color_format|max bpc|HDR_OUTPUT_METADATA|HDR_PANEL_METADATA|link-status):' |
  sed 's/^/  /'
echo

echo "----- REQUESTED: plane properties (modetest) -----"
modetest -M rockchip -p 2>/dev/null |
  grep -E -A4 '^[0-9]+\s|^\s+[0-9]+ (COLOR_ENCODING|COLOR_RANGE|FB_ID|CRTC_ID|type):' |
  sed 's/^/  /' | head -160
echo

echo "----- KODI: MP2COLOR trace, last 25 lines -----"
grep 'MP2COLOR' "\$kodi_log" 2>/dev/null | tail -25 | sed 's/^/  /'
echo

echo "----- KODI: player state -----"
curl -s --max-time 8 -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"Player.GetActivePlayers"}' \
  http://127.0.0.1:8080/jsonrpc
echo
curl -s --max-time 8 -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"Player.GetProperties","params":{"playerid":1,"properties":["time","speed","percentage"]}}' \
  http://127.0.0.1:8080/jsonrpc
echo

echo "----- SINK: what the display advertises -----"
echo "  (there is no IP-control API on this sink, so its HDR state is not"
echo "   machine-readable; the connector's view of the panel is recorded"
echo "   instead and the physical check is the operator's)"
# The connector this box is on, not a fixed one: a board with two HDMI sockets
# has an empty one, and reading its state says nothing about the panel.
conn=\$(/opt/rk3588-mediabox/bin/mediabox-platform connector-path 2>/dev/null || true)
for f in status enabled; do
  printf '  %s/%s: ' "\${conn:-<unresolved>}" "\$f"; cat "\$conn/\$f" 2>/dev/null; echo
done
REMOTE
echo "captured $label -> $out/$label.txt"
