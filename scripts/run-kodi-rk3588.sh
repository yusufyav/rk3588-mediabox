#!/usr/bin/env bash
# Launches Kodi on the RK3588 target in standalone GBM/DRM mode.
#
#   scripts/run-kodi-rk3588.sh start [--fresh]  start Kodi, wait for JSON-RPC
#   scripts/run-kodi-rk3588.sh stop             stop it and confirm
#   scripts/run-kodi-rk3588.sh rpc '<json>'     one JSON-RPC call
#   scripts/run-kodi-rk3588.sh log [N]          tail the Kodi log
#   scripts/run-kodi-rk3588.sh fetch-log        print the whole Kodi log
#   scripts/run-kodi-rk3588.sh gl-info          the GL strings from the last run
#
# MEDIABOX_GPU selects the GL user space, and is the only difference between an
# accelerated run and a software one:
#
#   mali (default) - the private libmali G610 runtime from
#                    scripts/install-mali-runtime.sh, prepended to
#                    LD_LIBRARY_PATH. Nothing is installed system-wide, so this
#                    is per-process and reversible by unsetting one variable.
#   mesa           - the untouched system Mesa, i.e. llvmpipe. Kept because the
#                    Mali GUI claim is only worth as much as the software run it
#                    is compared against, and because it is the fallback if the
#                    vendor blob ever regresses.
#
# The home directory is /var/tmp/kodi-home, not root's: Kodi writes a database,
# thumbnails and a log into it, none of which belong in this repository or in
# the operator's own dotfiles. --fresh wipes it so a run starts from the seeded
# appliance profile rather than from whatever the last run left.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=env.sh
source "$here/scripts/env.sh"

: "${KODI_PREFIX:=/opt/rk3588-mediabox/kodi}"
: "${KODI_RUN_HOME:=/var/tmp/kodi-home}"
: "${KODI_RPC_PORT:=8080}"
: "${MALI_RUNTIME:=/opt/rk3588-mediabox/mali-g24p0-runtime}"
: "${MEDIABOX_GPU:=mali}"

userdata="$KODI_RUN_HOME/.kodi/userdata"
kodi_log="$KODI_RUN_HOME/.kodi/temp/kodi.log"

# Only the GL stack is chosen here. The Rockchip MPP and RGA libraries this
# build needs are found through the RPATH the build put in it, naming MediaBox's
# own media runtime, so they are not on this line and cannot be swapped by an
# environment that drifted.
#
# Mali, when selected, goes in front of everything: kodi-gbm's DT_NEEDED entries
# are the generic sonames (libEGL.so.1, libGLESv2.so.2, libgbm.so.1), so first
# match on LD_LIBRARY_PATH decides the whole GL stack with no rebuild and no
# change under /usr/lib. Mesa is what is left when nothing is prepended.
case "$MEDIABOX_GPU" in
  mali) ld_path="$MALI_RUNTIME/lib" ;;
  mesa) ld_path="" ;;
  *)    echo "MEDIABOX_GPU must be 'mali' or 'mesa', not '$MEDIABOX_GPU'" >&2; exit 2 ;;
esac

rpc() {
  mediabox_ssh "curl -s --max-time 10 -H 'Content-Type: application/json' \
      -d '$1' http://127.0.0.1:$KODI_RPC_PORT/jsonrpc"
}

case "${1:-}" in
  start)
    fresh=""
    [ "${2:-}" = "--fresh" ] && fresh=1
    if [ "$MEDIABOX_GPU" = mali ] && ! mediabox_ssh "test -e '$MALI_RUNTIME/lib/libEGL.so.1'"; then
      echo "no Mali runtime at $MALI_RUNTIME - run scripts/install-mali-runtime.sh first" >&2
      exit 1
    fi
    # Never start a second DRM master: the first one would keep the display and
    # the second would fail in a way that looks like a Kodi bug.
    # Match process names exactly. `pkill -f kodi-gbm` also matches the remote
    # shell command that contains this script's launch line, terminating setup
    # halfway through and occasionally leaving two DRM masters behind.
    mediabox_ssh "pkill -x kodi.bin >/dev/null 2>&1; pkill -x kodi-gbm >/dev/null 2>&1; sleep 2; true"
    if [ -n "$fresh" ]; then
      mediabox_ssh "rm -rf '$KODI_RUN_HOME'"
    fi
    mediabox_ssh "mkdir -p '$userdata' '$KODI_RUN_HOME/.kodi/temp'"
    # The appliance profile, and then the two things in it that are not the
    # same on every board: the ALSA device the audio settings name, and the
    # card config that makes that device exist.
    #
    # Kodi reads passthrough capability off the PCM *name* -- only a name
    # starting with "hdmi" becomes AE_DEVTYPE_HDMI and gets AE_FMT_RAW -- so
    # without the card config the sink enumerates as AE_DEVTYPE_PCM and
    # compressed output is impossible to select, which is exactly how Gate MA1
    # found the machine. Both are rendered by mediabox-hdmi-prepare for the
    # sound card that belongs to the output this box is on, because on a board
    # with two HDMI sockets that is not the same card in both.
    mediabox_scp "$here/config/kodi/guisettings-appliance.xml" \
      "$MEDIABOX_TARGET:$userdata/guisettings.xml" >/dev/null
    mediabox_ssh "/opt/rk3588-mediabox/bin/mediabox-hdmi-prepare" || true
    echo "== starting Kodi (GBM/DRM standalone, GPU=$MEDIABOX_GPU)"
    # AE_SINK pins the audio engine to ALSA. The build already has
    # ENABLE_PULSEAUDIO=OFF, so this cannot silently pick PulseAudio; setting it
    # anyway means a future build that re-enables PulseAudio cannot change the
    # audio path underneath this gate's evidence without the change being loud.
    mediabox_ssh "cd '$KODI_RUN_HOME' && HOME='$KODI_RUN_HOME' \
        LD_LIBRARY_PATH='$ld_path' \
        AE_SINK=ALSA \
        MEDIABOX_GPU='$MEDIABOX_GPU' \
        setsid --fork '$KODI_PREFIX/lib/kodi/kodi-gbm' --standalone --debug \
        </dev/null > '$KODI_RUN_HOME/kodi-stdout.log' 2>&1; \
        sleep 1; pgrep -nx kodi-gbm | sed 's/^/pid=/'"
    echo "== waiting for JSON-RPC"
    for i in $(seq 1 40); do
      sleep 3
      if rpc '{"jsonrpc":"2.0","id":1,"method":"JSONRPC.Ping"}' 2>/dev/null | grep -q pong; then
        echo "== Kodi is up after $((i * 3))s"
        "$0" whitelist-modes
        "$0" gl-info
        exit 0
      fi
    done
    echo "== Kodi did not answer JSON-RPC within 120s" >&2
    exit 1
    ;;
  stop)
    rpc '{"jsonrpc":"2.0","id":1,"method":"Application.Quit"}' >/dev/null 2>&1
    sleep 6
    mediabox_ssh "pkill -x kodi.bin >/dev/null 2>&1; pkill -x kodi-gbm >/dev/null 2>&1; sleep 2; \
        pgrep -ax kodi.bin; pgrep -ax kodi-gbm; true"
    ;;
  rpc)
    rpc "$2"
    ;;
  log)
    mediabox_ssh "tail -${2:-60} '$kodi_log' 2>/dev/null || tail -${2:-60} '$KODI_RUN_HOME/kodi-stdout.log'"
    ;;
  fetch-log)
    mediabox_ssh "cat '$kodi_log' 2>/dev/null"
    ;;
  whitelist-modes)
    # Kodi's default mode whitelist is "every mode at least as large as the
    # current one". That is a reasonable rule for a desktop and the wrong one
    # for an appliance in front of a monitor whose native mode is not 16:9:
    # this panel's is 3840x2560, so 3840x2160 is *shorter* than the desktop and
    # is silently excluded -- and with it every 23.976 film mode. Playback then
    # stays at the desktop's 49.98 Hz and the cadence Gate MP1b proved is lost.
    #
    # So the whitelist is filled in explicitly, from the option list Kodi itself
    # publishes for this connector. Nothing about the modes is hard-coded or
    # guessed: the value strings come from Kodi, so they are correct for
    # whatever sink is attached, and a different monitor produces a different
    # whitelist with no change here.
    #
    # Modes below 720 lines are left out. Switching a 4K panel down to 720x480
    # for SD content is a worse picture than letting the display engine scale
    # it, and it is the one case where "use what the sink offers" is not the
    # right answer.
    echo "== building the mode whitelist from what this sink advertises"
    modes="$(rpc '{"jsonrpc":"2.0","id":1,"method":"Settings.GetSettings","params":{"level":"expert"}}'       | python3 -c '
import json, sys
doc = json.load(sys.stdin)
chosen = []
for setting in doc["result"]["settings"]:
    if setting["id"] != "videoscreen.whitelist":
        continue
    for option in setting["definition"]["options"]:
        value = option["value"]
        # "0384002160023.97600pstd" - width(5) height(5) refresh(9) flags
        try:
            height = int(value[5:10])
        except ValueError:
            continue
        if height >= 720:
            chosen.append(value)
print(json.dumps(chosen))
')"
    if [ -z "$modes" ] || [ "$modes" = "[]" ]; then
      echo "== no modes to whitelist (is Kodi up?)" >&2
      exit 1
    fi
    rpc "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"Settings.SetSettingValue\",\"params\":{\"setting\":\"videoscreen.whitelist\",\"value\":$modes}}"
    echo
    echo "== whitelisted $(echo "$modes" | python3 -c 'import json,sys; print(len(json.load(sys.stdin)))') modes"
    ;;
  gl-info)
    # Kodi logs the GL strings once at start-up. Reading them back from its own
    # log is the only claim that counts: a probe proving Mali says nothing about
    # what this process loaded.
    mediabox_ssh "
      grep -m1 -A3 'GL_VENDOR' '$kodi_log' 2>/dev/null \
        || grep -iE 'GL_VENDOR|GL_RENDERER|GL_VERSION|Vendor:|Renderer:|Version:' '$kodi_log' 2>/dev/null | head -8
      echo '--- objects actually mapped by kodi-gbm ---'
      pid=\$(pgrep -f kodi-gbm | head -1)
      if [ -n \"\$pid\" ]; then
        tr '\0' '\n' < /proc/\$pid/maps 2>/dev/null | true
        awk '{print \$6}' /proc/\$pid/maps | grep -E 'libmali|libEGL|libGLES|libgbm|librga|librockchip|gallium|swrast|dri' | sort -u
      else
        echo '(kodi not running)'
      fi"
    ;;
  *)
    echo "usage: $0 [start [--fresh]|stop|rpc <json>|log [N]|fetch-log|gl-info|whitelist-modes]" >&2
    exit 2
    ;;
esac
