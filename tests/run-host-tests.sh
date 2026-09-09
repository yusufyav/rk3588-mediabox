#!/usr/bin/env bash
# Hardware-free checks that run on the workstation.
#
# They guard the two things that are easy to get silently wrong and that no
# amount of on-target testing would catch: the generated assets not actually
# carrying HDR10 metadata, and the probe's step table drifting out of sync with
# the documented A/B ladder.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
failures=0

check() {
  if [ "$2" = "$3" ]; then
    printf 'ok   %s (%s)\n' "$1" "$2"
  else
    printf 'FAIL %s: expected %s, got %s\n' "$1" "$3" "$2"
    failures=$((failures + 1))
  fi
}

# Substring match without a pipe: `grep -q` exits as soon as it matches, and
# under `pipefail` the SIGPIPE that gives the writer fails the whole pipeline
# once the haystack exceeds the pipe buffer.
contains() {
  if [[ "$2" == *"$3"* ]]; then
    printf 'ok   %s\n' "$1"
  else
    printf 'FAIL %s: %s not found\n' "$1" "$3"
    failures=$((failures + 1))
  fi
}

src="$here/tools/hdr-signaling-probe.cpp"

echo "-- step table matches the documented ladder"
table="$(sed -n '/const StepConfig kSteps\[\]/,/};/p' "$src")"
contains "a0 is NV12, no HDR properties"  "$table" '{"a0", DRM_FORMAT_NV12, false, false, false}'
contains "a1 is NV15, no HDR properties"  "$table" '{"a1", DRM_FORMAT_NV15, false, false, false}'
contains "a2 adds BT.2020 only"           "$table" '{"a2", DRM_FORMAT_NV15, true, false, false}'
contains "a3 adds 30bit only"             "$table" '{"a3", DRM_FORMAT_NV15, true, true, false}'
contains "a4 adds HDR metadata only"      "$table" '{"a4", DRM_FORMAT_NV15, true, true, true}'

echo "-- probe refuses forbidden conversions"
contains "no libswscale include"  "$(cat "$src")" "libavcodec/avcodec.h"
if grep -qE 'swscale|sws_scale' "$src"; then
  printf 'FAIL probe references libswscale\n'
  failures=$((failures + 1))
else
  printf 'ok   probe never references libswscale\n'
fi
contains "non-drm_prime output is refused" "$(cat "$src")" "not drm_prime; refusing any CPU"

echo "-- HDR metadata unit conversions"
check "chroma unit scale"      "$(grep -c 'value) \* 50000.0' "$src")" "1"
check "min luminance scale"    "$(grep -c 'value) \* 10000.0' "$src")" "1"

assets="${MEDIABOX_ASSET_DIR:-$here/assets}"
hdr="$assets/hdr10-4k-2398-main10.mp4"
if [ -f "$hdr" ] && command -v ffprobe >/dev/null; then
  echo "-- generated HDR10 asset carries the metadata the probe depends on"
  info="$(ffprobe -hide_banner -v error -select_streams v:0 \
    -show_entries stream=profile,width,height,pix_fmt,color_transfer,color_primaries,color_space \
    -of default=noprint_wrappers=1 "$hdr")"
  contains "profile is Main 10"    "$info" "profile=Main 10"
  contains "3840 wide"             "$info" "width=3840"
  contains "10-bit pixel format"   "$info" "pix_fmt=yuv420p10le"
  contains "transfer is ST2084"    "$info" "color_transfer=smpte2084"
  contains "primaries are BT.2020" "$info" "color_primaries=bt2020"
  side="$(ffprobe -hide_banner -v error -select_streams v:0 -show_frames \
    -read_intervals '%+#1' -of json "$hdr")"
  contains "mastering display metadata present" "$side" "Mastering display metadata"
  contains "content light level present"        "$side" "Content light level metadata"
else
  echo "-- skipping asset checks (no $hdr)"
fi

echo
if [ "$failures" -eq 0 ]; then
  echo "all host tests passed"
else
  echo "$failures host test(s) failed"
fi
exit $((failures > 0))
