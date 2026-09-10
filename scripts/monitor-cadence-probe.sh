#!/usr/bin/env bash
# Reports everything needed to judge one sink's HDR + cadence behaviour.
# Read-only: it inspects DRM state and Kodi's log, and changes nothing.
#
# Usage:  MEDIABOX_HOST=<ip> scripts/monitor-cadence-probe.sh [content_fps]
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=env.sh
source "$here/scripts/env.sh"
fps="${1:-23.976023}"
: "${KODI_RUN_HOME:=/var/tmp/kodi-home}"
kodi_log="$KODI_RUN_HOME/.kodi/temp/kodi.log"

echo "=============================================================="
echo " sink probe   host=$MEDIABOX_HOST   content=${fps} fps   $(date +%H:%M:%S)"
echo "=============================================================="

echo
echo "## Attached sink and the modes it offers"
mediabox_ssh 'modetest -M rockchip -a -c 2>/dev/null' \
  | awk '/connected[[:space:]]+HDMI/{h=1; print; next} /^[0-9]+[[:space:]]+[0-9]+[[:space:]]+(connected|disconnected)/{h=0} h && /^[[:space:]]+#[0-9]+/{print}' \
  > /tmp/.probe_modes.$$ || true
python3 - "$fps" /tmp/.probe_modes.$$ <<'PY'
import re, sys
fps = float(sys.argv[1])
rows = open(sys.argv[2]).read().split("\n")

# A cadence is only "clean" if the accumulated error stays under one frame for
# an hour of playback. modetest prints the refresh rate rounded to 2 decimals,
# which is far too coarse to judge that -- 59.95 hides both a perfect 59.9400
# and a drifting 59.9505 -- so the rate is recomputed from the mode timing:
# refresh = pixel_clock / (htotal * vtotal).
CLEAN_SECONDS = 3600.0

print(f"{'mode':<13}{'Hz (exact)':>13}{'ratio':>9}   cadence")
print("-" * 78)
seen = set()
for l in rows:
    if "connected" in l:
        print(f"[sink: {l.split()[3] if len(l.split()) > 3 else l.strip()}]")
        continue
    t = l.split()
    m = re.match(r"^(\d+)x(\d+)$", t[1]) if len(t) > 11 else None
    if not m:
        continue
    w, h = int(m.group(1)), int(m.group(2))
    if h < 720:
        continue
    try:
        htotal, vtotal, clock = int(t[6]), int(t[10]), int(t[11])
        hz = clock * 1000.0 / (htotal * vtotal)
    except (ValueError, ZeroDivisionError):
        continue
    key = (w, h, round(hz, 4))
    if key in seen:
        continue
    seen.add(key)

    r = hz / fps
    targets = [1, 2, 2.5] + list(range(3, 9))
    best = min(targets, key=lambda x: abs(r - x))
    err = abs(r - best)
    slip = (1.0 / err / fps) if err > 0 else float("inf")
    if slip > CLEAN_SECONDS:
        if best in (1, 2, 2.5):
            note = f"CLEAN {best:g}:1  <- Kodi can select this"
        else:
            note = f"CLEAN {best:g}:1  (Kodi never looks for x{best:g})"
    else:
        note = f"drifts ~{slip:,.0f}s between jumps"
    print(f"{w}x{h:<8}{hz:>13.4f}{r:>9.4f}   {note}")

print()
print("Kodi matches only x1, x2 and x2.5 (3:2 pulldown), and only against the")
print("video's own resolution or the current desktop resolution -- a clean mode")
print("at some other resolution will never be chosen on its own.")
PY
rm -f /tmp/.probe_modes.$$

echo
echo "## Live output state"
mediabox_ssh 'modetest -M rockchip -a -p 2>/dev/null' > /tmp/.probe_planes.$$
python3 - /tmp/.probe_planes.$$ <<'PY'
import re, sys
txt = open(sys.argv[1]).read().split("\n")
inp = False; plane = None; out = []
for i, l in enumerate(txt):
    if l.startswith("Planes:"):
        inp = True; continue
    if inp and re.match(r"^\d+\s+\d+\s+\d+", l):
        f = l.split(); plane = (f[0], f[1], f[2])
    if inp and re.match(r"^\s*31 EOTF:", l):
        for j in range(i + 1, i + 5):
            m = re.match(r"^\s*value:\s*(\d+)\s*$", txt[j])
            if m:
                out.append((plane, m.group(1))); break
print(f"{'plane':>6}{'crtc':>6}{'fb':>6}{'EOTF':>6}   (2 = PQ/HDR10, 0 = SDR)")
for (p, c, f), v in out:
    if c != "0":
        print(f"{p:>6}{c:>6}{f:>6}{v:>6}   ACTIVE")
PY
rm -f /tmp/.probe_planes.$$

echo
mediabox_ssh 'cat /sys/kernel/debug/dri/0/summary 2>/dev/null' \
  | grep -E "Video Port|win[0-9]:|format:|color:|csc:|bus_format" | head -24

echo
echo "## Connector HDR signalling"
mediabox_ssh 'modetest -M rockchip -a -c 2>/dev/null' \
  | grep -A3 -E "^\s+(202 color_depth|203 color_format|214 Colorspace):" \
  | grep -E "color_depth|color_format|Colorspace|value:" || true

echo
echo "## What Kodi decided about the refresh rate"
mediabox_ssh "grep -a 'WHITELIST\|Display resolution ADJUST\|CVideoReferenceClock\|synctype set' '$kodi_log' 2>/dev/null | tail -14" || true

echo
echo "## Errors this session"
mediabox_ssh "L='$kodi_log'
  printf 'snd_pcm_writei(-77)  : %s\n' \"\$(grep -ac 'snd_pcm_writei(-77)' \$L)\"
  printf 'audio recovered      : %s\n' \"\$(grep -ac 'preparing it again' \$L)\"
  printf 'xrun / underrun      : %s\n' \"\$(grep -aci 'xrun\|underrun' \$L)\"
  printf 'kernel underflow     : %s\n' \"\$(dmesg --since '-15min' 2>/dev/null | grep -aci underflow)\"
  printf 'kernel iommu/gpu     : %s\n' \"\$(dmesg --since '-15min' 2>/dev/null | grep -aciE 'iommu|gpu fault')\"
  printf 'kodi alive           : %s\n' \"\$(pgrep -x kodi-gbm >/dev/null && echo yes || echo NO)\""
