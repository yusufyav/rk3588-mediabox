#!/usr/bin/env bash
#
# Render one fixture to a PNG, without a window.
#
#   tools/ui-preview/capture.sh home|media-settings|detail|<file.json> [out.png]
#
# The viewer's headless software renderer, at the window's preferred size
# (1920x1080). Lands in tools/ui-preview/out/ by default, which is not tracked.
set -euo pipefail

# shellcheck source=common.sh
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

data="$(fixture_path "${1:-}")"
out="${2:-$here/out/$(basename "$data" .json).png}"
mkdir -p "$(dirname "$out")"
bin="$(viewer)"

"$bin" --component MediaBoxWindow --load-data "$data" --screenshot "$out" "$app"
echo "$out"
