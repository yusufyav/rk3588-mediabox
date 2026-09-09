#!/usr/bin/env bash
# Snapshots everything Gate MP2 has to prove, in one call.
#
#   scripts/capture-kodi-state.sh <out-dir> <label>
#
# Kept separate from the Kodi launcher because the same snapshot is taken
# before, during and after playback, and the interesting comparisons are
# between those three rather than inside any one of them.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=env.sh
source "$here/scripts/env.sh"

out="${1:?usage: $0 <out-dir> <label>}"
label="${2:?usage: $0 <out-dir> <label>}"
mkdir -p "$out"

mediabox_ssh "cat /sys/kernel/debug/dri/0/summary" > "$out/$label-debugfs.txt" 2>&1

# Connector and plane properties, read straight from DRM rather than from
# Kodi's log: what Kodi believes it set and what the kernel holds are separate
# facts, and this gate has already been bitten by the difference once.
#
# modetest rather than a hand-rolled reader. It is the libdrm project's own
# tool, it needs no DRM master so it can be run while Kodi holds the display,
# and it prints property RANGES -- which matters here, because Kodi decides
# whether it may use a cursor-typed plane by whether INPUT_WIDTH/INPUT_HEIGHT
# exist as range properties, not by their current values.
mediabox_ssh "for c in /sys/class/drm/card0-HDMI-A-1/status /sys/class/drm/card0-HDMI-A-1/enabled; do
    printf '%s: ' \"\$c\"; cat \$c 2>/dev/null; done
  echo
  echo '===== connectors ====='
  modetest -M rockchip -c 2>/dev/null
  echo
  echo '===== planes ====='
  modetest -M rockchip -p 2>/dev/null" > "$out/$label-drm.txt" 2>&1

mediabox_ssh "cat /proc/asound/card0/pcm0p/sub0/hw_params 2>/dev/null; echo '--- status'; \
  cat /proc/asound/card0/pcm0p/sub0/status 2>/dev/null" > "$out/$label-alsa.txt" 2>&1

"$here/scripts/tv-state.sh" "$label" > "$out/$label-tv.json" 2>&1
echo "captured $label -> $out"
