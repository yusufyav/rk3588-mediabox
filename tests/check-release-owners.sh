#!/usr/bin/env bash
#
# Which package a path the product resolves belongs to, on a fake dpkg-query.
#
# The capture once ended in the middle of its inventory with rc=1 and no
# message: libgstlibav reaches /usr/lib/<triplet>/libblas.so.3, an
# alternatives link dpkg does not know by that name, and a failed dpkg-query
# under set -e and pipefail took the whole capture with it. This runs the
# capture's own release_owner (scripts/release/release-owners.sh) under the
# same shell options, on:
#
#   a path dpkg owns as given,
#   an alternatives link whose readlink -f target dpkg owns,
#   a path nobody owns -- recorded, not fatal, and the gate fails on it;
#
# and checks the readlink -f fallback is what makes the second pass: with it
# removed, this test has to fail.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
owners="$here/scripts/release/release-owners.sh"
failures=0

report() { printf 'FAIL %s\n' "$*"; failures=$((failures + 1)); }
okay()   { printf 'ok   %s\n' "$*"; }

work="$(mktemp -d "${TMPDIR:-/tmp}/mediabox-release-owners-XXXXXX")"
trap 'rm -rf "$work"' EXIT

# A filesystem with the three kinds of path, and a dpkg database that knows
# only the paths packages installed.
fs="$work/fs"
mkdir -p "$fs/usr/lib/blas" "$fs/etc/alternatives" "$work/bin"
: >"$fs/usr/lib/libdirect.so.1"
: >"$fs/usr/lib/blas/libblas.so.3.12.1"
ln -s "$fs/usr/lib/blas/libblas.so.3.12.1" "$fs/etc/alternatives/libblas.so.3"
ln -s "$fs/etc/alternatives/libblas.so.3" "$fs/usr/lib/libblas.so.3"
: >"$fs/usr/lib/libnobody.so.1"
printf '%s\n' \
  "libdirect1:arm64, libdirect-extra:arm64: $fs/usr/lib/libdirect.so.1" \
  "diversion by libdirect1 from: $fs/usr/lib/libdirect.so.1" \
  "libblas3:arm64: $fs/usr/lib/blas/libblas.so.3.12.1" >"$work/dpkg.db"
cat >"$work/bin/dpkg-query" <<EOF
#!/bin/sh
[ "\$1" = -S ] || exit 2
grep -F ": \$2" "$work/dpkg.db" | grep -q "\$2\$" || { echo "dpkg-query: no path found matching pattern \$2" >&2; exit 1; }
grep "\$2\$" "$work/dpkg.db"
EOF
chmod +x "$work/bin/dpkg-query"
export PATH="$work/bin:$PATH"

# The capture's shape: a set -e, pipefail block of release_owner calls feeding
# the package list. Prints the list; the record is left in $work/unowned.
packages() {
  local lib="$1"; shift
  : >"$work/unowned"
  ( set -euo pipefail
    . "$lib"
    { for p in "$@"; do release_owner "$p" "$work/unowned"; done
    } | sed '/^$/d' | sort -u )
}

# --------------------------------------------------------------- owned as is
out="$(packages "$owners" "$fs/usr/lib/libdirect.so.1")"; rc=$?
if [ "$rc" -eq 0 ] && [ "$out" = "$(printf 'libdirect1\nlibdirect-extra\n' | sort -u)" ] && [ ! -s "$work/unowned" ]; then
  okay "a path dpkg owns as given: libdirect1, libdirect-extra"
else
  report "owned as given: rc=$rc packages='$(printf '%s' "$out" | tr '\n' ' ')'"
fi

# ------------------------------------------------------ alternatives link
out="$(packages "$owners" "$fs/usr/lib/libblas.so.3")"; rc=$?
if [ "$rc" -eq 0 ] && [ "$out" = libblas3 ] && [ ! -s "$work/unowned" ]; then
  okay "an alternatives link: libblas3 through readlink -f"
else
  report "alternatives link: rc=$rc packages='$out' unowned='$(cat "$work/unowned")'"
fi

# ---------------------------------------------------------- owned by nobody
out="$(packages "$owners" "$fs/usr/lib/libdirect.so.1" "$fs/usr/lib/libnobody.so.1" "$fs/usr/lib/libblas.so.3")"; rc=$?
if [ "$rc" -eq 0 ] && [ "$(cat "$work/unowned")" = "$fs/usr/lib/libnobody.so.1" ] \
   && printf '%s\n' "$out" | grep -qx libblas3; then
  okay "a path nobody owns: recorded, the rest of the list still written (rc=0)"
else
  report "unowned: rc=$rc unowned='$(cat "$work/unowned")' packages='$(printf '%s' "$out" | tr '\n' ' ')'"
fi
gate="$( . "$owners"; release_unowned_report "$work/unowned" )"; grc=$?
if [ "$grc" -ne 0 ] && printf '%s' "$gate" | grep -qF "unowned: $fs/usr/lib/libnobody.so.1"; then
  okay "the gate fails on it and names the path"
else
  report "gate on an unowned path: rc=$grc output='$gate'"
fi
: >"$work/unowned"
( . "$owners"; release_unowned_report "$work/unowned" ) >/dev/null \
  && okay "the gate passes an empty record" || report "the gate fails an empty record"
( . "$owners"; release_unowned_report "$work/absent" ) >/dev/null \
  && report "the gate passes a missing record" || okay "the gate fails a missing record"

# ------------------------------------------------------------------ mutation
mutant="$work/release-owners.sh"
sed 's/^    c="\$(readlink -f "\$p" 2>\/dev\/null)" || c=""$/    c=""/' "$owners" >"$mutant"
if cmp -s "$owners" "$mutant"; then
  report "mutation: could not remove the readlink -f fallback"
else
  out="$(packages "$mutant" "$fs/usr/lib/libblas.so.3")"
  if [ "$out" = libblas3 ] && [ ! -s "$work/unowned" ]; then
    report "mutation: the alternatives link still resolves without readlink -f"
  else
    okay "mutation: without readlink -f the alternatives link is unowned"
  fi
fi

if [ "$failures" -eq 0 ]; then echo "RELEASE_OWNERS=PASS"; exit 0; fi
echo "RELEASE_OWNERS=FAIL ($failures)"; exit 1
