# Shared by preview.sh and capture.sh. Sourced, not run.
#
# The television's own interface, opened on this machine by Slint's own
# viewer: the same app.slint the appliance compiles, with a fixture standing in
# for what Rust would have set. Nothing here is built into mediabox-tv.

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
app="$repo/rust/crates/mediabox-tv/ui/app.slint"

fixtures() {
  (cd "$here/fixtures" && ls -1 ./*.json | sed 's|^\./||; s|\.json$||')
}

# A fixture by name (home, detail, ...) or a path to any JSON file of
# MediaBoxWindow properties.
fixture_path() {
  local name="${1:-}"
  if [ -z "$name" ]; then
    echo "usage: $(basename "$0") <fixture>  (one of: $(fixtures | tr '\n' ' '))" >&2
    exit 2
  fi
  if [ -f "$name" ]; then
    printf '%s\n' "$(cd "$(dirname "$name")" && pwd)/$(basename "$name")"
  elif [ -f "$here/fixtures/$name.json" ]; then
    printf '%s\n' "$here/fixtures/$name.json"
  else
    echo "no fixture '$name'; there is: $(fixtures | tr '\n' ' ')" >&2
    exit 2
  fi
}

# The viewer from the same Slint release the crate is built with, so a layout
# that looks right here is laid out by the same engine on the panel.
viewer() {
  local bin="${SLINT_VIEWER:-}"
  [ -n "$bin" ] || bin="$(command -v slint-viewer || true)"
  [ -n "$bin" ] || { [ -x "$HOME/.cargo/bin/slint-viewer" ] && bin="$HOME/.cargo/bin/slint-viewer"; }
  local want
  want="$(sed -n 's/^slint = { version = "\([^"]*\)".*/\1/p' "$repo/rust/crates/mediabox-tv/Cargo.toml")"
  if [ -z "$bin" ]; then
    echo "slint-viewer not found; install the crate's own version:" >&2
    echo "  cargo install slint-viewer --version ${want:-<see Cargo.toml>} --locked" >&2
    exit 1
  fi
  local have
  have="$("$bin" --version | awk '{print $2}')"
  if [ -n "$want" ] && [ "$have" != "$want" ]; then
    echo "warning: slint-viewer $have, but mediabox-tv uses Slint $want" >&2
  fi
  # The appliance draws in Inter. Without it the viewer falls back to another
  # face, and every width that elides or wraps is measured in the wrong font.
  # Not grep -q: it exits at the first match, fc-list dies of SIGPIPE, and
  # under pipefail an installed font reads as a missing one.
  if command -v fc-list >/dev/null && ! fc-list : family | grep -x 'Inter' >/dev/null; then
    echo "warning: the Inter font is not installed; text is measured in a fallback face" >&2
  fi
  printf '%s\n' "$bin"
}
