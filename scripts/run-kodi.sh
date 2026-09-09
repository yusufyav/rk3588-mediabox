#!/usr/bin/env bash
# Launches Kodi on the target in standalone GBM/DRM mode, in an isolated home.
#
#   scripts/run-kodi.sh start [--fresh]   start Kodi, wait for JSON-RPC
#   scripts/run-kodi.sh stop              stop it and confirm the link is back
#   scripts/run-kodi.sh rpc '<json>'      one JSON-RPC call
#   scripts/run-kodi.sh log [N]           tail the Kodi log
#
# The home directory is /var/tmp/kodi-home, not root's: Kodi writes a database,
# thumbnails and a log into it, none of which belong in this repository or in
# the operator's own dotfiles. --fresh wipes it so a run starts from the
# seeded appliance profile rather than from whatever the last run left.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=env.sh
source "$here/scripts/env.sh"

: "${KODI_PREFIX:=/opt/rk3588-mediabox/kodi}"
: "${KODI_RUN_HOME:=/var/tmp/kodi-home}"
: "${KODI_RPC_PORT:=8080}"

userdata="$KODI_RUN_HOME/.kodi/userdata"
kodi_log="$KODI_RUN_HOME/.kodi/temp/kodi.log"

rpc() {
  mediabox_ssh "curl -s --max-time 10 -H 'Content-Type: application/json' \
      -d '$1' http://127.0.0.1:$KODI_RPC_PORT/jsonrpc"
}

case "${1:-}" in
  start)
    fresh=""
    [ "${2:-}" = "--fresh" ] && fresh=1
    # Never start a second DRM master: the first one would keep the display and
    # the second would fail in a way that looks like a Kodi bug.
    mediabox_ssh "pkill -f kodi.bin >/dev/null 2>&1; sleep 2; true"
    if [ -n "$fresh" ]; then
      mediabox_ssh "rm -rf '$KODI_RUN_HOME'"
    fi
    mediabox_ssh "mkdir -p '$userdata' '$KODI_RUN_HOME/.kodi/temp'"
    mediabox_scp "$here/config/kodi/guisettings-appliance.xml" \
      "$MEDIABOX_TARGET:$userdata/guisettings.xml" >/dev/null
    echo "== starting Kodi (GBM/DRM standalone)"
    mediabox_ssh "cd '$KODI_RUN_HOME' && HOME='$KODI_RUN_HOME' \
        nohup '$KODI_PREFIX/lib/kodi/kodi-gbm' --standalone --debug \
        > '$KODI_RUN_HOME/kodi-stdout.log' 2>&1 & echo pid=\$!"
    echo "== waiting for JSON-RPC"
    for i in $(seq 1 40); do
      sleep 3
      if rpc '{"jsonrpc":"2.0","id":1,"method":"JSONRPC.Ping"}' 2>/dev/null | grep -q pong; then
        echo "== Kodi is up after $((i * 3))s"
        exit 0
      fi
    done
    echo "== Kodi did not answer JSON-RPC within 120s" >&2
    exit 1
    ;;
  stop)
    rpc '{"jsonrpc":"2.0","id":1,"method":"Application.Quit"}' >/dev/null 2>&1
    sleep 6
    mediabox_ssh "pkill -f kodi.bin >/dev/null 2>&1; sleep 2; \
        pgrep -af kodi.bin | grep -v pgrep || echo 'kodi stopped'"
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
  *)
    echo "usage: $0 [start [--fresh]|stop|rpc <json>|log [N]|fetch-log]" >&2
    exit 2
    ;;
esac
