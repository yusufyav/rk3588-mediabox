#!/usr/bin/env bash
# Capture one Android golden-reference display state (read-only).
# Usage: capture-state.sh <outdir> <label>
set -uo pipefail
OUT="$1"; LABEL="${2:-state}"
mkdir -p "$OUT"

A() { adb shell "$@" 2>&1 | tr -d '\r'; }
R() { adb shell "su 0 sh -c '$1'" 2>&1 | tr -d '\r'; }

{
  echo "# state: $LABEL"
  echo "# host_utc: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "# device_uptime: $(A 'cat /proc/uptime')"
} > "$OUT/00-meta.txt"

# --- DRM / VOP2 debugfs (read-only) ---
R 'cat /sys/kernel/debug/dri/0/summary'      > "$OUT/10-drm-summary.txt"
R 'cat /sys/kernel/debug/dri/0/state'        > "$OUT/11-drm-state.txt"
R 'cat /sys/kernel/debug/dri/0/framebuffer'  > "$OUT/12-drm-framebuffer.txt"
R 'cat /sys/kernel/debug/dri/0/clients'      > "$OUT/13-drm-clients.txt"
R 'cat /sys/kernel/debug/dri/0/mm_dump'      > "$OUT/14-drm-mm-dump.txt"
R 'cat /sys/kernel/debug/dri/0/active_regs'  > "$OUT/15-drm-active-regs.txt"

# --- connector / HDMI sysfs ---
R 'for f in /sys/class/drm/card0-HDMI-A-1/*; do case "$f" in *edid*|*modes*) ;; *) [ -f "$f" ] && echo "== $f" && cat "$f" 2>/dev/null ;; esac; done' \
    > "$OUT/16-connector-sysfs.txt"

# --- SurfaceFlinger ---
A 'dumpsys SurfaceFlinger --list'    > "$OUT/20-sf-list.txt"
A 'dumpsys SurfaceFlinger'           > "$OUT/21-sf-dump.txt"
A 'dumpsys SurfaceFlinger --display-id' > "$OUT/22-sf-display-id.txt"
A 'dumpsys display'                  > "$OUT/23-dumpsys-display.txt"

# --- HWC runtime properties ---
A 'getprop' | grep -Ei 'vendor\.(hwc|ghwc|gralloc)|hdr|dataspace|resolution' > "$OUT/30-hwc-props.txt"

# --- foreground activity / media ---
A 'dumpsys activity activities | grep -E "topResumedActivity|mResumedActivity"' > "$OUT/40-activity.txt"
A 'dumpsys media_session | head -60'  > "$OUT/41-media-session.txt"
