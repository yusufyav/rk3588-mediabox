#!/usr/bin/env bash
#
# Build live television's own player, mediabox-live, and what it stands on;
# install it on the appliance only when asked to, with a rollback copy.
#
# Everything is built here, nothing on the appliance:
#
#   1. GStreamer 1.26.11 at its own prefix, /opt/rk3588-mediabox/gstreamer-1.26
#      (scripts/build-gstreamer-runtime.sh). Not Debian trixie's 1.26.2: its
#      aacparse loses LATM frames until a channel's second StreamMuxConfig
#      (fixed in 1.26.8). It links against Debian's glib, orc, ALSA, mpg123
#      and FFmpeg, which the appliance already has; no package is installed.
#   2. Rockchip's GStreamer decoder, mppvideodec (scripts/build-mppvideodec.sh),
#      against that runtime and MediaBox's own MPP, into the runtime's own
#      plugin directory. The media runtime's lib/gstreamer-1.0 plugin, built
#      against Debian's GStreamer, is not touched and no longer loaded by
#      live television.
#   3. mediabox-live itself, cross-compiled against the appliance's libraries
#      (the sysroot build-mediabox-tv.sh keeps) and the runtime, with a
#      RUNPATH to the runtime.
#
# The result is a bundle, build/mediabox-live-bundle: the four files, their
# hashes, and the script that installs them on the appliance.
#
# Usage: build-mediabox-live.sh [--dry-run | --install]
#   (none)     build and bundle; nothing is written to the appliance
#   --dry-run  also read the appliance (ssh, read-only) and print every
#              write --install would make, and the checks it would refuse on
#   --install  copy the bundle to the appliance and install it: refused if
#              the appliance is not in the state the plan was made for, a
#              rollback copy first, every file put in place by rename, the
#              runtime's elements and loader checked before the launcher
#              switches over; rolled back if a check fails
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

mode=build
case "$#:${1:-}" in
  0:) ;;
  1:--dry-run) mode=dry-run ;;
  1:--install) mode=install ;;
  *) echo "usage: build-mediabox-live.sh [--dry-run | --install]" >&2; exit 2 ;;
esac

# The prefix is not a setting here: the runtime's plugin path and scanner,
# mppvideodec's RUNPATH, mediabox-live's RUNPATH and the launcher all carry
# /opt/rk3588-mediabox. Installed anywhere else, the binary would load
# Debian's GStreamer or nothing.
fixed_prefix=/opt/rk3588-mediabox
if [ -n "${MEDIABOX_PREFIX:-}" ] && [ "$MEDIABOX_PREFIX" != "$fixed_prefix" ]; then
  echo "MEDIABOX_PREFIX=$MEDIABOX_PREFIX, but mediabox-live and its runtime are built for $fixed_prefix" >&2
  exit 1
fi
crate="$here/rust/crates/mediabox-live"
triple="aarch64-unknown-linux-gnu"
sysroot="${MEDIABOX_SYSROOT:-$here/.sysroot/aarch64-trixie}"
gst_version=1.26.11
gst_prefix="$fixed_prefix/gstreamer-1.26"
gst_tarball="$here/build/gstreamer-runtime/gstreamer-$gst_version-aarch64.tar.xz"
mpp_plugin="$here/build/gstreamer-rockchip/libgstrockchipmpp.so"
bundle="$here/build/mediabox-live-bundle"

say() { printf '\n== %s\n' "$*"; }

[ -e "$sysroot/usr/lib/aarch64-linux-gnu/libc.so.6" ] || {
  echo "no sysroot at $sysroot; run scripts/build-mediabox-tv.sh once first" >&2
  exit 1
}

say "GStreamer runtime (scripts/build-gstreamer-runtime.sh)"
[ -e "$gst_tarball" ] || "$here/scripts/build-gstreamer-runtime.sh"
sha256sum -c "$gst_tarball.sha256"

say "mppvideodec (scripts/build-mppvideodec.sh)"
[ -e "$mpp_plugin" ] || "$here/scripts/build-mppvideodec.sh"
( cd "${mpp_plugin%/*}" && sha256sum -c libgstrockchipmpp.so.sha256 )

say "sysroot <- the same GStreamer runtime, at the same prefix"
if ! cmp -s <(tar -xJOf "$gst_tarball" "${gst_prefix#/}/BUILD-INFO") "$sysroot$gst_prefix/BUILD-INFO" 2>/dev/null; then
  rm -rf "${sysroot:?}$gst_prefix"
  tar -C "$sysroot" -xJf "$gst_tarball"
fi
head -1 "$sysroot$gst_prefix/BUILD-INFO"

say "mediabox-live ($triple, release)"
export PKG_CONFIG_ALLOW_CROSS=1
export PKG_CONFIG_SYSROOT_DIR="$sysroot"
export PKG_CONFIG_LIBDIR="$sysroot$gst_prefix/lib/pkgconfig:$sysroot/usr/lib/aarch64-linux-gnu/pkgconfig:$sysroot/usr/share/pkgconfig"
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
# Its GStreamer found through a RUNPATH, never Debian's in /usr/lib.
# Built from one fixed path (mediabox-live-source.sh), named nowhere in
# the binary (panic locations read ./rust/crates/...): the same commit
# builds the same bytes from any worktree, checked 2026-11-06 with two.
# shellcheck source=mediabox-live-source.sh
source "$here/scripts/mediabox-live-source.sh"
source_crate="$(mediabox_live_source "$here")"
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS="--remap-path-prefix=$MEDIABOX_LIVE_SRC=. -C link-arg=--sysroot=$sysroot -C link-arg=-Wl,-rpath,$gst_prefix/lib -C link-arg=-Wl,-rpath-link=$sysroot$gst_prefix/lib -C link-arg=-Wl,-rpath-link=$sysroot/usr/lib/aarch64-linux-gnu"
( cd "$source_crate" && CARGO_TARGET_DIR="$crate/target" cargo build -q --locked --release --target "$triple" )
bin="$crate/target/$triple/release/mediabox-live"
aarch64-linux-gnu-readelf -d "$bin" | grep -q "RUNPATH.*\[$gst_prefix/lib\]" ||
  { echo "mediabox-live has no RUNPATH to $gst_prefix/lib" >&2; exit 1; }
# The appliance runs glibc 2.41, as for the interface.
max="$(aarch64-linux-gnu-objdump -T "$bin" | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1)"
[ "${max##*.}" -le 41 ] || { echo "binary needs $max but the appliance has GLIBC_2.41" >&2; exit 1; }
printf '%-20s %s (highest %s)\n' mediabox-live "$(du -h "$bin" | cut -f1)" "$max"

say "bundle ($bundle)"
rm -rf "$bundle" && mkdir -p "$bundle"
cp "$gst_tarball" "$bundle/runtime.tar.xz"
cp "$mpp_plugin" "$bundle/libgstrockchipmpp.so"
cp "$bin" "$bundle/mediabox-live"
cp "$here/packaging/mediabox-player" "$bundle/mediabox-player"
# Every regular file of the runtime, by its path under the prefix: checked
# after it is unpacked on the appliance, before anything points at it.
work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT
tar -C "$work" -xJf "$gst_tarball"
( cd "$work$gst_prefix" && find . -type f -print0 | sort -z | xargs -0 sha256sum ) > "$bundle/runtime.files.sha256"
sed -n 1p "$work$gst_prefix/BUILD-INFO" > "$bundle/runtime.build-info"
cp "$here/scripts/mediabox-live-install.sh" "$bundle/install.sh"
( cd "$bundle" && sha256sum runtime.tar.xz libgstrockchipmpp.so mediabox-live mediabox-player \
    runtime.files.sha256 runtime.build-info install.sh > SHA256SUMS )
cat "$bundle/SHA256SUMS"

[ "$mode" = build ] && { echo; echo "built; nothing written to the appliance (--dry-run shows the install plan)"; exit 0; }

# shellcheck source=env.sh
source "$here/scripts/env.sh"
remote_ssh() { timeout "${REMOTE_TIMEOUT:-120}" ssh -F /dev/null -i "$MEDIABOX_SSH_KEY" -o IdentitiesOnly=yes \
  -o BatchMode=yes -o ConnectTimeout=10 "$MEDIABOX_TARGET" "$@"; }

# What the appliance holds now and what the plan was made against: the
# install refuses if they differ (MEDIABOX_EXPECT_LIVE / _PLAYER, sha256).
: "${MEDIABOX_EXPECT_LIVE:=b682d9bdf5bbd76a6f03f5f682320a2037bd1effade24577c38d8bdc04d1476f}"
: "${MEDIABOX_EXPECT_PLAYER:=ab1ddc49afc7346df18899bc9939d82194f8790fd67b113f4f788c16088f074d}"

if [ "$mode" = dry-run ]; then
  say "plan for $MEDIABOX_TARGET (read-only: nothing is written)"
  remote_ssh "sh -s -- --plan $MEDIABOX_EXPECT_LIVE $MEDIABOX_EXPECT_PLAYER" < "$bundle/install.sh" | sed 's/^/  /'
  echo
  echo "copied by --install: $bundle/* -> $MEDIABOX_TARGET:/var/tmp/mediabox-live-bundle/ (removed afterwards)"
  exit 0
fi

say "install on $MEDIABOX_TARGET"
remote_ssh "rm -rf /var/tmp/mediabox-live-bundle && mkdir -m 0700 /var/tmp/mediabox-live-bundle"
REMOTE_TIMEOUT=300 timeout 300 scp -F /dev/null -i "$MEDIABOX_SSH_KEY" -o IdentitiesOnly=yes -o BatchMode=yes \
  -o ConnectTimeout=10 "$bundle"/* "$MEDIABOX_TARGET:/var/tmp/mediabox-live-bundle/"
REMOTE_TIMEOUT=600 remote_ssh "sh /var/tmp/mediabox-live-bundle/install.sh --install $MEDIABOX_EXPECT_LIVE $MEDIABOX_EXPECT_PLAYER; s=\$?; rm -rf /var/tmp/mediabox-live-bundle; exit \$s"
