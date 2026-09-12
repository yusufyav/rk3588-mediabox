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
# sway rather than a plain kiosk compositor for one reason: it can choose the
# output mode. This panel prefers 3840x2160, and compositing a 1080p interface
# at 4K costs four times the bandwidth for nothing anyone can see.
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
cp_ "$here/packaging/mediabox-kiosk-browser" "root@$host:/var/tmp/"
cp_ "$here/config/sway-kiosk.conf" "root@$host:/var/tmp/"
sh_ "set -e
  mkdir -p /etc/mediabox
  install -m 0755 /var/tmp/mediabox-kiosk-browser $prefix/bin/mediabox-kiosk-browser
  install -m 0644 /var/tmp/sway-kiosk.conf /etc/mediabox/sway-kiosk.conf
  rm -f /var/tmp/mediabox-kiosk-browser /var/tmp/sway-kiosk.conf"

say "library manifest -> /etc/mediabox-library.json"
cp_ "$here/config/mediabox-library.json" "root@$host:/etc/mediabox-library.json"

say "units"
cp_ "$here/packaging/systemd/mediaboxd-rs.service" \
    "$here/packaging/systemd/mediabox-media-worker.service" \
    "$here/packaging/systemd/mediabox-tv-ui.service" \
    "root@$host:/etc/systemd/system/"

# The TV UI unit is installed but never enabled: which process owns the display
# is mediaboxd-rs's decision at runtime, not systemd's at boot.
sh_ "set -e
  systemctl daemon-reload
  systemctl disable mediabox-tv-ui.service >/dev/null 2>&1 || true
  systemctl restart mediabox-media-worker.service
  systemctl restart mediaboxd-rs.service
  systemctl enable mediaboxd-rs.service mediabox-media-worker.service >/dev/null"

say "state"
sh_ "systemctl is-active mediaboxd-rs mediabox-media-worker stremio-server; \
     echo '--- library ---'; $prefix/bin/mediaboxctl media library --json | head -c 400; echo; \
     echo '--- ui ---'; curl -sS -o /dev/null -w 'loopback %{http_code}\n' http://127.0.0.1:8787/
     curl -sS -o /dev/null -w 'lan      %{http_code}\n' http://127.0.0.1:8788/"
