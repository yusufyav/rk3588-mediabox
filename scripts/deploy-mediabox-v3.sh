#!/usr/bin/env bash
#
# Install the MediaBox V3 product and the control plane behind it.
#
# There are two interfaces and they are not the same thing. The television has a
# native one, cross-compiled for aarch64; a phone or a laptop on the network
# opens the WebAssembly one the daemon serves. Both are built on the developer's
# machine — the appliance carries no Rust toolchain and none is installed by
# this script.
#
# The order matters and is deliberate. Binaries and the UI land first, then the
# media worker gains its library, then the control plane is restarted — so at
# no point is a new daemon serving an old UI, or an old daemon asked for a
# command it does not have.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
host="${MEDIABOX_HOST:-10.27.27.25}"
key="${MEDIABOX_SSH_KEY:-$HOME/.ssh/id_ed25519}"
prefix="${MEDIABOX_PREFIX:-/opt/rk3588-mediabox}"
target_triple="aarch64-unknown-linux-gnu"
ui_dist="${MEDIABOX_UI_DIST:-$here/rust/crates/mediabox-ui/dist}"

ssh_opts=(-F /dev/null -i "$key" -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o ConnectTimeout=10)
sh_() { ssh "${ssh_opts[@]}" "root@$host" "$@"; }
cp_() { scp "${ssh_opts[@]}" -q "$@"; }
say() { printf '\n== %s\n' "$*"; }

say "control plane (cross-compile, $target_triple)"
( cd "$here/rust" && cargo build --release --target "$target_triple" )
bin="$here/rust/target/$target_triple/release"

say "television interface (native, $target_triple)"
# Cross-compiled against the appliance's own sysroot; see the script for why the
# host's cross glibc is not good enough.
"$here/scripts/build-mediabox-tv.sh"
tv_bin="$here/rust/crates/mediabox-tv/target/$target_triple/release/mediabox-tv"
[ -x "$tv_bin" ] || { echo "the television interface did not build" >&2; exit 1; }

say "product UI (wasm32)"
# Still built and still shipped: this is the surface a phone or a laptop on the
# network opens, and the television having its own interface does not take that
# away.
"$here/scripts/build-mediabox-ui.sh"
[ -f "$ui_dist/index.html" ] && [ -f "$ui_dist/mediabox_ui_bg.wasm" ] \
  || { echo "UI bundle is incomplete at $ui_dist" >&2; exit 1; }

say "typefaces"
# The native shell reads the system's fonts. It needs the product's typeface and
# an emoji face, and nothing else: there is no compositor under it and no
# browser engine in it.
sh_ "fc-list | grep -qi emoji && fc-list | grep -qi inter" || {
  echo "installing an emoji font and the product's typeface"
  sh_ "DEBIAN_FRONTEND=noninteractive apt-get update -qq && \
       DEBIAN_FRONTEND=noninteractive apt-get install -y -qq fonts-noto-color-emoji fonts-inter"
}

say "binaries -> $prefix/bin"
sh_ "mkdir -p $prefix/bin $prefix/ui $prefix/assets"
cp_ "$bin/mediaboxd-rs" "$bin/mediaboxctl" "$tv_bin" "root@$host:/var/tmp/"
sh_ "install -m 0755 /var/tmp/mediaboxd-rs /var/tmp/mediaboxctl /var/tmp/mediabox-tv $prefix/bin/ && \
     rm -f /var/tmp/mediaboxd-rs /var/tmp/mediaboxctl /var/tmp/mediabox-tv && \
     ln -sfn $prefix/bin/mediaboxctl /usr/local/bin/mediaboxctl"

say "media core -> $prefix"
tar -C "$here" -cf - media | sh_ "rm -rf $prefix/media && tar -C $prefix -xf -"

say "reference clips -> $prefix/assets"
cp_ "$here"/assets/*.mp4 "root@$host:$prefix/assets/"

say "product UI -> $prefix/ui"
tar -C "$ui_dist" -cf - . | sh_ "rm -rf $prefix/ui && mkdir -p $prefix/ui && tar -C $prefix/ui -xf -"

say "television shell"
# The shell is one binary on the bare display controller. There is no
# compositor under it, no browser engine in it and no launcher script in front
# of it: the unit runs $prefix/bin/mediabox-tv, and the interface works its own
# scale out from the mode the panel reports.
#
# mediabox-display-scale stays because the browser application still uses it.
cp_ "$here/packaging/mediabox-kiosk-smoke" \
    "$here/packaging/mediabox-hdmi-prepare" "$here/packaging/mediabox-display-scale" \
    "$here/packaging/mediabox-console-off" "$here/packaging/mediabox-tv-drive" \
    "root@$host:/var/tmp/"
cp_ "$here/packaging/systemd/mediabox-console-off.service" "root@$host:/etc/systemd/system/"
sh_ "set -e
  mkdir -p /etc/mediabox
  install -m 0755 /var/tmp/mediabox-kiosk-smoke $prefix/bin/mediabox-kiosk-smoke
  install -m 0755 /var/tmp/mediabox-hdmi-prepare $prefix/bin/mediabox-hdmi-prepare
  install -m 0755 /var/tmp/mediabox-display-scale $prefix/bin/mediabox-display-scale
  install -m 0755 /var/tmp/mediabox-console-off $prefix/bin/mediabox-console-off
  install -m 0755 /var/tmp/mediabox-tv-drive $prefix/bin/mediabox-tv-drive
  rm -f /var/tmp/mediabox-kiosk-smoke /var/tmp/mediabox-hdmi-prepare \
        /var/tmp/mediabox-display-scale /var/tmp/mediabox-console-off \
        /var/tmp/mediabox-tv-drive
  systemctl daemon-reload
  systemctl enable --now mediabox-console-off.service >/dev/null"

# What the compositor-based shell left behind. Removed rather than left in
# place: a launcher script that still exists is a launcher script somebody will
# run, and it would start sway on top of a television that already has a shell.
say "removing the compositor-based shell"
sh_ "rm -f $prefix/bin/mediabox-tv-native $prefix/bin/mediabox-kiosk-browser \
           $prefix/bin/mediabox-display-watch $prefix/bin/mediabox-display-settle \
           /etc/mediabox/sway-kiosk.conf"

# The television remote is not a power button.
#
# The HDMI block registers a CEC remote-control input device and udev tags it
# `power-switch`; systemd-logind watches every tagged device and its defaults
# are HandlePowerKey=poweroff and HandleRebootKey=reboot. A code from the
# television's own remote therefore reached init, and the appliance restarted
# with no application involved. The rule takes the tag off, the drop-in makes
# logind ignore those keys anyway, and ctrl-alt-del.target — which is an alias
# of reboot.target, reached by a SIGINT to PID 1 from the console keyboard — is
# masked. Restarting is a request to mediaboxd-rs now, confirmed twice.
say "power keys -> the appliance, not systemd-logind"
cp_ "$here/packaging/udev/80-mediabox-no-power-switch.rules" "root@$host:/var/tmp/"
cp_ "$here/packaging/systemd/logind.conf.d/10-mediabox.conf" "root@$host:/var/tmp/logind-mediabox.conf"
sh_ "set -e
  install -m 0644 /var/tmp/80-mediabox-no-power-switch.rules \
    /etc/udev/rules.d/80-mediabox-no-power-switch.rules
  mkdir -p /etc/systemd/logind.conf.d
  install -m 0644 /var/tmp/logind-mediabox.conf /etc/systemd/logind.conf.d/10-mediabox.conf
  rm -f /var/tmp/80-mediabox-no-power-switch.rules /var/tmp/logind-mediabox.conf
  systemctl mask ctrl-alt-del.target >/dev/null 2>&1 || true
  udevadm control --reload-rules
  udevadm trigger --subsystem-match=input --action=change
  systemctl daemon-reload
  # logind rereads its configuration on reload; the watched-button set is
  # rebuilt from udev's tags at the same time.
  systemctl kill -s HUP systemd-logind.service 2>/dev/null || systemctl restart systemd-logind.service"

say "television browser application"
# The browser is an application of the box in its own right: its own unit, its
# own compositor config, its own profile. The exit script is what gives the
# display back, and the address file is where this interface leaves a chosen
# address for it to open.
# sway and Chromium are the browser application's, and only the browser
# application's. The television's own shell has neither.
sh_ "command -v sway >/dev/null && command -v chromium >/dev/null" || {
  echo "installing sway and chromium for the browser application"
  sh_ "DEBIAN_FRONTEND=noninteractive apt-get update -qq && \
       DEBIAN_FRONTEND=noninteractive apt-get install -y -qq sway chromium"
}
cp_ "$here/packaging/mediabox-browser" "$here/packaging/mediabox-handback" "root@$host:/var/tmp/"
cp_ "$here/config/sway-browser.conf" "root@$host:/var/tmp/"
tar -C "$here/packaging" -cf - browser-remote | sh_ "rm -rf $prefix/browser-remote && tar -C $prefix -xf -"
sh_ "set -e
  mkdir -p /etc/mediabox /var/lib/mediabox-browser
  install -m 0755 /var/tmp/mediabox-browser $prefix/bin/mediabox-browser
  install -m 0755 /var/tmp/mediabox-handback $prefix/bin/mediabox-handback
  install -m 0644 /var/tmp/sway-browser.conf /etc/mediabox/sway-browser.conf
  rm -f /var/tmp/mediabox-browser /var/tmp/mediabox-handback /var/tmp/sway-browser.conf"

say "the player MediaBox owns"
# The player itself is built on the appliance by scripts/build-mediabox-player.sh:
# it links against the Rockchip ffmpeg that lives there and cannot be
# cross-compiled here. This step installs the launcher that gives it the right
# library path and points it at the interface's own compositor, so a film opens
# inside the application instead of taking the television away from it.
cp_ "$here/packaging/mediabox-player" "root@$host:/var/tmp/"
cp_ "$here/packaging/mediabox-player-input.conf" "root@$host:/var/tmp/"
cp_ "$here/packaging/mediabox-player-scripts/mediabox.lua" "root@$host:/var/tmp/"
sh_ "set -e
  mkdir -p $prefix/player/share
  install -m 0755 /var/tmp/mediabox-player $prefix/bin/mediabox-player
  install -m 0644 /var/tmp/mediabox-player-input.conf $prefix/player/share/mediabox-input.conf
  install -m 0644 /var/tmp/mediabox.lua $prefix/player/share/mediabox.lua
  rm -f /var/tmp/mediabox-player /var/tmp/mediabox-player-input.conf /var/tmp/mediabox.lua"

say "application table -> /etc/mediabox-applications.json"
cp_ "$here/config/mediabox-applications.json" "root@$host:/etc/mediabox-applications.json"

say "Kodi keymap -> Back hands the display back"
cp_ "$here/packaging/mediabox-kodi-keymap.xml" "root@$host:/var/tmp/"
cp_ "$here/packaging/mediabox-display-guard" "root@$host:/var/tmp/"
sh_ "install -m 0755 /var/tmp/mediabox-display-guard $prefix/bin/mediabox-display-guard && \
     rm -f /var/tmp/mediabox-display-guard"

cp_ "$here/packaging/mediabox-ui-reap" "root@$host:/var/tmp/"
sh_ "install -m 0755 /var/tmp/mediabox-ui-reap $prefix/bin/mediabox-ui-reap && \
     rm -f /var/tmp/mediabox-ui-reap"
sh_ "set -e
  mkdir -p /var/tmp/kodi-home/.kodi/userdata/keymaps
  install -m 0644 /var/tmp/mediabox-kodi-keymap.xml \
    /var/tmp/kodi-home/.kodi/userdata/keymaps/mediabox.xml
  rm -f /var/tmp/mediabox-kodi-keymap.xml"

say "library manifest -> /etc/mediabox-library.json"
cp_ "$here/config/mediabox-library.json" "root@$host:/etc/mediabox-library.json"

say "units"
cp_ "$here/packaging/systemd/mediaboxd-rs.service" \
    "$here/packaging/systemd/mediabox-media-worker.service" \
    "$here/packaging/systemd/mediabox-tv-ui.service" \
    "$here/packaging/systemd/mediabox-browser.service" \
    "$here/packaging/systemd/kodi.service" \
    "root@$host:/etc/systemd/system/"

# The TV UI unit is installed but never enabled: which process owns the display
# is mediaboxd-rs's decision at runtime, not systemd's at boot.
sh_ "set -e
  systemctl daemon-reload
  systemctl disable mediabox-tv-ui.service >/dev/null 2>&1 || true
  systemctl restart mediabox-media-worker.service
  systemctl restart mediaboxd-rs.service
  systemctl enable mediaboxd-rs.service mediabox-media-worker.service >/dev/null"

# The unit reporting "running" says nothing about what is on the television.
# This puts the interface up and checks that it is genuinely there; a failure
# here fails the deploy.
say "television"
sh_ "$prefix/bin/mediaboxctl surface switch ui >/dev/null && $prefix/bin/mediabox-kiosk-smoke"

say "state"
sh_ "systemctl is-active mediaboxd-rs mediabox-media-worker stremio-server; \
     echo '--- library ---'; $prefix/bin/mediaboxctl media library --json | head -c 400; echo; \
     echo '--- ui ---'; curl -sS -o /dev/null -w 'loopback %{http_code}\n' http://127.0.0.1:8787/
     curl -sS -o /dev/null -w 'lan      %{http_code}\n' http://127.0.0.1:8788/"
