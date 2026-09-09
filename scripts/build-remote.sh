#!/usr/bin/env bash
# Syncs this repository to the target and builds it there.
#
# The target already has gcc, cmake, pkg-config, libdrm-dev and an
# RKMPP-enabled FFmpeg, so nothing is installed by this script.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=env.sh
source "$here/scripts/env.sh"

echo "== syncing $here -> $MEDIABOX_TARGET:$MEDIABOX_REMOTE_DIR"
mediabox_ssh "mkdir -p '$MEDIABOX_REMOTE_DIR'"
mediabox_rsync --exclude '.git' --exclude 'build' --exclude 'assets' \
  "$here/" "$MEDIABOX_TARGET:$MEDIABOX_REMOTE_DIR/"

echo "== configuring and building on target"
mediabox_ssh "cmake -S '$MEDIABOX_REMOTE_DIR' -B '$MEDIABOX_REMOTE_DIR/build' \
    -DCMAKE_BUILD_TYPE=Release \
    -DMEDIABOX_FFMPEG_PREFIX='$MEDIABOX_FFMPEG_PREFIX' >/dev/null && \
  cmake --build '$MEDIABOX_REMOTE_DIR/build' -j\$(nproc)"

mediabox_ssh "ls -l '$MEDIABOX_REMOTE_DIR/build/hdr-signaling-probe'"
