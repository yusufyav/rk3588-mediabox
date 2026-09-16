#!/usr/bin/env bash
# Builds tools/mali-gbm-probe on the target and runs it twice: once under the
# system Mesa, once under the private Mali runtime.
#
#   scripts/mali-probe.sh [outdir]
#
# The A/B is the evidence. One binary, linked against the generic Mesa sonames
# exactly as Kodi is, so the only variable between the two runs is
# LD_LIBRARY_PATH -- which is also the only mechanism run-kodi-rk3588.sh uses.
# A run that reported Mali only because the probe was linked differently from
# Kodi would prove nothing about Kodi.
#
# dmesg is sampled either side of the whole sequence, because a vendor GPU blob
# that renders one frame and then leaves the kernel logging MMU faults is not a
# pass, and the probe's own exit status cannot see that.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=env.sh
source "$here/scripts/env.sh"

: "${MALI_RUNTIME:=/opt/rk3588-mediabox/mali-g24p0-runtime}"
: "${MALI_STAGE:=/var/tmp/mali-stage}"
# The render device the vendor GBM implementation drives, asked of the board
# rather than written down. Override it to probe a specific node.
: "${DRM_NODE:=$(mediabox_ssh '/opt/rk3588-mediabox/bin/mediabox-platform render-node' 2>/dev/null || echo /dev/dri/renderD128)}"
outdir="${1:-}"

echo "== uploading probe source"
mediabox_ssh "mkdir -p '$MALI_STAGE/src'"
mediabox_scp "$here/tools/mali-gbm-probe.c" "$MEDIABOX_TARGET:$MALI_STAGE/src/" >/dev/null

echo "== building on the target against system Mesa headers and sonames"
mediabox_ssh "cd '$MALI_STAGE' && gcc -O2 -Wall -Wextra -o mali-gbm-probe \
    src/mali-gbm-probe.c \$(pkg-config --cflags --libs gbm egl glesv2)"
mediabox_ssh "readelf -d '$MALI_STAGE/mali-gbm-probe' | grep NEEDED"

echo "== running A (Mesa) and B (Mali)"
mediabox_ssh "bash -s" <<REMOTE
set -u
cd '$MALI_STAGE'
dmesg > probe-dmesg-before.txt
{
  echo "mali-gbm-probe - A/B over one binary, two loader environments"
  echo "date   : \$(date -Is)"
  echo "kernel : \$(uname -r)"
  echo "kernel Mali DDK : \$(dmesg | grep -o 'Kernel DDK version g[0-9]*p[0-9]*-[0-9a-f]*' | head -1)"
  echo "GPU id : \$(dmesg | grep -o 'GPU identified as .*' | head -1)"
  echo "binary NEEDED : \$(readelf -d mali-gbm-probe | grep NEEDED | grep -oE '\[.*\]' | tr '\n' ' ')"
  echo
  echo "############ A - system Mesa (no override) ############"
  timeout 60 ./mali-gbm-probe '$DRM_NODE'; echo "exit=\$?"
  echo
  echo "############ B - private Mali runtime ############"
  echo "LD_LIBRARY_PATH=$MALI_RUNTIME/lib"
  timeout 60 env LD_LIBRARY_PATH='$MALI_RUNTIME/lib' ./mali-gbm-probe '$DRM_NODE'; echo "exit=\$?"
  echo
  echo "############ B x3 - init/teardown stability ############"
  for i in 1 2 3; do
    timeout 60 env LD_LIBRARY_PATH='$MALI_RUNTIME/lib' ./mali-gbm-probe '$DRM_NODE' > rep\$i.txt 2>&1
    rc=\$?
    printf 'repeat %d: exit=%d  renderer=%-14s %s\n' "\$i" "\$rc" \
      "\$(grep -m1 GL_RENDERER rep\$i.txt | sed 's/.*: //')" "\$(tail -1 rep\$i.txt)"
  done
} > gpu-probe.log 2>&1
dmesg > probe-dmesg-after.txt
{
  echo "dmesg delta across the probe sequence (A, B, B x3)"
  echo "date: \$(date -Is)"
  echo
  diff probe-dmesg-before.txt probe-dmesg-after.txt | grep '^>' | sed 's/^> //' || true
  echo "(nothing above = the kernel logged nothing new)"
  echo
  echo "-- abort conditions --"
  if diff probe-dmesg-before.txt probe-dmesg-after.txt | grep '^>' \
       | grep -iE 'oops|panic|reset|hang|iommu|fault|underflow|atomic|BUG'; then
    echo "^^ FOUND - stop GPU probing"
  else
    echo "NONE of: oops panic reset hang iommu fault underflow atomic BUG"
  fi
} > gpu-probe-dmesg.txt 2>&1
sed -n '/mapped shared objects/,/teardown/p' rep1.txt > gpu-probe-maps.txt
REMOTE

mediabox_ssh "grep -E 'GL_VENDOR|GL_RENDERER|EGL_VENDOR|gbm backend|MALI PROBE|repeat |exit=|####' '$MALI_STAGE/gpu-probe.log'"
mediabox_ssh "tail -4 '$MALI_STAGE/gpu-probe-dmesg.txt'"

if [ -n "$outdir" ]; then
  mkdir -p "$outdir"
  for f in gpu-probe.log gpu-probe-dmesg.txt gpu-probe-maps.txt; do
    mediabox_scp "$MEDIABOX_TARGET:$MALI_STAGE/$f" "$outdir/" >/dev/null
  done
  echo "== evidence written to $outdir"
fi
