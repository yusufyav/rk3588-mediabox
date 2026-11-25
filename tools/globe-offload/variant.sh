#!/bin/sh
# On the appliance: the interface restarted with one of the globe experiment's
# methods, or as the product has it. Nothing is written past the next boot
# (/run), and nothing is done while somebody is watching something.
#
#   sh variant.sh <idle.py> slint|nothing|still|decode|cache <frames' directory>
set -eu
idle=$1 method=$2 frames=$3
python3 "$idle" 5
dir=/run/systemd/system/mediabox-tv-ui.service.d
rm -f "$dir/globe.conf"
if [ "$method" != slint ]; then
  file=globe.h265
  [ "$method" = still ] && file=globe.xrgb
  mkdir -p "$dir"
  printf '[Service]\nEnvironment=MEDIABOX_TV_GLOBE_OFFLOAD=%s\nEnvironment=MEDIABOX_TV_GLOBE_FRAMES=%s\nDeviceAllow=/dev/mpp_service rw\n' \
    "$method" "$frames/$file" >"$dir/globe.conf"
fi
systemctl daemon-reload
systemctl restart mediabox-tv-ui
sleep 6
systemctl is-active mediabox-tv-ui
journalctl -u mediabox-tv-ui --since -7s -o cat --no-pager | grep -E "globe|rror|panick|mpp" || true
