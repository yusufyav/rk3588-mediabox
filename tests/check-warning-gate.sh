#!/usr/bin/env bash
#
# The television's warning gate (scripts/cargo-no-warnings.sh), on a scratch
# copy of the Rust tree -- never the tree itself, which keeps no planted
# warning:
#   - as it is, the gate passes;
#   - with a rustc warning planted (an unused function), it fails;
#   - with a Slint warning planted (the wireless spinner's rotation back on
#     its component's root, as it was before 65ffbe1), it fails.
#
# A host `cargo check` of the binary, so no sysroot and no device. It builds
# the crate's dependencies once, into WARNING_GATE_TARGET if given (a
# directory to reuse between runs) or a temporary one.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
gate="$here/scripts/cargo-no-warnings.sh"
failures=0

report() { printf 'FAIL %s\n' "$*"; failures=$((failures + 1)); }
okay()   { printf 'ok   %s\n' "$*"; }

work="$(mktemp -d "${TMPDIR:-/tmp}/mediabox-warning-gate-XXXXXX")"
trap 'rm -rf "$work"' EXIT
export CARGO_TARGET_DIR="${WARNING_GATE_TARGET:-$work/target}"

# The sources as they are now, committed or not, without anyone's build output.
rsync -a --exclude target "$here/rust/" "$work/rust/"
# The model tests read a fixture from outside rust/.
mkdir -p "$work/media/tests"
cp -a "$here/media/tests/data" "$work/media/tests/" 2>/dev/null || true
crate="$work/rust/crates/mediabox-tv"
main_rs="$crate/src/main.rs"
wireless="$crate/ui/wireless.slint"
cp "$main_rs" "$work/main.rs.clean"
cp "$wireless" "$work/wireless.slint.clean"

gate_check() { "$gate" "$crate" check --locked --release --bins >"$work/out" 2>&1; }

if gate_check; then
  okay "the tree as it is passes the gate"
else
  report "the tree as it is fails the gate:"
  grep -E '^(warning|error)' "$work/out" | head -20 | sed 's/^/     /'
fi

printf '\nfn planted_by_the_warning_gate_test() {}\n' >>"$main_rs"
if gate_check; then
  report "a planted rustc warning passes the gate"
elif grep -q 'planted_by_the_warning_gate_test' "$work/out"; then
  okay "a planted rustc warning fails the gate, and is shown"
else
  report "the gate failed, but not on the planted rustc warning:"
  tail -20 "$work/out" | sed 's/^/     /'
fi
cp "$work/main.rs.clean" "$main_rs"

git -C "$here" show 65ffbe1^:rust/crates/mediabox-tv/ui/wireless.slint >"$wireless"
if gate_check; then
  report "a planted Slint warning passes the gate"
elif grep -q 'transform-rotation property cannot be used on the root element' "$work/out"; then
  okay "a planted Slint warning fails the gate, and is shown"
else
  report "the gate failed, but not on the planted Slint warning:"
  tail -20 "$work/out" | sed 's/^/     /'
fi
cp "$work/wireless.slint.clean" "$wireless"

if [ "$failures" -gt 0 ]; then
  echo "$failures check(s) failed"
  exit 1
fi
