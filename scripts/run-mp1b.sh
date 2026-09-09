#!/usr/bin/env bash
# Runs the Gate MP1b real-HDR10 playback probe on the target and collects
# evidence locally.
#
#   scripts/run-mp1b.sh --input REMOTE_PATH [--duration 60] [--start 0] [out-dir]
#
# Unlike the MP1a ladder this is a single run: the output state is fixed at the
# A4 configuration MP1a proved, and the only variable is the content. The TV is
# sampled before, during and after, and the kernel log is diffed around the run.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=env.sh
source "$here/scripts/env.sh"

input=""
duration=60
start=0
extra=()
out=""
while [ $# -gt 0 ]; do
  case "$1" in
    --input) input="$2"; shift 2 ;;
    --duration) duration="$2"; shift 2 ;;
    --start) start="$2"; shift 2 ;;
    --out) out="$2"; shift 2 ;;
    --) shift; extra+=("$@"); break ;;
    *) extra+=("$1"); shift ;;
  esac
done
[ -n "$input" ] || { echo "usage: $0 --input REMOTE_PATH [--duration N] [--start N]" >&2; exit 2; }

stamp="${MEDIABOX_RUN_DATE:-$(date +%Y-%m-%d)}"
: "${out:=$here/logs/orangepi5-ultra-vendor/real-hdr10-playback-mp1b-$stamp}"
mkdir -p "$out"

probe="$MEDIABOX_REMOTE_DIR/build/hdr-playback-probe"
label="${MEDIABOX_RUN_LABEL:-run}"

echo "== asset metadata (ffprobe from the RKMPP-enabled build)"
mediabox_ssh "'$MEDIABOX_FFMPEG_PREFIX/bin/ffprobe' -hide_banner -v error \
    -show_entries format=format_name,format_long_name,duration,size,bit_rate \
    -show_streams '$input'" > "$out/$label-00-ffprobe-full.txt" 2>&1
mediabox_ssh "'$MEDIABOX_FFMPEG_PREFIX/bin/ffprobe' -hide_banner -v error -select_streams v:0 \
    -read_intervals '%+#1' -show_frames '$input'" > "$out/$label-00-ffprobe-frame0.txt" 2>&1

echo "== baseline sink state"
"$here/scripts/tv-state.sh" mp1b-before > "$out/$label-01-tv-state-before.json" 2>&1

echo "== baseline kernel log + debugfs"
mediabox_ssh "dmesg" > "$out/$label-01-dmesg-before.txt" 2>&1
mediabox_ssh "cat /sys/kernel/debug/dri/0/summary" > "$out/$label-01-summary-before.txt" 2>&1

echo "== playback: ${duration}s from ${start}s"
mediabox_ssh "'$probe' --input '$input' --duration $duration --start $start ${extra[*]:-}" \
  > "$out/$label-10-playback.txt" 2>&1 &
probe_pid=$!

# Sample the sink and the video port while the content under test is actually
# on the wire. The offsets are chosen to land after the decoder has settled.
sleep $(( duration / 3 + 12 ))
"$here/scripts/tv-state.sh" mp1b-during > "$out/$label-11-tv-state-during.json" 2>&1
mediabox_ssh "cat /sys/kernel/debug/dri/0/summary" > "$out/$label-11-summary-during.txt" 2>&1
sleep $(( duration / 3 ))
mediabox_ssh "cat /sys/kernel/debug/dri/0/summary" > "$out/$label-12-summary-during2.txt" 2>&1

wait "$probe_pid"
rc=$?

mediabox_ssh "dmesg" > "$out/$label-20-dmesg-after.txt" 2>&1
diff "$out/$label-01-dmesg-before.txt" "$out/$label-20-dmesg-after.txt" \
  | sed -n 's/^> //p' > "$out/$label-20-dmesg-delta.txt"
grep -inE 'drm|vop|vop2|hdmi|dw-hdmi|dw_hdmi|phy|hdr|colorspace|bpc|color_depth|infoframe|edid|underflow|timeout|iommu|mpp|rkvdec|error|fail|warn' \
  "$out/$label-20-dmesg-delta.txt" > "$out/$label-20-dmesg-delta-filtered.txt" 2>/dev/null

echo "== returning the connector to its neutral SDR state"
mediabox_ssh "'$probe' --reset" > "$out/$label-30-reset.txt" 2>&1
"$here/scripts/tv-state.sh" mp1b-after > "$out/$label-31-tv-state-after.json" 2>&1
mediabox_ssh "cat /sys/kernel/debug/dri/0/summary" > "$out/$label-31-summary-after.txt" 2>&1
mediabox_ssh "cat /sys/class/drm/card0-HDMI-A-1/status /sys/class/drm/card0-HDMI-A-1/enabled" \
  > "$out/$label-31-connector-state.txt" 2>&1

result="$(sed -n 's/^PLAYBACK RESULT: //p' "$out/$label-10-playback.txt" | tail -1)"
: "${result:=NO-RESULT-LINE}"
echo "== $label -> $result (exit $rc)"
echo "$label $result $rc" >> "$out/run-summary.txt"
sed -n '/^cadence /p' "$out/$label-10-playback.txt"
echo "== evidence in $out"
exit $rc
