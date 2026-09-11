#!/usr/bin/env bash
# Installs the locally cross-built RK3588 kernel onto the appliance, beside the
# one that is already there.
#
# Nothing is overwritten. The new image goes to /boot/vmlinuz-<release> and its
# modules to /lib/modules/<release>, both named by the build's own release
# string, and only the /boot/Image symlink is repointed. Rolling back is
# therefore re-pointing that one symlink, which `rollback` does.
#
#   scripts/deploy-kernel.sh install    copy image + modules, do NOT switch
#   scripts/deploy-kernel.sh activate   point /boot/Image at the new kernel
#   scripts/deploy-kernel.sh rollback   point it back at the previous one
#   scripts/deploy-kernel.sh status     what is installed and what will boot
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=env.sh
source "$here/scripts/env.sh"

: "${KERNEL_SRC:=/home/yu/build/linux-rockchip-vop2}"
: "${KERNEL_PREV:=vmlinuz-6.1.115-vendor-rk35xx-screenbridge-hdmirx-audio}"

krelease() {
  make -s -C "$KERNEL_SRC" ARCH=arm64 CROSS_COMPILE=aarch64-linux-gnu- \
    LOCALVERSION=-vendor-rk35xx-screenbridge-hdmirx-audio-vop2rgb kernelrelease
}

case "${1:-status}" in
  install)
    rel="$(krelease)"
    echo "== release: $rel"
    [ -f "$KERNEL_SRC/arch/arm64/boot/Image" ] || { echo "no Image built" >&2; exit 1; }

    echo "== staging modules locally"
    stage="$(mktemp -d)"
    trap 'rm -rf "$stage"' EXIT
    make -s -C "$KERNEL_SRC" ARCH=arm64 CROSS_COMPILE=aarch64-linux-gnu- \
      LOCALVERSION=-vendor-rk35xx-screenbridge-hdmirx-audio-vop2rgb \
      INSTALL_MOD_PATH="$stage" modules_install >/dev/null
    # The build-tree symlinks point at paths that do not exist on the target
    # and make depmod noisy; the target does not build modules, so drop them.
    rm -f "$stage/lib/modules/$rel/build" "$stage/lib/modules/$rel/source"

    echo "== copying modules to the target"
    mediabox_rsync "$stage/lib/modules/$rel/" \
      "$MEDIABOX_TARGET:/lib/modules/$rel/"

    echo "== copying the image"
    mediabox_scp "$KERNEL_SRC/arch/arm64/boot/Image" \
      "$MEDIABOX_TARGET:/boot/vmlinuz-$rel" >/dev/null

    mediabox_ssh "depmod -a '$rel' && echo '== depmod ok' && \
      ls -l /boot/vmlinuz-$rel && ls /lib/modules/ && \
      echo '== still booting:' && readlink /boot/Image"
    ;;

  activate)
    rel="$(krelease)"
    mediabox_ssh "test -f /boot/vmlinuz-$rel && test -d /lib/modules/$rel" \
      || { echo "kernel $rel is not installed; run install first" >&2; exit 1; }
    # Record what to go back to before changing anything.
    mediabox_ssh "readlink /boot/Image > /boot/.previous-image && \
      ln -sf 'vmlinuz-$rel' /boot/Image && sync && \
      echo '== /boot/Image -> ' \$(readlink /boot/Image) && \
      echo '== rollback target: ' \$(cat /boot/.previous-image)"
    ;;

  rollback)
    mediabox_ssh "prev=\$(cat /boot/.previous-image 2>/dev/null || echo '$KERNEL_PREV'); \
      test -f \"/boot/\$prev\" || { echo \"no \$prev to roll back to\" >&2; exit 1; }; \
      ln -sf \"\$prev\" /boot/Image && sync && \
      echo '== /boot/Image -> ' \$(readlink /boot/Image)"
    ;;

  status)
    mediabox_ssh "echo '== running:  ' \$(uname -r)
      echo '== will boot:' \$(readlink /boot/Image)
      echo '== images:'; ls /boot/ | grep '^vmlinuz' | sed 's/^/     /'
      echo '== modules:'; ls /lib/modules/ | sed 's/^/     /'"
    ;;

  *)
    echo "usage: $0 [install|activate|rollback|status]" >&2
    exit 2
    ;;
esac
