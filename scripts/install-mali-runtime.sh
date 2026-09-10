#!/usr/bin/env bash
# Installs the Mali G610 user-space driver as a PRIVATE runtime on the target.
#
# "Private" is the whole point. The upstream .deb drops
# /etc/ld.so.conf.d/00-aarch64-mali.conf, which puts Mali ahead of Mesa for
# every process on the machine; if the vendor blob then misbehaves there is no
# software fallback left to debug it with. So the package is only ever
# *extracted*, and one directory of symlinks is built from it. Nothing under
# /usr/lib, /etc/ld.so.conf.d or the glvnd vendor directory is touched, and no
# maintainer script is run. Mesa keeps working for everything else, including
# for the A side of scripts/mali-probe.sh.
#
#   scripts/install-mali-runtime.sh
#
# Consumers set LD_LIBRARY_PATH to $MALI_RUNTIME/lib; scripts/run-kodi-rk3588.sh
# does exactly that.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=env.sh
source "$here/scripts/env.sh"

# Pinned. Both the release tag and the hash are recorded in the Gate MP2
# quality-recovery evidence; a different build of "the same" driver is a
# different experiment and must not be picked up silently.
: "${MALI_RELEASE:=v1.9-1-20260312-bd33ee2}"
: "${MALI_DEB:=libmali-valhall-g610-g24p0-gbm_1.9-1_arm64.deb}"
: "${MALI_SHA256:=32ffe853e8d56295284637252f1da15dd868a8f7c6b8da6b9f77616ba285eb1a}"
: "${MALI_URL:=https://github.com/tsukumijima/libmali-rockchip/releases/download/$MALI_RELEASE/$MALI_DEB}"
: "${MALI_ROOT:=/opt/rk3588-mediabox/mali-g24p0-root}"
: "${MALI_RUNTIME:=/opt/rk3588-mediabox/mali-g24p0-runtime}"
: "${MALI_STAGE:=/var/tmp/mali-stage}"

echo "== fetching $MALI_DEB on the target"
mediabox_ssh "mkdir -p '$MALI_STAGE'"
mediabox_ssh "cd '$MALI_STAGE' && [ -f '$MALI_DEB' ] || curl -sSLf -o '$MALI_DEB' '$MALI_URL'"

echo "== verifying SHA256"
actual="$(mediabox_ssh "sha256sum '$MALI_STAGE/$MALI_DEB' | cut -d' ' -f1")"
if [ "$actual" != "$MALI_SHA256" ]; then
  echo "SHA256 MISMATCH" >&2
  echo "  expected $MALI_SHA256" >&2
  echo "  actual   $actual" >&2
  echo "refusing to use this package" >&2
  exit 1
fi
echo "   ok: $actual"

echo "== extracting (no dpkg -i, no maintainer scripts)"
mediabox_ssh "rm -rf '$MALI_ROOT' && mkdir -p '$MALI_ROOT' && \
    dpkg-deb -x '$MALI_STAGE/$MALI_DEB' '$MALI_ROOT'"

echo "== building the private runtime directory"
# The package splits the generic sonames into a mali/ subdirectory and keeps
# the real object one level up, a layout that only resolves via ld.so.conf.
# Flattening it into one directory is what makes a single LD_LIBRARY_PATH
# entry enough.
mediabox_ssh "
set -e
R='$MALI_ROOT/usr/lib/aarch64-linux-gnu'
L='$MALI_RUNTIME/lib'
rm -rf '$MALI_RUNTIME'
mkdir -p \"\$L\"
ln -s \"\$R/libmali.so.1.9.0\"      \"\$L/libmali.so.1.9.0\"
ln -s libmali.so.1.9.0             \"\$L/libmali.so.1\"
ln -s libmali.so.1                 \"\$L/libmali.so\"
ln -s \"\$R/libmali-hook.so.1.9.0\" \"\$L/libmali-hook.so.1.9.0\"
ln -s libmali-hook.so.1.9.0        \"\$L/libmali-hook.so.1\"
ln -s libmali-hook.so.1            \"\$L/libmali-hook.so\"
for s in libEGL.so.1 libGLESv1_CM.so.1 libGLESv2.so.2 libgbm.so.1; do
  ln -s \"\$R/mali/\$s\" \"\$L/\$s\"
done
ln -s libEGL.so.1       \"\$L/libEGL.so\"
ln -s libGLESv2.so.2    \"\$L/libGLESv2.so\"
ln -s libgbm.so.1       \"\$L/libgbm.so\"
ln -s libGLESv1_CM.so.1 \"\$L/libGLESv1_CM.so\"
"

echo "== confirming the system stack is untouched"
mediabox_ssh "
echo '   ld.so.conf.d :' \$(ls /etc/ld.so.conf.d/ | tr '\n' ' ')
echo '   libmali under /usr/lib :' \$(find /usr/lib -name 'libmali*' 2>/dev/null | wc -l) files
echo '   glvnd egl vendors :' \$(ls /usr/share/glvnd/egl_vendor.d/ | tr '\n' ' ')
echo '   dpkg-installed libmali packages :' \$(dpkg -l 2>/dev/null | grep -c libmali)
"
echo "== private Mali runtime ready at $MALI_RUNTIME/lib"
