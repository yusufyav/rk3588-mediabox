#!/usr/bin/env bash
# Builds Kodi for the RK3588 appliance, on the target, reproducibly.
#
# Three decisions are baked in here and each is evidenced in the MP2 report:
#
#   * the Kodi revision is PINNED. Gate MP2 audited 22.0b2-Piers and proved by
#     `git log` that the GBM/DRM/DRMPRIME/HDR paths are identical to master at
#     audit time, so "latest master" would buy nothing and cost reproducibility.
#
#   * the build uses the SYSTEM FFmpeg at /opt/rk3588-screenbridge rather than
#     Kodi's internal one. Kodi 22 bundles FFmpeg 9.0.1, which has no Rockchip
#     MPP decoder; the ScreenBridge build is the exact RKMPP-enabled FFmpeg that
#     Gates MP1a/MP1b proved end to end. libpostproc is absent from it, which is
#     why the FFmpeg source plugins are disabled.
#
#   * patches are applied from patches/kodi/*.patch in order, never committed as
#     a vendored source dump, so what this project changes about Kodi stays
#     readable as a diff against a named upstream revision.
#
# Usage (from a workstation):  scripts/build-kodi.sh
# It syncs nothing; it runs entirely on the target over ssh.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=env.sh
source "$here/scripts/env.sh"

: "${KODI_REPO:=https://github.com/xbmc/xbmc.git}"
: "${KODI_REV:=e513e0ff4331fc25fd2454659a9dd3e6b7670146}"   # 22.0b2-Piers
: "${KODI_TAG:=22.0b2-Piers}"
: "${KODI_SRC:=/var/tmp/kodi-src}"
: "${KODI_PREFIX:=/opt/rk3588-mediabox/kodi}"
: "${KODI_JOBS:=$(nproc 2>/dev/null || echo 4)}"

step="${1:-all}"

remote_patch_dir="$MEDIABOX_REMOTE_DIR/patches/kodi"

case "$step" in
  deps)
    echo "== installing Kodi build dependencies (no upgrade, no recommends)"
    # build-dep on Debian's own kodi source package pulls the right set for
    # this distribution without guessing at package names. The image ships no
    # deb-src URIs, so one is added as its own file rather than by editing the
    # distribution's sources: it is removable with a single rm, and it changes
    # what apt can *fetch*, never what is installed.
    mediabox_ssh "cat > /etc/apt/sources.list.d/debian-src.sources <<'SRC'
Types: deb-src
URIs: http://deb.debian.org/debian
Suites: trixie
Components: main contrib non-free
Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg
SRC
      apt-get update -qq 2>&1 | tail -2"
    mediabox_ssh "DEBIAN_FRONTEND=noninteractive apt-get build-dep -y --no-install-recommends kodi 2>&1 | tail -5"
    mediabox_ssh "DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        git cmake ninja-build build-essential nasm gperf swig default-jre-headless \
        libgbm-dev libegl-dev libgles-dev libdrm-dev libinput-dev libxkbcommon-dev \
        libudev-dev libasound2-dev libpulse-dev 2>&1 | tail -5"
    ;;

  fetch)
    echo "== fetching Kodi $KODI_TAG ($KODI_REV)"
    mediabox_ssh "set -e
      if [ ! -d '$KODI_SRC/.git' ]; then
        git clone --filter=blob:none '$KODI_REPO' '$KODI_SRC'
      fi
      cd '$KODI_SRC'
      git fetch --tags origin
      # Discard any previously applied patch set before checking out, so a
      # rebuild is not an increment on top of the last one.
      git checkout -f '$KODI_REV'
      git clean -fdx -e build -e .git
      git rev-parse HEAD"
    ;;

  patch)
    echo "== applying patches from patches/kodi/"
    mediabox_ssh "mkdir -p '$remote_patch_dir'"
    if compgen -G "$here/patches/kodi/*.patch" > /dev/null; then
      mediabox_rsync "$here/patches/kodi/" "$MEDIABOX_TARGET:$remote_patch_dir/"
      mediabox_ssh "set -e; cd '$KODI_SRC'
        for p in '$remote_patch_dir'/*.patch; do
          echo \"-- \$(basename \$p)\"
          git apply --verbose \"\$p\"
        done
        git diff --stat"
    else
      echo "-- no patches; building vanilla upstream"
    fi
    ;;

  configure)
    echo "== configuring (GBM/GLES, system FFmpeg at $MEDIABOX_FFMPEG_PREFIX)"
    mediabox_ssh "set -e; cd '$KODI_SRC'
      PKG_CONFIG_PATH='$MEDIABOX_FFMPEG_PREFIX/lib/pkgconfig' \
      cmake -S . -B build -G Ninja \
        -DCMAKE_BUILD_TYPE=Release \
        -DCMAKE_INSTALL_PREFIX='$KODI_PREFIX' \
        -DCORE_PLATFORM_NAME=gbm \
        -DAPP_RENDER_SYSTEM=gles \
        -DENABLE_INTERNAL_FFMPEG=OFF \
        -DFFMPEG_PATH='$MEDIABOX_FFMPEG_PREFIX' \
        -DDISABLE_FFMPEG_SOURCE_PLUGINS=ON \
        -DENABLE_ALSA=ON \
        -DENABLE_PULSEAUDIO=OFF \
        -DENABLE_VAAPI=OFF \
        -DENABLE_VDPAU=OFF \
        -DENABLE_CEC=OFF \
        -DENABLE_BLURAY=OFF \
        -DENABLE_OPTICAL=OFF \
        -DENABLE_TESTING=OFF \
        -DENABLE_INTERNAL_EXIV2=ON"
    ;;

  build)
    echo "== building with $KODI_JOBS jobs"
    mediabox_ssh "cd '$KODI_SRC' && cmake --build build -j$KODI_JOBS"
    ;;

  install)
    echo "== installing to $KODI_PREFIX"
    mediabox_ssh "cd '$KODI_SRC' && cmake --install build && ls -l '$KODI_PREFIX/lib/kodi/' | head"
    ;;

  all)
    for s in deps fetch patch configure build install; do
      "$0" "$s"
    done
    ;;

  *)
    echo "usage: $0 [deps|fetch|patch|configure|build|install|all]" >&2
    exit 2
    ;;
esac
