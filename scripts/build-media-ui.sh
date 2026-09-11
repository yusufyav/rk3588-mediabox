#!/usr/bin/env bash
#
# Build the MediaBox media surface: upstream Stremio plus the appliance shell,
# composed into the single static root mediaboxd serves at /ui/.
#
# The composition is the whole integration. Upstream's build output is copied
# unmodified and its page gains exactly two lines: a stylesheet and a script for
# the appliance shell. There is no fork, no patched bundle, and no iframe — the
# shell and the media app share one document and one origin.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=../packaging/upstream.env
source "$here/packaging/upstream.env"

work="${MEDIABOX_BUILD_DIR:-/var/tmp/mediabox-build}"
out="${MEDIABOX_DIST_DIR:-$work/ui}"
mkdir -p "$work"

say() { printf '\n== %s\n' "$*"; }

say "upstream stremio-web @ ${STREMIO_WEB_REV:0:12} (${STREMIO_WEB_VERSION})"
if [ ! -d "$work/stremio-web/.git" ]; then
  git clone --filter=blob:none "$STREMIO_WEB_REPO" "$work/stremio-web"
fi
git -C "$work/stremio-web" fetch --quiet origin "$STREMIO_WEB_REV" 2>/dev/null || true
git -C "$work/stremio-web" checkout --quiet "$STREMIO_WEB_REV"
actual_rev="$(git -C "$work/stremio-web" rev-parse HEAD)"
[ "$actual_rev" = "$STREMIO_WEB_REV" ] || { echo "stremio-web revision mismatch: $actual_rev"; exit 1; }

# The lockfile is honoured, so the dependency tree is the pinned revision's own.
if [ ! -x "$work/tools/node_modules/.bin/pnpm" ]; then
  npm install --prefix "$work/tools" "pnpm@$PNPM_VERSION" --no-audit --no-fund >/dev/null
fi
pnpm="$work/tools/node_modules/.bin/pnpm"
( cd "$work/stremio-web" && "$pnpm" install --frozen-lockfile && "$pnpm" run build )

say "appliance shell"
( cd "$here/webui" && npm run build )

say "compose $out"
rm -rf "$out"
mkdir -p "$out"
cp -a "$work/stremio-web/build/." "$out/"
cp -a "$here/webui/dist/mediabox" "$out/mediabox"

# The service worker would cache the media app's page across deployments and
# serve a shell-less copy of it after an update. The appliance is not offline
# tolerant in the first place, so it is dropped rather than versioned.
rm -f "$out/service-worker.js" "$out/service-worker.js.map" "$out"/workbox-*.js "$out"/workbox-*.js.map

python3 - "$out/index.html" <<'PY'
import pathlib, sys, re

page = pathlib.Path(sys.argv[1])
html = page.read_text(encoding="utf-8")
if 'mediabox/shell.js' in html:
    sys.exit(0)

# Upstream's own service worker registration goes with the file itself.
html = re.sub(r'<script[^>]*service-worker[^>]*>.*?</script>', '', html, flags=re.S)

# The appliance stylesheet loads after upstream's so its own scoped rules win
# inside the shell, and the script loads after the app so window.core exists by
# the time it looks for it.
html = html.replace('</head>', '<link href="mediabox/shell.css" rel="stylesheet"></head>', 1)
html = html.replace('</body>', '<script defer src="mediabox/shell.js"></script></body>', 1)
page.write_text(html, encoding="utf-8")
PY

grep -q 'mediabox/shell.js' "$out/index.html" || { echo "composition failed"; exit 1; }
say "done: $out ($(du -sh "$out" | cut -f1))"
