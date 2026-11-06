#!/usr/bin/env bash
#
# The acceptance gate for live television's player, on the target's own
# terms: mediabox-live built for aarch64 and its tests run under
# qemu-aarch64-static, on the appliance's libraries (the cross sysroot) and
# the GStreamer it is built for (scripts/build-gstreamer-runtime.sh), not
# the host's. Nothing goes to the appliance.
#
#   1. GStreamer runtime: built if missing, its sha256 checked, unpacked into
#      the sysroot at its prefix
#   2. mediabox-live tests, aarch64, under qemu
#   3. mediabox-dvb tests
#   4. clippy on mediabox-live, no warnings beyond the known ones
#   5. git diff --check, and no trailing blanks in untracked sources
#   6. mediabox-live release build, aarch64, its RUNPATH and hash
#
# Usage: scripts/test-mediabox-live-arm64.sh
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
sysroot="${MEDIABOX_SYSROOT:-$here/.sysroot/aarch64-trixie}"
triple=aarch64-unknown-linux-gnu
crate="$here/rust/crates/mediabox-live"
gst_prefix=/opt/rk3588-mediabox/gstreamer-1.26
tarball="$here/build/gstreamer-runtime/gstreamer-1.26.11-aarch64.tar.xz"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

say() { printf '\n== %s\n' "$*"; }

[ -e "$sysroot/usr/lib/aarch64-linux-gnu/libc.so.6" ] || { echo "no sysroot at $sysroot" >&2; exit 1; }
command -v qemu-aarch64-static >/dev/null || { echo "qemu-aarch64-static is needed" >&2; exit 1; }

say "1. GStreamer runtime"
[ -e "$tarball" ] || "$here/scripts/build-gstreamer-runtime.sh"
sha256sum -c "$tarball.sha256"
if ! cmp -s <(tar -xJOf "$tarball" "${gst_prefix#/}/BUILD-INFO") "$sysroot$gst_prefix/BUILD-INFO" 2>/dev/null; then
  rm -rf "${sysroot:?}$gst_prefix"
  tar -C "$sysroot" -xJf "$tarball"
fi
head -1 "$sysroot$gst_prefix/BUILD-INFO"

# The sysroot is copied off the appliance for linking; its BLAS and LAPACK
# are /etc/alternatives links that did not come with it. libavfilter needs
# them through pocketsphinx, and gst-libav libavfilter: without them
# avdec_* do not load here, as they do on the appliance. The same trixie
# packages, from the snapshot the runtime was built against, pinned.
if [ ! -e "$sysroot/usr/lib/aarch64-linux-gnu/libblas.so.3" ]; then
  debs="$here/build/gstreamer-runtime/sysroot-debs"
  declare -A pinned=(
    [libblas3_3.12.1-6_arm64.deb]=33ed1d0770978f11493d24e0416a51ea7de77ede8c23d179a8ad88802a167be2
    [liblapack3_3.12.1-6_arm64.deb]=b1375f31fd6977f0cdacdd1de44a5f5081433d91737ea8fe9d0e64ab2ccfab12
    [libgfortran5_14.2.0-19_arm64.deb]=5f771ed84bf8779d4598e66b719121ab1413c11412c87629e50c79d26b042612
  )
  mkdir -p "$debs"
  for deb in "${!pinned[@]}"; do
    pkg="${deb%%_*}"; first="${pkg:0:1}"
    case "$pkg" in libblas3|liblapack3) dir=lapack ;; libgfortran5) dir=gcc-14 ;; esac
    [ -e "$debs/$deb" ] || curl -fsSL --retry 3 -o "$debs/$deb" \
      "http://snapshot.debian.org/archive/debian/20261101T000000Z/pool/main/$first/$dir/$deb"
    echo "${pinned[$deb]}  $debs/$deb" | sha256sum -c --quiet -
    mkdir -p "$work/deb" && (cd "$work/deb" && ar x "$debs/$deb" && bsdtar -xf data.tar.* -C "$work/deb" && rm -f data.tar.* control.tar.* debian-binary)
  done
  for lib in blas/libblas.so.3 lapack/liblapack.so.3; do
    mkdir -p "$sysroot/usr/lib/aarch64-linux-gnu/${lib%/*}"
    cp -an "$work/deb/usr/lib/aarch64-linux-gnu/${lib%/*}/." "$sysroot/usr/lib/aarch64-linux-gnu/${lib%/*}/"
    mkdir -p "$sysroot/etc/alternatives"
    ln -sfn "$sysroot/usr/lib/aarch64-linux-gnu/$lib" "$sysroot/etc/alternatives/${lib#*/}-aarch64-linux-gnu"
  done
  cp -an "$work/deb/usr/lib/aarch64-linux-gnu/"libgfortran.so.5* "$sysroot/usr/lib/aarch64-linux-gnu/"
fi

export PKG_CONFIG_ALLOW_CROSS=1
export PKG_CONFIG_SYSROOT_DIR="$sysroot"
export PKG_CONFIG_LIBDIR="$sysroot$gst_prefix/lib/pkgconfig:$sysroot/usr/lib/aarch64-linux-gnu/pkgconfig:$sysroot/usr/share/pkgconfig"
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
# The checkout's own path is not in the binary (panic locations read
# ./rust/crates/...): the same commit builds the same bytes from any
# worktree.
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS="--remap-path-prefix=$here=. -C link-arg=--sysroot=$sysroot -C link-arg=-Wl,-rpath,$gst_prefix/lib -C link-arg=-Wl,-rpath-link=$sysroot$gst_prefix/lib -C link-arg=-Wl,-rpath-link=$sysroot/usr/lib/aarch64-linux-gnu"
# qemu resolves every absolute path in the sysroot first: the binary's
# RUNPATH, the plugin path and the scanner compiled into libgstreamer are
# the appliance's paths, as they will be there.
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUNNER="qemu-aarch64-static -L $sysroot"
export GST_REGISTRY_1_0="$work/registry.aarch64.bin"
export GST_REGISTRY_FORK=no
unset GST_PLUGIN_PATH GST_PLUGIN_PATH_1_0 GST_PLUGIN_SYSTEM_PATH GST_PLUGIN_SYSTEM_PATH_1_0

say "2. mediabox-live tests ($triple, qemu)"
( cd "$crate" && cargo test --bins --target "$triple" 2>&1 | tee "$work/tests.log" | grep -E '^test result|^running|panicked|FAILED' )
grep -q '^test result: ok' "$work/tests.log" || { echo "mediabox-live tests failed" >&2; exit 1; }
if grep -E '^test .* ignored$' "$work/tests.log"; then echo "ignored tests" >&2; exit 1; fi
# The faults made on purpose (MEDIABOX_LIVE_FAULT) are in no product build;
# the diagnostic build that has them must still compile.
( cd "$crate" && cargo check -q --bins --target "$triple" --features fault-injection )

say "3. mediabox-dvb tests"
( cd "$here/rust/crates/mediabox-dvb" && env -u CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUNNER cargo test -q 2>&1 | grep '^test result' )
# The transport hole (src/inject.rs) is in no product build; its tests run
# with the diagnostic feature that puts it in.
( cd "$here/rust/crates/mediabox-dvb" && env -u CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUNNER cargo test -q --features transport-injection 2>&1 | grep '^test result' )
# The opening capture (src/capture.rs) is in no product build either.
( cd "$here/rust/crates/mediabox-dvb" && env -u CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUNNER cargo test -q --features opening-capture 2>&1 | grep '^test result' )

say "4. clippy (mediabox-live)"
( cd "$crate" && cargo clippy --bins --tests -q 2>&1 ) > "$work/clippy.log" || true
grep -E '^(warning|error)' "$work/clippy.log" | sort | uniq -c
# The two known ones: is_multiple_of in the fault injection (engine.rs),
# older than this gate.
unknown="$(grep -E '^(warning|error)' "$work/clippy.log" | grep -vc 'manual implementation of `.is_multiple_of()`' || true)"
[ "$unknown" = 0 ] || { cat "$work/clippy.log"; echo "new clippy findings" >&2; exit 1; }

say "5. whitespace"
git -C "$here" diff --check
if git -C "$here" ls-files --others --exclude-standard -- rust scripts | xargs -r grep -lE '[[:space:]]+$' 2>/dev/null | grep -vE '\.(mp2|adts|latm|ac3|eac3|h264)$'; then
  echo "trailing blanks in untracked files" >&2; exit 1
fi
echo clean

say "6. release build ($triple)"
# As build-mediabox-live.sh builds it: from the fixed path, so the hash
# below is the bundle's.
# shellcheck source=mediabox-live-source.sh
source "$here/scripts/mediabox-live-source.sh"
mediabox_live_source "$here"
source_crate="$MEDIABOX_LIVE_CRATE"
echo "mediabox-live source: commit $MEDIABOX_LIVE_COMMIT"
( cd "$source_crate" && CARGO_TARGET_DIR="$crate/target" \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS="$MEDIABOX_LIVE_REMAP -C link-arg=--sysroot=$sysroot -C link-arg=-Wl,-rpath,$gst_prefix/lib -C link-arg=-Wl,-rpath-link=$sysroot$gst_prefix/lib -C link-arg=-Wl,-rpath-link=$sysroot/usr/lib/aarch64-linux-gnu" \
    cargo build -q --locked --release --target "$triple" )
bin="$crate/target/$triple/release/mediabox-live"
aarch64-linux-gnu-readelf -d "$bin" | grep -E 'RUNPATH|RPATH'
if grep -aqE 'MEDIABOX_LIVE_FAULT|BİLEREK KUSUR|audio-stall ms=|pts-jump ms=|pcm-zero ms=|ts-drop pid=' "$bin"; then echo "fault injection in the product build" >&2; exit 1; fi
sha256sum "$bin"
echo "gate passed"
