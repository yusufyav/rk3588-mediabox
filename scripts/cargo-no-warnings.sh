#!/usr/bin/env bash
#
# cargo, refusing every warning: the television's build is warning-free or
# it is no build.
#
#   scripts/cargo-no-warnings.sh <crate dir> <cargo subcommand> [args...]
#   e.g. scripts/cargo-no-warnings.sh rust/crates/mediabox-tv check --release --bins
#
# Three things say "warning", and each is turned into a failure where it is
# said:
#   - rustc's lints: cargo's `build.warnings = "deny"`, replayed warnings
#     from a cached build included;
#   - the Slint compiler's, which a build script prints as cargo:warning and
#     no rustc flag reaches: slint-build's SLINT_COMPILER_DENY_WARNINGS (the
#     crate's build.rs reruns when it changes);
#   - anything else cargo prints as a warning -- an unknown config key, a
#     build script's own: the output is read once the build is done, and a
#     line that starts with "warning" fails it. Nothing is filtered out of
#     what is shown.
#
# The build's own exit status is kept (pipefail); the log is a temporary
# file, removed on exit.
set -euo pipefail

[ $# -ge 2 ] || {
  echo "usage: cargo-no-warnings.sh <crate dir> <cargo subcommand> [args...]" >&2
  exit 2
}
dir=$1
shift

log="$(mktemp "${TMPDIR:-/tmp}/cargo-no-warnings.XXXXXX")"
trap 'rm -f "$log"' EXIT

export SLINT_COMPILER_DENY_WARNINGS=1
# --config before the subcommand: arguments after it may end in `--`, which
# hands everything past it to a test binary.
( cd "$dir" && cargo --config 'build.warnings="deny"' "$@" ) 2>&1 | tee "$log"

if grep -qE '^warning' "$log"; then
  echo "cargo-no-warnings: the build above printed a warning; refused" >&2
  exit 1
fi
