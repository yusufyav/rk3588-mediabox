#!/usr/bin/env bash
#
# Open the television's interface in an ordinary desktop window.
#
#   tools/ui-preview/preview.sh home|media-settings|detail|<file.json> [viewer args]
#
# The window starts at MediaBoxWindow's preferred 1920x1080 and can be resized
# freely; the interface follows it, because app.slint feeds the window's size
# into Metrics.vw/vh. Saving any .slint file under ui/, or the fixture,
# reloads the open window.
set -euo pipefail

# shellcheck source=common.sh
source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

data="$(fixture_path "${1:-}")"
shift || true
bin="$(viewer)"

# slint-viewer 1.17.1 reloads when a file is written in place, but misses one
# an editor replaced by renaming a temporary file over it (Kate, JetBrains'
# safe write, Vim for most files). A touch afterwards is a change it does see,
# so a file whose inode changed is touched.
nudge() {
  local -A seen=()
  local inode file
  while sleep 0.5; do
    while read -r inode file; do
      if [ -n "${seen[$file]:-}" ] && [ "${seen[$file]}" != "$inode" ]; then
        touch "$file"
      fi
      seen[$file]=$inode
    done < <(stat -c '%i %n' "$(dirname "$app")"/*.slint "$data" 2>/dev/null)
  done
}
nudge &
nudger=$!
trap 'kill "$nudger" 2>/dev/null' EXIT

"$bin" --auto-reload \
  --component MediaBoxWindow \
  --load-data "$data" \
  "$@" \
  "$app"
