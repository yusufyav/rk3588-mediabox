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
#     copies the checkout's rust/ (tracked files and untracked ones not
#     ignored: what the build would read there) to $MEDIABOX_LIVE_SRC, holds
#     a lock on it until the calling script ends (one build at a time), and
#     says the crate's directory there. Pass $MEDIABOX_LIVE_SRC to
#     --remap-path-prefix.
#
# The fixed path is under $HOME, as Cargo's registry is: the same user's
# builds agree; another user's differ in both.

MEDIABOX_LIVE_SRC="$HOME/.cache/rk3588-mediabox/mediabox-live-src"

mediabox_live_source() {
  local checkout=$1
  mkdir -p "${MEDIABOX_LIVE_SRC%/*}"
  exec {mediabox_live_lock}>"$MEDIABOX_LIVE_SRC.lock"
  flock "$mediabox_live_lock"
  rm -rf "${MEDIABOX_LIVE_SRC:?}"
  mkdir -p "$MEDIABOX_LIVE_SRC"
  git -C "$checkout" ls-files -z --cached --others --exclude-standard -- rust |
    tar -C "$checkout" --null -T - -cf - | tar -C "$MEDIABOX_LIVE_SRC" -xf -
  echo "$MEDIABOX_LIVE_SRC/rust/crates/mediabox-live"
}
