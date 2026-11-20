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
#     copies the checkout's rust/ as it is in the working tree -- tracked
#     files with their uncommitted changes, and untracked files that are not
#     ignored; never what .gitignore leaves out (target/, build output) -- to
#     $MEDIABOX_LIVE_SRC, holds a lock on it until the calling script ends
#     (one build at a time), and puts the crate's directory there in
#     $MEDIABOX_LIVE_CRATE. Called in the script's own shell, not $(...): a
#     subshell let the lock go as it ended.
#
#     What it was built from: $MEDIABOX_LIVE_COMMIT (HEAD),
#     $MEDIABOX_LIVE_DIRTY (1 when rust/ differs from it), $MEDIABOX_LIVE_SNAPSHOT
#     (sha256 over every copied file's path and contents, sorted) and
#     $MEDIABOX_LIVE_SOURCE: the commit for a clean tree, "<commit>+dirty"
#     otherwise -- never the commit alone for what is not the commit's. A
#     dirty tree is built, and said so; it is a development binary, traced by
#     its snapshot, not refused.
#
# The fixed path is the same for every user and home, and so is Cargo's own
# home in the build image (/cargo, scripts/build-container.sh): its
# registry's sources in panic locations need no remapping.
# $MEDIABOX_LIVE_REMAP is the --remap-path-prefix pair for RUSTFLAGS.

MEDIABOX_LIVE_SRC="/tmp/rk3588-mediabox-build/mediabox-live-src"
MEDIABOX_LIVE_REMAP="--remap-path-prefix=$MEDIABOX_LIVE_SRC=."

mediabox_live_source() {
  local checkout=$1
  MEDIABOX_LIVE_COMMIT="$(git -C "$checkout" rev-parse HEAD)"
  if [ -n "$(git -C "$checkout" status --porcelain --untracked-files=normal -- rust)" ]; then
    MEDIABOX_LIVE_DIRTY=1
    MEDIABOX_LIVE_SOURCE="$MEDIABOX_LIVE_COMMIT+dirty"
  else
    MEDIABOX_LIVE_DIRTY=0
    MEDIABOX_LIVE_SOURCE="$MEDIABOX_LIVE_COMMIT"
  fi
  mkdir -p "${MEDIABOX_LIVE_SRC%/*}"
  exec {mediabox_live_lock}>"$MEDIABOX_LIVE_SRC.lock"
  flock "$mediabox_live_lock"
  rm -rf "${MEDIABOX_LIVE_SRC:?}"
  mkdir -p "$MEDIABOX_LIVE_SRC"
  # Tracked files as the working tree has them, and untracked ones not
  # ignored. A tracked file deleted in the tree is not there to copy.
  ( cd "$checkout" &&
    git ls-files -z --cached --others --exclude-standard -- rust |
      while IFS= read -r -d '' path; do
        if [ -e "$path" ] || [ -L "$path" ]; then printf '%s\0' "$path"; fi
      done |
      tar --null -T - -cf - ) | tar -C "$MEDIABOX_LIVE_SRC" -xf -
  MEDIABOX_LIVE_SNAPSHOT="$(cd "$MEDIABOX_LIVE_SRC" &&
    find rust \( -type f -o -type l \) -print0 | LC_ALL=C sort -z | xargs -0 sha256sum | sha256sum | cut -c1-64)"
  MEDIABOX_LIVE_CRATE="$MEDIABOX_LIVE_SRC/rust/crates/mediabox-live"
}
