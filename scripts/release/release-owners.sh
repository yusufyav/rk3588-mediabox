# shellcheck shell=bash
#
# Which Debian package a path the product resolves belongs to.
#
# Read by the capture on the board (sent beside release-scope.sh) and by
# tests/check-release-owners.sh on a fake dpkg-query.
#
# dpkg knows the path a package installed, not every path that reaches it. An
# alternatives link -- /usr/lib/<triplet>/libblas.so.3 through /etc/alternatives
# to blas/libblas.so.3.12.1 -- is owned by nobody under its own name, so the
# path is asked for first as given and then as readlink -f resolves it. A path
# neither answers for is written down, not dropped: under set -e and pipefail a
# failed query used to end the capture with no message at all, and a list that
# skipped it would be a release missing a package. The gate fails on the record
# instead (release_unowned_report).

# release_owner <path> <unowned-record>: the owning packages, one per line.
# Always returns 0; an unowned path goes to <unowned-record>.
release_owner() {
  local p="$1" record="$2" out="" c=""
  out="$(dpkg-query -S "$p" 2>/dev/null)" || out=""
  if [ -z "$out" ]; then
    c="$(readlink -f "$p" 2>/dev/null)" || c=""
    if [ -n "$c" ] && [ "$c" != "$p" ]; then
      out="$(dpkg-query -S "$c" 2>/dev/null)" || out=""
    fi
  fi
  if [ -z "$out" ]; then
    printf '%s\n' "$p" >>"$record"
    return 0
  fi
  # "pkg-a:arm64, pkg-b:arm64: /path" -> pkg-a, pkg-b (the names before the
  # first colon of each owner, as the package list has always carried them);
  # a diversion line names no owner.
  printf '%s\n' "$out" | sed -n '/^diversion by /d; s/: \/.*$//p' | tr ',' '\n' | tr -d ' ' | cut -d: -f1 | sed '/^$/d'
  return 0
}

# release_unowned_report <unowned-record>: prints every path no package owns;
# returns 0 when there are none.
release_unowned_report() {
  [ -f "$1" ] || { echo "    no record of unowned paths: $1"; return 1; }
  [ -s "$1" ] || return 0
  sort -u "$1" | sed 's/^/    unowned: /'
  return 1
}
