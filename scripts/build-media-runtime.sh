#!/usr/bin/env bash
#
# Build MediaBox's own hardware media runtime, on the appliance.
#
# This is the stack that drives the RK3588 video hardware: Rockchip MPP, the
# RGA user-space library, and the FFmpeg fork that knows about both. Every
# player this product ships links against it — Kodi through pkg-config at build
# time, the embedded mpv the same way — and both find it again at run time
# through an RPATH this script puts in them, not through an environment
# variable somebody has to remember.
#
# It lives at its own prefix, and that is the point of the file.
#
#   /opt/rk3588-screenbridge      belongs to rk3588-screenbridge
#   /opt/rk3588-mediabox/...      belongs to this product
#
# The two used to be the same directory. On a board that has only MediaBox on
# it that is invisible; on a board that has both, whichever product was built
# last owns the other's decoder. So MediaBox builds its own, from its own pins,
# and never writes into the other prefix again. Nothing in this script names
# /opt/rk3588-screenbridge, and tests/run-host-tests.sh checks that it stays
# that way.
#
# The revisions below are not "whatever the branch tip was on the day". Each one
# was read back off the appliance that the display and audio gates were measured
# on — see docs/platform/custom-runtime.md for which artefact carries which
# proof — so this script reproduces that machine rather than approximating it.
#
#   scripts/build-media-runtime.sh            everything
#   scripts/build-media-runtime.sh mpp        one component
#   scripts/build-media-runtime.sh verify     just the checks
#
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=env.sh
source "$here/scripts/env.sh"

# Rockchip's Media Process Platform: the kernel's decoder, from user space.
# 0986d01 is the revision that is in the appliance's own librockchip_mpp.so.0 —
# MPP stamps its build into the library and prints it on every start, so this
# pin is readable from the running machine rather than trusted from a note.
: "${MPP_REPO:=https://github.com/rockchip-linux/mpp.git}"
: "${MPP_REV:=0986d01294d5c2449c14cf13af9b740368c33967}"

# The 2D block. Upstream ships it as a prebuilt object rather than as sources,
# so the pin is a commit whose binary is byte-identical to the appliance's:
# git blob 8e10103, "Update librga version to 1.10.6_[3]".
: "${RGA_REPO:=https://github.com/airockchip/librga.git}"
: "${RGA_REV:=2b32edcb97b601b25683e2941d888c8515da6d55}"
: "${RGA_VERSION:=1.10.6}"

# FFmpeg with the Rockchip decoders, encoders and RGA filters. d90e3a1 is what
# the appliance's ffmpeg reports as its own version string.
: "${FFMPEG_REPO:=https://github.com/nyanmisaka/ffmpeg-rockchip.git}"
: "${FFMPEG_REV:=d90e3a1c18d7929383cf88c1b3da2e2d1c966cbf}"

: "${MEDIA_RUNTIME_SRC:=/var/tmp/mediabox-media-runtime}"
: "${MEDIA_RUNTIME_JOBS:=0}"

prefix="$MEDIABOX_MEDIA_PREFIX"

case "$prefix" in
  /opt/rk3588-screenbridge*)
    echo "refusing to build into the ScreenBridge prefix: $prefix" >&2
    exit 2
    ;;
esac

step="${1:-all}"

# Everything below runs on the appliance. The libraries carry the prefix inside
# their own pkg-config files and RPATHs, so a build made anywhere else is a
# build for a different machine.
remote() { mediabox_ssh "bash -s" ; }

fetch_and_build() {
cat <<REMOTE | remote
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
prefix="$prefix"
src="$MEDIA_RUNTIME_SRC"
step="$1"
jobs="\$( [ "$MEDIA_RUNTIME_JOBS" -gt 0 ] && echo "$MEDIA_RUNTIME_JOBS" || nproc )"
mkdir -p "\$src" "\$prefix"

need() {
  # Build dependencies only. Nothing here installs a runtime library that a
  # player then links by accident: the Rockchip stack is the three things this
  # script builds, and they are the only things that end up in \$prefix.
  missing=""
  for p in "\$@"; do
    dpkg-query -W -f='\${Status}' "\$p" 2>/dev/null | grep -q "install ok installed" || missing="\$missing \$p"
  done
  if [ -n "\$missing" ]; then
    echo "== installing build dependencies:\$missing"
    apt-get update -qq
    # shellcheck disable=SC2086
    apt-get install -y -qq --no-install-recommends \$missing
  fi
}

checkout() {
  name="\$1"; repo="\$2"; rev="\$3"
  if [ ! -d "\$src/\$name/.git" ]; then
    git clone --filter=blob:none "\$repo" "\$src/\$name"
  fi
  cd "\$src/\$name"
  git fetch --quiet origin "\$rev" 2>/dev/null || git fetch --quiet origin
  git checkout --quiet --force "\$rev"
  git clean -qfdx
  actual="\$(git rev-parse HEAD)"
  [ "\$actual" = "\$rev" ] || { echo "\$name is at \$actual, not \$rev" >&2; exit 1; }
  echo "   \$name @ \$actual"
}

build_mpp() {
  need build-essential cmake git ca-certificates
  echo "== Rockchip MPP"
  checkout mpp "$MPP_REPO" "$MPP_REV"
  cd "\$src"
  rm -rf mpp-build
  cmake -S mpp -B mpp-build \
    -DCMAKE_INSTALL_PREFIX="\$prefix" \
    -DCMAKE_BUILD_TYPE=Release \
    -DBUILD_SHARED_LIBS=ON \
    -DBUILD_TEST=OFF
  cmake --build mpp-build --parallel "\$jobs"
  cmake --install mpp-build
}

build_rga() {
  need git ca-certificates
  echo "== librga"
  checkout librga "$RGA_REPO" "$RGA_REV"
  cd "\$src/librga"
  install -d "\$prefix/include/rga" "\$prefix/lib/pkgconfig"
  install -m 0644 include/*.h "\$prefix/include/rga/"
  install -m 0755 libs/Linux/gcc-aarch64/librga.so "\$prefix/lib/librga.so"
  cat > "\$prefix/lib/pkgconfig/librga.pc" <<PC
prefix=\$prefix
exec_prefix=\\\${prefix}
libdir=\\\${prefix}/lib
includedir=\\\${prefix}/include

Name: librga
Description: Rockchip RGA userspace library
Version: $RGA_VERSION
Libs: -L\\\${libdir} -lrga
Cflags: -I\\\${includedir}
PC
  sha256sum "\$prefix/lib/librga.so"
}

build_ffmpeg() {
  # libsrt is a runtime protocol this product's sources can arrive over, and it
  # is in the appliance's measured configuration; it is the distribution's, not
  # ours to pin.
  need build-essential git ca-certificates pkg-config nasm yasm libdrm-dev libsrt-openssl-dev
  echo "== ffmpeg-rockchip"
  checkout ffmpeg-rockchip "$FFMPEG_REPO" "$FFMPEG_REV"
  cd "\$src/ffmpeg-rockchip"
  export PKG_CONFIG_PATH="\$prefix/lib/pkgconfig\${PKG_CONFIG_PATH:+:\$PKG_CONFIG_PATH}"
  export LD_LIBRARY_PATH="\$prefix/lib\${LD_LIBRARY_PATH:+:\$LD_LIBRARY_PATH}"
  # The RPATH is what makes this runtime findable without an environment
  # variable. ffmpeg's own binaries get it here; the players get it from their
  # own link lines, which name the same directory.
  ./configure \
    --prefix="\$prefix" \
    --disable-doc \
    --enable-gpl \
    --enable-version3 \
    --enable-libdrm \
    --enable-rkmpp \
    --enable-rkrga \
    --enable-libsrt \
    --extra-ldflags="-Wl,-rpath,\$prefix/lib"
  make -j "\$jobs"
  make install
}

verify() {
  echo "== verification"
  fail=0
  say() { printf '  %-4s %-34s %s\n' "\$1" "\$2" "\$3"; [ "\$1" = FAIL ] && fail=1 || true; }

  for f in lib/librockchip_mpp.so lib/librga.so lib/pkgconfig/rockchip_mpp.pc \
           lib/pkgconfig/librga.pc lib/pkgconfig/libavcodec.pc bin/ffmpeg; do
    [ -e "\$prefix/\$f" ] && say PASS "\$f" present || say FAIL "\$f" missing
  done

  # MPP prints the revision it was built from on every start, and stamps it into
  # the library. That line is the pin, checked against the artefact that was
  # just built rather than against this script's own variable. The length of a
  # git short hash is not fixed — the same commit reads as seven characters in
  # one clone and eight in another — so the stamp is matched by prefix.
  mpp_stamp="\$(strings "\$prefix/lib/librockchip_mpp.so" 2>/dev/null \
    | grep -m1 -oE '^[0-9a-f]{7,} author: .*' || true)"
  case "\$mpp_stamp" in
    ${MPP_REV:0:7}*) say PASS "mpp revision" "\$mpp_stamp" ;;
    *)               say FAIL "mpp revision" "\${mpp_stamp:-unreadable}" ;;
  esac

  # Each of these captures ffmpeg's whole answer before looking at it, rather
  # than piping it into a grep that quits early. Under pipefail that pipe is a
  # trap:
  # grep finds its match in the first few lines and exits, ffmpeg is killed by
  # SIGPIPE writing the rest, and the pipeline reports 141 — so the check fails
  # precisely because the thing it was looking for was there.
  ffmpeg_says() {
    LD_LIBRARY_PATH="\$prefix/lib" "\$prefix/bin/ffmpeg" -hide_banner "\$1" 2>/dev/null || true
  }

  ver="\$(ffmpeg_says -version | head -1)"
  case "\$ver" in
    *${FFMPEG_REV:0:7}*) say PASS "ffmpeg revision" "\$ver" ;;
    *)                   say FAIL "ffmpeg revision" "\${ver:-unreadable}" ;;
  esac

  # The three things this runtime exists for.
  decoders="\$(ffmpeg_says -decoders)"
  case "\$decoders" in
    *hevc_rkmpp*) say PASS "hevc_rkmpp decoder" available ;;
    *)            say FAIL "hevc_rkmpp decoder" absent ;;
  esac
  filters="\$(ffmpeg_says -filters)"
  case "\$filters" in
    *rkrga*) say PASS "rkrga filters" available ;;
    *)       say FAIL "rkrga filters" absent ;;
  esac
  protocols="\$(ffmpeg_says -protocols)"
  case "\$protocols" in
    *srt*) say PASS "srt protocol" available ;;
    *)     say FAIL "srt protocol" absent ;;
  esac

  # And the thing this whole change is for: nothing in it reaches into the
  # other product's prefix.
  if grep -rqs 'rk3588-screenbridge' "\$prefix/lib/pkgconfig"; then
    say FAIL "screenbridge prefix" "named in pkg-config"
  else
    say PASS "screenbridge prefix" "unreferenced"
  fi
  if readelf -d "\$prefix/bin/ffmpeg" 2>/dev/null | grep -qi 'rk3588-screenbridge'; then
    say FAIL "ffmpeg RUNPATH" "points at ScreenBridge"
  else
    say PASS "ffmpeg RUNPATH" "\$(readelf -d "\$prefix/bin/ffmpeg" 2>/dev/null | sed -n 's/.*R\(UN\)\?PATH.*\[\(.*\)\]/\2/p' | head -1)"
  fi
  [ "\$fail" -eq 0 ] || { echo "== media runtime verification FAILED" >&2; exit 1; }
  echo "== media runtime verified at \$prefix"
}

case "\$step" in
  mpp)     build_mpp ;;
  rga)     build_rga ;;
  ffmpeg)  build_ffmpeg ;;
  verify)  verify ;;
  all)     build_mpp; build_rga; build_ffmpeg; verify ;;
  *) echo "usage: build-media-runtime.sh [all|mpp|rga|ffmpeg|verify]" >&2; exit 2 ;;
esac
REMOTE
}

echo "== MediaBox media runtime -> $prefix on ${MEDIABOX_HOST}"
fetch_and_build "$step"
