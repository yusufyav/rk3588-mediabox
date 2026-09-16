#!/usr/bin/env bash
# Runs the Gate MP1a A/B ladder on the target and collects evidence locally.
#
#   scripts/run-mp1a.sh [output-dir] [hold-seconds]
#
# Each rung only runs if the previous one passed: the point of the ladder is to
# localise a failure, not to collect a full sweep regardless.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=env.sh
source "$here/scripts/env.sh"

stamp="${MEDIABOX_RUN_DATE:-$(date +%Y-%m-%d)}"
out="${1:-$here/logs/orangepi5-ultra-vendor/hdr-signaling-mp1a-$stamp}"
hold="${2:-20}"
mkdir -p "$out"
: > "$out/ladder-summary.txt"

probe="$MEDIABOX_REMOTE_DIR/build/hdr-signaling-probe"
sdr_asset="$MEDIABOX_REMOTE_DIR/assets/sdr-4k-2398-main.mp4"
hdr_asset="$MEDIABOX_REMOTE_DIR/assets/hdr10-4k-2398-main10.mp4"

echo "== inventory (--probe, no modeset)"
mediabox_ssh "'$probe' --probe" > "$out/00-probe.txt" 2>&1
tail -3 "$out/00-probe.txt"

echo "== baseline sink state"
"$here/scripts/tv-state.sh" baseline > "$out/01-tv-state-baseline.json" 2>&1

echo "== baseline kernel log + debugfs"
mediabox_ssh "dmesg" > "$out/01-dmesg-baseline.txt" 2>&1
mediabox_ssh "cat /sys/kernel/debug/dri/0/summary" > "$out/01-summary-baseline.txt" 2>&1

overall=0
for step in a0 a1 a2 a3 a4; do
  case "$step" in
    a0) asset="$sdr_asset" ;;
    *)  asset="$hdr_asset" ;;
  esac
  echo "== step $step (asset $(basename "$asset"), hold ${hold}s)"
  mediabox_ssh "dmesg" > "$out/$step-dmesg-before.txt" 2>&1
  mediabox_ssh "'$probe' --step $step --asset '$asset' --hold $hold" \
    > "$out/$step-run.txt" 2>&1 &
  probe_pid=$!
  # Sample the sink about halfway through the hold, while the state under test
  # is actually on the wire.
  sleep $(( hold / 2 + 5 ))
  "$here/scripts/tv-state.sh" "$step" > "$out/$step-tv-state.json" 2>&1
  wait "$probe_pid"
  rc=$?
  mediabox_ssh "dmesg" > "$out/$step-dmesg-after.txt" 2>&1
  diff "$out/$step-dmesg-before.txt" "$out/$step-dmesg-after.txt" \
    | sed -n 's/^> //p' > "$out/$step-dmesg-delta.txt"
  grep -inE 'drm|vop|hdmi|dw-hdmi|dw_hdmi|phy|hdr|colorspace|bpc|color_depth|infoframe|edid' \
    "$out/$step-dmesg-delta.txt" > "$out/$step-dmesg-delta-filtered.txt" 2>/dev/null

  result="$(sed -n 's/^STEP .* RESULT: //p' "$out/$step-run.txt" | tail -1)"
  : "${result:=NO-RESULT-LINE}"
  echo "   $step -> $result (exit $rc, dmesg delta $(wc -l < "$out/$step-dmesg-delta.txt") lines)"
  echo "$step $result $rc" >> "$out/ladder-summary.txt"
  if [ "$result" != "PASS" ]; then
    echo "   ladder stops at $step; not running the remaining rungs"
    overall=1
    break
  fi
done

echo "== returning the connector to its neutral SDR state"
mediabox_ssh "'$probe' --reset" > "$out/98-reset.txt" 2>&1
tail -1 "$out/98-reset.txt"
"$here/scripts/tv-state.sh" post-reset > "$out/99-tv-state-after.json" 2>&1

echo "== post-run state"
mediabox_ssh "cat /sys/kernel/debug/dri/0/summary" > "$out/99-summary-after.txt" 2>&1
mediabox_ssh "conn=\$(/opt/rk3588-mediabox/bin/mediabox-platform connector-path 2>/dev/null || true)
  echo \"\$conn\"; cat \"\$conn/status\" \"\$conn/enabled\" 2>/dev/null" \
  > "$out/99-connector-state.txt" 2>&1

( cd "$out" && sha256sum -- *.txt *.json > SHA256SUMS )
echo "== evidence in $out"
cat "$out/ladder-summary.txt"
exit $overall
