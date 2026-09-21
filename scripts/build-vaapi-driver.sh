#!/usr/bin/env bash
#
# Build the VA-API driver that lets Chromium reach the RK3588 video decoder.
#
# This is the piece that was missing, and the reason it was missing is worth
# writing down rather than rediscovering.
#
# The appliance runs a Rockchip BSP kernel. On it the VPU is exposed as
# /dev/mpp_service -- a Rockchip ioctl interface that only librockchip_mpp
# speaks -- and *not* as a V4L2 memory-to-memory device. There is no
# /dev/video* on this board at all, CONFIG_VIDEO_HANTRO is unset and
# /sys/class/video4linux is empty. Chromium has two accelerated decode
# backends on Linux, V4L2 and VA-API, and the Debian build here is the VA-API
# one: with no driver installed its GPU process says
#
#     ERROR:media/gpu/vaapi/vaapi_wrapper.cc:1801] vaInitialize failed:
#     unknown libva error
#
# and chrome://gpu lists no decode profiles at all, while still printing
# "Video Decode: Hardware accelerated" -- a line about the feature being
# enabled, not about a decoder existing. Every 4K profile answers
# powerEfficient:false and the CPU does the work.
#
# So the gap is a VA-API driver on top of MPP, and that is exactly what this
# builds. Nothing here is a second media stack: the driver is compiled against
# the MPP and librga that scripts/build-media-runtime.sh already put at
# MEDIABOX_MEDIA_PREFIX, and it carries an RPATH naming that same directory, so
# there is one MPP on the box and the browser links the one Kodi and the
# embedded player link.
#
# The revision is pinned. "main" would make the appliance a different product
# on every build, and this component is young enough that the difference would
# be a decoder that works and one that does not -- v2.1.5 is the first release
# that exports a surface's bit depth correctly for Chromium, which creates and
# exports a surface *before* decoding into it.
#
#   scripts/build-vaapi-driver.sh            build, install and verify
#   scripts/build-vaapi-driver.sh verify     just the checks
#
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=env.sh
source "$here/scripts/env.sh"

# libva bridged onto Rockchip MPP. The fork rather than woodyst/rockchip-vaapi
# upstream: upstream is a one-tag prototype whose own DEVELOPMENT.md says the
# HEVC, VP9 and AV1 paths fall through to the H.264 stub, and this is where the
# HEVC assembler, the 10-bit export and the Chromium surface-export fix live.
: "${VAAPI_REPO:=https://github.com/defcom5-rockchip/rockchip-vaapi.git}"
: "${VAAPI_REV:=8e41d7853415a401984dc71521e9fc4fc5f7fe97}"   # v2.2.0
: "${VAAPI_TAG:=v2.2.0}"

: "${VAAPI_SRC:=/var/tmp/mediabox-vaapi}"

prefix="$MEDIABOX_MEDIA_PREFIX"

case "$prefix" in
  /opt/rk3588-screenbridge*)
    echo "refusing to build into the ScreenBridge prefix: $prefix" >&2
    exit 2
    ;;
esac

step="${1:-all}"

remote() { mediabox_ssh "bash -s"; }

run() {
cat <<REMOTE | remote
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
prefix="$prefix"
src="$VAAPI_SRC"
step="$1"

need() {
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

build() {
  # libva-dev for the backend headers, vainfo to read the result back, and a
  # compiler. Deliberately *not* librockchip-mpp-dev: the distribution's MPP
  # would be a second decoder on the box, and this driver is built against the
  # one at \$prefix.
  need build-essential pkg-config libva-dev vainfo git ca-certificates
  echo "== rockchip-vaapi"
  if [ ! -d "\$src/rockchip-vaapi/.git" ]; then
    git clone --quiet --filter=blob:none "$VAAPI_REPO" "\$src/rockchip-vaapi"
  fi
  cd "\$src/rockchip-vaapi"
  git fetch --quiet origin "$VAAPI_REV" 2>/dev/null || git fetch --quiet --tags origin
  git checkout --quiet --force "$VAAPI_REV"
  git clean -qfdx
  actual="\$(git rev-parse HEAD)"
  [ "\$actual" = "$VAAPI_REV" ] || { echo "rockchip-vaapi is at \$actual, not $VAAPI_REV" >&2; exit 1; }
  echo "   rockchip-vaapi @ \$actual ($VAAPI_TAG)"

  # The shipped Makefile hard-codes /usr/include/rockchip and a bare
  # -lrockchip_mpp, which is the distribution's MPP. Both are replaced here so
  # the driver compiles and links against this product's runtime, and the
  # RPATH is what makes it find that runtime again without an environment
  # variable -- the same arrangement the players have.
  export PKG_CONFIG_PATH="\$prefix/lib/pkgconfig"
  make -s CC=gcc \
    CFLAGS="-O2 -Wall -Wextra -fPIC -shared \$(pkg-config --cflags libva rockchip_mpp librga) -DHAVE_RGA" \
    LDFLAGS="\$(pkg-config --libs libva rockchip_mpp librga) -lpthread -ldl -Wl,-rpath,\$prefix/lib"

  # Its own directory under this product's prefix rather than the system's
  # /usr/lib/aarch64-linux-gnu/dri. Nothing else on the box gets a Rockchip
  # decoder it did not ask for, and the browser is pointed here by
  # LIBVA_DRIVERS_PATH in its unit.
  install -d "\$prefix/lib/dri"
  install -m 0755 rockchip_drv_video.so "\$prefix/lib/dri/rockchip_drv_video.so"
}

verify() {
  echo "== verification"
  fail=0
  say() { printf '  %-4s %-34s %s\n' "\$1" "\$2" "\$3"; [ "\$1" = FAIL ] && fail=1 || true; }
  so="\$prefix/lib/dri/rockchip_drv_video.so"

  [ -e "\$so" ] && say PASS "rockchip_drv_video.so" present || { say FAIL "rockchip_drv_video.so" missing; return 1; }

  # The decoder this box actually has. Without the BSP node there is no point
  # in any of the rest: the driver would load and fail on the first decode.
  [ -e /dev/mpp_service ] && say PASS "/dev/mpp_service" present \
    || say FAIL "/dev/mpp_service" "absent -- not a Rockchip BSP kernel"

  # One MPP on this box, and it is this product's.
  linked="\$(ldd "\$so" 2>/dev/null | sed -n 's/.*librockchip_mpp[^ ]* => \([^ ]*\).*/\1/p' | head -1)"
  case "\$linked" in
    "\$prefix"/*) say PASS "librockchip_mpp" "\$linked" ;;
    *)            say FAIL "librockchip_mpp" "\${linked:-unresolved}" ;;
  esac
  if readelf -d "\$so" 2>/dev/null | grep -qi 'rk3588-screenbridge'; then
    say FAIL "screenbridge prefix" "named in RUNPATH"
  else
    say PASS "screenbridge prefix" "unreferenced"
  fi

  # And what libva makes of it. The profile list is the answer that matters:
  # a driver that loads but advertises nothing leaves Chromium exactly where
  # it was.
  info="\$(LIBVA_DRIVERS_PATH="\$prefix/lib/dri" LIBVA_DRIVER_NAME=rockchip \
           vainfo --display drm --device /dev/dri/renderD128 2>&1 || true)"
  case "\$info" in
    *"va_openDriver() returns 0"*) say PASS "va_openDriver" "returns 0" ;;
    *) say FAIL "va_openDriver" "\$(printf '%s' "\$info" | grep -i error | head -1)" ;;
  esac
  ver="\$(printf '%s' "\$info" | sed -n 's/^vainfo: Driver version: *//p')"
  case "\$ver" in
    *"${VAAPI_TAG#v}"*) say PASS "driver version" "\$ver" ;;
    *)                  say FAIL "driver version" "\${ver:-unreadable}" ;;
  esac
  # The two the television needs: H.264 and VP9, which is what a 4K browser
  # workload is made of once AV1 is asked not to appear.
  for p in VAProfileH264High VAProfileVP9Profile0 VAProfileHEVCMain; do
    case "\$info" in
      *"\$p"*) say PASS "\$p" "VLD" ;;
      *)       say FAIL "\$p" absent ;;
    esac
  done

  [ "\$fail" -eq 0 ] || { echo "== VA-API driver verification FAILED" >&2; exit 1; }
  echo "== VA-API driver verified at \$prefix/lib/dri"
}

case "\$step" in
  build)  build ;;
  verify) verify ;;
  all)    build; verify ;;
  *) echo "usage: build-vaapi-driver.sh [all|build|verify]" >&2; exit 2 ;;
esac
REMOTE
}

echo "== rockchip-vaapi $VAAPI_TAG -> $prefix/lib/dri on ${MEDIABOX_HOST}"
run "$step"
