#!/usr/bin/env bash
#
# The production installer does not compile anything.
#
# This is the one property of the install path that cannot be checked by running
# it: a build step that only appears on a board without some package, or in the
# error branch nobody reaches, is exactly the step that will appear the first
# time someone installs on a clean disk. So it is checked in the text.
#
# The check is for a *command*, not for a word. Comments and quoted strings are
# removed first, because the installer legitimately mentions gcc and cargo -- in
# the sentence that explains why it refuses to run them, and in the pattern that
# rejects a compiler in the runtime package list. A test that cannot tell those
# apart is a test that gets disabled.
#
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# The production install path: the entry point and every product helper it runs.
targets=(
  scripts/install/install-mediabox.sh
  packaging/mediabox-product-verify
  releases/current.env
)

forbidden='cargo|rustc|gcc|g\+\+|cc|c\+\+|clang|make|cmake|meson|ninja|ninja-build|dkms|pip|pip3'
forbidden_scripts='build-media-runtime|build-mediabox-player|build-kodi|build-mediabox-tv|build-mediabox-ui'

failures=0
report() { printf 'FAIL %s\n' "$*"; failures=$((failures + 1)); }

# Strip comments and quoted text, then look only where a command can begin: the
# start of a line, or just after ; | & ( { or a command substitution.
strip() {
  sed -E \
    -e 's/(^|[[:space:]])#.*$/\1/' \
    -e "s/'[^']*'/''/g" \
    -e 's/"[^"]*"/""/g' "$1"
}

echo "-- the production installer invokes no build"
for t in "${targets[@]}"; do
  [ -e "$here/$t" ] || { report "$t is missing from the repository"; continue; }
  hits="$(strip "$here/$t" |
    grep -nE "(^|[;&|(){]|&&|\|\||\\\$\()[[:space:]]*(sudo[[:space:]]+)?($forbidden)([[:space:]]|$)" \
    || true)"
  if [ -n "$hits" ]; then
    report "$t invokes a compiler or build tool:"
    printf '     %s\n' "$hits"
  else
    printf 'ok   %s invokes no compiler\n' "$t"
  fi

  hits="$(strip "$here/$t" | grep -nE "($forbidden_scripts)" || true)"
  if [ -n "$hits" ]; then
    report "$t calls a build script:"
    printf '     %s\n' "$hits"
  else
    printf 'ok   %s calls no build script\n' "$t"
  fi
done

# And the installer must not reach into scripts/ for anything at all: every
# build script this repository has lives there, and the install path having no
# business there is easier to keep true than a list of exceptions.
if strip "$here/scripts/install/install-mediabox.sh" | grep -qE '(^|[[:space:]])[^ ]*scripts/build-'; then
  report "the installer references scripts/build-*"
else
  echo "ok   the installer references no build script path"
fi

# The installer asks apt for runtime packages. Asking for a toolchain would be
# building by another route, so the package names are checked too.
if strip "$here/scripts/install/install-mediabox.sh" |
   grep -nE 'apt-get[^|;]*install[^|;]*(build-essential|[[:space:]](gcc|g\+\+|rustc|cargo|cmake|meson|ninja-build)[[:space:]])'; then
  report "the installer asks apt for a build toolchain"
else
  echo "ok   the installer asks apt for no toolchain"
fi

echo
if [ "$failures" -eq 0 ]; then
  echo "build-free installer: PASS"
else
  echo "build-free installer: FAIL ($failures)"
fi
exit $((failures > 0))
