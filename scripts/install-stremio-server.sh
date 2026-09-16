#!/usr/bin/env bash
#
# Install the streaming server, and the Node it runs on, on the appliance.
#
# This is stream and torrent resolution: the catalogue asks it what a title can
# be played from, and it answers with something the player or Kodi can open. It
# is a separate concern from the product's own binaries, which is why it is a
# separate script — it changes when upstream changes and not when MediaBox does.
#
# Both artefacts are pinned in packaging/upstream.env and verified by checksum
# before they land, so a run either installs the exact reviewed bytes or fails.
# The server is a vendored bundle that nobody builds from source, upstream
# included, so a content hash is the only honest pin there is for it.
#
# This is what is left of the old scripts/deploy-mediabox.sh. The rest of that
# script installed the first architecture — a Python control plane and a web
# surface composed from upstream stremio-web — and both were replaced: the
# control plane by rust/crates/mediaboxd-rs, the surface by
# rust/crates/mediabox-ui. Neither the Python package nor the composed surface
# is in this repository any more, so the parts of that script that installed
# them could not run at all.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=env.sh
source "$here/scripts/env.sh"
# shellcheck source=../packaging/upstream.env
source "$here/packaging/upstream.env"

work="${MEDIABOX_BUILD_DIR:-/var/tmp/mediabox-build}"
mkdir -p "$work"
say() { printf '\n== %s\n' "$*"; }

say "node ${NODE_VERSION} (arm64)"
if ! mediabox_ssh "test -x $TARGET_NODE_DIR/bin/node && $TARGET_NODE_DIR/bin/node --version | grep -qx v$NODE_VERSION"; then
  tarball="$work/$NODE_TARBALL"
  [ -f "$tarball" ] || curl -sSL -o "$tarball" "$NODE_URL"
  echo "$NODE_SHA256  $tarball" | sha256sum -c -
  mediabox_scp "$tarball" "$MEDIABOX_TARGET:/var/tmp/$NODE_TARBALL"
  mediabox_ssh "set -e
    echo '$NODE_SHA256  /var/tmp/$NODE_TARBALL' | sha256sum -c -
    mkdir -p $TARGET_NODE_DIR
    tar -xJf /var/tmp/$NODE_TARBALL -C $TARGET_NODE_DIR --strip-components=1
    rm -f /var/tmp/$NODE_TARBALL"
fi

say "streaming server (${STREMIO_SHELL_TAG})"
server="$work/shell/$STREMIO_SERVER_RELPATH"
if [ ! -f "$server" ]; then
  [ -d "$work/shell/.git" ] || git clone --filter=blob:none "$STREMIO_SHELL_REPO" "$work/shell"
  git -C "$work/shell" checkout --quiet "$STREMIO_SHELL_REV"
fi
echo "$STREMIO_SERVER_SHA256  $server" | sha256sum -c -
mediabox_scp "$server" "$MEDIABOX_TARGET:/var/tmp/server.js"
mediabox_ssh "set -e
  echo '$STREMIO_SERVER_SHA256  /var/tmp/server.js' | sha256sum -c -
  mkdir -p $TARGET_SERVER_DIR $TARGET_SERVER_STATE
  install -m 0644 /var/tmp/server.js $TARGET_SERVER_DIR/server.js
  rm -f /var/tmp/server.js
  id stremio >/dev/null 2>&1 || useradd --system --home-dir $TARGET_SERVER_STATE --shell /usr/sbin/nologin stremio
  chown -R stremio:stremio $TARGET_SERVER_STATE"

say "unit"
# The start-up guard goes with the unit: the unit names it as an ExecStartPre,
# so shipping one without the other leaves the server pinned to whatever address
# it was installed on.
mediabox_scp "$here/packaging/stremio-unpin-address" "$MEDIABOX_TARGET:/var/tmp/"
mediabox_scp "$here/packaging/systemd/stremio-server.service" "$MEDIABOX_TARGET:/etc/systemd/system/"
mediabox_ssh "set -e
  mkdir -p $TARGET_PREFIX/bin
  install -m 0755 /var/tmp/stremio-unpin-address $TARGET_PREFIX/bin/stremio-unpin-address
  rm -f /var/tmp/stremio-unpin-address
  systemctl daemon-reload
  systemctl enable --now stremio-server.service"

say "state"
mediabox_ssh "systemctl is-active stremio-server; systemctl is-enabled stremio-server"
