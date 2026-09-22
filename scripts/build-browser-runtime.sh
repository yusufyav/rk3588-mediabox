#!/usr/bin/env bash
#
# Build the V4L2 runtime that lets Chromium reach the RK3588 AV1 decoder.
#
# Why there is a second runtime beside the VA-API one, and why AV1 needs it.
#
# The appliance already has a VA-API driver on top of MPP
# (scripts/build-vaapi-driver.sh) and it carries H.264, HEVC and VP9. It does
# not carry AV1, and the reason is not the silicon: RK3588 has an AV1 decoder
# of its own -- a separate block at fdc70000, not the rkvdec cores -- and the
# kernel advertises it, `DEVICE[ 4]:AV1DEC HW_ID:0x80019000` in
# /proc/mpp_service/supports-device. The mismatch is at the API. VA-API's AV1
# entry point hands the driver tile data that the browser has already parsed,
# while MPP's public AV1 decoder wants the OBU stream it can parse itself, and
# nothing bridges those two halves.
#
# Chromium's other accelerated decode backend does want a whole stream. Its
# V4L2 stateful decoder sends compressed bytes at a device and takes frames
# back, which is exactly the shape MPP has. Debian's Chromium carries that
# backend -- both backends are compiled in, and `media/base/media_switches.h`
# says which one runs:
#
#   When both VA-API and V4L2 are compiled in, selects the active backend:
#   disabled (default) => VA-API, enabled => V4L2. Toggle via
#   --enable-features=PreferV4L2VideoAcceleration.
#
# and its V4L2 codec table has AV01 in it, mapped to AV1PROFILE_PROFILE_MAIN.
# So no browser is built here: the browser already has the road, and this is
# the device at the end of it.
#
# What gets built, all of it under this product's own prefix:
#
#   libv4l2 + v4l2convert.so   v4l-utils, pinned, with one upstream Rockchip
#                              patch that lets a plugin serve mmap(). Chromium
#                              is not linked against libv4l2, so v4l2convert.so
#                              is LD_PRELOADed into it and routes /dev/video*
#                              through the plugin. It intercepts nothing else:
#                              its open() looks only at that prefix, and every
#                              other call falls through on an fd lookup.
#
#   libv4l-rkmpp.so            the V4L2 decoder Rockchip wrote for Chromium,
#                              pinned, compiled against *this product's*
#                              librockchip_mpp. There is one MPP on the box
#                              and this links the one Kodi and the player link.
#
#   etc/video-dec0             the codec list, as a file. libv4l-rkmpp takes
#                              its capabilities from the contents of the node
#                              it is opened on, and the unit bind-mounts this
#                              at /dev/video0 inside the browser's own /dev.
#                              Nothing is created in the host's /dev and
#                              nothing is left behind by hand.
#
#   scripts/build-browser-runtime.sh            build, install and verify
#   scripts/build-browser-runtime.sh verify     just the checks
#
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=env.sh
source "$here/scripts/env.sh"

# v4l-utils, by release tarball and content hash. There is no git revision to
# pin that is also what distributions ship, and the tarball is what upstream
# signs its releases as.
: "${V4L_UTILS_VERSION:=1.30.1}"
: "${V4L_UTILS_URL:=https://linuxtv.org/downloads/v4l-utils/v4l-utils-${V4L_UTILS_VERSION}.tar.xz}"
: "${V4L_UTILS_SHA256:=c1cf549c2ec3cf39eb5ec7bf15731349e61b26a21b5e963922db422333bae197}"

# The one patch libv4l-rkmpp's README names as its dependency: without it a
# plugin can answer every ioctl and still not hand the application a buffer,
# because v4l2_mmap() goes to the kernel behind the plugin's back. Fetched
# from upstream rather than vendored, and checked by hash for the same reason
# the tarball is.
: "${V4L_MMAP_PATCH_URL:=https://raw.githubusercontent.com/JeffyCN/meta-rockchip/release-1.3.0_20200915/recipes-multimedia/v4l2apps/v4l-utils/0001-libv4l2-Support-mmap-to-libv4l-plugin.patch}"
: "${V4L_MMAP_PATCH_SHA256:=9b56d4219eae95a6597cd63b9fac72ca7b2c122bf210772df467333d9d0bdd56}"

# libv4l-rkmpp, pinned. "master" would make the appliance a different product
# on every build, and this component is the decoder.
: "${RKMPP_V4L_REPO:=https://github.com/JeffyCN/libv4l-rkmpp.git}"
: "${RKMPP_V4L_REV:=c5bc0aef0bc571872eb67508dcd75e05247eb83a}"
: "${RKMPP_V4L_TAG:=1.8.0+3}"

: "${BROWSER_RUNTIME_SRC:=/var/tmp/mediabox-browser-runtime}"

prefix="$MEDIABOX_PREFIX/browser-runtime"
media="$MEDIABOX_MEDIA_PREFIX"

case "$prefix" in
  /opt/rk3588-screenbridge*)
    echo "refusing to build into the ScreenBridge prefix: $prefix" >&2
    exit 2
    ;;
esac

step="${1:-all}"

# The codec list the plugin is opened with, and the largest picture it will
# claim. 4096x2304 is DCI 4K: it covers everything the television is served
# and stays inside what every one of these five codecs can do on RK3588.
patch_dir="$here/packaging/libv4l-rkmpp-patches"
v4l_patch_dir="$here/packaging/v4l-utils-patches"

remote() { mediabox_ssh "bash -s"; }

run() {
cat <<REMOTE | remote
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
prefix="$prefix"
media="$media"
src="$BROWSER_RUNTIME_SRC"
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

build_v4l_utils() {
  echo "== v4l-utils $V4L_UTILS_VERSION"
  mkdir -p "\$src"
  tarball="\$src/v4l-utils-$V4L_UTILS_VERSION.tar.xz"
  [ -s "\$tarball" ] || curl -fsSL -o "\$tarball" "$V4L_UTILS_URL"
  echo "$V4L_UTILS_SHA256  \$tarball" | sha256sum -c -

  patch_file="\$src/mmap-to-plugin.patch"
  [ -s "\$patch_file" ] || curl -fsSL -o "\$patch_file" "$V4L_MMAP_PATCH_URL"
  echo "$V4L_MMAP_PATCH_SHA256  \$patch_file" | sha256sum -c -

  rm -rf "\$src/v4l-utils-$V4L_UTILS_VERSION"
  tar -C "\$src" -xf "\$tarball"
  cd "\$src/v4l-utils-$V4L_UTILS_VERSION"
  patch -p1 < "\$patch_file"

  # And this product's own, which is what makes the wrapper reach a browser
  # built the way distributions build them.
  for p in /var/tmp/mediabox-v4l-utils-patches/*.patch; do
    [ -e "\$p" ] || continue
    echo "   applying \$(basename "\$p")"
    patch -p1 < "\$p"
  done

  # Only the libraries and the wrapper. The utilities, the DVB stack, the Qt
  # viewers and the BPF decoders are a distribution's business, not this
  # appliance's, and every one of them is a dependency the box would carry for
  # nothing.
  meson setup build --prefix="\$prefix" --libdir=lib \
    -Dv4l-utils=false -Dlibdvbv5=disabled \
    -Dv4l-plugins=true -Dv4l-wrappers=true \
    -Dqv4l2=disabled -Dqvidcap=disabled -Dbpf=disabled -Djpeg=disabled \
    -Dgconv=disabled -Dv4l2-tracer=disabled -Ddoxygen-doc=disabled \
    -Dc_args="-O2" -Dcpp_args="-O2" \
    -Dc_link_args="-Wl,-rpath,\$prefix/lib -ldl -lpthread" \
    -Dcpp_link_args="-Wl,-rpath,\$prefix/lib" >/dev/null
  ninja -C build >/dev/null
  ninja -C build install >/dev/null

  # libv4l2 dlopens every .so in its plugin directory and calls init() on the
  # fd. Only this product's decoder belongs in there; the generic mplane
  # shim would be one more thing probing a node it has no business in.
  rm -f "\$prefix/lib/libv4l/plugins/libv4l-mplane.so"
}

build_plugin() {
  echo "== libv4l-rkmpp $RKMPP_V4L_TAG"
  if [ ! -d "\$src/libv4l-rkmpp/.git" ]; then
    git clone --quiet "$RKMPP_V4L_REPO" "\$src/libv4l-rkmpp"
  fi
  cd "\$src/libv4l-rkmpp"
  git fetch --quiet origin 2>/dev/null || true
  git checkout --quiet --force "$RKMPP_V4L_REV"
  git clean -qfdx
  actual="\$(git rev-parse HEAD)"
  [ "\$actual" = "$RKMPP_V4L_REV" ] || { echo "libv4l-rkmpp is at \$actual, not $RKMPP_V4L_REV" >&2; exit 1; }
  echo "   libv4l-rkmpp @ \$actual"

  for p in /var/tmp/mediabox-rkmpp-patches/*.patch; do
    [ -e "\$p" ] || continue
    echo "   applying \$(basename "\$p")"
    patch -p1 < "\$p"
  done

  # libv4l2 from the runtime just built; MPP from the product's media runtime.
  # The RPATH names both, which is what keeps this working without an
  # environment variable and keeps a second MPP off the box.
  export PKG_CONFIG_PATH="\$prefix/lib/pkgconfig:\$media/lib/pkgconfig"
  rm -rf build
  meson setup build --prefix="\$prefix" --libdir=lib \
    -Dmax-dec-width=4096 -Dmax-dec-height=2304 \
    -Dc_args="-O2" \
    -Dc_link_args="-Wl,-rpath,\$media/lib:\$prefix/lib" >/dev/null
  ninja -C build >/dev/null
  ninja -C build install >/dev/null
}

install_device_config() {
  # What the decoder claims, as a file. This is the whole of the "device":
  # libv4l-rkmpp reads its capabilities out of the node it is opened on, and
  # refuses any node that is a character device, so this has to be an ordinary
  # file. The browser's unit bind-mounts it at /dev/video0 inside its own
  # /dev; the host's /dev never gets one.
  install -d "\$prefix/etc"
  cat > "\$prefix/etc/video-dec0" <<'DEVCFG'
type=dec
codecs=AV1:VP9:VP8:H.264:H.265
max-width=4096
max-height=2304
DEVCFG
  chmod 0644 "\$prefix/etc/video-dec0"
}

build() {
  need build-essential pkg-config git ca-certificates curl meson ninja-build binutils
  build_v4l_utils
  build_plugin
  install_device_config
}

verify() {
  echo "== verification"
  fail=0
  say() { printf '  %-4s %-34s %s\n' "\$1" "\$2" "\$3"; [ "\$1" = FAIL ] && fail=1 || true; }

  shim="\$prefix/lib/libv4l/v4l2convert.so"
  plugin="\$prefix/lib/libv4l/plugins/libv4l-rkmpp.so"
  cfg="\$prefix/etc/video-dec0"

  [ -e "\$shim" ]   && say PASS "v4l2convert.so"   present || say FAIL "v4l2convert.so"   missing
  [ -e "\$plugin" ] && say PASS "libv4l-rkmpp.so"  present || say FAIL "libv4l-rkmpp.so"  missing
  [ -e "\$cfg" ]    && say PASS "video-dec0"       present || say FAIL "video-dec0"       missing

  # Nothing here may leave a video node on the appliance: the browser gets its
  # own inside its namespace, and a node out here would be a second decoder
  # nobody asked for.
  if ls /dev/video* >/dev/null 2>&1; then
    say FAIL "host /dev" "\$(ls -d /dev/video* | tr '\n' ' ')-- this build left nodes behind"
  else
    say PASS "host /dev" "no video nodes"
  fi

  # The decoder this board actually has, and the AV1 block behind it. Without
  # either there is nothing for any of the above to talk to.
  [ -e /dev/mpp_service ] && say PASS "/dev/mpp_service" present \
    || say FAIL "/dev/mpp_service" "absent -- not a Rockchip BSP kernel"
  if grep -q 'AV1DEC' /proc/mpp_service/supports-device 2>/dev/null; then
    say PASS "AV1 hardware endpoint" "\$(sed -n 's/.*\(AV1DEC.*\)/\1/p' /proc/mpp_service/supports-device | head -1)"
  else
    say FAIL "AV1 hardware endpoint" "not advertised by mpp_service"
  fi

  # One MPP on this box, and it is this product's.
  linked="\$(ldd "\$plugin" 2>/dev/null | sed -n 's/.*librockchip_mpp[^ ]* => \([^ ]*\).*/\1/p' | head -1)"
  case "\$linked" in
    "\$media"/*) say PASS "librockchip_mpp" "\$linked" ;;
    *)           say FAIL "librockchip_mpp" "\${linked:-unresolved}" ;;
  esac
  if readelf -d "\$plugin" 2>/dev/null | grep -qi 'rk3588-screenbridge'; then
    say FAIL "screenbridge prefix" "named in RUNPATH"
  else
    say PASS "screenbridge prefix" "unreferenced"
  fi

  # The spellings of open() a hardened browser actually calls. Without these
  # the wrapper loads, interposes nothing, and the decoder is never asked a
  # question -- which looks exactly like a decoder that answered wrongly.
  for sym in open open64 __open_2 __open64_2 ioctl mmap poll; do
    if nm -D --defined-only "\$shim" 2>/dev/null | grep -q " T \$sym\$"; then
      say PASS "interposes \$sym" exported
    else
      say FAIL "interposes \$sym" "not exported"
    fi
  done

  # The shim has to find its own libv4l2 without an environment variable,
  # because it is loaded into a browser that sets neither.
  if ldd "\$shim" 2>/dev/null | grep -q "\$prefix/lib/libv4l2.so.0"; then
    say PASS "v4l2convert RUNPATH" "\$prefix/lib"
  else
    say FAIL "v4l2convert RUNPATH" "libv4l2 not resolved from the product prefix"
  fi

  # And what the chain answers when it is asked the questions Chromium asks.
  #
  # In a /dev of its own. A mount namespace is not enough by itself: devtmpfs
  # has one superblock for the whole machine, so creating the node inside the
  # namespace creates it on the host too and leaves it there -- which is how an
  # earlier version of this check left a /dev/video0 behind on the appliance
  # and then failed its own "nothing should create these" test. A tmpfs over
  # /dev is what actually keeps it private; the three pseudo-devices are put
  # back because a dynamically linked program is entitled to them.
  probe="\$(unshare -m sh -c "
    mount -t tmpfs none /dev || exit 70
    mknod /dev/null    c 1 3 && chmod 666 /dev/null
    mknod /dev/zero    c 1 5 && chmod 666 /dev/zero
    mknod /dev/urandom c 1 9 && chmod 666 /dev/urandom
    cat \$cfg > /dev/video0 || exit 71
    LD_PRELOAD=\$shim \$prefix/bin/mediabox-v4l2-probe /dev/video0
  " 2>&1 || true)"
  case "\$probe" in
    *"driver=rkmpp"*) say PASS "VIDIOC_QUERYCAP" "driver=rkmpp" ;;
    *)                say FAIL "VIDIOC_QUERYCAP" "\$(printf '%s' "\$probe" | head -1)" ;;
  esac
  for cc in AV01 VP90 H264 HEVC VP80; do
    case "\$probe" in
      *"OUTPUT"*"\$cc"*) say PASS "coded format \$cc" advertised ;;
      *)                 say FAIL "coded format \$cc" absent ;;
    esac
  done
  case "\$probe" in
    *"CAPTURE"*"NV12"*) say PASS "frame format NV12" advertised ;;
    *)                  say FAIL "frame format NV12" absent ;;
  esac
  case "\$probe" in
    *"AV1_PROFILE ctrl: min=0"*) say PASS "AV1 profile control" "Main" ;;
    *)                           say FAIL "AV1 profile control" "not readable" ;;
  esac

  [ "\$fail" -eq 0 ] || { echo "== browser runtime verification FAILED" >&2; exit 1; }
  echo "== browser runtime verified at \$prefix"
}

case "\$step" in
  build)  build ;;
  verify) verify ;;
  all)    build; verify ;;
  *) echo "usage: build-browser-runtime.sh [all|build|verify]" >&2; exit 2 ;;
esac
REMOTE
}

if [ "$step" != verify ]; then
  echo "== staging patches and the probe on ${MEDIABOX_HOST}"
  mediabox_ssh "rm -rf /var/tmp/mediabox-rkmpp-patches /var/tmp/mediabox-v4l-utils-patches && mkdir -p /var/tmp/mediabox-rkmpp-patches /var/tmp/mediabox-v4l-utils-patches"
  mediabox_scp "$patch_dir"/*.patch "$MEDIABOX_TARGET:/var/tmp/mediabox-rkmpp-patches/"
  mediabox_scp "$v4l_patch_dir"/*.patch "$MEDIABOX_TARGET:/var/tmp/mediabox-v4l-utils-patches/"
  mediabox_scp "$here/tools/v4l2-probe.c" "$MEDIABOX_TARGET:/var/tmp/mediabox-v4l2-probe.c"
  mediabox_ssh "install -d $prefix/bin && cc -O2 -o $prefix/bin/mediabox-v4l2-probe /var/tmp/mediabox-v4l2-probe.c"
fi

echo "== browser runtime -> $prefix on ${MEDIABOX_HOST}"
run "$step"
