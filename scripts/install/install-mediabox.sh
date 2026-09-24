#!/usr/bin/env bash
#
# Install MediaBox on a clean Armbian board.
#
#   git clone https://github.com/yusufyav/rk3588-mediabox.git
#   cd rk3588-mediabox
#   sudo ./scripts/install/install-mediabox.sh
#
# Nothing is compiled. Not here, not in anything this calls, and not as a
# fallback when a prebuilt piece does not fit: the product is an archive
# captured from an appliance that works, and a board that cannot run that
# archive is a board this installer refuses, not one it builds a new product
# for. `tests/run-host-tests.sh` enforces that in the repository.
#
#   --bundle FILE    install from a local archive instead of the release
#   --verify-only    check the archive and this board, change nothing
#   --force          install although the capability gate failed (not advised)
#
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
prefix=/opt/rk3588-mediabox
bundle=""
verify_only=0
force=0
reboot_needed=0

while [ $# -gt 0 ]; do
  case "$1" in
    --bundle)      bundle="$2"; shift 2 ;;
    --verify-only) verify_only=1; shift ;;
    --force)       force=1; shift ;;
    -h|--help)     sed -n '2,18p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

step() { printf '\n== %s\n' "$*"; }
ok()   { printf '  ok    %s\n' "$*"; }
note() { printf '  --    %s\n' "$*"; }
die()  { printf '\nFAIL: %s\n' "$*" >&2; exit 1; }

pin="$here/releases/current.env"
[ -f "$pin" ] || die "no release is pinned: $pin is missing"
# shellcheck source=/dev/null
. "$pin"
: "${MEDIABOX_RELEASE_REPO:?releases/current.env does not name a repository}"
: "${MEDIABOX_RELEASE_TAG:?releases/current.env does not name a tag}"
: "${MEDIABOX_RELEASE_ASSET:?releases/current.env does not name an asset}"
: "${MEDIABOX_RELEASE_SHA256:?releases/current.env does not name a digest}"
: "${MEDIABOX_RELEASE_SIZE:?releases/current.env does not name a size}"
: "${MEDIABOX_RELEASE_ARCH:?releases/current.env does not name an architecture}"

# ------------------------------------------------------------------ 1. root
step "this machine"
if [ "$verify_only" -eq 0 ]; then
  [ "$(id -u)" -eq 0 ] || die "run this with sudo"
  ok "running as root"
else
  note "verify-only: nothing on this board will be changed"
fi

# ---------------------------------------------------------- 2. architecture
arch="$(uname -m)"
if [ "$arch" = "$MEDIABOX_RELEASE_ARCH" ]; then
  ok "architecture $arch"
elif [ "$verify_only" -eq 1 ]; then
  # Checking the archive is not running it, and the check is worth running on
  # the workstation that produced it.
  note "architecture $arch, release is $MEDIABOX_RELEASE_ARCH -- verifying anyway"
else
  die "this release is $MEDIABOX_RELEASE_ARCH, this board is $arch"
fi

# --------------------------------------------- 3. kernel capability preflight
#
# The reference board ran a 6.1.115 vendor kernel and a clean Ultra will come up
# on 6.1.172. The version is not the contract -- the capabilities are, and a
# kernel that has them runs these binaries whatever it calls itself. A kernel
# that does not is reported as such and the install stops. It does not become a
# reason to build anything.

step "kernel capability preflight"
kernel="$(uname -r)"
note "kernel $kernel (reference ${MEDIABOX_REFERENCE_KERNEL:-unknown}, exact match not required)"
cap_fail=0
cap() {
  if [ "$2" = yes ]; then ok "$1"
  else printf '  MISS  %s\n' "$1"; cap_fail=$((cap_fail + 1)); fi
}
cap "Rockchip MPP service (/dev/mpp_service)" "$([ -e /dev/mpp_service ] && echo yes || echo no)"
cap "DRM render node" "$(compgen -G '/dev/dri/renderD*' >/dev/null && echo yes || echo no)"
cap "DRM card node"   "$(compgen -G '/dev/dri/card*' >/dev/null && echo yes || echo no)"
rockchip=no
for d in /sys/class/drm/card*/device/driver; do
  [ -e "$d" ] || continue
  case "$(basename "$(readlink -f "$d")")" in rockchip*|*rockchip*) rockchip=yes ;; esac
done
cap "Rockchip DRM topology" "$rockchip"
cap "ALSA cards" "$([ -e /proc/asound/cards ] && echo yes || echo no)"

# A cable is not a kernel capability.
#
# "a connected display connector" used to be one of the lines above, and it
# refused a perfectly good board: the Plus was installed over ssh while the only
# television in the house was on the Ultra, and the installer answered
# KERNEL_CAPABILITY_FAIL over an HDMI socket that was merely empty. Nothing
# about that kernel was missing. What a display is actually needed for is the
# two gates at the end -- the verifier's display checks and the film -- so those
# are skipped and named, and the install itself goes through.
display=""
for s in /sys/class/drm/card*-*/status; do
  [ -e "$s" ] || continue
  [ "$(cat "$s")" = connected ] && display="$display $(basename "$(dirname "$s")")"
done
headless=0
if [ -n "$display" ]; then
  ok "a connected display:$display"
else
  headless=1
  note "no display is connected: installing headless, and the gates that need one
        are skipped at the end rather than waived"
fi

if [ "$cap_fail" -gt 0 ]; then
  if [ "$force" -eq 1 ]; then
    note "KERNEL_CAPABILITY_FAIL overridden by --force"
  elif [ "$verify_only" -eq 1 ]; then
    note "KERNEL_CAPABILITY_FAIL ($cap_fail missing) -- reported, not fatal in verify-only"
  else
    die "KERNEL_CAPABILITY_FAIL: $cap_fail capability/ies this product depends on are missing.
     This kernel cannot run the prebuilt appliance. Nothing will be rebuilt for it."
  fi
fi

# ------------------------------------------------------- 4. the archive
step "the appliance archive"
work="$(mktemp -d /var/tmp/mediabox-install-XXXXXX)"
trap 'rm -rf "$work"' EXIT

if [ -n "$bundle" ]; then
  [ -f "$bundle" ] || die "no such bundle: $bundle"
  archive="$bundle"
  note "offline bundle $archive"
else
  need_bootstrap=""
  command -v curl >/dev/null || need_bootstrap="$need_bootstrap curl"
  command -v zstd >/dev/null || need_bootstrap="$need_bootstrap zstd"
  [ -e /etc/ssl/certs/ca-certificates.crt ] || need_bootstrap="$need_bootstrap ca-certificates"
  if [ -n "$need_bootstrap" ]; then
    [ "$(id -u)" -eq 0 ] || die "need$need_bootstrap to fetch the release; re-run with sudo"
    note "fetching$need_bootstrap"
    DEBIAN_FRONTEND=noninteractive apt-get update -qq
    # shellcheck disable=SC2086
    DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends $need_bootstrap
  fi
  # An exact tag and an exact digest. Never "latest": a moving target is a
  # product that differs between two boards installed on two afternoons.
  #
  # The repository is private, so the plain releases/download URL answers 404 to
  # anyone without a credential -- including the board that has just cloned this
  # tree with one. Whatever credential got the clone here is used to get the
  # asset: the gh CLI if it is signed in, otherwise a token in the environment.
  # If the repository is made public, the last branch works with neither.
  archive="$work/$MEDIABOX_RELEASE_ASSET"
  url="https://github.com/$MEDIABOX_RELEASE_REPO/releases/download/$MEDIABOX_RELEASE_TAG/$MEDIABOX_RELEASE_ASSET"
  token="${GH_TOKEN:-${GITHUB_TOKEN:-}}"
  got=0
  if [ "$got" -eq 0 ] && command -v gh >/dev/null && gh auth status >/dev/null 2>&1; then
    note "gh release download $MEDIABOX_RELEASE_TAG"
    gh release download "$MEDIABOX_RELEASE_TAG" --repo "$MEDIABOX_RELEASE_REPO" \
       --pattern "$MEDIABOX_RELEASE_ASSET" --dir "$work" --clobber && got=1 || true
  fi
  if [ "$got" -eq 0 ] && [ -n "$token" ]; then
    note "GitHub API, authenticated"
    api="https://api.github.com/repos/$MEDIABOX_RELEASE_REPO/releases/tags/$MEDIABOX_RELEASE_TAG"
    asset_url="$(curl -fsSL -H "Authorization: Bearer $token" \
        -H 'Accept: application/vnd.github+json' "$api" |
      python3 -c 'import json,sys
d = json.load(sys.stdin)
print(next((a["url"] for a in d.get("assets", []) if a["name"] == sys.argv[1]), ""))' \
        "$MEDIABOX_RELEASE_ASSET")" || asset_url=""
    [ -n "$asset_url" ] || die "the release does not carry $MEDIABOX_RELEASE_ASSET"
    curl -fSL --retry 3 --retry-delay 2 -H "Authorization: Bearer $token" \
      -H 'Accept: application/octet-stream' -o "$archive" "$asset_url" && got=1 || true
  fi
  if [ "$got" -eq 0 ]; then
    note "$url"
    curl -fSL --retry 3 --retry-delay 2 -o "$archive" "$url" && got=1 || true
  fi
  [ "$got" -eq 1 ] || die "could not download the release asset.
     The repository is private: sign in with \`gh auth login\`, or set GH_TOKEN,
     or install from a local copy with --bundle <file>."
fi

size="$(stat -c %s "$archive")"
[ "$size" = "$MEDIABOX_RELEASE_SIZE" ] \
  || die "size mismatch: expected $MEDIABOX_RELEASE_SIZE bytes, got $size"
ok "size $size bytes"

# ------------------------------------------------------- 5. expected SHA256
sha="$(sha256sum "$archive" | cut -d' ' -f1)"
[ "$sha" = "$MEDIABOX_RELEASE_SHA256" ] \
  || die "digest mismatch:
     expected $MEDIABOX_RELEASE_SHA256
     got      $sha"
ok "sha256 $sha"

# ------------------------------------------- 6. staging extract and manifest
step "unpacking and verifying the archive"
command -v zstd >/dev/null || die "zstd is not installed and is needed to unpack the release"
staging="$work/staging"
mkdir -p "$staging"
zstd -dc "$archive" | tar -x --numeric-owner -C "$staging" \
  || die "the archive did not unpack"
[ -f "$staging/meta/SHA256SUMS" ] || die "the archive carries no meta/SHA256SUMS"
[ -f "$staging/meta/release-info" ] || die "the archive carries no meta/release-info"
ok "meta/release-info $(sed -n 's/^version=//p' "$staging/meta/release-info")"

( cd "$staging" && sha256sum --quiet -c meta/SHA256SUMS ) \
  || die "internal digests do not match the unpacked files"
ok "internal manifest verified ($(wc -l <"$staging/meta/SHA256SUMS") files)"

# --------------------------------------- 7. mandatory artifact closure check
verifier="$here/packaging/mediabox-product-verify"
[ -x "$verifier" ] || die "the product verifier is missing: $verifier"
MEDIABOX_VERIFY_ROOT="$staging/rootfs" MEDIABOX_VERIFY_META="$staging/meta" \
  MEDIABOX_VERIFY_STATIC_ONLY=1 "$verifier" \
  || die "the archive does not carry a complete product"

if [ "$verify_only" -eq 1 ]; then
  step "verify-only"
  ok "archive, digests and artifact closure are sound; nothing was installed"
  exit 0
fi

# ------------------------------------------ 8. runtime packages (no builders)
#
# apt installing a runtime library is not a build. The list comes out of the
# capture's own dependency closure, so it holds libraries and the few commands
# the product runs -- sway, chromium, kbd, alsa-utils -- and no compiler and no
# -dev package, because there is nothing here to compile.

step "runtime packages"
pkgs="$staging/meta/runtime-packages.txt"
[ -f "$pkgs" ] || die "the archive carries no runtime package manifest"
if grep -qE '^(build-essential|gcc|g\+\+|rustc|cargo|cmake|meson|ninja-build|.+-dev)$' "$pkgs"; then
  die "the runtime package manifest names a build package; refusing to install it"
fi
DEBIAN_FRONTEND=noninteractive apt-get update -qq
# shellcheck disable=SC2046
DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
  $(tr '\n' ' ' <"$pkgs") \
  || die "could not install the runtime packages this product depends on"
ok "$(wc -l <"$pkgs") packages present"

# ----------------------------------------------- 9. users and mutable state
#
# State is not product. The archive carries no logs, no sessions and no keys,
# and the directories that will hold them are created empty here.

step "users and state"
if ! getent group stremio >/dev/null; then groupadd --system stremio; fi
if ! getent passwd stremio >/dev/null; then
  useradd --system --gid stremio --home-dir /var/lib/stremio-server \
          --shell /usr/sbin/nologin stremio
fi
ok "service user stremio"
install -d -m 0750 -o root -g root /var/lib/mediabox /var/lib/mediabox-ui \
                                   /var/lib/mediabox-browser /var/lib/mediabox-media-worker
install -d -m 0755 -o stremio -g stremio /var/lib/stremio-server
install -d -m 0755 /var/tmp/kodi-home
ok "state directories"

# --------------------------------------------------- 10. atomic /opt install
#
# The archive is never unpacked over a running prefix. It is moved into place in
# one rename, and a previous install is only removed once the new one is there.

step "installing $prefix"
new="$prefix.new-$$"
old="$prefix.old-$$"
rm -rf "$new"
mv "$staging/rootfs/opt/rk3588-mediabox" "$new"
if [ -d "$prefix" ]; then
  mv "$prefix" "$old"
  mv "$new" "$prefix"
  rm -rf "$old"
  ok "replaced the previous install"
else
  mv "$new" "$prefix"
  ok "installed"
fi

# ------------------------------------------------ 11. /etc integration files
#
# Each integration file comes from where the capture recorded it coming from.
# The unit files, the udev rules and the JSON tables are packaging: this
# repository holds them and is ahead of any archive as soon as one of them is
# fixed. The daemon's own configuration exists only on the appliance, so for
# those the archive is the only source.
#
# Taking them all from the archive instead would mean re-cutting a 350 MB
# release to change one line of a unit file -- and it would have re-installed
# the very unit whose missing PATH stopped the player from ever starting.
step "system integration"
manifest="$staging/meta/integration-manifest.tsv"
install_one() {
  rel="$1"; src="$2"; origin="$3"
  mode="$(stat -c %a "$staging/rootfs/$rel" 2>/dev/null || echo 0644)"
  install -D -m "$mode" "$src" "/$rel"
  printf '  ok    /%-52s %s (%s)\n' "$rel" "$origin" "$mode"
}
if [ -f "$manifest" ]; then
  while IFS="$(printf '\t')" read -r rel source _state; do
    [ -n "${rel:-}" ] || continue
    case "$source" in
      repo:*)
        repo_path="$here/${source#repo:}"
        if [ -f "$repo_path" ]; then
          install_one "$rel" "$repo_path" repo
        else
          install_one "$rel" "$staging/rootfs/$rel" "archive (not in this tree)"
        fi
        ;;
      *) install_one "$rel" "$staging/rootfs/$rel" archive ;;
    esac
  done <"$manifest"
else
  ( cd "$staging/rootfs" && find etc -type f -print0 ) |
    while IFS= read -r -d '' rel; do
      install_one "$rel" "$staging/rootfs/$rel" archive
    done
fi

# Integration files an archive captured before they existed does not list.
# They are packaging, so this tree is their source either way; the observer's
# unit only where the archive carries the observer.
for pair in \
  "packaging/systemd/mediabox-display-observer.service:etc/systemd/system/mediabox-display-observer.service:bin/mediabox-display-observer" \
  "config/alsa/60-mediabox-unrouted.conf:etc/alsa/conf.d/60-mediabox-unrouted.conf:"; do
  src="${pair%%:*}"; rest="${pair#*:}"; rel="${rest%%:*}"; needs="${rest#*:}"
  [ -e "/$rel" ] && continue
  [ -z "$needs" ] || [ -e "$prefix/$needs" ] || continue
  install -D -m 0644 "$here/$src" "/$rel"
  printf '  ok    /%-52s %s\n' "$rel" "repo (not in this archive's manifest)"
done

# The product's own checks live beside the product, so the appliance can be
# asked whether it is whole without this repository being present.
install -m 0755 "$here/packaging/mediabox-product-verify" "$prefix/bin/mediabox-product-verify"
install -m 0755 "$here/packaging/mediabox-playback-smoke" "$prefix/bin/mediabox-playback-smoke"
ok "product checks installed"

# The launcher, for the same reason the unit files come from here: it is a
# shell script, and a line of it must not cost a 350 MB re-capture. It is the
# file that decides which sound card the film's audio goes to, and that
# decision was wrong on any board whose HDMI is not ALSA card 0.
install -m 0755 "$here/packaging/mediabox-player" "$prefix/bin/mediabox-player"
ok "player launcher"

# And the preparation step, for the same reason: it is the file that decides,
# per board, which sound card the television's audio goes to and which display
# modes Kodi may switch between. Both are answers about *this* board, and both
# were wrong for a board that was not the one the archive was captured on.
install -m 0755 "$here/packaging/mediabox-hdmi-prepare" "$prefix/bin/mediabox-hdmi-prepare"
ok "display and audio preparation"

# ------------------------------------------- 11b. the video port crossbar
#
# Which HDMI socket a television is plugged into decides what it gets, and it
# should not.
#
# On RK3588 the VOP2's HDR conversion block belongs to Video Port 0: a film
# there can be tone-mapped, and an SDR interface drawn over an HDR film is
# lifted into the signal rather than coming out washed out and displaced. VP1
# and VP2 have none of it. The board's device tree wires both HDMI
# transmitters to all three ports and then switches most of those links off,
# so on an Orange Pi 5 Plus the kernel publishes one socket as VP0-only and the
# other as VP1-only, and no choice in userspace can get past it:
#
#     216 TMDS  possible crtcs 0x1      HDMI-A-1
#     235 TMDS  possible crtcs 0x2      HDMI-A-2
#
# The overlay switches the two crossing links back on. Measured after it, on
# the same board: both encoders report 0x3, and the interface puts whichever
# television is connected on the port with the block behind it. It forces no
# assignment and takes nothing away -- a second display still lights up on VP1.
step "video port crossbar"
overlay="$here/packaging/overlays/mediabox-hdmi-any-vp.dtbo"
env_file=/boot/armbianEnv.txt
if [ ! -f "$env_file" ]; then
  note "no $env_file: this board does not load user overlays"
elif [ ! -f "$overlay" ]; then
  note "the overlay is not in this tree: $overlay"
elif [ ! -d /proc/device-tree/hdmi@fde80000 ] || [ ! -d /proc/device-tree/hdmi@fdea0000 ]; then
  note "one HDMI transmitter on this board's device tree; nothing to cross"
else
  install -d -m 0755 /boot/overlay-user
  if cmp -s "$overlay" /boot/overlay-user/mediabox-hdmi-any-vp.dtbo; then
    ok "overlay present"
  else
    install -m 0644 "$overlay" /boot/overlay-user/mediabox-hdmi-any-vp.dtbo
    ok "overlay installed"
    reboot_needed=1
  fi
  if grep -qE '^user_overlays=.*mediabox-hdmi-any-vp' "$env_file"; then
    ok "armbianEnv.txt loads it"
  else
    # The file decides whether this board boots. It is copied before it is
    # touched, and an existing list is added to rather than replaced.
    cp -a "$env_file" "$env_file.mediabox-$(date +%Y%m%d-%H%M%S)"
    if grep -q '^user_overlays=' "$env_file"; then
      sed -i 's/^user_overlays=\(.*\)$/user_overlays=\1 mediabox-hdmi-any-vp/' "$env_file"
    else
      printf 'user_overlays=mediabox-hdmi-any-vp\n' >>"$env_file"
    fi
    ok "armbianEnv.txt updated, previous copy kept beside it"
    reboot_needed=1
  fi
fi

# ------------------------------------------------- 11c. the fan's overlays
#
# The kernel's pwm-fan drives the fan from a curve in the device tree, and the
# settings screen can choose that curve for the next boot. The setup script
# makes the boot loader look for the curve's overlay and, on an Orange Pi 5
# Plus only, installs the board's 50 Hz carrier correction: on its own 20 kHz
# carrier the Plus's fan does not start at the lowest cooling level. It keeps
# every other user overlay, the crossbar above included, and changes nothing
# on a second run.
step "fan"
install -D -m 0644 "$here/packaging/overlays/mediabox-fan-opi5plus-50hz.dtbo" \
  "$prefix/share/overlays/mediabox-fan-opi5plus-50hz.dtbo"
install -m 0755 "$here/packaging/mediabox-fan-setup" "$prefix/bin/mediabox-fan-setup"
fan_out="$("$prefix/bin/mediabox-fan-setup")"
printf '%s\n' "$fan_out" | sed 's/^fan-setup: /  --    /'
case "$fan_out" in *"reboot needed"*) reboot_needed=1 ;; esac

# Kodi's settings for this appliance, which kodi.service seeds on a first run.
install -D -m 0644 "$here/config/kodi/guisettings-appliance.xml" \
  "$prefix/share/kodi/guisettings-appliance.xml"
ok "Kodi appliance profile"

# ------------------------------------ 12-14. reload, udev, platform discovery
step "activating"
systemctl daemon-reload
ok "systemd reloaded"
udevadm control --reload-rules
udevadm trigger --subsystem-match=input --action=change
udevadm trigger --subsystem-match=drm --action=change
ok "udev rules reloaded"

# The third way to reboot a television by leaning on it.
#
# The udev rule in the archive takes the power-switch tag off the CEC remote and
# the logind drop-in beside it ignores the power and reboot keys, and both are
# files, so a clean install gets them. ctrl-alt-del.target is neither: it is an
# alias of reboot.target that init reaches on a SIGINT from a console keyboard,
# and masking it is a state change rather than a file to unpack. The deploy
# script did it and the installer did not, so every board installed from a
# release kept that path open and the kiosk smoke said so:
#
#   FAIL  ctrl-alt-del  alias
#
# Restarting stays possible; it is a request to mediaboxd-rs, confirmed twice.
systemctl mask ctrl-alt-del.target >/dev/null 2>&1 || true
[ "$(systemctl is-enabled ctrl-alt-del.target 2>&1)" = masked ] \
  && ok "ctrl-alt-del masked" || note "ctrl-alt-del.target could not be masked"

#
# On a board with nothing plugged in, `inspect` prints the whole topology --
# the KMS node, every transmitter, every output with its ALSA card and its CEC
# node -- and then exits 1, because it could not select an output. That is the
# empty socket again, and reading it as "the appliance does not fit this
# kernel" was the same misdiagnosis one gate further down.
if inspect_out="$("$prefix/bin/mediabox-platform" inspect 2>&1)"; then
  ok "platform discovery"
elif [ "$headless" -eq 1 ] &&
     printf '%s\n' "$inspect_out" | grep -q '^Transmitters' &&
     printf '%s\n' "$inspect_out" | grep -q 'no connected output'; then
  ok "platform discovery (the board is described; no output is selected, nothing is plugged in)"
else
  printf '%s\n' "$inspect_out" | sed 's/^/     /' >&2
  die "KERNEL_CAPABILITY_FAIL: mediabox-platform cannot describe this board.
     The prebuilt appliance does not fit this kernel. Nothing will be rebuilt."
fi

# --------------------------------------------- 15-16. HDMI and ALSA preparation
if [ -x "$prefix/bin/mediabox-hdmi-prepare" ]; then
  if "$prefix/bin/mediabox-hdmi-prepare"; then ok "HDMI/ALSA prepared"
  else note "HDMI/ALSA preparation reported a problem; the verifier will judge it"; fi
fi

# Which video port the television landed on.
#
# A board with two HDMI sockets has two different pictures behind them. On
# RK3588 the VOP2's HDR conversion block belongs to Video Port 0: a film on
# VP0 can be tone-mapped -- measured on the Ultra, `hdr2sdr[1]` with the
# connector in SDR -- and SDR content can be lifted for an HDR panel. VP1 and
# VP2 have neither. HDR10 passthrough works on all of them, so nothing here
# fails; what changes is that on VP1 an OSD drawn over HDR content is the
# defect this project has already chased once.
#
# The installer cannot move a cable, and this is not a reason to refuse a
# board. It is a reason not to let someone discover it on the screen three
# days later, so the port is measured and named.
if [ "$headless" -eq 0 ]; then
  picture_connector="$("$prefix/bin/mediabox-platform" output 2>/dev/null || true)"
  vp=""
  for summary in /sys/kernel/debug/dri/*/summary; do
    [ -r "$summary" ] || continue
    vp="$(awk -v want="${picture_connector#card*-}" '
      /^Video Port[0-9]+:/ { port = $2; sub(/^Port/, "", port); sub(/:$/, "", port) }
      /Connector:/ { c = $0; sub(/.*Connector:/, "", c); sub(/[ \t].*/, "", c)
                     if (c == want) { print port; exit } }' "$summary")"
    [ -n "$vp" ] && break
  done
  case "$vp" in
    "")  note "the video port behind ${picture_connector:-the display} could not be read" ;;
    0)   ok "video port VP0 (the port with the VOP2 HDR conversion block)" ;;
    *)   note "the television is on VP$vp, which has no VOP2 HDR conversion block.
        HDR10 reaches the panel, but tone-mapping and SDR->HDR do not, and an
        OSD over HDR content is where that shows. If this board has another
        HDMI socket, that is the one to use." ;;
  esac
fi

# ------------------------------------------------- 17. services
#
# The same set the golden board runs, with one deliberate difference: the
# interface unit is enabled here. On the reference board it was started by hand
# and left disabled, which is fine for a board someone is sitting at and wrong
# for a clean install, where a television that stays black is the whole product
# missing.

step "services"
systemctl enable --now mediabox-console-off.service >/dev/null 2>&1 || true
# The boot record the hotplug helper compares against; see the unit.
systemctl enable mediabox-display-seed.service >/dev/null 2>&1 || true
# The display's hardware state, which the control plane reads and takes from
# nothing else. Started now: it touches nothing on the television. An archive
# from before the observer has no binary for it, and that is said, not hidden.
if [ -x "$prefix/bin/mediabox-display-observer" ]; then
  systemctl daemon-reload
  systemctl enable --now mediabox-display-observer.service >/dev/null
  ok "mediabox-display-observer.service"
else
  note "this archive predates the display observer; it is not installed"
fi

# The radios come up in an order this hardware survives, and the product
# decides that order rather than systemd restoring whatever state a radio was
# last left in.
#
# On the Plus, Wi-Fi and Bluetooth are two halves of one RTL8852BE reached by
# two buses. With systemd-rfkill restoring "Bluetooth on" early, the Bluetooth
# firmware load held the chip and the Wi-Fi driver could not read its efuse:
#
#   rtw89_8852be: failed to dump efuse physical map
#   rtw89_8852be: probe of 0002:21:00.0 failed with error -16
#
# The board then had no wireless interface at all. So systemd-rfkill is masked
# and mediabox-wireless.service unblocks Wi-Fi first, waits for the interface,
# and only then unblocks Bluetooth. See packaging/mediabox-wireless-up.
systemctl mask systemd-rfkill.service systemd-rfkill.socket >/dev/null 2>&1 || true
if [ -e /etc/systemd/system/mediabox-wireless.service ]; then
  systemctl enable mediabox-wireless.service >/dev/null 2>&1 || true
  systemctl start mediabox-wireless.service >/dev/null 2>&1 || true
  ok "mediabox-wireless.service"
fi
# Pairing needs something on this side that answers. Without a registered
# agent bluez refuses every request -- "No agent available for request type 2"
# -- and a keyboard cannot be paired at all.
if [ -e /etc/systemd/system/mediabox-bt-agent.service ]; then
  systemctl enable mediabox-bt-agent.service >/dev/null 2>&1 || true
  systemctl start mediabox-bt-agent.service >/dev/null 2>&1 || true
  ok "mediabox-bt-agent.service"
fi
for u in stremio-server.service mediabox-media-worker.service mediaboxd-rs.service; do
  systemctl enable "$u" >/dev/null
  systemctl restart "$u"
  ok "$u"
done
systemctl enable mediabox-tv-ui.service >/dev/null
if [ "$headless" -eq 1 ]; then
  # Enabled, so the next boot with a television attached brings it up; not
  # started now, because it opens a connector and there is none to open.
  note "mediabox-tv-ui.service enabled, not started: it draws on a display"
else
  systemctl restart mediabox-tv-ui.service
  ok "mediabox-tv-ui.service"
fi

# ------------------------------------------------- 18. the product verifier
step "product verifier"
MEDIABOX_VERIFY_NO_DISPLAY="$headless" "$verifier" \
  || die "the installed product did not verify"

# ------------------------------------------- 19. and it has to play a film
#
# A verifier that only reads files signed off on a clean board whose own player
# could not start anything. So the last gate is a film: the daemon is asked to
# play a clip that ships with the product, and the decoder, the buffers and the
# display plane are read back off the running player. Fifteen seconds, no
# network, no account, and no way to reach the final PASS without it.
step "own-player playback"
if [ "$headless" -eq 1 ]; then
  note "skipped: a film needs a television and none is attached"

  # Not PASS. Everything this board can be asked without a display is
  # installed and verified, and the one gate that proves the product -- a film
  # on the screen -- has not run. Saying PASS here would be the same lie the
  # film gate was added to stop.
  step "INSTALLED, NOT YET PROVEN"
  cat <<EOF
  MediaBox ${MEDIABOX_RELEASE_TAG} is installed at $prefix
  from $MEDIABOX_RELEASE_ASSET
  nothing was compiled on this board
  no display was attached, so the display checks and the film did not run.
  Plug the television in and finish with:

      systemctl start mediabox-tv-ui.service
      $prefix/bin/mediabox-product-verify
      $prefix/bin/mediabox-playback-smoke
EOF
  [ "$reboot_needed" -eq 1 ] && \
    note "the video port overlay is loaded by U-Boot and needs a restart"
else
  "$here/packaging/mediabox-playback-smoke" \
    || die "the appliance's own player did not play.
     Everything is installed, but the default player is not operational --
     which is the state this installer used to call PASS."

  step "PASS"
  cat <<EOF
  MediaBox ${MEDIABOX_RELEASE_TAG} is installed at $prefix
  from $MEDIABOX_RELEASE_ASSET
  the default player is operational: it decoded a film with rkmpp and put it
  on the display's own video plane
  nothing was compiled on this board
EOF
  [ "$reboot_needed" -eq 1 ] && cat <<'EOF'

  One thing is waiting for a restart: the video port overlay is loaded by
  U-Boot, so until this board is rebooted each HDMI socket still reaches only
  the port its device tree pinned it to.
EOF
fi
