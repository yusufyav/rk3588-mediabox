# shellcheck shell=bash
#
# What of the golden board's prefix is the product.
#
# Decided here once, and read by every part of the capture that walks the
# prefix: the manifest, the digest list, the ELF closure, the archive and the
# leftover gate. The capture sends this file to the board's staging directory
# and sources it there; tests/check-release-scope.sh sources it on a synthetic
# prefix. A path the manifest leaves out and the tar carries -- or the other way
# round -- is a release whose own SHA256SUMS does not describe it.
#
# RELEASE_NOT_PRODUCT, prefix-relative:
#
#   rollback   the install-time undo history the deploy scripts keep beside the
#              product (scripts/build-mediabox-live.sh --install and friends).
#              Operational, per board, never runtime: nothing the product runs
#              reads it, and a clean board has none.
#
# Development leftovers are *not* listed here. Leaving them out of the archive
# would ship a board that still has them as if it did not; release_leftovers
# below makes the capture refuse instead.
RELEASE_NOT_PRODUCT="rollback"

# find(1) over the product part of <root><prefix>; the remaining arguments are
# one find expression, applied to everything that is not pruned.
release_find() {
  local base="$1$2" p
  shift 2
  local prune=()
  for p in $RELEASE_NOT_PRODUCT; do prune+=(-path "$base/$p" -o); done
  find "$base" -xdev -mindepth 1 \( "${prune[@]}" -false \) -prune -o \( "$@" \)
}

# One file inside the product that the archive still leaves out: generated
# caches and the backup the account work once left beside the interface.
release_skip_file() {
  case "$1" in
    */bin/mediabox-tv.pre-account) return 0 ;;
    */__pycache__|*/__pycache__/*|*.pyc) return 0 ;;
    *) return 1 ;;
  esac
}

# release_inventory <root> <prefix> <meta-dir> <elf-list>
#
# artifact-manifest.tsv: one line per object, with the mode, the ownership as
# it will be written, the size and -- for regular files -- the digest. Paths are
# written as the archive will carry them: rootfs<prefix>/...
release_inventory() {
  local root="$1" prefix="$2" m="$3" elf_list="$4"
  local kind mode size path rel sum
  release_find "$root" "$prefix" \( -type f -o -type l -o -type d \) -printf '%y\t%m\t%s\t%p\n' |
    sort -k4 |
    while IFS=$'\t' read -r kind mode size path; do
      release_skip_file "$path" && continue
      rel="rootfs${path#"$root"}"
      case "$kind" in
        d) printf 'dir\t%s\t-\t-\t%s\n' "$mode" "$rel" >>"$m/artifact-manifest.tsv" ;;
        l) printf 'link\t%s\t-\t%s\t%s\n' "$mode" "$(readlink "$path")" "$rel" >>"$m/artifact-manifest.tsv" ;;
        f) sum="$(sha256sum "$path" | cut -d' ' -f1)"
           printf 'file\t%s\t%s\t%s\t%s\n' "$mode" "$size" "$sum" "$rel" >>"$m/artifact-manifest.tsv"
           printf '%s  %s\n' "$sum" "$rel" >>"$m/SHA256SUMS"
           if [ "$(head -c4 "$path" 2>/dev/null | od -An -tx1 | tr -d ' ')" = 7f454c46 ]; then
             printf '%s\n' "$path" >>"$elf_list"
           fi
           ;;
      esac
    done
}

# release_tar <root> <prefix>, run from the staging directory: the archive's
# tar on stdout -- meta/, the staged rootfs/etc, and the product part of the
# prefix renamed into rootfs/. The exclusions are the inventory's, from the same
# list, so the archive carries exactly what the manifest names.
release_tar() {
  local root="$1" rel="${2#/}" p
  local staged=(meta rootfs/etc)
  [ -d rootfs/var/lib/mediabox-dvb ] && staged+=(rootfs/var/lib/mediabox-dvb)
  local ex=(--exclude='*/__pycache__' --exclude='*.pyc'
            --exclude="$rel/bin/mediabox-tv.pre-account")
  for p in $RELEASE_NOT_PRODUCT; do ex+=(--exclude="$rel/$p"); done
  tar --numeric-owner --owner=0 --group=0 --xattrs --acls "${ex[@]}" \
    -cf - "${staged[@]}" -C "$root/" --transform='s,^opt/,rootfs/opt/,' "$rel"
}

# release_leftovers <root> <prefix>: what in the product part of the prefix is
# staging or development debris, one prefix-relative path per line.
#
# The broad names (.orig, .bak, candidate, staging, kodi-src, a build tree in
# the top two levels -- npm vendors directories called build inside
# node_modules) and, by name, the leftovers the live-television work left in
# the prefix: binaries kept as bin/*.pre-faz3j*, and the media core's old and
# subtitle-test copies.
release_leftovers() {
  local base="$1$2" p
  { release_find "$1" "$2" \( -name '*.orig' -o -name '*.bak' -o -name '*candidate*' \
      -o -name 'staging' -o -name 'kodi-src' \) -printf '%P\n' 2>/dev/null
    release_find "$1" "$2" -name 'build' -printf '%P\n' 2>/dev/null | awk -F/ 'NF <= 2'
    find "$base/bin" -mindepth 1 -maxdepth 1 -name '*.pre-faz3j*' -printf 'bin/%P\n' 2>/dev/null || true
    for p in media.old media.subtitles-test; do
      if [ -e "$base/$p" ] || [ -L "$base/$p" ]; then printf '%s\n' "$p"; fi
    done
    true
  } | sort -u
}
