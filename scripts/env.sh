# Shared connection and layout settings for an RK3588 MediaBox appliance.
# Override any of these in the environment before sourcing.

# There is no default address, and that is deliberate. This product runs on more
# than one board and the boards move between networks; a baked-in address is a
# script that quietly deploys to yesterday's machine. Name the appliance.
if [ -z "${MEDIABOX_HOST:-}" ]; then
  echo "MEDIABOX_HOST is not set." >&2
  echo "  export MEDIABOX_HOST=<the appliance's address>" >&2
  return 1 2>/dev/null || exit 1
fi
: "${MEDIABOX_USER:=root}"
: "${MEDIABOX_SSH_KEY:=$HOME/.ssh/id_ed25519}"
: "${MEDIABOX_REMOTE_DIR:=/tmp/rk3588-mediabox}"

# Where the product lives on the appliance.
: "${MEDIABOX_PREFIX:=/opt/rk3588-mediabox}"

# MediaBox's own hardware media runtime: Rockchip MPP, librga and the FFmpeg
# fork that drives them. Built by scripts/build-media-runtime.sh from the pins
# recorded in docs/platform/custom-runtime.md.
#
# This used to be /opt/rk3588-screenbridge — the other product's prefix, shared
# because the Ultra had only one of the two products on it. It is not shared any
# more: on a board carrying both, whichever was built last would own the other's
# decoder. MediaBox neither reads nor writes that prefix now.
: "${MEDIABOX_MEDIA_PREFIX:=$MEDIABOX_PREFIX/media-runtime}"

# The private Mali G610 user space, extracted rather than installed; see
# scripts/install-mali-runtime.sh.
: "${MALI_RUNTIME:=$MEDIABOX_PREFIX/mali-g24p0-runtime}"

# What a player needs on its library path: the vendor GL stack in front, this
# product's own hardware media libraries behind it.
: "${MEDIABOX_RUNTIME_LD_PATH:=$MALI_RUNTIME/lib:$MEDIABOX_MEDIA_PREFIX/lib}"

MEDIABOX_TARGET="${MEDIABOX_USER}@${MEDIABOX_HOST}"

mediabox_ssh() {
  ssh -F /dev/null -i "$MEDIABOX_SSH_KEY" -o IdentitiesOnly=yes \
      -o BatchMode=yes -o ConnectTimeout=10 "$MEDIABOX_TARGET" "$@"
}

mediabox_scp() {
  scp -F /dev/null -i "$MEDIABOX_SSH_KEY" -o IdentitiesOnly=yes \
      -o BatchMode=yes -o ConnectTimeout=10 "$@"
}

mediabox_rsync() {
  rsync -az --delete \
    -e "ssh -F /dev/null -i $MEDIABOX_SSH_KEY -o IdentitiesOnly=yes -o BatchMode=yes" "$@"
}
