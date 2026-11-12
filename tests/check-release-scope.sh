#!/usr/bin/env bash
#
# What of the golden board's prefix is the product, checked on a synthetic one.
#
# rollback/ is the install-time undo history the deploy scripts keep beside the
# product. It is not the product, so a candidate's copy kept there must not
# fail the product verifier, and the release must leave it out of the manifest,
# the digest list, the ELF list and the archive alike -- the same paths in the
# manifest as in the tar, or the archive's SHA256SUMS does not describe it.
#
# Development leftovers outside it are the opposite case: they are on the
# board, so the verifier and the capture's gate must fail on them rather than
# the archive quietly leaving them out.
#
# Each property is also checked against a mutation of the code that holds it
# (the rollback exclusion removed), which has to make this test fail: a check
# that still passes with the rule gone was not checking the rule.
#
# Needs no board. Runs the real packaging/mediabox-product-verify and the real
# functions of scripts/release/release-scope.sh.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
verifier="$here/packaging/mediabox-product-verify"
scope="$here/scripts/release/release-scope.sh"
failures=0

report() { printf 'FAIL %s\n' "$*"; failures=$((failures + 1)); }
okay()   { printf 'ok   %s\n' "$*"; }

work="$(mktemp -d "${TMPDIR:-/tmp}/mediabox-release-scope-XXXXXX")"
trap 'rm -rf "$work"' EXIT

prefix=/opt/rk3588-mediabox

# A board root with a product in it and an undo history beside it, the history
# holding what the Plus's did: a candidate's copy and an ELF object.
make_board() {
  local root="$1" p="$1$prefix"
  rm -rf "$root"
  mkdir -p "$p/bin" "$p/lib" "$p/media/core/__pycache__" "$p/rollback/mediabox-tv-x/bin"
  printf '\177ELF product' >"$p/bin/mediabox-live"
  printf '\177ELF library' >"$p/lib/libthing.so.1"
  ln -s libthing.so.1 "$p/lib/libthing.so"
  printf 'print(1)\n' >"$p/media/core/main.py"
  printf 'cache' >"$p/media/core/__pycache__/main.cpython-313.pyc"
  printf '\177ELF old' >"$p/rollback/mediabox-tv-x/mediabox-tv.candidate-10b18f4"
  printf '\177ELF old' >"$p/rollback/mediabox-tv-x/bin/mediabox-tv"
  printf 'abc  bin/mediabox-tv\n' >"$p/rollback/mediabox-tv-x/SHA256SUMS"
}

# The verifier's own line for the leftovers, nothing else of its report: the
# synthetic prefix is not a product and fails everything else.
verify_leftovers() {
  local v="$1" root="$2"
  MEDIABOX_VERIFY_ROOT="$root" MEDIABOX_VERIFY_META="$work/meta-none" \
    MEDIABOX_VERIFY_STATIC_ONLY=1 sh "$v" 2>&1 | grep 'no staging leftovers'
}

# expect <PASS|FAIL> <what> <verifier> <root>
expect_verify() {
  local want="$1" what="$2" line
  line="$(verify_leftovers "$3" "$4")"
  case "$line" in
    *"$want"*) okay "verifier $want: $what" ;;
    *) report "verifier: $what: wanted $want, got '${line:-no line}'" ;;
  esac
}

# The capture's gate, from the scope file given.
gate_leftovers() { ( . "$1"; release_leftovers "$2" "$prefix" ); }
expect_gate() {
  local want="$1" what="$2" out
  out="$(gate_leftovers "$3" "$4")"
  if { [ "$want" = PASS ] && [ -z "$out" ]; } || { [ "$want" = FAIL ] && [ -n "$out" ]; }; then
    okay "capture gate $want: $what"
  else
    report "capture gate: $what: wanted $want, got '$(printf '%s' "$out" | tr '\n' ' ')'"
  fi
}

# --------------------------------------------------------------- 1 and 2, 3
board="$work/board"
for v in verify gate; do
  check="expect_$v"; tool="$verifier"; [ "$v" = gate ] && tool="$scope"
  make_board "$board"
  "$check" PASS "a candidate kept under rollback/" "$tool" "$board"
  make_board "$board"; printf x >"$board$prefix/lib/libthing.so.candidate-y"
  "$check" FAIL "a candidate outside rollback/" "$tool" "$board"
  make_board "$board"; printf x >"$board$prefix/bin/foo.pre-faz3j-x"
  "$check" FAIL "bin/foo.pre-faz3j-x" "$tool" "$board"
  make_board "$board"; mkdir -p "$board$prefix/media.old/stremio"
  "$check" FAIL "media.old" "$tool" "$board"
  make_board "$board"; mkdir -p "$board$prefix/media.subtitles-test"
  "$check" FAIL "media.subtitles-test" "$tool" "$board"
done

# ------------------------------------------------------------------------ 4
# The capture's inventory and archive, run from a staging directory the way the
# board runs them. Prints nothing and returns 0 when the scope holds.
capture_scope() {
  local scopefile="$1" stage="$work/stage" bad=0
  rm -rf "$stage"; mkdir -p "$stage/meta" "$stage/rootfs/etc/mediabox"
  printf 'x\n' >"$stage/rootfs/etc/mediabox/x.conf"
  : >"$stage/meta/artifact-manifest.tsv"; : >"$stage/meta/SHA256SUMS"; : >"$work/elf"
  ( . "$scopefile"
    release_inventory "$board" "$prefix" "$stage/meta" "$work/elf"
    cd "$stage" && release_tar "$board" "$prefix" ) >"$work/archive.tar" || { echo "capture failed"; return 1; }

  for f in "$stage/meta/artifact-manifest.tsv" "$stage/meta/SHA256SUMS" "$work/elf"; do
    grep -q 'rollback' "$f" && { echo "rollback in ${f##*/}"; bad=1; }
  done
  tar -tf "$work/archive.tar" >"$work/tar.list"
  grep -q 'rollback' "$work/tar.list" && { echo "rollback in the tar"; bad=1; }

  # The product part of the prefix, path for path: manifest = tar.
  awk -F'\t' '{print $5}' "$stage/meta/artifact-manifest.tsv" | sort >"$work/m.list"
  sed 's,/$,,' "$work/tar.list" | grep "^rootfs$prefix/" | sort >"$work/t.list"
  if ! cmp -s "$work/m.list" "$work/t.list"; then
    echo "manifest and tar differ:"; diff "$work/m.list" "$work/t.list" | sed 's/^/  /'; bad=1
  fi
  # And the product is in both, so an empty capture cannot pass.
  grep -qx "rootfs$prefix/bin/mediabox-live" "$work/m.list" || { echo "the product is missing"; bad=1; }
  grep -qx "$board$prefix/bin/mediabox-live" "$work/elf" || { echo "the product's ELF is missing"; bad=1; }
  return "$bad"
}

make_board "$board"
if out="$(capture_scope "$scope")"; then
  okay "capture: rollback/ out of the manifest, SHA256SUMS, ELF list and tar; manifest = tar"
else
  report "capture scope: $out"
fi

# ------------------------------------------------------------------------ 5
mutant="$work/mutant"; mkdir -p "$mutant"
sed 's/^RELEASE_NOT_PRODUCT="rollback"$/RELEASE_NOT_PRODUCT=""/' "$scope" >"$mutant/release-scope.sh"
if cmp -s "$scope" "$mutant/release-scope.sh"; then
  report "mutation: could not remove the rollback exclusion from release-scope.sh"
else
  make_board "$board"
  if capture_scope "$mutant/release-scope.sh" >/dev/null; then
    report "mutation: capture check still passes with the rollback exclusion removed"
  else
    okay "mutation: capture check fails with the rollback exclusion removed"
  fi
  make_board "$board"
  [ -z "$(gate_leftovers "$mutant/release-scope.sh" "$board")" ] \
    && report "mutation: capture gate still passes rollback/ with the exclusion removed" \
    || okay "mutation: capture gate fails on rollback/ with the exclusion removed"
fi

sed 's| -path "\$P/rollback" -prune -o||' "$verifier" >"$mutant/mediabox-product-verify"
if cmp -s "$verifier" "$mutant/mediabox-product-verify"; then
  report "mutation: could not remove the rollback prune from the verifier"
else
  make_board "$board"
  case "$(verify_leftovers "$mutant/mediabox-product-verify" "$board")" in
    *FAIL*) okay "mutation: verifier fails on rollback/ with the prune removed" ;;
    *)      report "mutation: verifier still passes rollback/ with the prune removed" ;;
  esac
fi

if [ "$failures" -eq 0 ]; then echo "RELEASE_SCOPE=PASS"; exit 0; fi
echo "RELEASE_SCOPE=FAIL ($failures)"; exit 1
