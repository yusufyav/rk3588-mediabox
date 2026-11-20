#!/usr/bin/env bash
#
# Build the native television interface for the appliance.
#
# The appliance carries no Rust toolchain, so this cross-compiles and ships a
# binary -- the same arrangement the control plane has, in the same build
# image (scripts/build-container.sh): Debian's cross gcc and the appliance's
# glibc 2.41, and Slint's libraries (fontconfig, freetype, EGL, GBM,
# libinput, udev) as trixie's arm64 packages at the pinned snapshot
# (packaging/build-image/arm64-packages.txt). Nothing is read from an
# appliance or from this machine's own compiler and libraries.
#
# Output: build/cargo-arm64/mediabox-tv/aarch64-unknown-linux-gnu/release/mediabox-tv
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=build-container.sh
source "$here/scripts/build-container.sh"
triple="aarch64-unknown-linux-gnu"
target="$(mediabox_cargo_target mediabox-tv)"
bin="$target/$triple/release/mediabox-tv"

say() { printf '\n== %s\n' "$*"; }

say "mediabox-tv ($triple, release, build image)"
# Slint's -sys crates ask pkg-config for fontconfig and freetype2: the
# image's PKG_CONFIG_LIBDIR is the arm64 /usr/lib/<triple>/pkgconfig and
# /usr/share/pkgconfig, never an amd64 one.
#
# Warning-free or no binary: rustc's lints, the Slint compiler's and anything
# else cargo calls a warning fail the build (scripts/cargo-no-warnings.sh).
#
# The appliance runs glibc 2.41, and so does the image; a binary that asks for
# anything newer is still refused here rather than on the television.
mediabox_build_run -w /work/rust/crates/mediabox-tv -- "
  CARGO_TARGET_DIR=/work/build/cargo-arm64/mediabox-tv \\
    /work/scripts/cargo-no-warnings.sh /work/rust/crates/mediabox-tv build --release --target $triple
  bin=/work/build/cargo-arm64/mediabox-tv/$triple/release/mediabox-tv
  max=\"\$(aarch64-linux-gnu-objdump -T \"\$bin\" | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1)\"
  echo \"glibc: highest referenced \$max\"
  [ \"\${max##*.}\" -le 41 ] || { echo \"binary needs \$max but the appliance has GLIBC_2.41\" >&2; exit 1; }
"

printf '\n%-20s %s  %s\n' mediabox-tv "$(du -h "$bin" | cut -f1)" "$(sha256sum < "$bin" | cut -c1-64)"
