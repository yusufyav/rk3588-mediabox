#!/usr/bin/env bash
# Builds and drives tools/drm-writeback-probe on the target.
#
# The probe reproduces the display-plane composition Kodi produces, without
# Kodi, and reads the composited frame back through the DRM writeback
# connector. It exists because the RK3588 VOP2 register file is not a
# sufficient witness for this fault: every geometry register is bit-identical
# whether the OSD plane is in the mix or not, while the picture provably moves.
#
#   scripts/writeback-probe.sh audit          enumerate writeback capability
#   scripts/writeback-probe.sh ab             the EOTF A/B, with measurements
#   scripts/writeback-probe.sh show <eotf>    hold one arm on screen to look at
#   scripts/writeback-probe.sh run <args...>  raw pass-through to the probe
#
# Capturing needs the CRTC and therefore DRM master, so Kodi has to be stopped
# first; the probe refuses rather than fighting over the display.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=env.sh
source "$here/scripts/env.sh"

remote_src=/tmp/drm-writeback-probe.c
remote_bin=/tmp/drm-writeback-probe
remote_out=/tmp/wb

# Kodi's measured HDR playback geometry, so a synthetic arm is comparable to
# the real one rather than approximately like it.
GEOM=(--mode 3840x2160@24 --hdr --hdr-out --nv12
      --src 3840x2080 --dst 3840x2076+0+42)

build() {
  mediabox_scp "$here/tools/drm-writeback-probe.c" "$MEDIABOX_TARGET:$remote_src" >/dev/null
  mediabox_ssh "gcc -O2 -Wall -o '$remote_bin' '$remote_src' \
      \$(pkg-config --cflags --libs libdrm)"
  mediabox_ssh "mkdir -p '$remote_out'"
}

case "${1:-ab}" in
  audit)
    build
    mediabox_ssh "'$remote_bin' audit"
    ;;

  ab)
    build
    echo "== EOTF A/B on the composited GUI plane (Kodi must be stopped)"
    mediabox_ssh "cd '$remote_out' && rm -f ab*.nv12
      for e in 0 1 2 3; do
        '$remote_bin' run ${GEOM[*]} --gui-eotf \$e --hold 3 --toggle 1 \
            --writeback "$remote_out/ab\$e" >/dev/null 2>&1
        sleep 1
      done"
    mediabox_scp "$here/scripts/writeback-registration.py" \
      "$MEDIABOX_TARGET:/tmp/writeback-registration.py" >/dev/null
    mediabox_ssh "cd '$remote_out' && for e in 0 1 2 3; do
        echo \"===== GUI plane EOTF=\$e =====\"
        python3 /tmp/writeback-registration.py ab\$e.c0.novideo.nv12 ab\$e.c0.gui.nv12 \
            --band 900:1100 --limit 64
        echo
      done"
    ;;

  show)
    build
    eotf="${2:?usage: $0 show <eotf 0|1|2|3>}"
    echo "== holding the GUI layer on screen with EOTF=$eotf, toggling"
    mediabox_ssh "'$remote_bin' run ${GEOM[*]} --gui-eotf '$eotf' --hold 6 --toggle 8"
    ;;

  run)
    build
    shift
    mediabox_ssh "'$remote_bin' run $*"
    ;;

  *)
    echo "usage: $0 [audit|ab|show <eotf>|run <args...>]" >&2
    exit 2
    ;;
esac
