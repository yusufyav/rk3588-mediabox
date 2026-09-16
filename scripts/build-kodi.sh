#!/usr/bin/env bash
# Builds Kodi for the RK3588 appliance, on the target, reproducibly.
#
# Three decisions are baked in here and each is evidenced in the MP2 report:
#
#   * the Kodi revision is PINNED. Gate MP2 audited 22.0b2-Piers and proved by
#     `git log` that the GBM/DRM/DRMPRIME/HDR paths are identical to master at
#     audit time, so "latest master" would buy nothing and cost reproducibility.
#
#   * the build uses MediaBox's own media runtime rather than Kodi's internal
#     FFmpeg. Kodi 22 bundles FFmpeg 9.0.1, which has no Rockchip MPP decoder;
#     $MEDIABOX_MEDIA_PREFIX holds the RKMPP-enabled FFmpeg that Gates MP1a/MP1b
#     proved end to end, built from the pins in
#     docs/platform/custom-runtime.md. libpostproc is absent from it, which is
#     why the FFmpeg source plugins are disabled.
#
#     That prefix used to be /opt/rk3588-screenbridge — the other product's.
#     Kodi now links MediaBox's own and carries an RPATH naming it, so a board
#     with both products on it cannot have one of them decide what the other
#     decodes with.
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
# `production` or `diagnostic`. The second adds the instrumentation patches on
# top of the first; it is for a measurement run, not for an appliance.
: "${KODI_PATCH_PROFILE:=production}"

case "$KODI_PATCH_PROFILE" in
  production) kodi_patch_dirs="patches/kodi" ;;
  diagnostic) kodi_patch_dirs="patches/kodi patches/kodi-diagnostics" ;;
  *) echo "KODI_PATCH_PROFILE must be 'production' or 'diagnostic'" >&2; exit 2 ;;
esac

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
    # Two profiles, and the default is the product's.
    #
    # patches/kodi/ is what the appliance needs to behave correctly: the
    # colour and EOTF tagging the HDR baseline rests on, the ALSA recovery
    # after a modeset, and the bounded front-buffer queue without which the
    # vendor GBM leaks a GUI-sized buffer per frame and Kodi dies inside a
    # minute. patches/kodi-diagnostics/ is instrumentation — log lines that
    # cost nothing but are only wanted when something is being measured.
    #
    # They used to be one directory, so every production build carried the
    # instrumentation and nobody could say which patch was load-bearing.
    # docs/platform/custom-runtime.md has the table.
    echo "== applying patches (profile: $KODI_PATCH_PROFILE)"
    mediabox_ssh "rm -rf '$remote_patch_dir' && mkdir -p '$remote_patch_dir'"
    # Copied rather than rsynced, because both profiles land in one directory
    # and mediabox_rsync carries --delete: the second sync would take the first
    # profile's patches away again, which is a build that quietly drops the
    # patches the appliance depends on.
    #
    # One directory is also what puts them back in their original order. The
    # numbers are a single series — the instrumentation patch sits between two
    # production ones — and applying them in that order is what the series was
    # generated against.
    for dir in $kodi_patch_dirs; do
      [ -d "$here/$dir" ] || continue
      compgen -G "$here/$dir/*.patch" > /dev/null || continue
      echo "-- from $dir/"
      mediabox_scp "$here/$dir"/*.patch "$MEDIABOX_TARGET:$remote_patch_dir/"
    done
    mediabox_ssh "set -e; cd '$KODI_SRC'
      shopt -s nullglob
      applied=0
      for p in '$remote_patch_dir'/*.patch; do
        echo \"-- \$(basename \$p)\"
        git apply --verbose \"\$p\"
        applied=\$((applied + 1))
      done
      [ \"\$applied\" -gt 0 ] || echo '-- no patches; building vanilla upstream'
      git diff --stat"
    ;;

  configure)
    echo "== configuring (GBM/GLES, MediaBox media runtime at $MEDIABOX_MEDIA_PREFIX)"
    mediabox_ssh "test -e '$MEDIABOX_MEDIA_PREFIX/lib/pkgconfig/libavcodec.pc'" || {
      echo "no MediaBox media runtime at $MEDIABOX_MEDIA_PREFIX" >&2
      echo "  run scripts/build-media-runtime.sh first" >&2
      exit 1
    }
    mediabox_ssh "set -e; cd '$KODI_SRC'
      PKG_CONFIG_PATH='$MEDIABOX_MEDIA_PREFIX/lib/pkgconfig' \
      cmake -S . -B build -G Ninja \
        -DCMAKE_BUILD_TYPE=Release \
        -DCMAKE_INSTALL_PREFIX='$KODI_PREFIX' \
        -DCMAKE_EXE_LINKER_FLAGS='-Wl,-rpath,$MEDIABOX_MEDIA_PREFIX/lib' \
        -DCMAKE_SHARED_LINKER_FLAGS='-Wl,-rpath,$MEDIABOX_MEDIA_PREFIX/lib' \
        -DCORE_PLATFORM_NAME=gbm \
        -DAPP_RENDER_SYSTEM=gles \
        -DENABLE_INTERNAL_FFMPEG=OFF \
        -DFFMPEG_PATH='$MEDIABOX_MEDIA_PREFIX' \
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
    echo "== proof: Kodi resolves its media runtime from MediaBox's own prefix"
    mediabox_ssh "set -e
      readelf -d '$KODI_PREFIX/lib/kodi/kodi-gbm' | sed -n 's/.*R\(UN\)\?PATH.*\[\(.*\)\]/   RUNPATH \\2/p'
      ldd '$KODI_PREFIX/lib/kodi/kodi-gbm' | grep -E 'librga|librockchip' || true
      if ldd '$KODI_PREFIX/lib/kodi/kodi-gbm' | grep -q 'rk3588-screenbridge'; then
        echo 'Kodi still resolves the ScreenBridge prefix' >&2
        exit 1
      fi"
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
