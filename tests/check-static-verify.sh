#!/usr/bin/env bash
#
# The archive check has to run on the image it is aimed at.
#
# A clean Armbian Minimal image carries no binutils, and the installer verifies
# the unpacked archive *before* it installs the runtime packages -- it has to,
# the package list comes out of the archive. So for a while every install on a
# fresh Orange Pi 5 Plus stopped here:
#
#   FAIL  runpath player/bin/mpv         readelf is not installed; cannot check
#   FAIL  runpath kodi/lib/kodi/kodi-gbm readelf is not installed; cannot check
#
# The fix was to read the dynamic section in the verifier itself rather than
# shell out. This test is what stops that from coming back, and it is built to
# fail if the check is ever weakened instead of fixed: the verifier is run
# against a synthetic archive on a PATH that holds no readelf at all, and it has
# to say PASS -- and then, against the same archive with one thing wrong at a
# time, it has to say FAIL for each.
#
# The binaries in the fixture are real ELF64 objects, assembled here byte by
# byte: a program header table, a PT_DYNAMIC segment and a string table, which
# is exactly the path through the file the verifier walks. A fixture made of
# empty files would pass a verifier that had quietly stopped looking.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
verifier="$here/packaging/mediabox-product-verify"
failures=0

report() { printf 'FAIL %s\n' "$*"; failures=$((failures + 1)); }
okay()   { printf 'ok   %s\n' "$*"; }

work="$(mktemp -d "${TMPDIR:-/tmp}/mediabox-static-verify-XXXXXX")"
trap 'rm -rf "$work"' EXIT

# ---------------------------------------------------------------- a PATH with
# no readelf on it, and nothing else missing that would make the run prove
# something other than what it is meant to prove.
bin="$work/bin"
mkdir -p "$bin"
for t in sh od awk grep sed find sort uniq wc tr cat head cut readlink dirname basename printf test ls; do
  p="$(command -v "$t" 2>/dev/null)" || continue
  ln -sf "$p" "$bin/$t"
done
if PATH="$bin" command -v readelf >/dev/null 2>&1; then
  report "the sanitised PATH still has a readelf on it; this test proves nothing"
  exit 1
fi
okay "the test PATH carries no readelf"

# ------------------------------------------------------------- the fixture
#
# $1 output file, $2 the RUNPATH to bake in, $3.. extra strings to carry.
make_elf() {
  python3 - "$@" <<'PYEOF'
import struct, sys

out, runpath, *extra = sys.argv[1:]

EHSIZE, PHENTSIZE, PHNUM = 64, 56, 2
phoff = EHSIZE
dynoff = phoff + PHENTSIZE * PHNUM
DYN = [(5, None), (29, 1), (1, 0), (0, 0)]        # STRTAB, RUNPATH, NEEDED, NULL
dynsz = len(DYN) * 16
stroff = dynoff + dynsz
strtab = b"\0" + runpath.encode() + b"\0"
trailer = b"".join(s.encode() + b"\0" for s in extra)
total = stroff + len(strtab) + len(trailer)

eh = bytearray(EHSIZE)
eh[0:4] = b"\x7fELF"
eh[4] = 2                                          # ELFCLASS64
eh[5] = 1                                          # ELFDATA2LSB
eh[6] = 1                                          # EV_CURRENT
struct.pack_into("<HHI", eh, 16, 3, 183, 1)        # ET_DYN, EM_AARCH64
struct.pack_into("<QQQ", eh, 24, 0, phoff, 0)      # e_entry, e_phoff, e_shoff
struct.pack_into("<IHHHHHH", eh, 48, 0, EHSIZE, PHENTSIZE, PHNUM, 64, 0, 0)

def phdr(ptype, off, vaddr, filesz):
    return struct.pack("<IIQQQQQQ", ptype, 5, off, vaddr, vaddr, filesz, filesz, 0x1000)

# One PT_LOAD mapping the whole file at vaddr == file offset, so the string
# table's virtual address resolves back to where it actually is.
ph = phdr(1, 0, 0, total) + phdr(2, dynoff, dynoff, dynsz)

dyn = b"".join(struct.pack("<QQ", t, stroff if v is None else v) for t, v in DYN)

with open(out, "wb") as f:
    f.write(bytes(eh) + ph + dyn + strtab + trailer)
PYEOF
}

RP=/opt/rk3588-mediabox/media-runtime/lib
KODI_PREFIX=/opt/rk3588-mediabox/kodi/share/kodi

# A whole unpacked archive, as the installer hands one to the verifier.
build_archive() {
  root="$1"; mpv_rp="$2"; kodi_rp="$3"; recorded="$4"
  rm -rf "$root"
  P="$root/rootfs/opt/rk3588-mediabox"
  M="$root/meta"
  mkdir -p "$P"/{bin,player/bin,player/share,media-runtime/lib,media-runtime/lib/dri,mali-g24p0-root} \
           "$P"/{mali-g24p0-runtime/lib,node/bin,stremio-server,media,ui} \
           "$P"/kodi/lib/kodi "$P"/kodi/share/kodi/system/settings "$M" \
           "$root"/rootfs/etc/systemd/system "$root"/rootfs/etc/mediabox \
           "$root"/rootfs/etc/udev/rules.d "$root"/rootfs/etc/systemd/logind.conf.d \
           "$root"/rootfs/etc/alsa/conf.d

  for b in mediaboxd-rs mediaboxctl mediabox-platform mediabox-tv mediabox-player \
           mediabox-browser-verify; do
    : >"$P/bin/$b"; chmod 755 "$P/bin/$b"
  done
  # The browser's VA-API driver. A file rather than an ELF: the verifier asks
  # whether it was installed, and the questions about what it links are asked
  # on the appliance by mediabox-browser-verify, where there is an MPP to link.
  : >"$P/media-runtime/lib/dri/rockchip_drv_video.so"
  : >"$P/node/bin/node"; chmod 755 "$P/node/bin/node"
  : >"$P/stremio-server/server.js"
  : >"$P/kodi/share/kodi/system/settings/settings.xml"

  make_elf "$P/player/bin/mpv" "$mpv_rp"
  make_elf "$P/kodi/lib/kodi/kodi-gbm" "$kodi_rp" "$KODI_PREFIX"
  chmod 755 "$P/player/bin/mpv" "$P/kodi/lib/kodi/kodi-gbm"

  # The Mali chain, including the absolute last hop that only resolves when the
  # verifier resolves links against the archive root rather than against /.
  : >"$P/mali-g24p0-root/libmali.so"
  ln -sf /opt/rk3588-mediabox/mali-g24p0-root/libmali.so "$P/mali-g24p0-runtime/lib/libmali.so.1.9.0"
  ln -sf libmali.so.1.9.0 "$P/mali-g24p0-runtime/lib/libmali.so.1"
  for l in libEGL.so.1 libGLESv2.so.2 libgbm.so.1 libmali-hook.so.1; do
    ln -sf libmali.so.1 "$P/mali-g24p0-runtime/lib/$l"
  done

  printf 'mpv-vo-mediabox\tpresent\nmpv-hwdec-rkmpp\tpresent\n' >"$M/capability-baseline.txt"
  printf '# binary<TAB>soname<TAB>resolved<TAB>class\n' >"$M/elf-closure.txt"
  if [ -n "$recorded" ]; then
    { printf '# binary<TAB>runpath\n'
      printf 'player/bin/mpv\t%s\n' "$recorded"
      printf 'kodi/lib/kodi/kodi-gbm\t%s\n' "$recorded"
    } >"$M/runpath.tsv"
  fi

  for u in mediaboxd-rs mediabox-tv-ui mediabox-media-worker stremio-server \
           mediabox-console-off mediabox-display-changed mediabox-display-seed \
           mediabox-display-observer mediabox-browser kodi; do
    : >"$root/rootfs/etc/systemd/system/$u.service"
  done
  for f in etc/mediaboxd.toml etc/mediabox-library.json etc/mediabox-applications.json \
           etc/mediabox-media-worker.env etc/mediabox/sway-browser.conf \
           etc/udev/rules.d/80-mediabox-no-power-switch.rules \
           etc/udev/rules.d/81-mediabox-display-hotplug.rules \
           etc/alsa/conf.d/60-mediabox-unrouted.conf \
           etc/systemd/logind.conf.d/10-mediabox.conf; do
    : >"$root/rootfs/$f"
  done
}

run_verify() {
  env -i PATH="$bin" \
    MEDIABOX_VERIFY_ROOT="$1/rootfs" MEDIABOX_VERIFY_META="$1/meta" \
    MEDIABOX_VERIFY_STATIC_ONLY=1 "$bin/sh" "$verifier" 2>&1
}

# ------------------------------------------------- 1. the regression itself
echo "-- a sound archive verifies with no readelf anywhere on PATH"
build_archive "$work/good" "$RP" "$RP" "$RP"
out="$(run_verify "$work/good")"; rc=$?
if [ "$rc" -eq 0 ] && [[ "$out" == *"product verify PASS"* ]]; then
  okay "static verify passes without readelf"
else
  report "static verify did not pass without readelf (exit $rc):"
  printf '%s\n' "$out" | sed -n '/FAIL/p' | sed 's/^/     /'
fi
if [[ "$out" == *"readelf is not installed"* ]]; then
  report "the verifier still reports readelf as missing"
else
  okay "no readelf complaint in the output"
fi
if [[ "$out" == *"PASS  runpath player/bin/mpv"* && "$out" == *"PASS  runpath kodi/lib/kodi/kodi-gbm"* ]]; then
  okay "both runpaths were read out of the binaries"
else
  report "the runpath lines did not pass:"
  printf '%s\n' "$out" | grep -i runpath | sed 's/^/     /'
fi

# ------------------------------------------------ 2. and it still fails closed
fails_with() {
  label="$1"; expect="$2"; shift 2
  out="$("$@")"
  if [[ "$out" == *"$expect"* && "$out" == *"product verify FAIL"* ]]; then
    okay "$label"
  else
    report "$label -- expected a FAIL naming '$expect'"
    printf '%s\n' "$out" | grep -iE 'runpath|verify (PASS|FAIL)' | sed 's/^/     /'
  fi
}

echo "-- a wrong runpath is still a failure"
build_archive "$work/wrong" /usr/lib /usr/lib /usr/lib
fails_with "mpv pointing outside the media runtime" \
  "expected the MediaBox media runtime" run_verify "$work/wrong"

echo "-- the other product's prefix is still a failure"
build_archive "$work/bridge" "$RP" "$RP:/opt/rk3588-screenbridge/lib" ""
fails_with "kodi linked against ScreenBridge" \
  "FAIL  screenbridge runpath kodi/lib/kodi/kodi-gbm" run_verify "$work/bridge"

echo "-- a binary with no runpath at all is still a failure"
build_archive "$work/none" "" "" ""
fails_with "a binary carrying no runpath" "got 'none'" run_verify "$work/none"

echo "-- a binary whose dynamic section cannot be read is still a failure"
build_archive "$work/junk" "$RP" "$RP" "$RP"
printf 'this is not an ELF file at all\n' >"$work/junk/rootfs/opt/rk3588-mediabox/player/bin/mpv"
fails_with "an unreadable binary" "could not be read" run_verify "$work/junk"

echo "-- an archive that disagrees with its own capture is still a failure"
build_archive "$work/drift" "$RP" "$RP" /opt/rk3588-mediabox/media-runtime/lib:/somewhere/else
fails_with "recorded runpath differing from the binary" \
  "the capture recorded" run_verify "$work/drift"

echo
if [ "$failures" -eq 0 ]; then
  echo "STATIC_VERIFY_WITHOUT_READELF=PASS"
else
  echo "STATIC_VERIFY_WITHOUT_READELF=FAIL ($failures check(s) failed)"
fi
exit $((failures > 0))
