#!/usr/bin/env bash
#
# Build the MediaBox product UI.
#
# The result is architecture independent — WebAssembly runs the same on this
# machine and on the appliance — so this builds on the developer's host and
# ships bytes, with no Rust toolchain on the target.
#
# wasm-bindgen's generated glue must come from exactly the version of the
# wasm-bindgen crate the library was compiled against. That is checked here
# rather than discovered as a blank screen on a television.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
crate="$here/rust/crates/mediabox-ui"
out="${MEDIABOX_UI_DIST:-$here/rust/crates/mediabox-ui/dist}"

command -v wasm-bindgen >/dev/null || {
  echo "wasm-bindgen not found; install it with:" >&2
  echo "  cargo install wasm-bindgen-cli --version <crate version> --locked" >&2
  exit 1
}

crate_version="$(sed -n 's/^wasm-bindgen = "=\(.*\)"$/\1/p' "$crate/Cargo.toml")"
cli_version="$(wasm-bindgen --version | awk '{print $2}')"
[ -n "$crate_version" ] || { echo "wasm-bindgen is not pinned in $crate/Cargo.toml" >&2; exit 1; }
[ "$crate_version" = "$cli_version" ] || {
  echo "wasm-bindgen mismatch: crate pins $crate_version, CLI is $cli_version" >&2
  echo "  cargo install wasm-bindgen-cli --version $crate_version --locked --force" >&2
  exit 1
}

rustup target list --installed | grep -qx wasm32-unknown-unknown || {
  echo "missing target; run: rustup target add wasm32-unknown-unknown" >&2
  exit 1
}

echo "== compiling mediabox-ui (wasm32, release)"
( cd "$crate" && cargo build --release --target wasm32-unknown-unknown )

echo "== generating bindings -> $out"
rm -rf "$out"
mkdir -p "$out"
wasm-bindgen --target web --no-typescript --out-dir "$out" \
  "$crate/target/wasm32-unknown-unknown/release/mediabox_ui.wasm"

# The typeface ships with the bundle rather than being asked of the system.
# The appliance has DejaVu Sans and nothing else, so every name in the CSS font
# stack resolved to it and the whole product was drawn in a 2004 desktop font.
# It is served from the appliance's own loopback, so there is no third party in
# the path and the television works with no network at all.
install -m 0644 \
  "$crate/assets/index.html" \
  "$crate/assets/style.css" \
  "$crate/assets/InterVariable.woff2" \
  "$out/"

# The daemon serves only flat, known-safe filenames; anything nested would be
# refused at request time, so it is refused at build time instead.
find "$out" -mindepth 2 -print -quit | grep -q . && {
  echo "the UI bundle must be flat; nested files will not be served" >&2
  exit 1
}

echo
printf '%-28s %s\n' "$(basename "$out")" "$(du -sh "$out" | cut -f1)"
ls -lh "$out" | tail -n +2 | awk '{printf "  %-28s %s\n", $9, $5}'
