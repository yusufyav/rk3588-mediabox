#!/usr/bin/env bash
#
# Build MediaBox's bluez-alsa, on the appliance, and install it there.
#
# Debian's bluez-alsa with one patch, patches/bluez-alsa/: the A2DP SBC source
# sends full packets only, as Android's encoder does. Unpatched, every full
# packet was followed by a two-frame one, and with a BLE keyboard on the same
# controller a Bluetooth headset went short of audio (docs/bluetooth.md).
#
# Built natively from Debian's own source package, the version pinned below,
# because the patch is written against it. What it leaves on the board:
#   - bluez-alsa-utils and libasound2-plugin-bluez at <version>+mediabox1,
#     installed and held, so no apt upgrade puts the unpatched one back;
#   - the two .debs under $MEDIABOX_PREFIX/packages/bluez-alsa/, which the
#     release captures with the rest of the prefix and the installer installs.
# The build dependencies are removed again and the package list is checked
# against the one from before the build; a deb-src entry is added for the
# build only if the board had none.
#
#   MEDIABOX_HOST=<appliance> scripts/build-bluez-alsa.sh
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=/dev/null
source "$here/scripts/env.sh"

debian_version="${MEDIABOX_BLUEZ_ALSA_DEBIAN_VERSION:-4.3.1-3}"
ours="${debian_version}+mediabox1"
work=/var/tmp/bluez-alsa-build
packages="$MEDIABOX_PREFIX/packages/bluez-alsa"

echo "== patches -> appliance"
mediabox_ssh "rm -rf $work && mkdir -p $work"
mediabox_scp "$here"/patches/bluez-alsa/*.patch "$MEDIABOX_TARGET:$work/"

echo "== building bluez-alsa $ours on the appliance"
mediabox_ssh "WORK='$work' DEBIAN_VERSION='$debian_version' OURS='$ours' PACKAGES='$packages' bash -s" <<'REMOTE'
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
cd "$WORK"
dpkg-query -W -f='${Package}\n' | sort >before.txt

added_src=""
cleanup() {
  if [ -n "$added_src" ]; then
    rm -f "$added_src"
    apt-get update -qq || true
  fi
}
trap cleanup EXIT
if ! grep -rqs '^Types:.*deb-src\|^deb-src ' /etc/apt/sources.list /etc/apt/sources.list.d/; then
  added_src=/etc/apt/sources.list.d/mediabox-build-src.sources
  . /etc/os-release
  cat >"$added_src" <<EOF
Types: deb-src
URIs: http://deb.debian.org/debian
Suites: $VERSION_CODENAME
Components: main
Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg
EOF
fi
apt-get update -qq
apt-get install -y -qq --no-install-recommends dpkg-dev >/dev/null
apt-get source -qq "bluez-alsa=$DEBIAN_VERSION"
apt-get build-dep -y -qq "bluez-alsa=$DEBIAN_VERSION" >/dev/null

src="$(find . -maxdepth 1 -type d -name 'bluez-alsa-*' | head -1)"
cd "$src"
for p in "$WORK"/*.patch; do
  echo "  applying $(basename "$p")"
  patch -p1 --forward --no-backup-if-mismatch <"$p"
done
{
  printf 'bluez-alsa (%s) unstable; urgency=medium\n\n' "$OURS"
  printf '  * A2DP SBC source: send full packets only (rk3588-mediabox).\n\n'
  printf ' -- rk3588-mediabox <noreply@rk3588-mediabox.invalid>  %s\n\n' "$(date -R)"
  cat debian/changelog
} >debian/changelog.new
mv debian/changelog.new debian/changelog
DEB_BUILD_OPTIONS="nocheck noddebs" dpkg-buildpackage -b -uc -us >"$WORK/build.log" 2>&1 \
  || { tail -30 "$WORK/build.log" >&2; exit 1; }
cd "$WORK"

debs=("bluez-alsa-utils_${OURS}_"*.deb "libasound2-plugin-bluez_${OURS}_"*.deb)
for d in "${debs[@]}"; do [ -f "$d" ] || { echo "not built: $d" >&2; exit 1; }; done
rm -rf "$PACKAGES" && install -d -m 0755 "$PACKAGES"
install -m 0644 "${debs[@]}" "$PACKAGES/"
apt-get install -y -qq --no-install-recommends --allow-downgrades \
  "$PACKAGES"/bluez-alsa-utils_*.deb "$PACKAGES"/libasound2-plugin-bluez_*.deb >/dev/null
apt-mark hold bluez-alsa-utils libasound2-plugin-bluez >/dev/null

# The build's own packages go again; the product's two stay.
dpkg-query -W -f='${Package}\n' | sort >after.txt
added="$(comm -13 before.txt after.txt | grep -vxE 'bluez-alsa-utils|libasound2-plugin-bluez' || true)"
if [ -n "$added" ]; then
  # shellcheck disable=SC2086
  apt-get purge -y -qq $added >/dev/null
  apt-get autoremove -y -qq >/dev/null
fi
dpkg-query -W -f='${Package}\n' | sort >now.txt
extra="$(comm -13 before.txt now.txt | grep -vxE 'bluez-alsa-utils|libasound2-plugin-bluez' || true)"
gone="$(comm -23 before.txt now.txt || true)"
if [ -n "$extra$gone" ]; then
  echo "the package list is not what it was before the build" >&2
  echo "  left: ${extra:-none}" >&2
  echo "  gone: ${gone:-none}" >&2
  exit 1
fi

systemctl restart bluealsa.service >/dev/null 2>&1 || true
cd / && rm -rf "$WORK"
REMOTE

echo "== proof"
mediabox_ssh "dpkg-query -W -f='\${Package} \${Version} \${db:Status-Abbrev}\n' bluez-alsa-utils libasound2-plugin-bluez
  apt-mark showhold | grep -E 'bluez-alsa-utils|libasound2-plugin-bluez'
  ls -l '$packages'"
