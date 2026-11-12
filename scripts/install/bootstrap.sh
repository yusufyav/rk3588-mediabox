# shellcheck shell=bash
#
# What the installer needs before it can read the appliance archive at all.
#
# Sourced by scripts/install/install-mediabox.sh, and by
# tests/check-install-bootstrap.sh on stubbed tools. Expects the installer's
# note and die.
#
# zstd unpacks the archive, wherever the archive came from: a clean Armbian
# image has none, and an offline --bundle install that skipped it stopped at
# the unpack with the archive already verified. curl and the CA bundle fetch
# the release, so they are needed only when there is one to fetch.
#
# Runtime tools only, from the distribution: nothing here compiles anything
# (tests/check-build-free.sh).

# mediabox_bootstrap_need <online 0|1> [ca-file]: the missing packages, on one
# line, space-separated; empty when nothing is missing.
mediabox_bootstrap_need() {
  local online="$1" ca="${2:-/etc/ssl/certs/ca-certificates.crt}" need=""
  command -v zstd >/dev/null 2>&1 || need="$need zstd"
  if [ "$online" -eq 1 ]; then
    command -v curl >/dev/null 2>&1 || need="$need curl"
    [ -e "$ca" ] || need="$need ca-certificates"
  fi
  printf '%s\n' "${need# }"
}

# mediabox_bootstrap <online 0|1> <verify-only 0|1> [ca-file]
#
# Installs what is missing with apt on a normal root install. A verify-only run
# changes nothing on the board, so it names what is missing and stops; so does
# a run without root. Every failure says which step failed.
mediabox_bootstrap() {
  local online="$1" verify_only="$2" ca="${3:-/etc/ssl/certs/ca-certificates.crt}" need
  need="$(mediabox_bootstrap_need "$online" "$ca")"
  [ -n "$need" ] || return 0
  if [ "$verify_only" -eq 1 ]; then
    die "verify-only changes nothing on this board, and the board lacks: $need
     install them first (apt-get install $need), or run without --verify-only as root"
  fi
  [ "$(id -u)" -eq 0 ] || die "this board lacks: $need; re-run with sudo to install them"
  note "installing $need"
  DEBIAN_FRONTEND=noninteractive apt-get update -qq \
    || die "apt-get update failed while installing: $need"
  # shellcheck disable=SC2086
  DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends $need \
    || die "apt-get could not install: $need"
  need="$(mediabox_bootstrap_need "$online" "$ca")"
  [ -z "$need" ] || die "still missing after apt-get install: $need"
  return 0
}
