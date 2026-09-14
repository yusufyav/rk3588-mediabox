#!/usr/bin/env bash
#
# Build MediaBox's own player, on the appliance.
#
# This is the one thing here that is not cross-compiled. The player has to be
# linked against the appliance's own ffmpeg — the Rockchip fork that ships with
# the screen bridge, the only build on this box that can drive the hardware
# decoder — and that lives at an absolute path inside its own pkg-config files.
# Reproducing that on a developer's machine means reproducing the appliance;
# building here takes eight native cores and about ten minutes.
#
# Two patches are applied, both small, both explained in their own headers:
# without them mpv decodes this board's films in software at a third of the
# machine instead of one per cent of it.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
host="${MEDIABOX_HOST:-10.27.27.25}"
key="${MEDIABOX_SSH_KEY:-$HOME/.ssh/id_ed25519}"
version="${MEDIABOX_MPV_VERSION:-v0.41.0}"

ssh_opts=(-F /dev/null -i "$key" -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o ConnectTimeout=10)
sh_() { ssh "${ssh_opts[@]}" "root@$host" "$@"; }
cp_() { scp "${ssh_opts[@]}" -q "$@"; }

echo "== patches -> appliance"
sh_ "mkdir -p /var/tmp/mpv-build/patches"
cp_ "$here"/packaging/mpv-patches/*.patch "root@$host:/var/tmp/mpv-build/patches/"

echo "== building mpv $version on the appliance"
sh_ "bash -s" <<REMOTE
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq --no-install-recommends \
  build-essential meson ninja-build pkg-config python3 ca-certificates wget patch \
  libplacebo-dev libdrm-dev libgbm-dev libegl-dev libgl-dev \
  libwayland-dev wayland-protocols libxkbcommon-dev libdisplay-info-dev \
  libass-dev libasound2-dev libpipewire-0.3-dev \
  libarchive-dev libjpeg-dev liblcms2-dev libuchardet-dev libzimg-dev \
  libbluray-dev libdvdnav-dev libcdio-paranoia-dev liblua5.2-dev libzstd-dev

cd /var/tmp/mpv-build
rm -rf mpv
wget -q -O mpv.tar.gz "https://github.com/mpv-player/mpv/archive/refs/tags/$version.tar.gz"
mkdir mpv && tar -C mpv --strip-components=1 -xzf mpv.tar.gz
cd mpv
for p in /var/tmp/mpv-build/patches/*.patch; do
  echo "  applying \$(basename \$p)"
  patch -p0 --forward < "\$p"
done

# The appliance's ffmpeg, not Debian's: only this one has the Rockchip decoder.
export PKG_CONFIG_PATH=/opt/rk3588-screenbridge/lib/pkgconfig
echo "  linking against libavcodec \$(pkg-config --modversion libavcodec)"

meson setup build \
  --prefix=/opt/rk3588-mediabox/player \
  -Dlibmpv=true -Dcplayer=true \
  -Ddrm=enabled -Dgbm=enabled -Dwayland=enabled -Degl-drm=enabled \
  -Dx11=disabled -Dsdl2-audio=disabled -Dsdl2-video=disabled \
  -Dalsa=enabled -Dpipewire=enabled \
  -Dlibarchive=enabled -Dlua=enabled \
  -Dvulkan=disabled -Dshaderc=disabled
ninja -C build -j"\$(nproc)"
ninja -C build install
REMOTE

echo "== proof: the hardware decoder is reachable"
sh_ "LD_LIBRARY_PATH=/opt/rk3588-screenbridge/lib \
     /opt/rk3588-mediabox/player/bin/mpv --hwdec=help | grep -c rkmpp"
