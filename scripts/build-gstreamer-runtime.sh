#!/usr/bin/env bash
#
# GStreamer 1.26 for live television: one consistent set, at its own prefix.
#
# Debian trixie ships GStreamer 1.26.2. Its aacparse loses the LATM frames
# between a channel's first StreamMuxConfig and its next one
# (gstaacparse.c, gst_aac_parse_set_src_caps resets the parsed config).
# DVB sends that config only now and then (ETSI TS 101 154 V2.10.1
# 6.5.1.1), so up to one repetition period of sound is lost at every start of
# a LATM channel. The fix is upstream commit a34d9006f5 on the 1.26 branch,
# first released in 1.26.8. Debian has no 1.26 build with it: trixie stays at
# 1.26.2, and testing and sid are at 1.28.
#
# So the whole set mediabox-live runs on is built here from the release
# tarballs of one version: core, -base, -good, -bad and gst-libav. It is
# never one library swapped into Debian's, and never mixed with Debian's
# GStreamer at run time:
#   - everything is under $PREFIX with a RUNPATH to $PREFIX/lib;
#   - the plugin path and the plugin scanner compiled into libgstreamer are
#     $PREFIX's;
#   - only the plugins mediabox-live uses are built: tsdemux, the video and
#     audio parsers, mpg123, libav, alsasink, appsrc/appsink, input-selector,
#     queue, audioconvert, audioresample, volume and typefind.
# The libraries GStreamer links against (glib, orc, ALSA, mpg123, Debian's
# FFmpeg) stay trixie's, pinned by a snapshot.debian.org date: the same
# sonames the appliance has.
#
# The cross build runs in a Debian trixie container pinned by digest, on any
# x86-64 host with docker or podman. The result is a reproducible tarball
# (sorted, fixed mtime, numeric owner 0).
#
# Usage: scripts/build-gstreamer-runtime.sh [--sysroot]
#   --sysroot  also unpack it into the cross sysroot at $PREFIX: what
#              mediabox-live links against, and what the ARM64 tests run on
#              (scripts/test-mediabox-live-arm64.sh)
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

GST_VERSION="1.26.11"
PREFIX="/opt/rk3588-mediabox/gstreamer-1.26"
# Release tarballs, https://gstreamer.freedesktop.org/src/<module>/, with the
# sha256 sums published beside them.
declare -A GST_SHA256=(
  [gstreamer]=2e0bd192d0438ea606a6f76a95c8e16542167656ffec2c2bc3aaf6ee0837fbf6
  [gst-plugins-base]=fc50f885d41f5d0407ce0876ec7235d9e7b82d48db2f4bc72c5f244a4ac79263
  [gst-plugins-good]=001deb0876d5d743cd3448abf74a27adec3fd850012fcb1b00994861bd6c1145
  [gst-plugins-bad]=110fb82795f0e569b1e27b12ab9699d35c7762e1ff4db95335d6ac8d1442af3d
  [gst-libav]=9bb3d2681ef0de92d1b1a9d955186236ee22e64f372db9bfc0d22e2d0df19865
)
MODULES=(gstreamer gst-plugins-base gst-plugins-good gst-plugins-bad gst-libav)
# debian:trixie as pulled on 2026-11-02.
IMAGE="debian@sha256:2dc419c6d7fe3dad0a2f9fd469c34cc90b0f9ba642d8fde837fe3d7d34a0e7f1"
SNAPSHOT="20261101T000000Z"
# The build's own clock: every timestamp it writes.
SOURCE_DATE_EPOCH=1761955200 # 2025-11-01, after the 1.26.8 fix

out="$here/build/gstreamer-runtime"
src="$out/src"
tarball="$out/gstreamer-$GST_VERSION-aarch64.tar.xz"
mkdir -p "$src"

engine="$(command -v docker || command -v podman || true)"
[ -n "$engine" ] || { echo "docker or podman is needed" >&2; exit 1; }

for m in "${MODULES[@]}"; do
  f="$src/$m-$GST_VERSION.tar.xz"
  [ -e "$f" ] || curl -fsSL --retry 3 -o "$f" "https://gstreamer.freedesktop.org/src/$m/$m-$GST_VERSION.tar.xz"
  echo "${GST_SHA256[$m]}  $f" | sha256sum -c --quiet - || { echo "checksum mismatch: $f" >&2; exit 1; }
done

"$engine" run --rm --platform linux/amd64 \
  -e GST_VERSION="$GST_VERSION" -e PREFIX="$PREFIX" -e SNAPSHOT="$SNAPSHOT" \
  -e SOURCE_DATE_EPOCH="$SOURCE_DATE_EPOCH" -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" \
  -v "$src:/src:ro" -v "$out:/out" "$IMAGE" bash -euo pipefail -c '
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
# The packages of the image to the snapshot first: arm64 and amd64 copies of
# one library must be the same version (Multi-Arch: same).
DEBIAN_FRONTEND=noninteractive apt-get -qq -o Acquire::Retries=5 -y --allow-downgrades dist-upgrade >/dev/null
DEBIAN_FRONTEND=noninteractive apt-get -qq -o Acquire::Retries=5 install -y --no-install-recommends \
  crossbuild-essential-arm64 meson ninja-build pkg-config flex bison python3 xz-utils file \
  libglib2.0-dev-bin libglib2.0-dev:arm64 liborc-0.4-dev:arm64 liborc-0.4-dev-bin \
  libasound2-dev:arm64 libmpg123-dev:arm64 zlib1g-dev:arm64 \
  libavcodec-dev:arm64 libavformat-dev:arm64 libavfilter-dev:arm64 libavutil-dev:arm64 \
  libswresample-dev:arm64 >/dev/null
cat > /cross.ini <<CROSS
[binaries]
c = '"'"'aarch64-linux-gnu-gcc'"'"'
cpp = '"'"'aarch64-linux-gnu-g++'"'"'
ar = '"'"'aarch64-linux-gnu-ar'"'"'
strip = '"'"'aarch64-linux-gnu-strip'"'"'
pkg-config = '"'"'pkg-config'"'"'
[properties]
pkg_config_libdir = ['"'"'$PREFIX/lib/pkgconfig'"'"', '"'"'/usr/lib/aarch64-linux-gnu/pkgconfig'"'"', '"'"'/usr/share/pkgconfig'"'"']
[built-in options]
c_link_args = ['"'"'-Wl,-rpath,$PREFIX/lib'"'"']
cpp_link_args = ['"'"'-Wl,-rpath,$PREFIX/lib'"'"']
[host_machine]
system = '"'"'linux'"'"'
cpu_family = '"'"'aarch64'"'"'
cpu = '"'"'cortex-a76'"'"'
endian = '"'"'little'"'"'
CROSS
common="--cross-file /cross.ini --prefix=$PREFIX --libdir=lib --buildtype=release -Dauto_features=disabled -Ddoc=disabled -Dnls=disabled -Dpackage-origin=rk3588-mediabox -Dpackage-name=MediaBox-GStreamer-$GST_VERSION"
declare -A opts=(
  [gstreamer]="-Dtools=enabled -Dtests=disabled -Dexamples=disabled -Dintrospection=disabled -Dregistry=true -Dgst_parse=true -Dcheck=enabled"
  [gst-plugins-base]="-Dtools=enabled -Dtests=disabled -Dexamples=disabled -Dintrospection=disabled -Dorc=enabled -Dapp=enabled -Daudioconvert=enabled -Daudioresample=enabled -Dvolume=enabled -Dtypefind=enabled -Dalsa=enabled -Dpbtypes=enabled"
  [gst-plugins-good]="-Dtests=disabled -Dexamples=disabled -Dorc=enabled -Daudioparsers=enabled -Dmpg123=enabled"
  [gst-plugins-bad]="-Dtools=disabled -Dtests=disabled -Dexamples=disabled -Dintrospection=disabled -Dorc=enabled -Dmpegtsdemux=enabled -Dvideoparsers=enabled"
  [gst-libav]="-Dtests=disabled"
)
for m in gstreamer gst-plugins-base gst-plugins-good gst-plugins-bad gst-libav; do
  tar -C /tmp -xf /src/$m-$GST_VERSION.tar.xz
  case $m in gst-libav) set -- --cross-file /cross.ini --prefix=$PREFIX --libdir=lib --buildtype=release -Ddoc=disabled ;; *) set -- $common ;; esac
  meson setup /tmp/build-$m /tmp/$m-$GST_VERSION "$@" ${opts[$m]} >/tmp/setup-$m.log 2>&1 || { tail -40 /tmp/setup-$m.log; exit 1; }
  ninja -C /tmp/build-$m >/tmp/ninja-$m.log 2>&1 || { tail -60 /tmp/ninja-$m.log; exit 1; }
  ninja -C /tmp/build-$m install >/dev/null
  echo "built $m"
done
# What was built, against which libraries.
{
  echo "GStreamer $GST_VERSION for aarch64, prefix $PREFIX"
  echo "image: debian trixie, apt snapshot $SNAPSHOT"
  echo "linked against (arm64 trixie packages):"
  dpkg-query -W -f="  \${Package} \${Version}\n" libglib2.0-0t64:arm64 liborc-0.4-0t64:arm64 libasound2t64:arm64 libmpg123-0t64:arm64 libavcodec61:arm64 libavformat61:arm64 libavfilter10:arm64 libavutil59:arm64 libswresample5:arm64 zlib1g:arm64
  echo "plugins:"
  ls $PREFIX/lib/gstreamer-1.0/ | sed "s/^/  /"
} > $PREFIX/BUILD-INFO
find $PREFIX -exec touch -h -d @$SOURCE_DATE_EPOCH {} +
tar --sort=name --mtime=@$SOURCE_DATE_EPOCH --owner=0 --group=0 --numeric-owner -C / -cf - ${PREFIX#/} | xz -9 -T1 > /out/gstreamer-$GST_VERSION-aarch64.tar.xz
chown $HOST_UID:$HOST_GID /out/gstreamer-$GST_VERSION-aarch64.tar.xz
cat $PREFIX/BUILD-INFO
'
sha256sum "$tarball" | tee "$tarball.sha256"

if [ "${1:-}" = "--sysroot" ]; then
  sysroot="${MEDIABOX_SYSROOT:-$here/.sysroot/aarch64-trixie}"
  rm -rf "$sysroot${PREFIX:?}"
  tar -C "$sysroot" -xJf "$tarball"
  echo "unpacked into $sysroot$PREFIX"
fi
