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
# shellcheck source=env.sh
source "$here/scripts/env.sh"
prefix="$MEDIABOX_PREFIX"
target_triple="aarch64-unknown-linux-gnu"
ui_dist="${MEDIABOX_UI_DIST:-$here/rust/crates/mediabox-ui/dist}"

sh_() { mediabox_ssh "$@"; }
cp_() { mediabox_scp "$@"; }
say() { printf '\n== %s\n' "$*"; }

# The other product's prefix, recorded before anything is installed and checked
# again at the end.
#
# MediaBox has its own media runtime now and has no business in
# /opt/rk3588-screenbridge; on a board that carries both products, a deploy that
# wrote there would replace the decoder the other one links against. This is the
# check that says it did not — not a promise in a comment, a hash of every file
# in that directory before and after.
SCREENBRIDGE_PREFIX=/opt/rk3588-screenbridge
screenbridge_manifest() {
  sh_ "test -d $SCREENBRIDGE_PREFIX || exit 0
    cd $SCREENBRIDGE_PREFIX && find . -type f -o -type l | LC_ALL=C sort | while read -r f; do
      if [ -L \"\$f\" ]; then printf 'L %s -> %s\\n' \"\$f\" \"\$(readlink \"\$f\")\";
      else printf '%s  %s\\n' \"\$(sha256sum \"\$f\" | cut -d' ' -f1)\" \"\$f\"; fi
    done"
}
screenbridge_before="$(screenbridge_manifest)"

case "$prefix" in
  "$SCREENBRIDGE_PREFIX"*)
    echo "MEDIABOX_PREFIX may not be inside $SCREENBRIDGE_PREFIX" >&2
    exit 2
    ;;
esac
case "$MEDIABOX_MEDIA_PREFIX" in
  "$SCREENBRIDGE_PREFIX"*)
    echo "MEDIABOX_MEDIA_PREFIX may not be inside $SCREENBRIDGE_PREFIX" >&2
    exit 2
    ;;
esac

say "media runtime"
# Not built here — it is built on the appliance, takes half an hour and changes
# only when its pins do. This is the check that it is there, because both
# players link against it and a deploy that quietly installed binaries with
# nothing behind them would fail on the television rather than here.
sh_ "test -e '$MEDIABOX_MEDIA_PREFIX/lib/pkgconfig/libavcodec.pc'" || {
  echo "no MediaBox media runtime at $MEDIABOX_MEDIA_PREFIX" >&2
  echo "  run scripts/build-media-runtime.sh" >&2
  exit 1
}
# The browser's decoder, which is the same MPP reached through a different door.
#
# Built on the appliance for the same reason the runtime is, and checked rather
# than installed here: without it the browser starts, looks perfectly healthy
# and decodes 4K in software, which is the failure this whole path exists to
# stop. Not fatal -- a board can be deployed and the driver built afterwards --
# but it is said plainly rather than discovered on the television.
sh_ "test -e '$MEDIABOX_PREFIX/browser-runtime/lib/libv4l/plugins/libv4l-rkmpp.so'" || {
  echo "   no browser runtime at $MEDIABOX_PREFIX/browser-runtime" >&2
  echo "   the browser will decode video in software until you run" >&2
  echo "     scripts/build-browser-runtime.sh" >&2
}
# The VA-API driver is the backend the browser no longer selects. It is still
# checked, because it is the documented way back: dropping one feature name
# from the launcher has to land on a decoder that is there.
sh_ "test -e '$MEDIABOX_MEDIA_PREFIX/lib/dri/rockchip_drv_video.so'" || {
  echo "   no VA-API driver at $MEDIABOX_MEDIA_PREFIX/lib/dri" >&2
  echo "   the fallback decode path is absent; build it with" >&2
  echo "     scripts/build-vaapi-driver.sh" >&2
}

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
cp_ "$bin/mediaboxd-rs" "$bin/mediaboxctl" "$bin/mediabox-platform" "$tv_bin" "$MEDIABOX_TARGET:/var/tmp/"
sh_ "install -m 0755 /var/tmp/mediaboxd-rs /var/tmp/mediaboxctl /var/tmp/mediabox-platform \
       /var/tmp/mediabox-tv $prefix/bin/ && \
     rm -f /var/tmp/mediaboxd-rs /var/tmp/mediaboxctl /var/tmp/mediabox-platform /var/tmp/mediabox-tv && \
     ln -sfn $prefix/bin/mediaboxctl /usr/local/bin/mediaboxctl"

say "media core -> $prefix"
tar -C "$here" -cf - media | sh_ "rm -rf $prefix/media && tar -C $prefix -xf -"

say "reference clips -> $prefix/assets"
# Test material, not product: generated by scripts/make-test-assets.sh and not
# kept in the repository. A workstation that has never generated them is a
# workstation that can still deploy the appliance.
if compgen -G "$here/assets/*.mp4" > /dev/null; then
  cp_ "$here"/assets/*.mp4 "$MEDIABOX_TARGET:$prefix/assets/"
else
  echo "   none here; run scripts/make-test-assets.sh to refresh them"
fi

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
    "$here/packaging/mediabox-wireless-up" "$here/packaging/mediabox-bt-agent" \
    "$MEDIABOX_TARGET:/var/tmp/"
cp_ "$here/packaging/systemd/mediabox-console-off.service" \
    "$here/packaging/systemd/mediabox-wireless.service" \
    "$here/packaging/systemd/mediabox-bt-agent.service" \
    "$MEDIABOX_TARGET:/etc/systemd/system/"
sh_ "set -e
  mkdir -p /etc/mediabox
  install -m 0755 /var/tmp/mediabox-kiosk-smoke $prefix/bin/mediabox-kiosk-smoke
  install -m 0755 /var/tmp/mediabox-hdmi-prepare $prefix/bin/mediabox-hdmi-prepare
  install -m 0755 /var/tmp/mediabox-display-scale $prefix/bin/mediabox-display-scale
  install -m 0755 /var/tmp/mediabox-console-off $prefix/bin/mediabox-console-off
  install -m 0755 /var/tmp/mediabox-tv-drive $prefix/bin/mediabox-tv-drive
  install -m 0755 /var/tmp/mediabox-wireless-up $prefix/bin/mediabox-wireless-up
  install -m 0755 /var/tmp/mediabox-bt-agent $prefix/bin/mediabox-bt-agent
  rm -f /var/tmp/mediabox-kiosk-smoke /var/tmp/mediabox-hdmi-prepare \
        /var/tmp/mediabox-display-scale /var/tmp/mediabox-console-off \
        /var/tmp/mediabox-tv-drive /var/tmp/mediabox-wireless-up \
        /var/tmp/mediabox-bt-agent
  # The radios, in an order this hardware survives; see the script.
  systemctl mask systemd-rfkill.service systemd-rfkill.socket >/dev/null 2>&1 || true
  systemctl enable mediabox-wireless.service >/dev/null 2>&1 || true
  systemctl enable --now mediabox-bt-agent.service >/dev/null 2>&1 || true
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
cp_ "$here/packaging/udev/80-mediabox-no-power-switch.rules" \
    "$here/packaging/udev/81-mediabox-display-hotplug.rules" \
    "$here/packaging/mediabox-display-changed" "$MEDIABOX_TARGET:/var/tmp/"
cp_ "$here/packaging/systemd/mediabox-display-changed.service" \
    "$here/packaging/systemd/mediabox-display-seed.service" "$MEDIABOX_TARGET:/etc/systemd/system/"
cp_ "$here/packaging/systemd/logind.conf.d/10-mediabox.conf" "$MEDIABOX_TARGET:/var/tmp/logind-mediabox.conf"
sh_ "set -e
  install -m 0644 /var/tmp/80-mediabox-no-power-switch.rules \
    /etc/udev/rules.d/80-mediabox-no-power-switch.rules
  install -m 0644 /var/tmp/81-mediabox-display-hotplug.rules \
    /etc/udev/rules.d/81-mediabox-display-hotplug.rules
  install -m 0755 /var/tmp/mediabox-display-changed $prefix/bin/mediabox-display-changed
  rm -f /var/tmp/81-mediabox-display-hotplug.rules /var/tmp/mediabox-display-changed
  mkdir -p /etc/systemd/logind.conf.d
  install -m 0644 /var/tmp/logind-mediabox.conf /etc/systemd/logind.conf.d/10-mediabox.conf
  rm -f /var/tmp/80-mediabox-no-power-switch.rules /var/tmp/logind-mediabox.conf
  systemctl mask ctrl-alt-del.target >/dev/null 2>&1 || true
  udevadm control --reload-rules
  udevadm trigger --subsystem-match=input --action=change
  systemctl daemon-reload
  systemctl enable mediabox-display-seed.service >/dev/null
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
# kbd is in the list for chvt: the unit brings its own virtual terminal
# forward, and neither sway nor chromium pulls that package in.
sh_ "command -v sway >/dev/null && command -v chromium >/dev/null && command -v chvt >/dev/null" || {
  echo "installing sway, chromium and kbd for the browser application"
  sh_ "DEBIAN_FRONTEND=noninteractive apt-get update -qq && \
       DEBIAN_FRONTEND=noninteractive apt-get install -y -qq sway chromium kbd"
}
cp_ "$here/packaging/mediabox-browser" "$here/packaging/mediabox-handback" \
    "$here/packaging/mediabox-browser-verify" "$MEDIABOX_TARGET:/var/tmp/"
cp_ "$here/config/sway-browser.conf" "$MEDIABOX_TARGET:/var/tmp/"
tar -C "$here/packaging" -cf - browser-remote | sh_ "rm -rf $prefix/browser-remote && tar -C $prefix -xf -"
sh_ "set -e
  mkdir -p /etc/mediabox /var/lib/mediabox-browser
  install -m 0755 /var/tmp/mediabox-browser $prefix/bin/mediabox-browser
  install -m 0755 /var/tmp/mediabox-handback $prefix/bin/mediabox-handback
  install -m 0755 /var/tmp/mediabox-browser-verify $prefix/bin/mediabox-browser-verify
  install -m 0644 /var/tmp/sway-browser.conf /etc/mediabox/sway-browser.conf
  rm -f /var/tmp/mediabox-browser /var/tmp/mediabox-handback \
        /var/tmp/mediabox-browser-verify /var/tmp/sway-browser.conf"

say "the player MediaBox owns"
# The player itself is built on the appliance by scripts/build-mediabox-player.sh:
# it links against the Rockchip ffmpeg that lives there and cannot be
# cross-compiled here. This step installs the launcher that gives it the right
# library path and points it at the interface's own compositor, so a film opens
# inside the application instead of taking the television away from it.
cp_ "$here/packaging/mediabox-player" "$MEDIABOX_TARGET:/var/tmp/"
cp_ "$here/packaging/mediabox-player-input.conf" "$MEDIABOX_TARGET:/var/tmp/"
cp_ "$here/packaging/mediabox-player-scripts/mediabox.lua" "$MEDIABOX_TARGET:/var/tmp/"
sh_ "set -e
  mkdir -p $prefix/player/share
  install -m 0755 /var/tmp/mediabox-player $prefix/bin/mediabox-player
  install -m 0644 /var/tmp/mediabox-player-input.conf $prefix/player/share/mediabox-input.conf
  install -m 0644 /var/tmp/mediabox.lua $prefix/player/share/mediabox.lua
  rm -f /var/tmp/mediabox-player /var/tmp/mediabox-player-input.conf /var/tmp/mediabox.lua"

say "templates that are filled in from the board"
# Two files that used to be one board's answer written down. The ALSA card
# config has to be named after the sound card of the output this box is on, and
# Kodi's profile has to name the PCM that config defines; on a board with two
# HDMI sockets neither is the same in both. mediabox-hdmi-prepare renders them
# before anything draws, from mediabox-platform's answer.
cp_ "$here/config/alsa/mediabox-hdmi.conf.in" "$MEDIABOX_TARGET:/var/tmp/"
cp_ "$here/config/kodi/guisettings-appliance.xml" "$MEDIABOX_TARGET:/var/tmp/"
sh_ "set -e
  mkdir -p $prefix/share/alsa /var/tmp/kodi-home/.kodi/userdata
  install -m 0644 /var/tmp/mediabox-hdmi.conf.in $prefix/share/alsa/mediabox-hdmi.conf.in
  test -f /var/tmp/kodi-home/.kodi/userdata/guisettings.xml \
    || install -m 0644 /var/tmp/guisettings-appliance.xml /var/tmp/kodi-home/.kodi/userdata/guisettings.xml
  rm -f /var/tmp/mediabox-hdmi.conf.in /var/tmp/guisettings-appliance.xml"

say "application table -> /etc/mediabox-applications.json"
cp_ "$here/config/mediabox-applications.json" "$MEDIABOX_TARGET:/etc/mediabox-applications.json"

say "Kodi keymap -> Back hands the display back"
cp_ "$here/packaging/mediabox-kodi-keymap.xml" "$MEDIABOX_TARGET:/var/tmp/"
cp_ "$here/packaging/mediabox-display-guard" "$MEDIABOX_TARGET:/var/tmp/"
sh_ "install -m 0755 /var/tmp/mediabox-display-guard $prefix/bin/mediabox-display-guard && \
     rm -f /var/tmp/mediabox-display-guard"

# The half of the handback that must not run inside kodi.service's stop.
cp_ "$here/packaging/mediabox-display-recover" "$MEDIABOX_TARGET:/var/tmp/"
sh_ "install -m 0755 /var/tmp/mediabox-display-recover $prefix/bin/mediabox-display-recover && \
     rm -f /var/tmp/mediabox-display-recover"

cp_ "$here/packaging/mediabox-ui-reap" "$MEDIABOX_TARGET:/var/tmp/"
sh_ "install -m 0755 /var/tmp/mediabox-ui-reap $prefix/bin/mediabox-ui-reap && \
     rm -f /var/tmp/mediabox-ui-reap"
sh_ "set -e
  mkdir -p /var/tmp/kodi-home/.kodi/userdata/keymaps
  install -m 0644 /var/tmp/mediabox-kodi-keymap.xml \
    /var/tmp/kodi-home/.kodi/userdata/keymaps/mediabox.xml
  rm -f /var/tmp/mediabox-kodi-keymap.xml"

say "library manifest -> /etc/mediabox-library.json"
cp_ "$here/config/mediabox-library.json" "$MEDIABOX_TARGET:/etc/mediabox-library.json"

say "units"
cp_ "$here/packaging/systemd/mediaboxd-rs.service" \
    "$here/packaging/systemd/mediabox-media-worker.service" \
    "$here/packaging/systemd/mediabox-tv-ui.service" \
    "$here/packaging/systemd/mediabox-browser.service" \
    "$here/packaging/systemd/kodi.service" \
    "$MEDIABOX_TARGET:/etc/systemd/system/"

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
#
# And then it puts the board's role back exactly as it found it. Installing
# this product must not decide that a board belongs to it: a Plus that has been
# a rk3588-screenbridge appliance stays one until somebody says otherwise, with
# `mediaboxctl display-owner set mediabox`. The smoke needs the display for
# twelve seconds, so it borrows it and gives it back -- including the absence of
# the preference file, which is itself the answer "nobody has chosen yet".
say "television"
sh_ "set -e
  owner_file=/var/lib/mediabox/display-owner
  if [ -f \"\$owner_file\" ]; then before=\"\$(cat \"\$owner_file\")\"; else before=absent; fi
  $prefix/bin/mediaboxctl surface switch ui >/dev/null
  smoke=0; $prefix/bin/mediabox-kiosk-smoke || smoke=\$?
  case \"\$before\" in
    mediabox) : ;;
    absent)
      $prefix/bin/mediaboxctl surface switch idle >/dev/null || true
      rm -f \"\$owner_file\"
      echo '   display owner left unchosen; this board is not claimed by MediaBox'
      ;;
    *)
      $prefix/bin/mediaboxctl display-owner set screenbridge >/dev/null || true
      echo \"   display owner put back to \$before\"
      ;;
  esac
  exit \$smoke"

say "the other product's prefix is untouched"
# The whole point of MediaBox owning its own media runtime. A difference here
# means this deploy wrote into rk3588-screenbridge's directory, which on a board
# carrying both products is a decoder swapped under the other one.
screenbridge_after="$(screenbridge_manifest)"
if [ "$screenbridge_before" = "$screenbridge_after" ]; then
  echo "   $SCREENBRIDGE_PREFIX unchanged ($(printf '%s' "$screenbridge_before" | grep -c . ) entries)"
else
  echo "   $SCREENBRIDGE_PREFIX CHANGED during this deploy:" >&2
  diff <(printf '%s\n' "$screenbridge_before") <(printf '%s\n' "$screenbridge_after") >&2 || true
  exit 1
fi

say "platform"
sh_ "$prefix/bin/mediabox-platform inspect" || true

say "state"
sh_ "systemctl is-active mediaboxd-rs mediabox-media-worker stremio-server; \
     echo '--- library ---'; $prefix/bin/mediaboxctl media library --json | head -c 400; echo; \
     echo '--- ui ---'; curl -sS -o /dev/null -w 'loopback %{http_code}\n' http://127.0.0.1:8787/
     curl -sS -o /dev/null -w 'lan      %{http_code}\n' http://127.0.0.1:8788/"
