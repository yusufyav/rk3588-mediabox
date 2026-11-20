#!/usr/bin/env bash
#
# The acceptance gate for live television's player, on the target's own
# terms: mediabox-live built for aarch64 and its tests run as aarch64
# binaries, in the build image (scripts/build-container.sh) -- the
# appliance's Debian libraries at the pinned snapshot and the GStreamer it is
# built for (scripts/build-gstreamer-runtime.sh) at its own prefix, not the
# host's. Nothing goes to the appliance.
#
#   1. GStreamer runtime: built if missing, its sha256 checked
#   2. mediabox-live tests, aarch64 (this machine's binfmt qemu-aarch64)
#   3. mediabox-dvb tests
#   4. clippy on mediabox-live, no warnings beyond the known ones
#   5. git diff --check, and no trailing blanks in untracked sources
#   6. mediabox-live release build, aarch64, its RUNPATH and hash
#
# Usage: scripts/test-mediabox-live-arm64.sh
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=build-container.sh
source "$here/scripts/build-container.sh"
triple=aarch64-unknown-linux-gnu
gst_prefix="$MEDIABOX_GST_PREFIX"
tarball="$here/build/gstreamer-runtime/gstreamer-1.26.11-aarch64.tar.xz"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

say() { printf '\n== %s\n' "$*"; }

# aarch64 test binaries are run by the kernel through qemu, in the
# container as here: the binfmt entry has to carry F (the interpreter is
# opened when it is registered, so it is found from inside the container).
grep -qs '^flags:.*F' /proc/sys/fs/binfmt_misc/qemu-aarch64 || {
  echo "binfmt qemu-aarch64 with the F flag is needed (qemu-user-static)" >&2; exit 1; }

say "1. GStreamer runtime"
[ -e "$tarball" ] || "$here/scripts/build-gstreamer-runtime.sh"
( cd "${tarball%/*}" && sha256sum -c "${tarball##*/}.sha256" )
head -1 "$(mediabox_gst_runtime_root)/BUILD-INFO"

# In the image: the runtime's .pc files first. The checkout's own path is
# /work in every container, and not in the binary either (panic locations
# read ./rust/crates/...). GStreamer reads its registry from $work, and the
# plugin path and scanner compiled into libgstreamer are the appliance's
# paths, as they are in the image.
env_live="
  export PKG_CONFIG_LIBDIR=$gst_prefix/lib/pkgconfig:\$PKG_CONFIG_LIBDIR
  export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS='--remap-path-prefix=/work=. -C link-arg=-Wl,-rpath,$gst_prefix/lib -C link-arg=-Wl,-rpath-link=$gst_prefix/lib'
  export CARGO_TARGET_DIR=/work/build/cargo-arm64/mediabox-live-test
  export GST_REGISTRY_1_0=$work/registry.aarch64.bin GST_REGISTRY_FORK=no
  unset GST_PLUGIN_PATH GST_PLUGIN_PATH_1_0 GST_PLUGIN_SYSTEM_PATH GST_PLUGIN_SYSTEM_PATH_1_0
"

say "2. mediabox-live tests ($triple)"
mediabox_build_run -g -v "$work:$work" -w /work/rust/crates/mediabox-live -- "$env_live
  cargo test --bins --target $triple 2>&1
" | tee "$work/tests.log" | grep -E '^test result|^running|panicked|FAILED'
grep -q '^test result: ok' "$work/tests.log" || { echo "mediabox-live tests failed" >&2; exit 1; }
if grep -E '^test .* ignored$' "$work/tests.log"; then echo "ignored tests" >&2; exit 1; fi
# The faults made on purpose (MEDIABOX_LIVE_FAULT) are in no product build;
# the diagnostic build that has them must still compile.
mediabox_build_run -g -v "$work:$work" -w /work/rust/crates/mediabox-live -- "$env_live
  cargo check -q --bins --target $triple --features fault-injection
"

say "3. mediabox-dvb tests"
# The transport hole (src/inject.rs), the opening capture (src/capture.rs):
# in no product build; their tests run with the diagnostic features.
mediabox_build_run -w /work/rust/crates/mediabox-dvb -- "
  export CARGO_TARGET_DIR=/work/build/cargo-arm64/rust
  for f in '' transport-injection opening-capture; do
    cargo test -q \${f:+--features \$f} 2>&1 | grep '^test result'
  done
"

say "4. clippy (mediabox-live)"
mediabox_build_run -g -v "$work:$work" -w /work/rust/crates/mediabox-live -- "$env_live
  cargo clippy --bins --tests -q 2>&1 || true
" > "$work/clippy.log"
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
# As build-mediabox-live.sh builds it: from the fixed path, into the same
# target, so the hash below is the bundle's.
# shellcheck source=mediabox-live-source.sh
source "$here/scripts/mediabox-live-source.sh"
mediabox_live_source "$here"
echo "mediabox-live source: $MEDIABOX_LIVE_SOURCE (snapshot $MEDIABOX_LIVE_SNAPSHOT)"
mediabox_build_run -g -v "$MEDIABOX_LIVE_SRC:$MEDIABOX_LIVE_SRC" -w "$MEDIABOX_LIVE_CRATE" -- "
  export PKG_CONFIG_LIBDIR=$gst_prefix/lib/pkgconfig:\$PKG_CONFIG_LIBDIR
  export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS='$MEDIABOX_LIVE_REMAP -C link-arg=-Wl,-rpath,$gst_prefix/lib -C link-arg=-Wl,-rpath-link=$gst_prefix/lib'
  CARGO_TARGET_DIR=/work/build/cargo-arm64/mediabox-live cargo build -q --locked --release --target $triple
  aarch64-linux-gnu-readelf -d /work/build/cargo-arm64/mediabox-live/$triple/release/mediabox-live | grep -E 'RUNPATH|RPATH'
"
bin="$(mediabox_cargo_target mediabox-live)/$triple/release/mediabox-live"
if grep -aqE 'MEDIABOX_LIVE_FAULT|BİLEREK KUSUR|audio-stall ms=|pts-jump ms=|pcm-zero ms=|ts-drop pid=|prepare-delay ms=|prepare-fail id=|hold-eos ms=|stale-reopen after=|phase=deadline-none' "$bin"; then echo "fault injection in the product build" >&2; exit 1; fi
sha256sum "$bin"
echo "gate passed"
