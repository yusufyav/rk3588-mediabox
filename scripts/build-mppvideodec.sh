#!/usr/bin/env bash
#
# Rockchip's GStreamer decoder, mppvideodec (gstreamer-rockchip), for
# mediabox-live's own GStreamer 1.26.11 (scripts/build-gstreamer-runtime.sh).
#
# Built here, in the same pinned Debian trixie container as the runtime,
# against the runtime's headers only, and against MediaBox's MPP in the media
# runtime. The plugin goes into the runtime's own plugin directory,
# $PREFIX/lib/gstreamer-1.0, never into the media runtime's
# lib/gstreamer-1.0: the plugin there is the one built against Debian's
# GStreamer, and it stays as it is.
#
# Its RUNPATH is $PREFIX/lib, then the media runtime's lib: GStreamer from the
# runtime, librockchip_mpp from MediaBox's MPP.
#
# MPP itself is built on the appliance (scripts/build-media-runtime.sh). Its
# headers, pkg-config file and library are needed here to link against; they
# are read off the appliance once, never written to it, into
# build/media-runtime-sdk, and checked against the hash pinned below. A
# different MPP on the appliance fails this build rather than linking against
# something unseen.
#
# Usage: scripts/build-mppvideodec.sh
#   MEDIABOX_HOST must name the appliance only when build/media-runtime-sdk
#   is not there yet (a read: tar of the files, over ssh).
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

PREFIX="/opt/rk3588-mediabox/gstreamer-1.26"
MEDIA_PREFIX="/opt/rk3588-mediabox/media-runtime"
# gstreamer-rockchip, branch gstreamer-rockchip of JeffyCN/mirrors, as fetched
# on 2026-10-24; MediaBox's patches in packaging/gstreamer-rockchip.
ROCKCHIP_URL="https://github.com/JeffyCN/mirrors.git"
ROCKCHIP_REV="ebc758a13d425d34858a98dbcdb4a4bd702daa0f"
# MediaBox's MPP 1.3.10: rockchip-linux/mpp 0986d01 with packaging/mpp-patches
# 0001 (no pulldown recovery), 0002 (every deinterlaced field at its own
# time) and 0003 (MPEG-2's first picture timed from its own PTS), as built and
# installed on the appliance by scripts/build-media-runtime.sh on 2026-11-12.
# A rebuild of the same sources is not the same bytes: the pin is that
# artefact, read back off the appliance.
MPP_SHA256="bd86814b3d49f42ead97195097f27da237662075c922a179d420254667f9385b"
# The runtime's own pins.
IMAGE="debian@sha256:2dc419c6d7fe3dad0a2f9fd469c34cc90b0f9ba642d8fde837fe3d7d34a0e7f1"
SNAPSHOT="20261101T000000Z"
SOURCE_DATE_EPOCH=1761955200

out="$here/build/gstreamer-rockchip"
runtime="$here/build/gstreamer-runtime/gstreamer-1.26.11-aarch64.tar.xz"
sdk="$here/build/media-runtime-sdk"
mkdir -p "$out"

engine="$(command -v docker || command -v podman || true)"
[ -n "$engine" ] || { echo "docker or podman is needed" >&2; exit 1; }

[ -e "$runtime" ] || "$here/scripts/build-gstreamer-runtime.sh"
# The .sha256 names the tarball by its bare name: checked where it lies,
# whatever directory this script is run from (as build-mediabox-live.sh).
( cd "${runtime%/*}" && sha256sum -c --quiet "${runtime##*/}.sha256" )

mpp_of() { sha256sum < "$1$MEDIA_PREFIX/lib/librockchip_mpp.so.0" 2>/dev/null | cut -c1-64; }
# The SDK read before is used only while it is the pinned MPP: a cache of
# another one (the appliance's before a rebuild of MPP) would build against
# something no longer there and hide that it changed. It is read again from
# the appliance, once, and that is checked against the pin too.
if [ "$(mpp_of "$sdk")" != "$MPP_SHA256" ]; then
  if [ -e "$sdk$MEDIA_PREFIX/lib/librockchip_mpp.so.0" ]; then
    echo "build/media-runtime-sdk has MPP $(mpp_of "$sdk"), not the pinned $MPP_SHA256: read again from the appliance" >&2
  fi
  # shellcheck source=env.sh
  source "$here/scripts/env.sh"
  rm -rf "$sdk.new" && mkdir -p "$sdk.new"
  m="${MEDIA_PREFIX#/}"
  timeout 120 ssh -F /dev/null -i "$MEDIABOX_SSH_KEY" -o IdentitiesOnly=yes -o BatchMode=yes \
    -o ConnectTimeout=10 "$MEDIABOX_TARGET" \
    "tar -C / -cf - $m/include/rockchip $m/lib/librockchip_mpp.so $m/lib/librockchip_mpp.so.0 $m/lib/librockchip_mpp.so.1 $m/lib/pkgconfig/rockchip_mpp.pc" |
    tar -C "$sdk.new" -xf -
  got="$(mpp_of "$sdk.new")"
  if [ "$got" != "$MPP_SHA256" ]; then
    rm -rf "$sdk.new"
    echo "MPP on the appliance is ${got:-unreadable}, not the one this build is pinned to ($MPP_SHA256)" >&2
    exit 1
  fi
  rm -rf "$sdk" && mv "$sdk.new" "$sdk"
fi

src="$out/src"
if [ ! -d "$src/.git" ]; then
  rm -rf "$src"
  timeout 600 git clone -q --filter=blob:none -b gstreamer-rockchip "$ROCKCHIP_URL" "$src"
fi
git -C "$src" cat-file -e "$ROCKCHIP_REV^{commit}" 2>/dev/null || timeout 300 git -C "$src" fetch -q origin gstreamer-rockchip
tree="$out/gstreamer-rockchip-${ROCKCHIP_REV:0:12}.tar"
git -C "$src" archive --format=tar --prefix=gstreamer-rockchip/ -o "$tree" "$ROCKCHIP_REV"

"$engine" run --rm --platform linux/amd64 \
  -e PREFIX="$PREFIX" -e MEDIA_PREFIX="$MEDIA_PREFIX" -e SNAPSHOT="$SNAPSHOT" \
  -e SOURCE_DATE_EPOCH="$SOURCE_DATE_EPOCH" -e ROCKCHIP_REV="$ROCKCHIP_REV" \
  -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" \
  -v "$runtime:/in/runtime.tar.xz:ro" -v "$sdk:/in/sdk:ro" -v "$tree:/in/src.tar:ro" \
  -v "$here/packaging/gstreamer-rockchip:/in/patches:ro" -v "$out:/out" "$IMAGE" bash -euo pipefail -c '
cat > /etc/apt/sources.list.d/debian.sources <<SOURCES
Types: deb
URIs: http://snapshot.debian.org/archive/debian/$SNAPSHOT
Suites: trixie trixie-updates
Components: main
Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg
Check-Valid-Until: no

Types: deb
URIs: http://snapshot.debian.org/archive/debian-security/$SNAPSHOT
Suites: trixie-security
Components: main
Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg
Check-Valid-Until: no
SOURCES
dpkg --add-architecture arm64
apt-get -qq -o Acquire::Retries=5 update
DEBIAN_FRONTEND=noninteractive apt-get -qq -o Acquire::Retries=5 -y --allow-downgrades dist-upgrade >/dev/null
DEBIAN_FRONTEND=noninteractive apt-get -qq -o Acquire::Retries=5 install -y --no-install-recommends \
  crossbuild-essential-arm64 meson ninja-build pkg-config python3 xz-utils git \
  libglib2.0-dev:arm64 liborc-0.4-dev:arm64 >/dev/null
# The runtime where it lives on the appliance, and MPP where it lives there.
tar -C / -xJf /in/runtime.tar.xz
cp -a /in/sdk/. /
# Debian GStreamer must not be in here at all: nothing to link against by
# mistake.
! ls /usr/lib/aarch64-linux-gnu/libgst* >/dev/null 2>&1 || { echo "Debian GStreamer present in the build container" >&2; exit 1; }
mkdir /build && tar -C /build -xf /in/src.tar
cd /build/gstreamer-rockchip
git init -q && git apply /in/patches/*.patch && rm -rf .git
cat > /cross.ini <<CROSS
[binaries]
c = '"'"'aarch64-linux-gnu-gcc'"'"'
ar = '"'"'aarch64-linux-gnu-ar'"'"'
strip = '"'"'aarch64-linux-gnu-strip'"'"'
pkg-config = '"'"'pkg-config'"'"'
[properties]
pkg_config_libdir = ['"'"'$PREFIX/lib/pkgconfig'"'"', '"'"'$MEDIA_PREFIX/lib/pkgconfig'"'"', '"'"'/usr/lib/aarch64-linux-gnu/pkgconfig'"'"', '"'"'/usr/share/pkgconfig'"'"']
[built-in options]
c_link_args = ['"'"'-Wl,-rpath,$PREFIX/lib:$MEDIA_PREFIX/lib'"'"']
[host_machine]
system = '"'"'linux'"'"'
cpu_family = '"'"'aarch64'"'"'
cpu = '"'"'cortex-a76'"'"'
endian = '"'"'little'"'"'
CROSS
meson setup /build/out /build/gstreamer-rockchip --cross-file /cross.ini \
  --prefix=$PREFIX --libdir=lib --buildtype=release \
  -Drockchipmpp=enabled -Drga=disabled -Dgpu=disabled -Drkximage=disabled \
  -Dkmssrc=disabled -Dvpxalphadec=disabled \
  -Dpackage-origin=rk3588-mediabox -Dpackage-name=MediaBox-gstreamer-rockchip >/tmp/setup.log 2>&1 || { tail -40 /tmp/setup.log; exit 1; }
grep -E "gstreamer-1.0|rockchip_mpp" /build/out/meson-logs/meson-log.txt | grep -E "found: YES" | sort -u
ninja -C /build/out >/tmp/ninja.log 2>&1 || { tail -60 /tmp/ninja.log; exit 1; }
so=/build/out/gst/rockchipmpp/libgstrockchipmpp.so
aarch64-linux-gnu-readelf -d $so | grep -E "NEEDED|RUNPATH"
cp $so /out/libgstrockchipmpp.so
touch -d @$SOURCE_DATE_EPOCH /out/libgstrockchipmpp.so
export PKG_CONFIG_LIBDIR=$PREFIX/lib/pkgconfig:$MEDIA_PREFIX/lib/pkgconfig:/usr/lib/aarch64-linux-gnu/pkgconfig
{
  echo "gstreamer-rockchip $ROCKCHIP_REV + packaging/gstreamer-rockchip patches"
  for p in /in/patches/*.patch; do echo "  $(sha256sum < $p | cut -c1-64)  ${p##*/}"; done
  echo "against: $(head -1 $PREFIX/BUILD-INFO)"
  echo "  pkg-config gstreamer-1.0 $(pkg-config --modversion gstreamer-1.0) from $(pkg-config --variable=prefix gstreamer-1.0)"
  echo "  rockchip_mpp $(pkg-config --modversion rockchip_mpp) from $(pkg-config --variable=prefix rockchip_mpp), librockchip_mpp.so.0 $(sha256sum < $MEDIA_PREFIX/lib/librockchip_mpp.so.0 | cut -c1-64)"
  echo "image: debian trixie, apt snapshot $SNAPSHOT"
  echo "installs at: $PREFIX/lib/gstreamer-1.0/libgstrockchipmpp.so"
  aarch64-linux-gnu-readelf -d $so | grep -E "NEEDED|RUNPATH" | sed "s/^ */  /"
} > /out/BUILD-INFO
chown $HOST_UID:$HOST_GID /out/libgstrockchipmpp.so /out/BUILD-INFO
'
( cd "$out" && sha256sum libgstrockchipmpp.so | tee libgstrockchipmpp.so.sha256 )
cat "$out/BUILD-INFO"
