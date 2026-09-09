#!/usr/bin/env bash
# Copies the generated test assets to the target.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=env.sh
source "$here/scripts/env.sh"

src_dir="${1:-$here/assets}"
[ -d "$src_dir" ] || { echo "no asset directory $src_dir; run scripts/make-test-assets.sh" >&2; exit 1; }

mediabox_ssh "mkdir -p '$MEDIABOX_REMOTE_DIR/assets'"
mediabox_scp "$src_dir"/*.mp4 "$MEDIABOX_TARGET:$MEDIABOX_REMOTE_DIR/assets/"
mediabox_ssh "cd '$MEDIABOX_REMOTE_DIR/assets' && sha256sum *.mp4"
