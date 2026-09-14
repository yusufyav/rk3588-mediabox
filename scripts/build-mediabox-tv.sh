#!/usr/bin/env bash
#
# Build the native television interface for the appliance.
#
# The appliance carries no Rust toolchain, so this cross-compiles here and ships
# a binary — the same arrangement the control plane already has.
#
# What is different from the control plane's build is the sysroot. This host's
# cross glibc is newer than the appliance's (2.44 against 2.41), and linking a
# Slint client against the newer one produces references to symbol versions the
# television does not have. So the link is done against the appliance's own
# libraries, copied here once. That also supplies libwayland-client,
# libxkbcommon and libEGL if any crate in the tree asks for them at link time
# rather than dlopen'ing them.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
crate="$here/rust/crates/mediabox-tv"
triple="aarch64-unknown-linux-gnu"
sysroot="${MEDIABOX_SYSROOT:-$here/.sysroot/aarch64-trixie}"

host="${MEDIABOX_HOST:-10.27.27.25}"
user="${MEDIABOX_USER:-root}"
key="${MEDIABOX_SSH_KEY:-$HOME/.ssh/id_ed25519}"
ssh_opts=(-F /dev/null -i "$key" -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o ConnectTimeout=10)

say() { printf '\n== %s\n' "$*"; }

command -v aarch64-linux-gnu-gcc >/dev/null || {
  echo "aarch64-linux-gnu-gcc not found; install it from the official repositories:" >&2
  echo "  sudo pacman -S --needed aarch64-linux-gnu-gcc aarch64-linux-gnu-binutils" >&2
  exit 1
}
rustup target list --installed | grep -qx "$triple" || {
  echo "missing target; run: rustup target add $triple" >&2
  exit 1
}

# The sysroot is fetched once. Delete the directory to refresh it after the
# appliance is upgraded.
if [ ! -e "$sysroot/usr/lib/aarch64-linux-gnu/libc.so.6" ]; then
  say "sysroot <- $user@$host"
  mkdir -p "$sysroot"
  rsync -a --delete-after \
    -e "ssh ${ssh_opts[*]}" \
    --exclude '*.ko' --exclude 'firmware' \
    "$user@$host":/lib/aarch64-linux-gnu \
    "$user@$host":/usr/lib/aarch64-linux-gnu \
    "$sysroot/usr/lib/" 2>/dev/null || {
      # rsync cannot take two remote sources in one call on every version.
      rsync -a -e "ssh ${ssh_opts[*]}" "$user@$host":/usr/lib/aarch64-linux-gnu/ "$sysroot/usr/lib/aarch64-linux-gnu/"
      rsync -a -e "ssh ${ssh_opts[*]}" "$user@$host":/lib/aarch64-linux-gnu/     "$sysroot/lib/aarch64-linux-gnu/"
    }
  rsync -a -e "ssh ${ssh_opts[*]}" "$user@$host":/usr/include/ "$sysroot/usr/include/"
  # Architecture-independent pkg-config metadata. Small, and freetype2.pc does
  # not resolve without it.
  mkdir -p "$sysroot/usr/share/pkgconfig"
  rsync -a -e "ssh ${ssh_opts[*]}" \
    "$user@$host":/usr/share/pkgconfig/ "$sysroot/usr/share/pkgconfig/"
  # Absolute symlinks inside the copied tree point at this host's own /lib.
  # Rewritten to be relative to the sysroot, or the cross linker follows them out.
  find "$sysroot" -type l | while read -r link; do
    dest="$(readlink "$link")"
    case "$dest" in
      /*) ln -sfn "$sysroot$dest" "$link" ;;
    esac
  done

  # The dynamic loader, where the linker looks for it.
  #
  # The appliance has merged /usr — /lib is a symlink to usr/lib — and the
  # loader is reached through /lib/ld-linux-aarch64.so.1, a symlink into
  # /lib/aarch64-linux-gnu/. Copying the directories does not bring the symlink
  # that sits beside them, and without it every link ends in
  # "cannot find /lib/ld-linux-aarch64.so.1 inside <sysroot>".
  ln -sfn aarch64-linux-gnu/ld-linux-aarch64.so.1 "$sysroot/lib/ld-linux-aarch64.so.1"
  ln -sfn aarch64-linux-gnu/ld-linux-aarch64.so.1 "$sysroot/usr/lib/ld-linux-aarch64.so.1"
fi

say "mediabox-tv ($triple, release)"

# Slint's font handling links fontconfig, and its -sys crate asks pkg-config
# where that is. Pointed at the appliance's own .pc files rather than this
# host's: LIBDIR replaces the search path entirely, so a host library cannot
# leak into an aarch64 link, and SYSROOT_DIR prefixes the paths they hand back.
#
# Both directories are named because Debian splits them: the architecture's own
# metadata lives under /usr/lib/<triple>/pkgconfig, but anything
# architecture-independent — bzip2.pc, which freetype2.pc requires — is in
# /usr/share/pkgconfig. Copying only the first is what made this fail with
# "Package bzip2 was not found", on a box that has libbz2 and its development
# files installed.
export PKG_CONFIG_ALLOW_CROSS=1
export PKG_CONFIG_SYSROOT_DIR="$sysroot"
export PKG_CONFIG_LIBDIR="$sysroot/usr/lib/aarch64-linux-gnu/pkgconfig:$sysroot/usr/share/pkgconfig"

export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS="-C link-arg=--sysroot=$sysroot -C link-arg=-Wl,-rpath-link=$sysroot/usr/lib/aarch64-linux-gnu"
export CC_aarch64_unknown_linux_gnu="aarch64-linux-gnu-gcc"
export CFLAGS_aarch64_unknown_linux_gnu="--sysroot=$sysroot"
( cd "$crate" && cargo build --release --target "$triple" )

bin="$crate/target/$triple/release/mediabox-tv"

# The appliance runs glibc 2.41. A binary that asks for anything newer links
# here and dies there, so it is checked here instead.
say "glibc symbol versions"
want=41
max="$(aarch64-linux-gnu-objdump -T "$bin" | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1)"
echo "highest referenced: $max"
minor="${max##*.}"
[ "$minor" -le "$want" ] || {
  echo "binary needs $max but the appliance has GLIBC_2.$want" >&2
  exit 1
}

printf '\n%-20s %s\n' mediabox-tv "$(du -h "$bin" | cut -f1)"
