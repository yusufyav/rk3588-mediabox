# shellcheck shell=bash
# The aarch64 build environment: one image, made from pinned inputs only
# (packaging/build-image), and the way a build runs inside it. Sourced by
# every script that builds an aarch64 binary of the product.
#
# Why: a build that reads the developer's machine or an appliance is a
# build of that machine. Built against a sysroot rsynced off whichever board
# was at hand, with the host's own cross gcc and glibc and whatever "stable"
# Rust was that day, the same commit built on another machine failed
# (2026-11-18: orc-0.4.pc was in one sysroot by accident and not in the
# other), and where it built, it was not the same binary. Here the compiler,
# glibc, every library linked against and Rust are the image's, the image
# is named by the hash of what it is made from, and the repository is at
# one path, /work, on every machine.
#
#   mediabox_build_image
#     the image's name in $MEDIABOX_BUILD_IMAGE, built first if this
#     machine does not have it (a few minutes, once per change of
#     packaging/build-image or rust/rust-toolchain.toml).
#   mediabox_build_run [-w <dir in /work>] [-g] [-v <host>:<ctr>[:ro]]... -- <bash>
#     runs <bash> in the image as the caller's uid: the checkout at /work,
#     Cargo's registry at /cargo ($MEDIABOX_CARGO_HOME on this machine),
#     -g the GStreamer runtime (build/gstreamer-runtime) at its prefix.
#   mediabox_cargo_target <name>
#     where a build's output lands on this machine: build/cargo-arm64/<name>,
#     /work/build/cargo-arm64/<name> inside. Apart from target/: those hold
#     host builds by another compiler.

mediabox_build_here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=../packaging/build-image/pins.env
source "$mediabox_build_here/packaging/build-image/pins.env"
MEDIABOX_CARGO_HOME="${MEDIABOX_CARGO_HOME:-/var/tmp/rk3588-mediabox/cargo-home}"
MEDIABOX_GST_PREFIX=/opt/rk3588-mediabox/gstreamer-1.26

mediabox_build_engine() {
  command -v docker || command -v podman || {
    echo "docker or podman is needed to build for the appliance" >&2
    return 1
  }
}

# rust/rust-toolchain.toml's value for <key>, a string or a list, as words.
mediabox_toolchain_value() {
  sed -n "s/^$1[[:space:]]*=[[:space:]]*//p" "$mediabox_build_here/rust/rust-toolchain.toml" | tr -d '[]",'
}

mediabox_build_image() {
  local engine context tag
  engine="$(mediabox_build_engine)" || return 1
  context="$mediabox_build_here/packaging/build-image"
  tag="$(cat "$context/Containerfile" "$context/arm64-packages.txt" "$context/pins.env" \
           "$mediabox_build_here/rust/rust-toolchain.toml" | sha256sum | cut -c1-16)"
  MEDIABOX_BUILD_IMAGE="localhost/rk3588-mediabox-build:$tag"
  if ! "$engine" image inspect "$MEDIABOX_BUILD_IMAGE" >/dev/null 2>&1; then
    echo "== build image $MEDIABOX_BUILD_IMAGE (once; Debian $MEDIABOX_BUILD_SNAPSHOT, Rust $(mediabox_toolchain_value channel))" >&2
    "$engine" build --platform linux/amd64 -q -t "$MEDIABOX_BUILD_IMAGE" -f "$context/Containerfile" \
      --build-arg BASE_IMAGE="$MEDIABOX_BUILD_BASE_IMAGE" \
      --build-arg SNAPSHOT="$MEDIABOX_BUILD_SNAPSHOT" \
      --build-arg RUSTUP_VERSION="$MEDIABOX_RUSTUP_VERSION" \
      --build-arg RUSTUP_SHA256="$MEDIABOX_RUSTUP_SHA256" \
      --build-arg RUST_VERSION="$(mediabox_toolchain_value channel)" \
      --build-arg RUST_COMPONENTS="$(mediabox_toolchain_value components)" \
      --build-arg RUST_TARGETS="$(mediabox_toolchain_value targets)" \
      "$context" >/dev/null
  fi
}

mediabox_cargo_target() {
  printf '%s\n' "$mediabox_build_here/build/cargo-arm64/$1"
}

# build/gstreamer-runtime's tarball unpacked once per tarball, for -g.
mediabox_gst_runtime_root() {
  local tarball="$mediabox_build_here/build/gstreamer-runtime/gstreamer-1.26.11-aarch64.tar.xz"
  local root="$mediabox_build_here/build/gstreamer-runtime/root"
  [ -e "$tarball" ] || { echo "no GStreamer runtime at $tarball (scripts/build-gstreamer-runtime.sh)" >&2; return 1; }
  local sum; sum="$(sha256sum < "$tarball" | cut -c1-64)"
  if [ "$(cat "$root/.tarball.sha256" 2>/dev/null)" != "$sum" ]; then
    rm -rf "${root:?}"
    mkdir -p "$root"
    tar -C "$root" -xJf "$tarball"
    echo "$sum" > "$root/.tarball.sha256"
  fi
  printf '%s\n' "$root$MEDIABOX_GST_PREFIX"
}

mediabox_build_run() {
  local engine workdir=/work args=()
  engine="$(mediabox_build_engine)" || return 1
  mediabox_build_image || return 1
  while [ $# -gt 0 ]; do
    case "$1" in
      -w) workdir="$2"; shift 2 ;;
      -v) args+=(-v "$2"); shift 2 ;;
      -g) args+=(-v "$(mediabox_gst_runtime_root):$MEDIABOX_GST_PREFIX:ro"); shift ;;
      --) shift; break ;;
      *) echo "mediabox_build_run: $1?" >&2; return 2 ;;
    esac
  done
  mkdir -p "$MEDIABOX_CARGO_HOME" "$mediabox_build_here/build/cargo-arm64"
  case "${engine##*/}" in
    podman) args+=(--userns=keep-id) ;;
    *) args+=(--user "$(id -u):$(id -g)") ;;
  esac
  "$engine" run --rm --platform linux/amd64 "${args[@]}" \
    -v "$mediabox_build_here:/work" -v "$MEDIABOX_CARGO_HOME:/cargo" \
    -e GIT_CONFIG_COUNT=1 -e GIT_CONFIG_KEY_0=safe.directory -e GIT_CONFIG_VALUE_0='*' \
    -w "$workdir" "$MEDIABOX_BUILD_IMAGE" bash -euo pipefail -c "$1"
}
