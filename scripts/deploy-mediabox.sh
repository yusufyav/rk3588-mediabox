#!/usr/bin/env bash
#
# Install the MediaBox media surface and its streaming server on the appliance.
#
# Everything installed here is pinned in packaging/upstream.env and verified by
# checksum before it lands, so a deployment either installs the exact reviewed
# artefacts or fails.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=../packaging/upstream.env
source "$here/packaging/upstream.env"

host="${MEDIABOX_HOST:-10.27.27.25}"
key="${MEDIABOX_SSH_KEY:-$HOME/.ssh/id_ed25519}"
work="${MEDIABOX_BUILD_DIR:-/var/tmp/mediabox-build}"
ui="${MEDIABOX_DIST_DIR:-$work/ui}"

ssh_opts=(-F /dev/null -i "$key" -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o ConnectTimeout=10)
sh_() { ssh "${ssh_opts[@]}" "root@$host" "$@"; }
cp_() { scp "${ssh_opts[@]}" -q "$@"; }

say() { printf '\n== %s\n' "$*"; }

[ -f "$ui/index.html" ] || { echo "missing composed UI at $ui; run scripts/build-media-ui.sh"; exit 1; }
grep -q 'mediabox/shell.js' "$ui/index.html" || { echo "composed UI has no appliance shell"; exit 1; }

say "node ${NODE_VERSION} (arm64)"
if ! sh_ "test -x $TARGET_NODE_DIR/bin/node && $TARGET_NODE_DIR/bin/node --version | grep -qx v$NODE_VERSION"; then
  tarball="$work/$NODE_TARBALL"
  [ -f "$tarball" ] || curl -sSL -o "$tarball" "$NODE_URL"
  echo "$NODE_SHA256  $tarball" | sha256sum -c -
  cp_ "$tarball" "root@$host:/var/tmp/$NODE_TARBALL"
  sh_ "set -e
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
cp_ "$server" "root@$host:/var/tmp/server.js"
sh_ "set -e
  echo '$STREMIO_SERVER_SHA256  /var/tmp/server.js' | sha256sum -c -
  mkdir -p $TARGET_SERVER_DIR $TARGET_SERVER_STATE
  install -m 0644 /var/tmp/server.js $TARGET_SERVER_DIR/server.js
  rm -f /var/tmp/server.js
  id stremio >/dev/null 2>&1 || useradd --system --home-dir $TARGET_SERVER_STATE --shell /usr/sbin/nologin stremio
  chown -R stremio:stremio $TARGET_SERVER_STATE"

say "control plane"
tar -C "$here" -cf - mediaboxd | sh_ "rm -rf $TARGET_PREFIX/mediaboxd && mkdir -p $TARGET_PREFIX && tar -C $TARGET_PREFIX -xf -"

say "media surface"
tar -C "$ui" -cf - . | sh_ "rm -rf $TARGET_WEBUI_DIR && mkdir -p $TARGET_WEBUI_DIR && tar -C $TARGET_WEBUI_DIR -xf -"

say "units"
# The streaming server's start-up guard goes with its unit: the unit names it
# as an ExecStartPre, so shipping one without the other leaves the server
# pinned to whatever address it was installed on.
cp_ "$here/packaging/stremio-unpin-address" "root@$host:/var/tmp/"
sh_ "install -m 0755 /var/tmp/stremio-unpin-address $TARGET_PREFIX/bin/stremio-unpin-address && \
     rm -f /var/tmp/stremio-unpin-address"
cp_ "$here/packaging/systemd/mediaboxd.service" "$here/packaging/systemd/stremio-server.service" \
    "root@$host:/etc/systemd/system/"
[ -n "${MEDIABOX_SKIP_CONFIG:-}" ] || cp_ "$here/config/mediaboxd.example.toml" "root@$host:/etc/mediaboxd.toml.new"
sh_ "set -e
  test -f /etc/mediaboxd.toml || mv /etc/mediaboxd.toml.new /etc/mediaboxd.toml
  rm -f /etc/mediaboxd.toml.new
  systemctl daemon-reload
  systemctl enable --now stremio-server.service
  systemctl restart mediaboxd.service"

say "state"
sh_ "systemctl is-active stremio-server mediaboxd; systemctl is-enabled stremio-server mediaboxd"
