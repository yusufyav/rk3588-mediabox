# Shared connection settings for the Orange Pi 5 Ultra target.
# Override any of these in the environment before sourcing.
: "${MEDIABOX_HOST:=10.27.27.25}"
: "${MEDIABOX_USER:=root}"
: "${MEDIABOX_SSH_KEY:=$HOME/.ssh/id_ed25519}"
: "${MEDIABOX_REMOTE_DIR:=/tmp/rk3588-mediabox}"
: "${MEDIABOX_FFMPEG_PREFIX:=/opt/rk3588-screenbridge}"

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
