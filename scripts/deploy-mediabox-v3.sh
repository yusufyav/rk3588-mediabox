#!/usr/bin/env bash
#
# Install the MediaBox V3 product UI and the control plane that serves it.
#
# Everything shipped here is built on the developer's machine: the control
# plane is cross-compiled for aarch64 and the UI is WebAssembly, which is
# architecture independent. The appliance carries no Rust toolchain and none is
# installed by this script.
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

say "product UI (wasm32)"
"$here/scripts/build-mediabox-ui.sh"
[ -f "$ui_dist/index.html" ] && [ -f "$ui_dist/mediabox_ui_bg.wasm" ] \
  || { echo "UI bundle is incomplete at $ui_dist" >&2; exit 1; }

say "TV-local browser packages"
# sway rather than a plain kiosk compositor: it can make a window fullscreen
# without the browser asking for it, and it reports output changes, which is
# how the interface survives being moved to a different panel.
sh_ "command -v sway >/dev/null && command -v chromium >/dev/null && fc-list | grep -qi emoji" || {
  echo "installing sway, chromium and an emoji font"
  sh_ "DEBIAN_FRONTEND=noninteractive apt-get update -qq && \
       DEBIAN_FRONTEND=noninteractive apt-get install -y -qq sway chromium fonts-noto-color-emoji"
}

say "binaries -> $prefix/bin"
sh_ "mkdir -p $prefix/bin $prefix/ui $prefix/assets"
cp_ "$bin/mediaboxd-rs" "$bin/mediaboxctl" "root@$host:/var/tmp/"
sh_ "install -m 0755 /var/tmp/mediaboxd-rs /var/tmp/mediaboxctl $prefix/bin/ && \
     rm -f /var/tmp/mediaboxd-rs /var/tmp/mediaboxctl && \
     ln -sfn $prefix/bin/mediaboxctl /usr/local/bin/mediaboxctl"

say "media core -> $prefix"
tar -C "$here" -cf - media | sh_ "rm -rf $prefix/media && tar -C $prefix -xf -"

say "reference clips -> $prefix/assets"
cp_ "$here"/assets/*.mp4 "root@$host:$prefix/assets/"

say "product UI -> $prefix/ui"
tar -C "$ui_dist" -cf - . | sh_ "rm -rf $prefix/ui && mkdir -p $prefix/ui && tar -C $prefix/ui -xf -"

say "television kiosk"
cp_ "$here/packaging/mediabox-kiosk-browser" "$here/packaging/mediabox-kiosk-smoke" \
    "$here/packaging/mediabox-hdmi-prepare" "$here/packaging/mediabox-display-scale" \
    "$here/packaging/mediabox-display-watch" \
    "$here/packaging/mediabox-display-settle" "root@$host:/var/tmp/"
cp_ "$here/config/sway-kiosk.conf" "root@$host:/var/tmp/"
sh_ "set -e
  mkdir -p /etc/mediabox
  install -m 0755 /var/tmp/mediabox-kiosk-browser $prefix/bin/mediabox-kiosk-browser
  install -m 0755 /var/tmp/mediabox-kiosk-smoke $prefix/bin/mediabox-kiosk-smoke
  install -m 0755 /var/tmp/mediabox-hdmi-prepare $prefix/bin/mediabox-hdmi-prepare
  install -m 0755 /var/tmp/mediabox-display-scale $prefix/bin/mediabox-display-scale
  install -m 0755 /var/tmp/mediabox-display-watch $prefix/bin/mediabox-display-watch
  install -m 0755 /var/tmp/mediabox-display-settle $prefix/bin/mediabox-display-settle
  install -m 0644 /var/tmp/sway-kiosk.conf /etc/mediabox/sway-kiosk.conf
  rm -f /var/tmp/mediabox-kiosk-browser /var/tmp/mediabox-kiosk-smoke \
        /var/tmp/mediabox-hdmi-prepare /var/tmp/mediabox-display-scale \
        /var/tmp/mediabox-display-watch /var/tmp/mediabox-display-settle \
        /var/tmp/sway-kiosk.conf"

say "television browser application"
# The browser is an application of the box in its own right: its own unit, its
# own compositor config, its own profile. The exit script is what gives the
# display back, and the address file is where this interface leaves a chosen
# address for it to open.
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
