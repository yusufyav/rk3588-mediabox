# mediabox-live's sources at one fixed path, for a release build that is the
# same bytes from any checkout. Sourced by build-mediabox-live.sh and
# test-mediabox-live-arm64.sh.
#
# mediabox-live is a workspace of its own (its lockfile apart from
# mediaboxd-rs's) and mediabox-core, ../mediabox-core, is outside it. Cargo
# puts the absolute path of such a path dependency into its package id, and
# so into the -C metadata it gives rustc, every symbol's hash and the order
# the code is laid out in. --remap-path-prefix takes the path out of the
# strings, not out of that: the same commit built in two worktrees differed
# from .text on, mediabox_core's metadata hash the only one apart (FAZ 2D,
# 2026-11-06). Built from one fixed path, the two came out the same.
#
#   mediabox_live_source <checkout>
#     exports the checkout's HEAD commit's rust/ (git archive: what is
#     committed, nothing untracked or changed) to $MEDIABOX_LIVE_SRC, holds a
#     lock on it until the calling script ends (one build at a time), and
#     puts the crate's directory there in $MEDIABOX_LIVE_CRATE. Called in
#     the script's own shell, not $(...): a subshell let the lock go as it
#     ended. Refused while rust/ has changes or
#     untracked files: a binary is the commit's or it is not built. Its
#     commit is in $MEDIABOX_LIVE_COMMIT.
#
# The fixed path is the same for every user and home; Cargo's own home
# ($CARGO_HOME, its registry's sources in panic locations) is remapped
# too. $MEDIABOX_LIVE_REMAP is the --remap-path-prefix pair for RUSTFLAGS.

MEDIABOX_LIVE_SRC="/tmp/rk3588-mediabox-build/mediabox-live-src"
MEDIABOX_LIVE_REMAP="--remap-path-prefix=$MEDIABOX_LIVE_SRC=. --remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo"

mediabox_live_source() {
  local checkout=$1
  if [ -n "$(git -C "$checkout" status --porcelain --untracked-files=all -- rust)" ]; then
    git -C "$checkout" status --short -- rust >&2
    echo "rust/ is not the commit's: commit or stash first" >&2
    return 1
  fi
  MEDIABOX_LIVE_COMMIT="$(git -C "$checkout" rev-parse HEAD)"
  mkdir -p "${MEDIABOX_LIVE_SRC%/*}"
  exec {mediabox_live_lock}>"$MEDIABOX_LIVE_SRC.lock"
  flock "$mediabox_live_lock"
  rm -rf "${MEDIABOX_LIVE_SRC:?}"
  mkdir -p "$MEDIABOX_LIVE_SRC"
  git -C "$checkout" archive "$MEDIABOX_LIVE_COMMIT" rust | tar -C "$MEDIABOX_LIVE_SRC" -xf -
  MEDIABOX_LIVE_CRATE="$MEDIABOX_LIVE_SRC/rust/crates/mediabox-live"
}
