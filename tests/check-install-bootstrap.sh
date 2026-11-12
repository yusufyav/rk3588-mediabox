#!/usr/bin/env bash
#
# What the installer needs before it can read the archive, on stubbed tools.
#
# A clean Armbian has no zstd. The offline --bundle path used to skip the
# bootstrap that the download path ran, so a verified archive stopped at the
# unpack with "zstd is not installed". This runs the installer's own
# mediabox_bootstrap (scripts/install/bootstrap.sh) under set -euo pipefail, on
# a PATH whose zstd and curl exist only once a fake apt-get has "installed"
# them, and an id that answers root or not as asked.
#
# And with zstd taken out of the common list, the offline case has to fail.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
lib="$here/scripts/install/bootstrap.sh"
failures=0

report() { printf 'FAIL %s\n' "$*"; failures=$((failures + 1)); }
okay()   { printf 'ok   %s\n' "$*"; }

work="$(mktemp -d "${TMPDIR:-/tmp}/mediabox-install-bootstrap-XXXXXX")"
trap 'rm -rf "$work"' EXIT

# A PATH with only what the function itself runs, plus the stubs.
bin="$work/bin"
mkdir -p "$bin"
for t in sh bash printf cat; do ln -s "$(command -v "$t")" "$bin/$t"; done
cat >"$bin/id" <<EOF
#!/bin/sh
[ "\$1" = -u ] && cat "$work/uid"
EOF
# apt-get logs every call; "install" puts a stub of each named tool on the PATH,
# unless told to fail.
cat >"$bin/apt-get" <<EOF
#!/bin/sh
printf '%s\n' "\$*" >>"$work/apt.log"
[ -e "$work/apt-fails" ] && exit 100
if [ "\$1" = install ]; then
  for a in "\$@"; do
    case "\$a" in
      zstd|curl) printf '#!/bin/sh\nexit 0\n' >"$bin/\$a"; /usr/bin/chmod +x "$bin/\$a" ;;
      ca-certificates) : >"$work/ca.crt" ;;
    esac
  done
fi
exit 0
EOF
chmod +x "$bin/id" "$bin/apt-get"

# One run of the bootstrap: board state set up, then the function under the
# installer's shell options. Prints its output; returns its status.
# run <lib> <online> <verify-only> <uid> <have: list of zstd curl ca>
run() {
  local l="$1" online="$2" vo="$3" uid="$4"; shift 4
  rm -f "$bin/zstd" "$bin/curl" "$work/ca.crt" "$work/apt.log" "$work/apt-fails"
  printf '%s\n' "$uid" >"$work/uid"
  for h in "$@"; do
    case "$h" in
      zstd|curl) printf '#!/bin/sh\nexit 0\n' >"$bin/$h"; chmod +x "$bin/$h" ;;
      ca) : >"$work/ca.crt" ;;
      apt-fails) : >"$work/apt-fails" ;;
    esac
  done
  PATH="$bin" bash -c '
    set -euo pipefail
    note() { printf "note %s\n" "$*"; }
    die()  { printf "DIE %s\n" "$*"; exit 1; }
    . "$1"
    mediabox_bootstrap "$2" "$3" "$4"
    printf "done\n"' _ "$l" "$online" "$vo" "$work/ca.crt" 2>&1
}
installed() { sed -n 's/^install -y -qq --no-install-recommends //p' "$work/apt.log" 2>/dev/null; }

# ----------------------------------------------------------- offline bundle
out="$(run "$lib" 0 0 0 curl ca)"; rc=$?
if [ "$rc" -eq 0 ] && [ "$(installed)" = zstd ] && [ -x "$bin/zstd" ]; then
  okay "offline bundle on a clean board: apt installs zstd, and only zstd"
else
  report "offline bundle: rc=$rc installed='$(installed)' out='$out'"
fi
out="$(run "$lib" 0 0 0)"; rc=$?
if [ "$rc" -eq 0 ] && [ "$(installed)" = zstd ]; then
  okay "offline bundle needs neither curl nor the CA bundle"
else
  report "offline without curl/CA: rc=$rc installed='$(installed)'"
fi
out="$(run "$lib" 0 0 0 zstd)"; rc=$?
if [ "$rc" -eq 0 ] && [ ! -e "$work/apt.log" ]; then
  okay "offline bundle with zstd present: apt is not run"
else
  report "offline with zstd: rc=$rc apt='$(cat "$work/apt.log" 2>/dev/null)'"
fi

# ------------------------------------------------------------ online fetch
out="$(run "$lib" 1 0 0)"; rc=$?
if [ "$rc" -eq 0 ] && [ "$(installed)" = "zstd curl ca-certificates" ]; then
  okay "online on a clean board: zstd, curl and ca-certificates"
else
  report "online clean: rc=$rc installed='$(installed)'"
fi
out="$(run "$lib" 1 0 0 zstd curl ca)"; rc=$?
if [ "$rc" -eq 0 ] && [ ! -e "$work/apt.log" ]; then
  okay "online with everything present: apt is not run"
else
  report "online present: rc=$rc apt='$(cat "$work/apt.log" 2>/dev/null)'"
fi
out="$(run "$lib" 1 0 0 zstd ca)"; rc=$?
if [ "$rc" -eq 0 ] && [ "$(installed)" = curl ]; then
  okay "online without curl: curl alone"
else
  report "online without curl: rc=$rc installed='$(installed)'"
fi

# ------------------------------------------ nothing changed, and said so
out="$(run "$lib" 0 1 0)"; rc=$?
if [ "$rc" -ne 0 ] && [ ! -e "$work/apt.log" ] && printf '%s' "$out" | grep -q '^DIE verify-only changes nothing.*lacks: zstd'; then
  okay "verify-only as root: names zstd, changes nothing"
else
  report "verify-only root: rc=$rc apt='$(cat "$work/apt.log" 2>/dev/null)' out='$out'"
fi
out="$(run "$lib" 0 1 1000)"; rc=$?
if [ "$rc" -ne 0 ] && [ ! -e "$work/apt.log" ] && printf '%s' "$out" | grep -q '^DIE verify-only'; then
  okay "verify-only without root: names zstd, changes nothing"
else
  report "verify-only user: rc=$rc out='$out'"
fi
out="$(run "$lib" 0 0 1000)"; rc=$?
if [ "$rc" -ne 0 ] && [ ! -e "$work/apt.log" ] && printf '%s' "$out" | grep -q '^DIE .*lacks: zstd.*sudo'; then
  okay "install without root: names zstd, asks for sudo"
else
  report "install user: rc=$rc out='$out'"
fi
out="$(run "$lib" 0 0 0 apt-fails)"; rc=$?
if [ "$rc" -ne 0 ] && printf '%s' "$out" | grep -q '^DIE apt-get update failed while installing: zstd'; then
  okay "apt failing: an explicit message, not a silent set -e exit"
else
  report "apt failing: rc=$rc out='$out'"
fi

# ------------------------------------------------------------------ mutation
mutant="$work/bootstrap.sh"
sed '/command -v zstd >\/dev\/null 2>&1 || need="\$need zstd"/d' "$lib" >"$mutant"
if cmp -s "$lib" "$mutant"; then
  report "mutation: could not take zstd out of the common bootstrap"
else
  run "$mutant" 0 0 0 >/dev/null
  if [ "$(installed)" = zstd ] && [ -x "$bin/zstd" ]; then
    report "mutation: zstd still bootstrapped with it taken out of the common list"
  else
    okay "mutation: without zstd in the common list the offline bundle gets no zstd"
  fi
fi

# ------------------------------------------------------------ the installer
inst="$here/scripts/install/install-mediabox.sh"
if grep -q '^\. "\$here/scripts/install/bootstrap.sh"$' "$inst" \
   && grep -q '^  mediabox_bootstrap 0 "\$verify_only"$' "$inst" \
   && grep -q '^  mediabox_bootstrap 1 "\$verify_only"$' "$inst"; then
  okay "the installer runs the bootstrap on both the bundle and the download path"
else
  report "the installer does not run mediabox_bootstrap on both paths"
fi

if [ "$failures" -eq 0 ]; then echo "INSTALL_BOOTSTRAP=PASS"; exit 0; fi
echo "INSTALL_BOOTSTRAP=FAIL ($failures)"; exit 1
