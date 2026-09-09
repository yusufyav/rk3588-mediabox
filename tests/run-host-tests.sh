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

# -- Gate MP1b -------------------------------------------------------------
# The MP1b probe's whole value rests on it refusing every quiet degradation
# rather than papering over one, so those refusals are what is guarded here.
pb="$here/tools/hdr-playback-probe.cpp"
core="$(cat "$here"/src/*/*.cpp "$here"/src/*/*.h)"

echo "-- playback probe holds the MP1a A4 state fixed"
contains "BT2020_YCC is not optional"   "$(cat "$pb")" "state.bt2020_ycc = true;"
contains "30bit is not optional"        "$(cat "$pb")" "state.depth30 = true;"

echo "-- playback probe refuses forbidden conversions"
# Matches real use -- the libswscale include path and its sws_* API -- rather
# than the word, which appears in the comments that explain the prohibition.
if grep -qE 'libswscale/|sws_[a-z]+ *\(' "$pb" "$here"/src/*/*.cpp "$here"/src/*/*.h; then
  printf 'FAIL playback probe or core references libswscale\n'
  failures=$((failures + 1))
else
  printf 'ok   playback probe and core never reference libswscale\n'
fi
contains "non-drm_prime output is refused"  "$(cat "$pb")" "not drm_prime; this gate"
contains "NV12 is refused as 8-bit narrowing" "$(cat "$pb")" "narrowed to 8-bit"
contains "non-PQ assets are refused"        "$(cat "$pb")" "not SMPTE ST2084"
contains "mid-run format change is refused" "$(cat "$pb")" "stopping rather than converting"

echo "-- playback probe treats a wrong output rate as a failure"
contains "rate mismatch is reported"    "$(cat "$pb")" "mode MISMATCH"
contains "rate mismatch downgrades the verdict" "$(cat "$pb")" "rate_mismatch && !opt.allow_rate_mismatch"

echo "-- HDR metadata is built from the asset, never invented"
contains "absent primaries are left zero" "$core" "mastering display primaries unavailable (left zero)"
contains "absent MaxCLL is left zero"     "$core" "MaxCLL/MaxFALL unavailable (left zero"
check "chroma unit scale (core)"   "$(grep -c 'value) \* 50000.0' "$here/src/media/hdr_metadata.cpp")" "1"
check "min luminance scale (core)" "$(grep -c 'value) \* 10000.0' "$here/src/media/hdr_metadata.cpp")" "1"

echo "-- cadence is measured from the vblank counter, not wall clock"
contains "repeats come from sequence deltas" "$core" "stats->repeated += delta - 1"

# -- Gate MP1b-CSC ---------------------------------------------------------
# The A/B is only worth running if the A leg is unchanged, the B leg moves
# exactly one property, and the enum behind that property is discovered rather
# than guessed. Those three are what is guarded here.
echo "-- plane COLOR_ENCODING override is opt-in and single-variable"
contains "default leaves the property untouched" "$core" \
  "const char *plane_color_encoding = nullptr;"
contains "probe defaults to the MP1b path"       "$(cat "$pb")" \
  "const char *plane_color_encoding = nullptr;"
contains "COLOR_ENCODING is only added when asked" "$core" \
  "if (state.plane_color_encoding && plane_color_encoding_.present)"
# COLOR_RANGE must never reach an atomic request: the gate forbids moving two
# colour properties at once. The property may be read and logged, so what is
# checked is that its id is never handed to add_prop.
if grep -qE 'add_prop\(.*plane_color_range_' "$here"/src/*/*.cpp "$pb"; then
  printf 'FAIL COLOR_RANGE is set somewhere; the A/B would be two-variable\n'
  failures=$((failures + 1))
else
  printf 'ok   COLOR_RANGE is never written, only read back\n'
fi

echo "-- the BT.2020 enum value is discovered, never hard-coded"
contains "encoding is carried as a DRM enum name" "$(cat "$pb")" \
  'return "ITU-R BT.2020 YCbCr";'
contains "value comes from the plane enum map"    "$core" \
  "plane_color_encoding_.enums.find(state.plane_color_encoding)"
contains "property id comes from lookup"          "$core" \
  'lookup_property(fd_, plane_id_, DRM_MODE_OBJECT_PLANE, "COLOR_ENCODING")'
# A literal enum value next to the property would mean the mapping was assumed.
if grep -nE 'COLOR_ENCODING.*[=,] *2\b|plane_color_encoding_\.id, *[0-9]' \
     "$here"/src/*/*.cpp "$pb"; then
  printf 'FAIL a COLOR_ENCODING enum value looks hard-coded\n'
  failures=$((failures + 1))
else
  printf 'ok   no hard-coded COLOR_ENCODING enum value\n'
fi

echo "-- a missing enum or a rejected combination blocks rather than falls back"
contains "absent enum blocks the run"      "$(cat "$pb")" \
  "this kernel cannot express the requested encoding"
contains "TEST_ONLY runs before the modeset" "$(cat "$pb")" \
  "display.test_only(state, first_fb, placement.src, placement.dst)"
contains "a rejected TEST_ONLY is BLOCKED_DISPLAY" "$(cat "$pb")" \
  "no modeset was attempted"
contains "TEST_ONLY records the kernel errno"      "$(cat "$pb")" \
  "TEST_ONLY rejected the requested "
contains "readback is read from the plane object"  "$core" \
  "plane COLOR_ENCODING requested=%s actual=%s"

echo "-- the override is put back, so the next run's A leg is still A"
contains "the as-found encoding is saved"    "$core" \
  "saved_plane_color_encoding_ = plane_color_encoding_.value;"
contains "and restored on the way out"       "$core" \
  "saved_plane_color_encoding_);"

echo "-- CLI parser"
probe_bin="${MEDIABOX_PROBE_BIN:-}"
if [ -x "$probe_bin" ]; then
  parse_check() {
    local desc="$1" value="$2" want="$3" got
    "$probe_bin" --plane-color-encoding "$value" >/dev/null 2>&1
    got=$?
    # No --input, so a value the parser accepts falls through to usage (exit
    # 2 as well). The parser is therefore probed on its own error text.
    got="$("$probe_bin" --plane-color-encoding "$value" 2>&1 | \
           grep -c 'takes default|bt601-ycc' || true)"
    check "$desc" "$got" "$want"
  }
  parse_check "default is accepted"    "default"    "0"
  parse_check "bt601-ycc is accepted"  "bt601-ycc"  "0"
  parse_check "bt709-ycc is accepted"  "bt709-ycc"  "0"
  parse_check "bt2020-ycc is accepted" "bt2020-ycc" "0"
  parse_check "bt2020 (no suffix) is rejected" "bt2020" "1"
  parse_check "an empty value is rejected"     ""       "1"
else
  echo "-- skipping CLI parser checks (set MEDIABOX_PROBE_BIN to a built probe)"
fi

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
