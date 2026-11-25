#!/bin/sh
# On the appliance: the interface restarted with an experimental binary, or as
# the product has it. A drop-in under /run, gone at the next boot; the product's
# own files are not touched; nothing is done while somebody is watching, and a
# variant that does not stay up is taken back.
#
#   sh variant.sh <idle.py> product
#   sh variant.sh <idle.py> <binary> [NAME=value...]
set -eu
idle=$1 binary=$2
shift 2
# The last run's own movement is waited out; anything else that is going on is
# not going to be over in half a minute.
n=0
until python3 "$idle" 5; do
  n=$((n + 1))
  [ $n -lt 8 ] || exit 1
  sleep 4
done
dir=/run/systemd/system/mediabox-tv-ui.service.d
rm -f "$dir/advgfx.conf"
if [ "$binary" != product ]; then
  mkdir -p "$dir"
  {
    printf '[Service]\nExecStart=\nExecStart=%s\n' "$binary"
    for pair in "$@"; do printf 'Environment=%s\n' "$pair"; done
  } >"$dir/advgfx.conf"
fi
systemctl daemon-reload
since=$(date +%s)
systemctl restart mediabox-tv-ui
sleep 8
restarts=$(systemctl show -p NRestarts --value mediabox-tv-ui)
if ! systemctl is-active --quiet mediabox-tv-ui || [ "$restarts" != 0 ]; then
  journalctl -u mediabox-tv-ui --since "@$since" -o cat --no-pager | tail -30
  rm -f "$dir/advgfx.conf"
  systemctl daemon-reload
  systemctl reset-failed mediabox-tv-ui || true
  systemctl restart mediabox-tv-ui
  echo "variant did not stay up (restarts=$restarts): product restored" >&2
  exit 1
fi
journalctl -u mediabox-tv-ui --since "@$since" -o cat --no-pager | grep -E "tv.gpu|skia|spatial|glcount|rror|panick|tv.present" || true
