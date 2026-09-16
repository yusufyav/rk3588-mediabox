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

lacks() {
  if [[ "$2" != *"$3"* ]]; then
    printf 'ok   %s\n' "$1"
  else
    printf 'FAIL %s: %s is still there\n' "$1" "$3"
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

# -- Runtime isolation -----------------------------------------------------
# MediaBox has its own Rockchip media runtime and does not share the other
# product's prefix. That is a property of this repository, so it is checked
# here rather than discovered on a board that carries both products.
echo "-- MediaBox owns its media runtime"

# Where the prefix is defined, and that it is not the other product's.
media_prefix="$(sed -n 's/^: "${MEDIABOX_MEDIA_PREFIX:=\(.*\)}"$/\1/p' "$here/scripts/env.sh")"
contains "the media prefix is MediaBox's own" "$media_prefix" 'MEDIABOX_PREFIX/media-runtime'

# Anything that names the other prefix in a way that would *use* it: a library
# path, a pkg-config path, an install prefix or an rpath. Naming it in order to
# check it — a guard, an ldd, a diff — is the whole point of some of these
# files, so the rule is about what the line does, not about the word.
uses_foreign_prefix() {
  grep -rnE '(LD_LIBRARY_PATH|PKG_CONFIG_PATH|--prefix|-rpath|Environment)[^#]*/opt/rk3588-screenbridge' \
    "$here/scripts" "$here/packaging" "$here/config" "$here/rust/crates" 2>/dev/null \
    | grep -v '^.*:#' || true
}
offenders="$(uses_foreign_prefix)"
if [ -z "$offenders" ]; then
  printf 'ok   nothing builds or links against the ScreenBridge prefix\n'
else
  printf 'FAIL something still uses the ScreenBridge prefix:\n%s\n' "$offenders"
  failures=$((failures + 1))
fi

# The units are absolute about it: a service file has no reason to mention the
# other product except to declare the interlock, which names a unit and not a
# path.
if grep -rn '/opt/rk3588-screenbridge' "$here/packaging/systemd" >/dev/null 2>&1; then
  printf 'FAIL a unit file names the ScreenBridge prefix\n'
  failures=$((failures + 1))
else
  printf 'ok   no unit file names the ScreenBridge prefix\n'
fi

# And the runtime builder cannot be pointed at it.
contains "the runtime builder refuses that prefix" \
  "$(cat "$here/scripts/build-media-runtime.sh")" 'refusing to build into the ScreenBridge prefix'
contains "the deploy checks the prefix before and after" \
  "$(cat "$here/scripts/deploy-mediabox-v3.sh")" 'screenbridge_manifest'

# -- No board-specific production hardcodes --------------------------------
# The numbers the kernel hands out on one board on one boot: card names, render
# node names, connector names, CEC device names, ALSA card names. None of them
# belongs in a unit file or a launcher — they are what platform discovery is
# for. Diagnostic scripts and probes may still take them as arguments or print
# them; what is checked here is the production path.
echo "-- production paths name no board-specific device"
production=(
  "$here"/packaging/systemd/*.service
  "$here"/packaging/mediabox-hdmi-prepare
  "$here"/packaging/mediabox-player
  "$here"/packaging/mediabox-display-changed
  "$here"/packaging/mediabox-display-scale
  "$here"/packaging/mediabox-kiosk-smoke
  "$here"/config/mediabox-applications.json
)
pattern='/dev/dri/card[0-9]|/dev/dri/renderD[0-9]|/dev/cec[0-9]|rockchiphdmi[0-9]|rockchip-hdmi[0-9]|card[0-9]-HDMI|HDMI-A-[0-9]|\bDP-1\b'
hardcodes=""
for file in "${production[@]}"; do
  [ -f "$file" ] || continue
  # Comments explain why a name is *not* used any more; they are not the
  # product doing anything.
  while IFS= read -r line; do
    hardcodes="$hardcodes${file#"$here/"}: $line"$'\n'
  done < <(grep -nE "$pattern" "$file" | grep -vE '^[0-9]+: *#' || true)
done
if [ -z "$hardcodes" ]; then
  printf 'ok   no production file names a board-specific device\n'
else
  printf 'FAIL board-specific devices in production files:\n%s' "$hardcodes"
  failures=$((failures + 1))
fi

# -- The coexistence contract ----------------------------------------------
# Every unit that takes DRM master declares the interlock with the other
# product, and only those units do: the control plane and the media worker
# touch no display hardware and must stay able to run beside it.
echo "-- display owners declare the ScreenBridge interlock"
for unit in mediabox-tv-ui kodi mediabox-browser; do
  file="$here/packaging/systemd/$unit.service"
  if grep -q '^Conflicts=screenbridge-daemon.service$' "$file" \
     && grep -q '^After=screenbridge-daemon.service$' "$file"; then
    printf 'ok   %s conflicts with and is ordered after screenbridge-daemon\n' "$unit"
  else
    printf 'FAIL %s does not declare the interlock\n' "$unit"
    failures=$((failures + 1))
  fi
done
for unit in mediaboxd-rs mediabox-media-worker stremio-server; do
  file="$here/packaging/systemd/$unit.service"
  if grep -q 'screenbridge-daemon' "$file"; then
    printf 'FAIL %s declares an interlock it does not need\n' "$unit"
    failures=$((failures + 1))
  else
    printf 'ok   %s may run beside the other product\n' "$unit"
  fi
done

# -- The browser application survived the compositor cleanup ---------------
# The television shell stopped being Chromium inside sway. The *browser* is
# still Chromium inside sway, and is a product feature; the two were one thing
# once, which is exactly why this is checked.
echo "-- the browser application is intact"
contains "the browser is in the application table" \
  "$(cat "$here/config/mediabox-applications.json")" '"unit": "mediabox-browser.service"'
for f in packaging/systemd/mediabox-browser.service packaging/mediabox-browser \
         config/sway-browser.conf packaging/mediabox-handback \
         packaging/mediabox-display-scale; do
  if [ -f "$here/$f" ]; then
    printf 'ok   %s\n' "$f"
  else
    printf 'FAIL the browser application is missing %s\n' "$f"
    failures=$((failures + 1))
  fi
done
contains "the deploy installs it" "$(cat "$here/scripts/deploy-mediabox-v3.sh")" \
  'television browser application'

echo
echo "-- leaving Kodi gives the display back at once, and never deadlocks"

# The comments in these two carry the measurements that justify the shape, and
# they name the things that must not come back. So the assertions below read
# the code and not the prose.
code() { sed -e 's/[[:space:]]*#.*$//' "$here/$1"; }
guard="$(code packaging/mediabox-display-guard)"
recover="$(code packaging/mediabox-display-recover)"

# The 15 s grace period. It was correct only for a handover the control plane
# had asked for, and that is exactly the case the guard now recognises instead
# of waiting for; on a Kodi that ended by itself it was measured at 20.758 s of
# black screen between the player going inactive and the interface being asked
# for. Nothing about it may come back: not the turn count, not the sleep, and
# not the hundreds of systemctl invocations the turns cost.
lacks "no recovery poll loop"            "$guard$recover" 'while'
lacks "no grace period before recovery"  "$guard$recover" 'sleep'
lacks "recovery does not poll units"     "$guard$recover" 'is-active'

# The decision is a state, read without waiting for it. Waiting is the b34a48a
# deadlock: the control plane is inside `systemctl stop kodi`, and the stop is
# waiting for this script.
contains "the guard reads the transition gate" "$guard" 'flock -n'
contains "and never blocks on it"              "$guard" '-E 9'
contains "the recovery re-reads it"            "$recover" 'flock -n'
contains "the guard detaches the recovery"     "$guard" 'systemd-run --no-block'
contains "into a cgroup of its own"            "$guard" '--collect'
contains "ordered after the player's own stop" "$guard" '--property=After=kodi.service'

# One gate, shared. Two private mutexes meant a surface switch and an
# application launch could stop each other's target, and neither was visible
# from outside the process.
lifecycle="$(cat "$here/rust/crates/mediaboxd-rs/src/lifecycle.rs")"
contains "both managers take the shared gate" "$lifecycle" 'transition: DisplayTransition'
lacks   "and neither keeps a private one"     "$lifecycle" 'gate: std::sync::Arc<Mutex<()>>'
contains "the daemon wires exactly one"       "$(cat "$here/rust/crates/mediaboxd-rs/src/main.rs")" \
  'let handovers = DisplayTransition::new();'

# The marker had a writer and no reader for two releases, and the script that
# was meant to read it could only ever speak to a compositor. Neither may
# appear in anything that reaches an appliance. `docs/` is deliberately not in
# this list: the history is worth keeping, and it now says "obsolete".
for name in display-handback display-settle; do
  if (cd "$here" && git grep -qI -- "$name" packaging rust config); then
    printf 'FAIL %s is still in the shipped tree\n' "$name"
    failures=$((failures + 1))
  else
    printf 'ok   no %s contract in the shipped tree\n' "$name"
  fi
done

# The deploy is the one exception, and only because it deletes the stale copy
# from appliances installed before the compositor went.
cleanup="$(grep -A3 'removing the compositor-based shell' "$here/scripts/deploy-mediabox-v3.sh")"
contains "the deploy still removes a stale display-settle" "$cleanup" 'mediabox-display-settle'
contains "and only ever removes it"                        "$cleanup" 'rm -f'
check "the deploy names it exactly once, in that cleanup" \
  "$(grep -c display-settle "$here/scripts/deploy-mediabox-v3.sh")" "1"
contains "the docs mark it obsolete" \
  "$(cat "$here/docs/display-pipeline.md")" 'Both are gone'

contains "the deploy installs the recovery helper" \
  "$(cat "$here/scripts/deploy-mediabox-v3.sh")" 'bin/mediabox-display-recover'

for f in packaging/mediabox-display-guard packaging/mediabox-display-recover; do
  if sh -n "$here/$f" 2>/dev/null; then
    printf 'ok   %s parses\n' "$f"
  else
    printf 'FAIL %s does not parse\n' "$f"
    failures=$((failures + 1))
  fi
done

echo
echo "-- the television can sign in to a Stremio account, and cannot leak the password"

tv="$here/rust/crates/mediabox-tv/src"

# The screen exists at all, on the route the settings screen opens.
contains "the account screen is a route"     "$(cat "$tv/route.rs")" 'Route::Account'
contains "the settings screen opens it"      "$(cat "$tv/screens/settings.rs")" 'Action::OpenAccount'
contains "and offers the way out"            "$(cat "$tv/screens/settings.rs")" 'Action::SignOut'

# It goes through the control plane, like everything else this shell does.
# api.strem.io from the television would be a second client with its own idea
# of what an account is.
contains "login goes to the daemon"   "$(cat "$tv/rpc.rs")" '"command": "media_login"'
contains "logout goes to the daemon"  "$(cat "$tv/rpc.rs")" '"command": "media_logout"'
# Comments are stripped first: main.rs quotes an upstream error message that
# names the host, and a quoted error is not a call.
for f in $(cd "$here" && git ls-files rust/crates/mediabox-tv); do
  case "$(sed -e 's|//.*$||' "$here/$f")" in
    *strem.io*|*stremio.com*)
      printf 'FAIL %s reaches the account provider directly\n' "$f"
      failures=$((failures + 1))
      ;;
  esac
done
printf 'ok   the television never calls the provider itself\n'

# The password. It is read out of the screen in exactly one place -- the login
# request -- and it is wiped whichever way the attempt went.
account="$(cat "$tv/screens/account.rs")"
contains "the password is private to the screen" "$account" '    password: String,'
contains "and drawn only as a length"            "$account" 'fn password_mask'
contains "wiped after an attempt"                "$account" 'fn forget_password'
check "read out of the screen exactly once" \
  "$(grep -c 'account.password()' "$tv/main.rs")" "1"

# Nothing prints it. `password` appearing next to a print macro in this crate
# is the failure this guards, whatever the surrounding code looks like.
for f in $(cd "$here" && git ls-files rust/crates/mediabox-tv); do
  if grep -nE '(eprintln|println|dbg)!.*password' "$here/$f" >/dev/null 2>&1; then
    printf 'FAIL %s prints a password\n' "$f"
    failures=$((failures + 1))
  fi
done
printf 'ok   no print in the television carries a password\n'

# The panel is handed the mask, never the text.
slint="$(cat "$here/rust/crates/mediabox-tv/ui/account.slint")"
contains "the panel takes a mask"  "$slint" 'password-mask'
lacks "and never the password"     "$slint" 'in property <string> password;'
contains "the paint sends the mask" "$(cat "$tv/main.rs")" 'set_account_password_mask(account.password_mask()'

# The auth key is the media core's, and it stays there. What reaches the
# television is `authenticated`, an address and a count -- so there is no key
# for a screen or a log to show even by accident, and this is the contract that
# makes that true.
provider="$(sed -n '/def as_dict/,/^$/p' "$here/media/stremio/models.py")"
contains "the provider block says whether it is signed in" "$provider" '"authenticated"'
contains "and who"                                         "$provider" '"email"'
contains "and how many add-ons"                            "$provider" '"addonCount"'
lacks "and never the auth key"                             "$provider" 'authKey'
lacks "and never the password"                             "$provider" 'password'

# One grid, three callers eventually. Two copies of a letter grid is two places
# for the Turkish dotted I to be wrong.
contains "the letter grid is shared"     "$(cat "$tv/screens/search.rs")" 'use crate::keyboard::Cap'
contains "and the account screen uses it" "$account" 'use crate::keyboard::{Edit, Grid}'

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
