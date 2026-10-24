#!/usr/bin/env bash
#
# Build live television's own player, mediabox-live, and what it stands on.
#
# Three parts, each where it has to be built:
#
#   1. GStreamer from Debian, on the appliance: the core, tsdemux and the video
#      parsers (-bad), the audio parsers (-good), mpg123 and libav for MPEG-1
#      Layer II, AC-3 and AAC, and alsasink. Runtime packages, plus the -dev
#      files the next two steps link against.
#   2. Rockchip's GStreamer decoder, mppvideodec (gstreamer-rockchip), on the
#      appliance, against MediaBox's own MPP in the media runtime -- the same
#      library mpv and Kodi decode with, found through an RPATH as the player's
#      is (see build-mediabox-player.sh). Debian has neither MPP nor this.
#   3. mediabox-live itself, cross-compiled here against the appliance's
#      libraries (the sysroot build-mediabox-tv.sh keeps), as the interface is.
#
# Usage: build-mediabox-live.sh [--install]
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=env.sh
source "$here/scripts/env.sh"
crate="$here/rust/crates/mediabox-live"
triple="aarch64-unknown-linux-gnu"
sysroot="${MEDIABOX_SYSROOT:-$here/.sysroot/aarch64-trixie}"
media_prefix="$MEDIABOX_MEDIA_PREFIX"
# gstreamer-rockchip, branch gstreamer-rockchip of JeffyCN/mirrors, as fetched
# on 2026-10-24.
gst_rockchip_url="https://github.com/JeffyCN/mirrors.git"
gst_rockchip_rev="ebc758a13d425d34858a98dbcdb4a4bd702daa0f"

say() { printf '\n== %s\n' "$*"; }

[ -e "$sysroot/usr/lib/aarch64-linux-gnu/libc.so.6" ] || {
  echo "no sysroot at $sysroot; run scripts/build-mediabox-tv.sh once first" >&2
  exit 1
}
mediabox_ssh "test -e '$media_prefix/lib/pkgconfig/rockchip_mpp.pc'" || {
  echo "no MediaBox media runtime at $media_prefix; run scripts/build-media-runtime.sh first" >&2
  exit 1
}

say "GStreamer packages and mppvideodec on $MEDIABOX_TARGET"
mediabox_ssh "bash -s" <<REMOTE
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq --no-install-recommends \
  gstreamer1.0-tools gstreamer1.0-plugins-base gstreamer1.0-plugins-good \
  gstreamer1.0-plugins-bad gstreamer1.0-alsa gstreamer1.0-libav \
  libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
  meson ninja-build pkg-config git ca-certificates >/dev/null

src=/var/tmp/gstreamer-rockchip
if [ ! -d "\$src/.git" ]; then
  rm -rf "\$src"
  git clone -q --filter=blob:none -b gstreamer-rockchip "$gst_rockchip_url" "\$src"
fi
git -C "\$src" fetch -q origin gstreamer-rockchip || true
git -C "\$src" checkout -q "$gst_rockchip_rev"
rm -rf "\$src/build"
PKG_CONFIG_PATH="$media_prefix/lib/pkgconfig" \
  meson setup "\$src/build" "\$src" \
    --prefix="$media_prefix" --libdir=lib --buildtype=release \
    -Drockchipmpp=enabled -Drga=disabled -Dgpu=disabled -Drkximage=disabled \\
    -Dkmssrc=disabled -Dvpxalphadec=disabled \\
    -Dc_link_args="-Wl,-rpath,$media_prefix/lib" >/dev/null
ninja -C "\$src/build" >/dev/null
ninja -C "\$src/build" install >/dev/null
GST_PLUGIN_PATH="$media_prefix/lib/gstreamer-1.0" gst-inspect-1.0 mppvideodec | sed -n '1,12p'
for element in tsdemux h264parse h265parse mpegvideoparse mpegaudioparse aacparse ac3parse \
               mpg123audiodec avdec_aac avdec_ac3 avdec_eac3 alsasink appsrc appsink input-selector volume; do
  gst-inspect-1.0 "\$element" >/dev/null 2>&1 || { echo "missing GStreamer element: \$element" >&2; exit 1; }
done
echo "elements present"
REMOTE

say "sysroot <- the appliance's GStreamer"
ssh_cmd="ssh -F /dev/null -i $MEDIABOX_SSH_KEY -o IdentitiesOnly=yes -o BatchMode=yes"
rsync -a -e "$ssh_cmd" \
  --include='libgst*' --include='liborc*' --include='pkgconfig/' --include='pkgconfig/gstreamer*' \
  --include='pkgconfig/orc*' --exclude='*' \
  "$MEDIABOX_TARGET:/usr/lib/aarch64-linux-gnu/" "$sysroot/usr/lib/aarch64-linux-gnu/"
rsync -a -e "$ssh_cmd" \
  "$MEDIABOX_TARGET:/usr/include/gstreamer-1.0" "$sysroot/usr/include/"

say "mediabox-live ($triple, release)"
export PKG_CONFIG_ALLOW_CROSS=1
export PKG_CONFIG_SYSROOT_DIR="$sysroot"
export PKG_CONFIG_LIBDIR="$sysroot/usr/lib/aarch64-linux-gnu/pkgconfig:$sysroot/usr/share/pkgconfig"
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS="-C link-arg=--sysroot=$sysroot -C link-arg=-Wl,-rpath-link=$sysroot/usr/lib/aarch64-linux-gnu"
( cd "$crate" && cargo build --release --target "$triple" )
bin="$crate/target/$triple/release/mediabox-live"

# The appliance runs glibc 2.41, as for the interface.
max="$(aarch64-linux-gnu-objdump -T "$bin" | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1)"
[ "${max##*.}" -le 41 ] || { echo "binary needs $max but the appliance has GLIBC_2.41" >&2; exit 1; }
printf '\n%-20s %s (highest %s)\n' mediabox-live "$(du -h "$bin" | cut -f1)" "$max"

if [ "${1:-}" = --install ]; then
  say "install -> $MEDIABOX_TARGET:$MEDIABOX_PREFIX"
  mediabox_scp "$bin" "$here/packaging/mediabox-player" "$MEDIABOX_TARGET:/var/tmp/"
  mediabox_ssh "install -m 0755 /var/tmp/mediabox-live $MEDIABOX_PREFIX/bin/mediabox-live &&
    install -m 0755 /var/tmp/mediabox-player $MEDIABOX_PREFIX/bin/mediabox-player &&
    rm -f /var/tmp/mediabox-live /var/tmp/mediabox-player"
fi
