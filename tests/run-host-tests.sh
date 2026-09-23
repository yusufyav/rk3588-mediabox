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

# Comments and quoted text removed, so a word the script only talks about is
# not mistaken for a command it runs.
strip_sh() {
  sed -E -e 's/(^|[[:space:]])#.*$/\1/' -e "s/'[^']*'/''/g" -e 's/"[^"]*"/""/g' "$1"
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
  "$here"/packaging/mediabox-fan-setup
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
# A URL is a call; the bare hostname is not. main.rs quotes an upstream error
# that names the host and account.rs parses one, and neither dials anything --
# so this looks, line by line, for what dialling would actually need.
if (cd "$here" && git grep -nE '(https?://|"//)[a-z0-9.-]*(strem\.io|stremio\.com)' \
      -- rust/crates/mediabox-tv >/dev/null 2>&1); then
  printf 'FAIL the television reaches the account provider directly\n'
  failures=$((failures + 1))
fi
printf 'ok   the television never calls the provider itself\n'

# The password. It is read out of the screen in exactly one place -- the login
# request -- and it is wiped whichever way the attempt went.
account="$(cat "$tv/screens/account.rs")"
contains "the password is private to the screen" "$account" '    password: Entry,'
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

echo "-- the production installer compiles nothing"
if "$here/tests/check-build-free.sh" >/dev/null 2>&1; then
  echo "ok   build-free installer"
else
  echo "FAIL build-free installer:"
  "$here/tests/check-build-free.sh" | sed 's/^/     /'
  failures=$((failures + 1))
fi

echo "-- the release is pinned to an exact tag and digest"
pin="$here/releases/current.env"
if [ -f "$pin" ]; then
  # shellcheck source=/dev/null
  ( set -a; . "$pin"
    [ -n "${MEDIABOX_RELEASE_TAG:-}" ] &&
    [ -n "${MEDIABOX_RELEASE_ASSET:-}" ] &&
    [ -n "${MEDIABOX_RELEASE_SIZE:-}" ] &&
    [[ "${MEDIABOX_RELEASE_SHA256:-}" =~ ^[0-9a-f]{64}$ ]] ) &&
    echo "ok   releases/current.env carries a tag, an asset, a size and a digest" || {
      echo "FAIL releases/current.env is not fully pinned"
      failures=$((failures + 1)); }
  if grep -q 'releases/latest\|/latest/download' "$pin"; then
    echo "FAIL releases/current.env points at a moving target"
    failures=$((failures + 1))
  else
    echo "ok   releases/current.env names no moving target"
  fi
else
  echo "FAIL releases/current.env is missing"
  failures=$((failures + 1))
fi

echo "-- the installer will not report PASS without a film having played"
installer="$(cat "$here/scripts/install/install-mediabox.sh")"
contains "the playback smoke is a gate"   "$installer" 'packaging/mediabox-playback-smoke'
contains "and failing it stops the install" "$installer" "the appliance's own player did not play"
contains "PASS says the player works"     "$installer" 'the default player is operational'

echo "-- the media worker can find an ffprobe, and one that speaks TLS"
worker="$(cat "$here/packaging/systemd/mediabox-media-worker.service")"
contains "the unit names a PATH"           "$worker" 'Environment=PATH='
# The order is the whole point: the appliance's own FFmpeg has no TLS and
# answers "Protocol not found" to every catalogue source, so the system one
# has to win and the product's own build is only the fallback.
contains "the system path comes first"     "$worker" 'Environment=PATH=/usr/local/sbin:'
contains "the product's build is the tail" "$worker" ':/opt/rk3588-mediabox/media-runtime/bin'
capture="$(cat "$here/scripts/release/create-mediabox-release.sh")"
contains "and ffmpeg is a declared runtime dependency" "$capture" 'for c in python3 ffprobe ffmpeg'

echo "-- the playback smoke measures the things that were broken"
smoke="$(cat "$here/packaging/mediabox-playback-smoke")"
contains "it asks the daemon to play"     "$smoke" 'media_play_here'
contains "it checks the hardware decoder" "$smoke" '/dev/mpp_service'
contains "it checks the dma-buf heap"     "$smoke" '/dev/dma_heap/'
contains "it checks the video plane"      "$smoke" 'NV12|NV15'
contains "and that the interface comes back" "$smoke" 'interface has the display'

echo "-- the verifier probes a real source, not just files"
verifier="$(cat "$here/packaging/mediabox-product-verify")"
contains "it plans a packaged source"     "$verifier" 'media_policy'
contains "it names the probe failure"     "$verifier" 'cannot run ffprobe'
contains "and the socket must be listening" "$verifier" 'nothing is listening on it'
contains "and the probe must speak https"  "$verifier" 'probe speaks https'
contains "and a font must cover the catalogue" "$verifier" 'catalogue glyphs'
contains "Kodi counts as an owner"          "$verifier" 'the television has an owner'
contains "and Kodi must be reachable"       "$verifier" 'Kodi control endpoint'

echo "-- Kodi is given this appliance's settings on a first run"
kodi_unit="$(cat "$here/packaging/systemd/kodi.service")"
contains "the unit seeds the profile"       "$kodi_unit" 'guisettings-appliance.xml'
contains "and only when there is none"      "$kodi_unit" 'test -f /var/tmp/kodi-home/.kodi/userdata/guisettings.xml ||'
contains "the installer puts it in place"   "$installer" 'share/kodi/guisettings-appliance.xml'
profile="$(cat "$here/config/kodi/guisettings-appliance.xml")"
contains "and that profile opens the control endpoint" "$profile" '<setting id="services.webserver">true'

echo "-- the Kodi seed carries what was measured on the board, not defaults"
# Each of these was absent once, and each absence broke something that still
# let Kodi start: the mode list (picture came back blue and magenta on a Dolby
# Vision title), the renderer (10-bit went through the GUI plane), and the
# transcode pair (anything not plain AC-3 played as silence).
contains "the display mode whitelist" "$profile" '<setting id="videoscreen.whitelist">'
contains "the screen resolution"      "$profile" '<setting id="videoscreen.resolution">'
contains "direct-to-plane rendering"  "$profile" '<setting id="videoplayer.useprimerenderer">0'
# The timing is what every picture decision is made against: what the link can
# carry, and so whether HDR is signalled, is a per-mode answer.
contains "the film's own mode is selected" "$profile" '<setting id="videoplayer.adjustrefreshrate">2'
contains "AC-3 transcoding on"        "$profile" '<setting id="audiooutput.ac3transcode">true'
contains "E-AC-3 passthrough off"     "$profile" '<setting id="audiooutput.eac3passthrough">false'
contains "DTS passthrough off"        "$profile" '<setting id="audiooutput.dtspassthrough">false'
contains "the sink is named per board" "$profile" '@ALSA_DEVICE@'
if grep -q 'services.deviceuuid' "$here/config/kodi/guisettings-appliance.xml" | grep -q '<setting'; then
  echo "FAIL the seed carries a box's own identity"
  failures=$((failures + 1))
else
  echo "ok   the seed carries no per-box identity"
fi
contains "the capture reports Kodi drift" "$capture" 'settings differing from config/kodi/guisettings-appliance.xml'
# The browser's enterprise policy is product, it lives under /etc, and it was
# on the working board while no script in this repository installed it.
contains "the browser policy is installed" "$capture" 'etc/chromium/policies/managed/mediabox.json'
contains "the verifier requires them"     "$verifier" 'Kodi picture and sound settings'
contains "and the fonts are a declared dependency" "$capture" 'fc-list ":charset=$ch" file'

echo "-- the relay serves a local file without dying on its status"
relay="$(cat "$here/media/proxy/relay.py")"
contains "file URLs have their own path"  "$relay" 'if urllib.parse.urlsplit(url).scheme == "file"'
contains "and ranges are honoured"        "$relay" 'Content-Range'

echo "-- the kiosk smoke treats the product's own player as required"
smoke="$(cat "$here/packaging/mediabox-kiosk-smoke")"
contains "the player is checked for existence" "$smoke" "bad 'player present'"
contains "vo_mediabox is required"             "$smoke" 'vo_mediabox absent'
contains "rkmpp is required"                   "$smoke" 'rkmpp absent'

echo "-- a staged build cannot be shipped as the product"
# The failure this guards is specific: an accepted A/B candidate of Kodi copied
# over the production binary, still configured for the staging prefix, passing
# every check for as long as the staging tree was on the disk. So both the
# verifier and the capture have to look at the prefix baked into the binary --
# not at whether the data tree happens to exist right now.
contains "the verifier reads the baked prefix" "$verifier" "grep -aoE '/opt/rk3588-mediabox/[A-Za-z0-9._+-]+/share/kodi'"
contains "and rejects a candidate binary"      "$verifier" 'rk3588-mediabox/kodi-candidate'
contains "and sweeps staging leftovers"        "$verifier" 'no staging leftovers'
contains "the capture gates the baked prefix"  "$capture"  'Kodi data prefix'
contains "and gates staging leftovers"         "$capture"  'staging leftovers'
# `strings` is binutils and this appliance does not carry it; a verifier that
# needs it is a verifier that cannot run on a clean board.
lacks "the verifier needs no binutils"         "$verifier" 'strings -a'

echo "-- the player asks for a link the sink can actually receive"
# The defect this guards: ten-bit RGB does not fit a 300 MHz HDMI port at 4K,
# the vendor driver silently subsamples to 4:2:2 and does not write the
# negotiated format back, and a player that tags the wire from its own request
# then tells the television RGB over YCbCr pixels. Measured on a Sony
# KD-65XE9005, whose HDMI 1 and HDMI 3 declare 300 and 600 MHz respectively.
rgb_patch="$here/patches/kodi/0012-gbm-signal-hdr-only-where-the-link-can-carry-it.patch"
if [ -f "$rgb_patch" ]; then
  patch_text="$(cat "$rgb_patch")"
  contains "the RGB request is conditional"      "$patch_text" 'RequestRgbOutput(rgbFits)'
  contains "and the condition is the link budget" "$patch_text" 'RgbLinkFits(10)'
  contains "the ceiling comes from the EDID"      "$patch_text" 'SinkMaxCharacterRateKHz'
  contains "the HDMI Forum block wins"            "$patch_text" '0xD8 && payload[1] == 0x5D'
  contains "the mode carries its own clock"       "$patch_text" 'GetCurrentModeClockKHz'
  # A sink that declares nothing must keep the old behaviour: a DVI monitor
  # takes RGB and nothing else, and a missing byte is not evidence.
  contains "an undeclared ceiling stays on RGB"   "$patch_text" 'is not a sink that declared a narrow link'
  # The fix is not the tag, it is the transport: where HDR cannot be carried
  # the whole BT.2020 signal is given up, the depth drops to eight, and the
  # link stays RGB. Measured on a Sony KD-65XE9005 HDMI 1, where 4:2:2 renders
  # HDR wrong whatever it is labelled.
  contains "HDR is declined where it cannot be carried" "$patch_text" 'HdrLinkFits'
  contains "and the colorimetry drops with it"          "$patch_text" 'transmitting BT.709 SDR'
  contains "and so does the depth"                      "$patch_text" 'SetDeepColor(false)'
  # And the unconditional request it replaces must be gone.
  lacks "no unconditional RGB request"            "$patch_text" '+  const std::optional<bool> rgbOutput = RequestRgbOutput(true);'
else
  echo "FAIL patches/kodi/0012 is missing"
  failures=$((failures + 1))
fi

echo "-- the archive can be checked on the image the product is installed on"
# A clean Armbian Minimal carries no binutils, and the archive is verified
# before the runtime packages are installed, so the verifier had better not
# need any. This used to be `readelf` and it stopped every clean install.
if "$here/tests/check-static-verify.sh" >/dev/null 2>&1; then
  echo "ok   STATIC_VERIFY_WITHOUT_READELF=PASS"
else
  echo "FAIL STATIC_VERIFY_WITHOUT_READELF:"
  "$here/tests/check-static-verify.sh" | sed 's/^/     /'
  failures=$((failures + 1))
fi
# The text, too, so a verifier that grows a readelf back is caught even if the
# fixture ever stops exercising the line that would use it.
if strip_sh "$here/packaging/mediabox-product-verify" |
   grep -qE '(^|[;&|(){]|&&|\|\||\$\()[[:space:]]*readelf([[:space:]]|$)'; then
  echo "FAIL the verifier invokes readelf"
  failures=$((failures + 1))
else
  echo "ok   the verifier invokes no readelf"
fi
contains "it reads the dynamic section itself" "$verifier" 'elf_runpath()'
contains "and cross-checks the capture's reading" "$verifier" 'recorded_runpath'
contains "the capture records the runpath" "$capture" 'runpath.tsv'
contains "and gates what it recorded"      "$capture" 'mpv RUNPATH recorded'

# The line along the bottom of the panel has one way in.
#
# `say` gives it a lifetime and the quarter-second tick takes it away again.
# A caller that writes to the window directly gets a line that never leaves:
# that is how one source failing left "Kaynak açılamadı" in the corner of the
# panel through the next attempt, which worked, and over the film that was
# then playing. Two call sites are correct -- `say` itself and `clear_notice`
# -- and a third is the bug coming back.
notice_writes=$(grep -c 'set_notice(' "$here/rust/crates/mediabox-tv/src/main.rs" || true)
if [ "$notice_writes" -eq 2 ]; then
  echo "ok   the notice line is set in exactly two places"
else
  echo "FAIL set_notice is called $notice_writes times, expected 2 (say and clear_notice)"
  grep -n 'set_notice(' "$here/rust/crates/mediabox-tv/src/main.rs" | sed 's/^/     /'
  failures=$((failures + 1))
fi

# -- the handover does not ask the media core to resolve its own address
#
# A film playing here already has a session, and that session already carries
# the address Kodi is meant to open. Handing that address back to the core as
# a new source is what broke the handover: the core refuses its own loopback
# ("loopback sources are only allowed for the configured streaming server"),
# the reply was 400, and Kodi -- started in parallel -- lived 73 ms. So the
# handover must not go through `play_on_kodi`, which is the resolve path.
daemon="$here/rust/crates/mediaboxd-rs/src/daemon.rs"
handoff=$(awk '/async fn handoff_to_kodi/,/^    }$/' "$daemon")
if printf '%s' "$handoff" | grep -q 'is_proxy_address'; then
  echo "ok   the handover asks what kind of address it is holding"
else
  echo "FAIL handoff_to_kodi does not distinguish the core's proxy from an upstream address"
  failures=$((failures + 1))
fi
# And what Kodi is given is never one of our own addresses: the core refuses
# its own loopback as a source (400), and opening the live pipe puts Kodi in
# the middle of the container -- measured as "Input #0, ac3": sound, no
# picture, no duration.
if printf '%s' "$handoff" | grep -q 'playing.origin'; then
  echo "ok   a transformed film hands over by its upstream address"
else
  echo "FAIL the proxied handover does not use the upstream address"
  failures=$((failures + 1))
fi
# The catalogue's own "play on Kodi" goes the same way.
on_kodi=$(awk '/async fn play_on_kodi/,/^    }$/' "$daemon")
if printf '%s' "$on_kodi" | grep -q 'handoff/resolvedInput'; then
  echo "ok   playing on Kodi gives it the resolved source"
else
  echo "FAIL play_on_kodi still hands Kodi the media core's own address"
  failures=$((failures + 1))
fi

echo "-- a display change recovers whichever application owns the display"

# Run the helper against stubs: what is plugged in, which units are active, and
# a log of what it asked systemd and the preparation to do. No framework --
# three small scripts in a scratch directory.
hp="$(mktemp -d)"
trap 'rm -rf "$hp"' EXIT
mkdir -p "$hp/bin" "$hp/run" "$hp/browser" "$hp/sys/card0-HDMI-A-1" "$hp/sys/card0-HDMI-A-2"
: >"$hp/browser/sway-ipc.0.1.sock"
cat >"$hp/bin/platform" <<'EOF'
#!/bin/sh
case "$1" in
  outputs)
    # A sequence file plays one state per call, then keeps the last one.
    if [ -s "$HP/seq" ]; then head -1 "$HP/seq" | tr '|' '\n'; [ "$(wc -l <"$HP/seq")" -gt 1 ] && sed -i 1d "$HP/seq"
    else cat "$HP/outputs"; fi ;;
  output) awk -F '\t' '$2 == "connected" { print $1; exit }' "$HP/outputs" ;;
  *) ;;
esac
EOF
cat >"$hp/bin/systemctl" <<'EOF'
#!/bin/sh
if [ "$1" = is-active ]; then grep -qx "$3" "$HP/active"; exit; fi
echo "systemctl $*" >>"$HP/log"
EOF
cat >"$hp/bin/prepare" <<'EOF'
#!/bin/sh
echo "prepare boot-video=${MEDIABOX_HDMI_BOOT_VIDEO:-0} reset=${MEDIABOX_HDMI_CONNECTOR_RESET:-1}" >>"$HP/log"
# The browser's mode, as the real preparation would write it for this display.
[ -s "$HP/swaymode" ] && cp "$HP/swaymode" "$HP/run/sway-output.conf"
exit 0
EOF
printf '#!/bin/sh\necho "swaymsg $* sock=${SWAYSOCK##*/}" >>"$HP/log"\n' >"$hp/bin/swaymsg"
chmod +x "$hp/bin/"*
on=$'HDMI-A-1\tdisconnected\t-\nHDMI-A-2\tconnected\t2560x1440'
off=$'HDMI-A-1\tdisconnected\t-\nHDMI-A-2\tdisconnected\t2560x1440'
# What is recorded: each socket and whether something is on it, no mode.
off_state=$'HDMI-A-1\tdisconnected\nHDMI-A-2\tdisconnected'
on_state=$'HDMI-A-1\tdisconnected\nHDMI-A-2\tconnected'

# changed <was-or-SEED> <now> <active units...>: prints the log, one line each.
changed() {
  : >"$hp/log"; printf '%s\n' "${@:3}" >"$hp/active"
  printf '%s' "$2" >"$hp/outputs"
  if [ "$1" = SEED ]; then run_helper seed; else printf '%s' "$1" >"$hp/run/stamp"; fi
  printf '%s' "$2" >"$hp/outputs"
  run_helper
  tr '\n' ';' <"$hp/log"
}
run_helper() {
  HP="$hp" MEDIABOX_PLATFORM="$hp/bin/platform" MEDIABOX_HDMI_PREPARE="$hp/bin/prepare" \
    MEDIABOX_SYSTEMCTL="$hp/bin/systemctl" MEDIABOX_DISPLAY_STAMP="$hp/run/stamp" \
    MEDIABOX_TRANSITION_LOCK="$hp/run/lock" MEDIABOX_DISPLAY_SETTLE=0 MEDIABOX_RELINK_PAUSE=0 MEDIABOX_DRM_SYSFS="$hp/sys" \
    MEDIABOX_RUN_DIR="$hp/run" MEDIABOX_BROWSER_RUN="$hp/browser" MEDIABOX_SWAYMSG="$hp/bin/swaymsg" \
    sh "$here/packaging/mediabox-display-changed" "$@" 2>/dev/null
  echo "exit=$?" >>"$hp/log"
}
recover_ui="systemctl stop mediabox-tv-ui.service;prepare boot-video=1 reset=1;systemctl start mediabox-tv-ui.service;exit=0;"

# 1. Booted with nothing plugged in: the seed records it, so the first plug
#    is a change and is acted on instead of being the one that gets recorded.
: >"$hp/log"; printf '%s' "$off" >"$hp/outputs"; rm -f "$hp/run/stamp"
run_helper seed
check "seeding a headless boot records the disconnected state" "$(cat "$hp/run/stamp")" "$off_state"
printf 'mediabox-tv-ui.service\n' >"$hp/active"; : >"$hp/log"
printf '%s' "$on" >"$hp/outputs"; run_helper
check "the first plug after a headless boot recovers the owner" "$(tr '\n' ';' <"$hp/log")" "$recover_ui"
# 2. Nothing changed: no restart.
check "an unchanged display restarts nothing" "$(changed "$on_state" "$on" mediabox-tv-ui.service)" "exit=0;"
# 3-5. Whichever owner is active is the one recovered.
# 3. The browser is not restarted: its compositor modesets in place and the
#    same Chromium carries on. Its configuration is refreshed without touching
#    the connector it holds, and sway is reloaded only for a new mode.
echo 'output * mode 2560x1440@119.998Hz' >"$hp/swaymode"; cp "$hp/swaymode" "$hp/run/sway-output.conf"
check "the browser keeps running when the same display comes back" \
  "$(changed "$off_state" "$on" mediabox-browser.service)" "prepare boot-video=1 reset=0;exit=0;"
echo 'output * mode 1920x1080@60.000Hz' >"$hp/swaymode"
check "a display with another mode is handed to sway by a reload" \
  "$(changed "$off_state" "$on" mediabox-browser.service)" \
  "prepare boot-video=1 reset=0;swaymsg reload sock=sway-ipc.0.1.sock;exit=0;"
: >"$hp/swaymode"
check "the interface is recovered when it owns the display" \
  "$(changed "$off_state" "$on" mediabox-tv-ui.service)" "$recover_ui"
check "Kodi is recovered when it owns the display" \
  "$(changed "$off_state" "$on" kodi.service)" \
  "systemctl stop kodi.service;prepare boot-video=1 reset=1;systemctl start kodi.service;exit=0;"
# 6. Two owners at once is not a state to guess about.
check "two active owners restart nothing" \
  "$(changed "$off_state" "$on" mediabox-tv-ui.service mediabox-browser.service)" "exit=1;"
check "and leave the change to be seen again" "$(cat "$hp/run/stamp")" "$off_state"
# 7 is in every recovery line above: stop, then prepare, then start.
# A disconnect alone has nothing to recover and must not write video= (8).
check "only a disconnect touches no owner and prepares nothing" \
  "$(changed "$on_state" "$off" mediabox-browser.service)" "exit=0;"
check "and is recorded, so the replug is a change" "$(cat "$hp/run/stamp")" "$off_state"
check "a mode column drifting on its own is not a change" \
  "$(changed "$off_state" $'HDMI-A-1\tdisconnected\t-\nHDMI-A-2\tdisconnected\t-' kodi.service)" "exit=0;"
# A socket reads `connected -` until something probes it. Measured moving a
# monitor between sockets: waiting for the mode cost five seconds and then ran
# the recovery twice. The socket is probed, and recovered once.
printf '%s\n' 'HDMI-A-1\tconnected\t-|HDMI-A-2\tdisconnected' 'HDMI-A-1\tconnected\t-|HDMI-A-2\tdisconnected' \
  'HDMI-A-1\tconnected\t2560x1440|HDMI-A-2\tdisconnected' | sed 's/\\t/\t/g' >"$hp/seq"
check "a socket without its mode yet is recovered once" \
  "$(changed "$off_state" "" mediabox-tv-ui.service)" "$recover_ui"
check "and recorded without a mode" "$(cat "$hp/run/stamp")" $'HDMI-A-1\tconnected\nHDMI-A-2\tdisconnected'
rm -f "$hp/seq"
echo connected >"$hp/sys/card0-HDMI-A-2/status"; echo disconnected >"$hp/sys/card0-HDMI-A-1/status"
changed "$off_state" "$on" mediabox-tv-ui.service >/dev/null
check "a connected socket is probed before it is prepared" "$(cat "$hp/sys/card0-HDMI-A-2/status")" detect
check "an empty one is left alone" "$(cat "$hp/sys/card0-HDMI-A-1/status")" disconnected
# Kodi leaves the CRTC lit, and nothing after it modesets: the link is taken
# down and brought back between its stop and the preparation. Nobody else's.
echo connected >"$hp/sys/card0-HDMI-A-2/status"
: >"$hp/log"; printf 'kodi.service\n' >"$hp/active"; printf '%s' "$off_state" >"$hp/run/stamp"
printf '%s' "$on" >"$hp/outputs"
relinked="$(HP="$hp" MEDIABOX_PLATFORM="$hp/bin/platform" MEDIABOX_HDMI_PREPARE="$hp/bin/prepare" \
  MEDIABOX_SYSTEMCTL="$hp/bin/systemctl" MEDIABOX_DISPLAY_STAMP="$hp/run/stamp" \
  MEDIABOX_TRANSITION_LOCK="$hp/run/lock" MEDIABOX_DISPLAY_SETTLE=0 MEDIABOX_RELINK_PAUSE=0 \
  MEDIABOX_DRM_SYSFS="$hp/sys" MEDIABOX_RUN_DIR="$hp/run" \
  sh "$here/packaging/mediabox-display-changed" 2>&1 >/dev/null)"
contains "Kodi's connector is taken down and brought back" "$relinked" 'bağlantı yeniden kuruldu'
echo connected >"$hp/sys/card0-HDMI-A-2/status"
: >"$hp/log"; printf 'mediabox-tv-ui.service\n' >"$hp/active"; printf '%s' "$off_state" >"$hp/run/stamp"
relinked="$(HP="$hp" MEDIABOX_PLATFORM="$hp/bin/platform" MEDIABOX_HDMI_PREPARE="$hp/bin/prepare" \
  MEDIABOX_SYSTEMCTL="$hp/bin/systemctl" MEDIABOX_DISPLAY_STAMP="$hp/run/stamp" \
  MEDIABOX_TRANSITION_LOCK="$hp/run/lock" MEDIABOX_DISPLAY_SETTLE=0 MEDIABOX_RELINK_PAUSE=0 \
  MEDIABOX_DRM_SYSFS="$hp/sys" MEDIABOX_RUN_DIR="$hp/run" \
  sh "$here/packaging/mediabox-display-changed" 2>&1 >/dev/null)"
lacks "the interface's is not, it modesets from scratch itself" "$relinked" 'bağlantı yeniden kuruldu'
lacks "the helper no longer restarts without waiting" \
  "$(strip_sh "$here/packaging/mediabox-display-changed")" 'restart --no-block'
contains "the boot record is installed" \
  "$(cat "$here/scripts/release/create-mediabox-release.sh")" 'mediabox-display-seed.service'
contains "and runs before any owner" \
  "$(cat "$here/packaging/systemd/mediabox-display-seed.service")" \
  'Before=mediabox-tv-ui.service mediabox-browser.service kodi.service'

# 8-9. The boot argument, through the real preparation with modetest stubbed:
# written only when asked, only for a connected output, and only the one token.
cat >"$hp/bin/modetest" <<'EOF'
#!/bin/sh
if [ "$3" = -w ]; then echo "modetest -w $4" >>"$HP/log"; exit 0; fi
[ "$3" = -c ] || exit 0
printf '236\t235\tconnected\tHDMI-A-2       \t600x340\t\t27\t235\n  props:\n'
printf '\t1 EDID:\n\t\tflags: immutable blob\n\t\tblobs:\n\n\t\tvalue:\n\t\t\t00ffffffffffff00\n'
printf '\t7 HDR_OUTPUT_METADATA:\n\t\tflags: blob\n\t\tblobs:\n\n\t\tvalue:\n'
[ -s "$HP/hdr" ] && printf '\t\t\t%s\n' "$(cat "$HP/hdr")"
printf '\t238 color_format:\n\t\tflags: enum\n\t\tenums: rgb=0 ycbcr444=1\n\t\tvalue: %s\n' "$(cat "$HP/cf" 2>/dev/null || echo 0)"
printf '\t249 Colorspace:\n\t\tflags: enum\n\t\tvalue: 0\n  modes:\n'
printf '  #0 2560x1440 59.95 2560 2608 2640 2720 1440 1443 1448 1481 241500 flags: phsync, nvsync; type: preferred, driver\n'
printf '  #1 2560x1440 144.00 2560 2568 2600 2720 1440 1465 1473 1490 583600 flags: phsync, pvsync; type: userdef, driver\n'
printf '  #2 2560x1440 120.00 2560 2608 2640 2720 1440 1443 1448 1525 497750 flags: phsync, pvsync; type: driver\n'
EOF
chmod +x "$hp/bin/modetest"
env_before=$'verbosity=1\nextraargs=cma=256M video=HDMI-A-1:3840x2160@60\nuser_overlays=mediabox-hdmi-any-vp fan-pwm-50hz'
prepare_real() {
  HP="$hp" PATH="$hp/bin:$PATH" MEDIABOX_PLATFORM="$hp/bin/platform" \
    MEDIABOX_BOOT_ENV="$hp/armbianEnv.txt" MEDIABOX_RUN_DIR="$hp/run" MEDIABOX_HDMI_KEEP_HDR=1 \
    "$@" sh "$here/packaging/mediabox-hdmi-prepare" 2>&1
}
printf '%s\n' "$env_before" >"$hp/armbianEnv.txt"; rm -f "$hp/armbianEnv.txt.mediabox-video"
printf '%s' "$off" >"$hp/outputs"
prepare_real env >/dev/null
check "an application's own preparation leaves /boot alone, with no display" "$(cat "$hp/armbianEnv.txt")" "$env_before"
out="$(prepare_real env MEDIABOX_HDMI_BOOT_VIDEO=1)"
check "a boot with no display still removes a video= an older version left" \
  "$(cat "$hp/armbianEnv.txt")" \
  $'verbosity=1\nextraargs=cma=256M\nuser_overlays=mediabox-hdmi-any-vp fan-pwm-50hz'
contains "and says so" "$out" 'boot-video=removed'
printf '%s\n' "$env_before" >"$hp/armbianEnv.txt"; rm -f "$hp/armbianEnv.txt.mediabox-video"
printf '%s' "$on" >"$hp/outputs"
prepare_real env >/dev/null
check "an application's own preparation leaves /boot alone" "$(cat "$hp/armbianEnv.txt")" "$env_before"
out="$(prepare_real env MEDIABOX_HDMI_BOOT_VIDEO=1)"
check "a settled display removes a video= an older version left, and writes none" \
  "$(cat "$hp/armbianEnv.txt")" \
  $'verbosity=1\nextraargs=cma=256M\nuser_overlays=mediabox-hdmi-any-vp fan-pwm-50hz'
contains "and says it removed it" "$out" 'boot-video=removed'
check "the original is kept once" "$(cat "$hp/armbianEnv.txt.mediabox-video")" "$env_before"
printf '%s\n' 'kodi_screenmode=0256001440119.99800pstd' \
  'kodi_whitelist=0256001440119.99800pstd,0256001440059.95000pstd' \
  'browser_mode=2560x1440@119.998Hz' >"$hp/run/output-plan"
prepare_real env >/dev/null
check "the browser's mode is the display setting's, from the daemon's plan" \
  "$(cat "$hp/run/sway-output.conf")" 'output * mode 2560x1440@119.998Hz'
rm -f "$hp/run/output-plan"
echo 'output * mode 1920x1080@60.000Hz' >"$hp/run/sway-output.conf"
out="$(prepare_real env)"
check "with no plan yet the browser's mode is left as it is" \
  "$(cat "$hp/run/sway-output.conf")" 'output * mode 1920x1080@60.000Hz'
contains "and says so" "$out" 'browser-mode=absent'
contains "and a second run finds nothing to remove" \
  "$(prepare_real env MEDIABOX_HDMI_BOOT_VIDEO=1)" 'boot-video=absent'

# The colour reset writes only what is not already so: on this driver the
# first write after boot re-trains the link under a lit panel and the monitor
# stays black. What Kodi leaves behind is still put back.
colour() {
  : >"$hp/log"
  HP="$hp" PATH="$hp/bin:$PATH" MEDIABOX_PLATFORM="$hp/bin/platform" MEDIABOX_MODETEST="$hp/bin/modetest" \
    MEDIABOX_BOOT_ENV="$hp/armbianEnv.txt" MEDIABOX_RUN_DIR="$hp/run" \
    sh "$here/packaging/mediabox-hdmi-prepare" >/dev/null 2>&1
  tr '\n' ';' <"$hp/log"
}
printf '%s' "$on" >"$hp/outputs"; echo 0 >"$hp/cf"; : >"$hp/hdr"
check "a connector already in SDR RGB is not written to" "$(colour)" ""
echo 1 >"$hp/cf"; echo 0102030405 >"$hp/hdr"
check "the YUV wire and HDR metadata Kodi left are cleared" "$(colour)" \
  "modetest -w 236:HDR_OUTPUT_METADATA:0;modetest -w 236:color_format:0;"
check "nothing is committed to a connector an owner still holds" \
  "$(MEDIABOX_HDMI_CONNECTOR_RESET=0 colour)" ""

# -- The fan's boot configuration ------------------------------------------
# /boot/armbianEnv.txt decides whether the board boots, and its user_overlays
# line already carries the HDMI crossbar. The fan setup adds its own two
# entries -- the Plus's 50 Hz carrier correction on a Plus only, the curve on
# any board with a pwm-fan -- and leaves everything else where it was.
echo "-- fan boot configuration"
fs="$(mktemp -d)"
fan_board() {
  rm -rf "$fs/dt" "$fs/overlay-user"
  mkdir -p "$fs/dt/pwm-fan" "$fs/overlay-user"
  printf '%s\0' "$1" >"$fs/dt/model"
  printf 'pwm-fan\0' >"$fs/dt/pwm-fan/compatible"
  printf '%s\n' "$2" >"$fs/armbianEnv.txt"
  rm -f "$fs/armbianEnv.txt.mediabox-fan"
}
fan_setup() {
  MEDIABOX_BOOT_ENV="$fs/armbianEnv.txt" MEDIABOX_OVERLAY_DIR="$fs/overlay-user" \
    MEDIABOX_DT_ROOT="$fs/dt" \
    MEDIABOX_FAN_FIX_DTBO="$here/packaging/overlays/mediabox-fan-opi5plus-50hz.dtbo" \
    bash "$here/packaging/mediabox-fan-setup"
}
overlays() { grep '^user_overlays=' "$fs/armbianEnv.txt"; }

env_plus=$'verbosity=1\nextraargs=cma=256M video=HDMI-A-2:2560x1440@144\nuser_overlays=mediabox-hdmi-any-vp fan-pwm-50hz\nrootfstype=ext4'
fan_board "Orange Pi 5 Plus" "$env_plus"
out="$(fan_setup)"
check "a Plus gets its carrier fix and the curve, after what was there" "$(overlays)" \
  'user_overlays=mediabox-hdmi-any-vp fan-pwm-50hz mediabox-fan-curve mediabox-fan-opi5plus-50hz'
check "every other line is untouched" "$(grep -v '^user_overlays=' "$fs/armbianEnv.txt")" \
  "$(printf '%s\n' "$env_plus" | grep -v '^user_overlays=')"
check "the fix installed is the repository's" \
  "$(cmp -s "$fs/overlay-user/mediabox-fan-opi5plus-50hz.dtbo" "$here/packaging/overlays/mediabox-fan-opi5plus-50hz.dtbo" && echo same)" same
check "the original is kept once" "$(cat "$fs/armbianEnv.txt.mediabox-fan")" "$env_plus"
contains "a change says a reboot is needed" "$out" "reboot needed"
before="$(sha256sum "$fs/armbianEnv.txt")"
out="$(fan_setup)"
check "a second run adds nothing twice and writes nothing" "$(sha256sum "$fs/armbianEnv.txt")" "$before"
check "and asks for no reboot" "$([[ "$out" == *"reboot needed"* ]] && echo yes || echo no)" no

fan_board "Orange Pi 5 Plus" $'verbosity=1\nuser_overlays=mediabox-fan-curve mediabox-hdmi-any-vp mediabox-fan-curve'
fan_setup >/dev/null
check "a doubled entry of ours is folded, others keep their order" "$(overlays)" \
  'user_overlays=mediabox-fan-curve mediabox-hdmi-any-vp mediabox-fan-opi5plus-50hz'

fan_board "Orange Pi 5 Plus" 'verbosity=1'
fan_setup >/dev/null
check "a file with no overlay line gets one" "$(overlays)" \
  'user_overlays=mediabox-fan-curve mediabox-fan-opi5plus-50hz'

fan_board "Orange Pi 5 Ultra" $'user_overlays=mediabox-hdmi-any-vp'
fan_setup >/dev/null
check "an Ultra gets the curve and never the Plus fix" "$(overlays)" \
  'user_overlays=mediabox-hdmi-any-vp mediabox-fan-curve'
check "and no Plus fix file" "$([ -e "$fs/overlay-user/mediabox-fan-opi5plus-50hz.dtbo" ] && echo yes || echo no)" no

fan_board "Orange Pi 5 Ultra" $'user_overlays=mediabox-hdmi-any-vp mediabox-fan-opi5plus-50hz mediabox-fan-curve'
fan_setup >/dev/null
check "a disk moved from a Plus to an Ultra loses the Plus fix only" "$(overlays)" \
  'user_overlays=mediabox-hdmi-any-vp mediabox-fan-curve'

fan_board "" $'user_overlays=mediabox-hdmi-any-vp mediabox-fan-opi5plus-50hz'
fan_setup >/dev/null
check "a board that does not name itself keeps what it had" "$(overlays)" \
  'user_overlays=mediabox-hdmi-any-vp mediabox-fan-opi5plus-50hz mediabox-fan-curve'

fan_board "Orange Pi 5 Plus" $'user_overlays=mediabox-hdmi-any-vp'
rm -f "$fs/dt/pwm-fan/compatible"
before="$(sha256sum "$fs/armbianEnv.txt")"
fan_setup >/dev/null
check "a board with no pwm-fan is left alone" "$(sha256sum "$fs/armbianEnv.txt")" "$before"
rm -rf "$fs"

echo
if [ "$failures" -eq 0 ]; then
  echo "all host tests passed"
else
  echo "$failures host test(s) failed"
fi
exit $((failures > 0))
