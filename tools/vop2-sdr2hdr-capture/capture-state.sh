#!/usr/bin/env bash
# Captures the VOP2 mixed SDR/HDR composition state for one named state.
#
#   tools/vop2-sdr2hdr-capture/capture-state.sh <out-dir> <label>
#
# The question this gate asks is whether VOP2's hardware SDR-to-HDR block runs,
# so the SDR2HDR_CTRL register is read and decoded rather than described. The
# bit layout is the driver's own (RK3568_SDR2HDR_CTRL at VP base + 0x2010,
# rockchip_vop2_reg.c): eotf_en bit 0, r2r_en bit 1, r2r_mode bit 2,
# oetf_en bit 3, bypass_en bit 8. It is identical in the 5.10 tree the Android
# oracle was read from and in this appliance's 6.1.115 tree.
#
# Five files per state, kept apart by what they are evidence of:
#   gui-render-path.txt   what Kodi decided to do with the GUI (its own log)
#   plane-state.txt       which plane carries what, and how VOP2 tagged it
#   sdr2hdr-registers.txt the VOP2 HDR register block, raw and decoded
#   vop-summary.txt       the vendor debugfs summary, unedited
#   hdmi-state.txt        what is actually on the wire
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=../../scripts/env.sh
source "$here/scripts/env.sh"

out="${1:?usage: $0 <out-dir> <label>}"
label="${2:?usage: $0 <out-dir> <label>}"
: "${KODI_RUN_HOME:=/var/tmp/kodi-home}"
dest="$out/$label"
mkdir -p "$dest"

mediabox_ssh "bash -s" <<REMOTE > "$dest/vop-summary.txt" 2>&1
echo "===== VOP2 summary: $label ====="
echo "captured: \$(date -Is)"
echo
cat /sys/kernel/debug/dri/0/summary
REMOTE

mediabox_ssh "bash -s" <<'REMOTE' > "$dest/sdr2hdr-registers.txt" 2>&1
echo "----- raw HDR register block (/sys/kernel/debug/dri/0/active_regs) -----"
awk '/^HDR:/{f=1;print;next} f&&/^[a-f0-9]+:/{print;next} f&&NF==0{exit}' \
  /sys/kernel/debug/dri/0/active_regs
echo
echo "----- decoded -----"
line=$(awk '/^fdd92010:/{print;exit}' /sys/kernel/debug/dri/0/active_regs)
ctrl=$(echo "$line" | awk '{print $2}')
v=$((16#$ctrl))
echo "  SDR2HDR_CTRL (0xfdd92010) raw : 0x$ctrl"
echo "  sdr2hdr_eotf_en   (bit 0)     : $(( (v>>0)&1 ))"
echo "  sdr2hdr_r2r_en    (bit 1)     : $(( (v>>1)&1 ))"
echo "  sdr2hdr_r2r_mode  (bit 2)     : $(( (v>>2)&1 ))  # 0 = BT709_TO_BT2020"
echo "  sdr2hdr_oetf_en   (bit 3)     : $(( (v>>3)&1 ))"
echo "  sdr2hdr_bypass_en (bit 8)     : $(( (v>>8)&1 ))"
if [ $(( (v>>8)&1 )) -eq 1 ]; then
  echo "  => SDR2HDR: BYPASSED (hardware conversion NOT running)"
elif [ $(( v & 0x0b )) -eq 11 ]; then
  echo "  => SDR2HDR: ACTIVE, eotf+r2r+oetf, r2r_mode=BT709_TO_BT2020"
else
  echo "  => SDR2HDR: partially enabled, see bits above"
fi
echo
echo "----- HDR2SDR -----"
hs=$(awk '/^fdd92000:/{print $2;exit}' /sys/kernel/debug/dri/0/active_regs)
echo "  HDR2SDR_CTRL (0xfdd92000) raw : 0x$hs"
echo "  hdr2sdr_en   (bit 0)          : $(( (16#$hs)&1 ))"
REMOTE

mediabox_ssh "bash -s" <<'REMOTE' > "$dest/plane-state.txt" 2>&1
sum=$(cat /sys/kernel/debug/dri/0/summary)
echo "----- per-window colour tag (VOP2's own classification) -----"
echo "$sum" | awk '
  /-win[0-9]+: /       { win=$1; act=$2; next }
  win && /format:/     { fmt=$0; sub(/^[ \t]+/,"",fmt) }
  win && /^[ \t]+color: /  { col=$0; sub(/^[ \t]+/,"",col) }
  win && /^[ \t]+csc: /    { csc=$0; sub(/^[ \t]+/,"",csc) }
  win && /^[ \t]+zpos/     { zp=$0; sub(/^[ \t]+/,"",zp) }
  win && /^[ \t]+src: /    { printf "  %s %s\n    %s\n    %s\n    %s\n    %s\n", win, act, fmt, col, csc, zp; win="" }
'
echo
echo "----- overlay / output mode -----"
echo "$sum" | grep -E 'overlay_mode|Display mode|bus_format' | sed 's/^[ \t]*/  /'
echo
echo "----- atomic plane state (debugfs) -----"
sed -n '/^plane\[/,$p' /sys/kernel/debug/dri/0/state | head -60
REMOTE

mediabox_ssh "bash -s" <<REMOTE > "$dest/hdmi-state.txt" 2>&1
sum=\$(cat /sys/kernel/debug/dri/0/summary)
echo "----- what is on the wire -----"
echo "\$sum" | grep -E 'bus_format|overlay_mode|Display mode|Connector' | sed 's/^[ \t]*/  /'
echo
echo "----- connector properties (requested) -----"
modetest -M rockchip -c 2>/dev/null |
  grep -E -A4 '^[ \t]+[0-9]+ (Colorspace|color_depth|color_format|max bpc|HDR_OUTPUT_METADATA):' |
  sed 's/^/  /'
echo
echo "----- connector sysfs -----"
for f in status enabled; do
  printf '  %s: ' "\$f"; cat /sys/class/drm/card0-HDMI-A-1/\$f 2>/dev/null
done
REMOTE

mediabox_ssh "bash -s" <<REMOTE > "$dest/gui-render-path.txt" 2>&1
kodi_log="$KODI_RUN_HOME/.kodi/temp/kodi.log"
echo "----- did Kodi run its software HDR GUI compositor? -----"
echo "  (the composite shader and its PQ LUTs are built only by"
echo "   SetGuiCompositing(); no line here means the path never ran)"
grep -nE "GUI composite shader|failed to create LUTs|GUI left SDR for hardware|GUI compositing not supported|HDR passthrough" "\$kodi_log" 2>/dev/null | tail -20
echo
echo "----- GUI plane EOTF tag decisions -----"
grep -n "SetGuiPlaneEotf" "\$kodi_log" 2>/dev/null | tail -10
echo
echo "----- video plane configuration -----"
grep -n "VideoLayerBridge:Configure" "\$kodi_log" 2>/dev/null | tail -5
echo
echo "----- colour state transitions -----"
grep -n "MP2COLOR where=SetHDR:exit\|MP2COLOR where=SetColorimetry:exit" "\$kodi_log" 2>/dev/null | tail -6
REMOTE

echo "captured $label -> $dest"
