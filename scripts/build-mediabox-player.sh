#!/usr/bin/env bash
#
# Build MediaBox's own player, on the appliance.
#
# This is the one thing here that is not cross-compiled. The player has to be
# linked against MediaBox's own hardware media runtime — the Rockchip MPP, RGA
# and FFmpeg that scripts/build-media-runtime.sh puts at
# $MEDIABOX_MEDIA_PREFIX — and that prefix is written into its own pkg-config
# files as an absolute path. Reproducing that on a developer's machine means
# reproducing the appliance; building here takes eight native cores and about
# ten minutes.
#
# It used to link the *other* product's prefix, /opt/rk3588-screenbridge,
# because the board this was written on had only one of the two products on it.
# It does not any more: the player finds its decoder through an RPATH naming
# MediaBox's own prefix, so it needs no LD_LIBRARY_PATH and cannot be pointed at
# somebody else's build by an environment that drifted.
#
# Two patches are applied, both small, both explained in their own headers:
# without them mpv decodes this board's films in software at a third of the
# machine instead of one per cent of it.
#
# One video output is added, as a whole file rather than as a diff because it
# is one: packaging/mpv/vo_mediabox.c, which hands a decoded frame's dma-buf
# descriptors to the interface instead of drawing anywhere. The interface holds
# DRM master on this appliance and there is no compositor under it, so that is
# the only way a film reaches the panel without taking the television away from
# the interface. The third patch is the two lines that put the file in the
# build.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=env.sh
source "$here/scripts/env.sh"
version="${MEDIABOX_MPV_VERSION:-v0.41.0}"
player_prefix="$MEDIABOX_PREFIX/player"

sh_() { mediabox_ssh "$@"; }
cp_() { mediabox_scp "$@"; }

media_prefix="$MEDIABOX_MEDIA_PREFIX"
sh_ "test -e '$media_prefix/lib/pkgconfig/libavcodec.pc'" || {
  echo "no MediaBox media runtime at $media_prefix" >&2
  echo "  run scripts/build-media-runtime.sh first" >&2
  exit 1
}

echo "== patches -> appliance"
sh_ "mkdir -p /var/tmp/mpv-build/patches /var/tmp/mpv-build/sources"
cp_ "$here"/packaging/mpv-patches/*.patch "$MEDIABOX_TARGET:/var/tmp/mpv-build/patches/"
cp_ "$here"/packaging/mpv/*.c "$MEDIABOX_TARGET:/var/tmp/mpv-build/sources/"

echo "== building mpv $version on the appliance"
sh_ "bash -s" <<REMOTE
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq --no-install-recommends \
  build-essential meson ninja-build pkg-config python3 ca-certificates wget patch \
  libplacebo-dev libdrm-dev libgbm-dev libegl-dev libgl-dev \
  libdisplay-info-dev \
  libass-dev libasound2-dev libpipewire-0.3-dev \
  libarchive-dev libjpeg-dev liblcms2-dev libuchardet-dev libzimg-dev \
  libbluray-dev libdvdnav-dev libcdio-paranoia-dev liblua5.2-dev libzstd-dev \
  libva-dev libvdpau-dev libsrt-openssl-dev libxcb-shm0-dev libsndio-dev \
  libxv-dev libxext-dev
# The last two lines are not mpv's dependencies. They are what MediaBox's own
# FFmpeg was configured with, and pkg-config hands them to whoever links
# against it: `Libs.private` in libavcodec.pc names -lva, -lvdpau, -lsrt and
# the rest, and the linker wants every one of them present even though this
# player uses none of them. On the board this was first built on they happened
# to be installed. On a clean image they are not, and the build stopped with
#
#     /usr/bin/ld: cannot find -lva: No such file or directory
#
# on both boards, which is the whole point of naming them here rather than
# leaving them to whatever a machine happens to carry.

cd /var/tmp/mpv-build
rm -rf mpv
wget -q -O mpv.tar.gz "https://github.com/mpv-player/mpv/archive/refs/tags/$version.tar.gz"
mkdir mpv && tar -C mpv --strip-components=1 -xzf mpv.tar.gz
cd mpv
# Our own sources first: the registration patch below names one of them, and a
# build that applied the patch without the file would fail at link time with a
# missing symbol rather than here with a missing file.
cp /var/tmp/mpv-build/sources/vo_mediabox.c video/out/vo_mediabox.c

# -p1, and the patches name a/<path> and b/<path> like every other diff in
# this repository. They used to name an absolute path in the machine they were
# generated on — /var/tmp/<file>.orig — which GNU patch refuses as a dangerous
# file name and then cannot resolve, so the series silently stopped applying.
for p in /var/tmp/mpv-build/patches/*.patch; do
  echo "  applying \$(basename \$p)"
  patch -p1 --forward --no-backup-if-mismatch < "\$p"
done

# MediaBox's own media runtime, not Debian's ffmpeg: only this one has the
# Rockchip decoder. The rpath makes the result say so about itself, so nothing
# downstream needs an environment variable to find it again.
export PKG_CONFIG_PATH=$media_prefix/lib/pkgconfig
export LDFLAGS="-Wl,-rpath,$media_prefix/lib \${LDFLAGS:-}"
echo "  linking against libavcodec \$(pkg-config --modversion libavcodec)"

# Wayland is off, and this is the one build option here with a story.
#
# This player was a Wayland client once: the interface was a Chromium kiosk
# inside sway and mpv drew into that compositor with --vo=dmabuf-wayland. There
# is no compositor on the appliance any anymore — the interface is one process
# that holds DRM master itself — and the replacement is --vo=mediabox, which
# hands the decoded frame's dma-buf descriptors to that process over a unix
# socket. See packaging/mpv/vo_mediabox.c and the header of
# packaging/mpv-patches/0003, which records what the Wayland output did on a box
# with no compositor: exit 2, every time.
#
# So the Wayland outputs cannot be reached, and building them pulls
# libwayland-client, libwayland-cursor, libwayland-egl, wayland-protocols and
# libxkbcommon into a player that cannot use any of them.
meson setup build \
  --prefix=$player_prefix \
  -Dlibmpv=true -Dcplayer=true \
  -Ddrm=enabled -Dgbm=enabled -Degl-drm=enabled \
  -Dwayland=disabled -Dx11=disabled \
  -Dsdl2-audio=disabled -Dsdl2-video=disabled \
  -Dalsa=enabled -Dpipewire=enabled \
  -Dlibarchive=enabled -Dlua=enabled \
  -Dvulkan=disabled -Dshaderc=disabled
ninja -C build -j"\$(nproc)"
ninja -C build install
REMOTE

echo "== proof: the hardware decoder is reachable, with no library path set"
sh_ "$player_prefix/bin/mpv --hwdec=help | grep -c rkmpp"

echo "== proof: the player resolves its media runtime from MediaBox's own prefix"
sh_ "set -e
  echo '-- RUNPATH'
  readelf -d '$player_prefix/bin/mpv' | sed -n 's/.*R\(UN\)\?PATH.*\[\(.*\)\]/   \\2/p'
  echo '-- resolved Rockchip libraries'
  ldd '$player_prefix/bin/mpv' | grep -E 'librga|librockchip'
  if ldd '$player_prefix/bin/mpv' | grep -q 'rk3588-screenbridge'; then
    echo 'the player still resolves the ScreenBridge prefix' >&2
    exit 1
  fi
  echo '-- the Wayland outputs are gone'
  if '$player_prefix/bin/mpv' --vo=help | grep -qE 'dmabuf-wayland|^ *wlshm'; then
    echo 'a Wayland video output is still built' >&2
    exit 1
  fi
  '$player_prefix/bin/mpv' --vo=help | grep -E 'mediabox|drm' || true"
