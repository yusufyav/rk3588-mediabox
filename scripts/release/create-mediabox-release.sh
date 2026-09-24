#!/usr/bin/env bash
#
# Turn the appliance that works into a release that installs.
#
# There is no build here and there is none in the installer either. The product
# on the golden board is the product: this captures it, records what it is made
# of, and writes a single archive that a clean Armbian can unpack. Nothing is
# compiled on the way in and nothing is compiled on the way out.
#
# The golden board is read-only. Everything this writes on it lives under
# /var/tmp and is removed before the script returns.
#
#   MEDIABOX_HOST=10.27.27.25 scripts/release/create-mediabox-release.sh
#
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=/dev/null
. "$here/scripts/env.sh"

version="$(date -u +%Y-%m-%d)"
out_dir="$here/dist"
keep_remote=0
while [ $# -gt 0 ]; do
  case "$1" in
    --version) version="$2"; shift 2 ;;
    --out)     out_dir="$2"; shift 2 ;;
    --keep-remote-staging) keep_remote=1; shift ;;
    -h|--help) sed -n '2,14p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

asset="mediabox-appliance-${version}-arm64.tar.zst"
say() { printf '\n== %s\n' "$*"; }
die() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

# ---------------------------------------------------------------- the board

say "golden board"
# Which board it is, as the board says it. The product is the same on either
# supported board -- the installer and the control plane resolve what differs
# on the board they land on -- so any one that runs it can be the golden one,
# and the release records which it was rather than assuming.
identity="$(mediabox_ssh 'printf "%s\t%s\t%s\t%s\t%s\n" \
  "$(hostname)" "$(uname -r)" "$(uname -m)" \
  "$(. /etc/os-release; echo "$PRETTY_NAME")" \
  "$(tr -d "\0" </proc/device-tree/model 2>/dev/null)"')" || die "cannot reach $MEDIABOX_HOST"
IFS=$'\t' read -r g_host g_kernel g_arch g_os g_model <<<"$identity"
printf '  host=%s kernel=%s arch=%s\n  os=%s\n  model=%s\n' "$g_host" "$g_kernel" "$g_arch" "$g_os" "$g_model"
[ "$g_arch" = aarch64 ] || die "golden board is $g_arch, not aarch64"
mediabox_ssh "test -d $MEDIABOX_PREFIX" || die "$MEDIABOX_PREFIX absent on the golden board"

stage="/var/tmp/mediabox-release-$$"
cleanup() {
  [ "$keep_remote" -eq 1 ] && return 0
  mediabox_ssh "rm -rf '$stage'" 2>/dev/null || true
}
trap cleanup EXIT

# ------------------------------------------------- system integration files
#
# Two sources, and the split is not a matter of taste. The unit files, the udev
# rules and the JSON tables are packaging: the repository carries them and the
# repository is ahead of the board more often than behind it. The daemon's own
# configuration is not in the repository at all -- it was placed on the board by
# hand -- so for those the board is the only source there is. Which file came
# from where is written down in meta/integration-manifest.tsv rather than
# assumed, and a repository file that differs from the live one is reported.

say "system integration"
mediabox_ssh "mkdir -p '$stage/rootfs/etc/systemd/system' \
  '$stage/rootfs/etc/systemd/logind.conf.d' '$stage/rootfs/etc/udev/rules.d' \
  '$stage/rootfs/etc/alsa/conf.d' \
  '$stage/rootfs/etc/mediabox' '$stage/rootfs/etc/chromium/policies/managed' '$stage/meta'"

# repo path <TAB> installed path
repo_integration=$(cat <<'EOF'
packaging/systemd/mediaboxd-rs.service	etc/systemd/system/mediaboxd-rs.service
packaging/systemd/mediabox-tv-ui.service	etc/systemd/system/mediabox-tv-ui.service
packaging/systemd/mediabox-media-worker.service	etc/systemd/system/mediabox-media-worker.service
packaging/systemd/mediabox-console-off.service	etc/systemd/system/mediabox-console-off.service
packaging/systemd/mediabox-display-changed.service	etc/systemd/system/mediabox-display-changed.service
packaging/systemd/mediabox-display-seed.service	etc/systemd/system/mediabox-display-seed.service
packaging/systemd/mediabox-display-observer.service	etc/systemd/system/mediabox-display-observer.service
packaging/systemd/mediabox-browser.service	etc/systemd/system/mediabox-browser.service
packaging/systemd/mediabox-wireless.service	etc/systemd/system/mediabox-wireless.service
packaging/systemd/mediabox-bt-agent.service	etc/systemd/system/mediabox-bt-agent.service
packaging/systemd/kodi.service	etc/systemd/system/kodi.service
packaging/systemd/stremio-server.service	etc/systemd/system/stremio-server.service
packaging/systemd/logind.conf.d/10-mediabox.conf	etc/systemd/logind.conf.d/10-mediabox.conf
packaging/udev/80-mediabox-no-power-switch.rules	etc/udev/rules.d/80-mediabox-no-power-switch.rules
packaging/udev/81-mediabox-display-hotplug.rules	etc/udev/rules.d/81-mediabox-display-hotplug.rules
config/sway-browser.conf	etc/mediabox/sway-browser.conf
config/alsa/60-mediabox-unrouted.conf	etc/alsa/conf.d/60-mediabox-unrouted.conf
config/mediabox-applications.json	etc/mediabox-applications.json
config/mediabox-library.json	etc/mediabox-library.json
packaging/chromium-policies.json	etc/chromium/policies/managed/mediabox.json
EOF
)
# Installed on the board and nowhere else.
golden_integration="etc/mediaboxd.toml etc/mediabox-media-worker.env"

# Kodi's own profile is configuration, not state, and it does not live under
# /etc or /opt -- it sits in Kodi's home under /var/tmp, which every rule about
# not capturing mutable state says to skip. Skipping it cost a release: the
# clean install it produced could not show a Dolby Vision title and played
# anything that was not plain AC-3 as silence, because Kodi came up on its own
# defaults. The file is not captured into the archive (it is the appliance's
# working copy, with that box's own uuid in it); what happens here is narrower
# and more useful -- the values the product depends on are compared against the
# seed this repository ships, and a difference is reported rather than lost.
kodi_live_profile=/var/tmp/kodi-home/.kodi/userdata/guisettings.xml
kodi_seed="$here/config/kodi/guisettings-appliance.xml"
say "Kodi profile"
if mediabox_ssh "test -f $kodi_live_profile" </dev/null; then
  kodi_drift=0
  for key in videoplayer.useprimerenderer videoscreen.whitelist videoscreen.resolution \
             audiooutput.ac3transcode audiooutput.eac3passthrough audiooutput.dtspassthrough \
             audiooutput.truehdpassthrough audiooutput.dtshdpassthrough audiooutput.passthrough; do
    live="$(mediabox_ssh "sed -n 's|.*<setting id=\"$key\"[^>]*>\([^<]*\)</setting>.*|\1|p' $kodi_live_profile | head -1" </dev/null)"
    # The quotes around the setting's id are part of the pattern, and they have
    # to survive being written inside a double-quoted string: the version that
    # came before this closed the shell's quoting instead of matching an XML
    # one, so the seed side read empty for every key and the capture announced
    # nine settings "differing" from a seed that agreed with the board exactly.
    # A drift report that is always wrong is worse than no drift report.
    seed="$(sed -n "s|.*<setting id=\"$key\"[^>]*>\([^<]*\)</setting>.*|\1|p" "$kodi_seed" | head -1)"
    if [ "$live" = "$seed" ]; then
      printf '  same   %-34s %s\n' "$key" "$(printf '%s' "$live" | cut -c1-40)"
    else
      kodi_drift=$((kodi_drift + 1))
      printf '  DIFFER %-34s board=%s seed=%s\n' "$key" \
        "$(printf '%s' "${live:-<unset>}" | cut -c1-30)" "$(printf '%s' "${seed:-<unset>}" | cut -c1-30)"
    fi
  done
  echo "  settings differing from config/kodi/guisettings-appliance.xml: $kodi_drift"
  [ "$kodi_drift" -eq 0 ] || echo "  (the board is the truth here; fold these back into the seed before releasing)"
else
  echo "  no Kodi profile on the board yet"
fi

drift=0
integration_manifest="$(mktemp)"
while IFS=$'\t' read -r src dst; do
  [ -n "$src" ] || continue
  [ -f "$here/$src" ] || die "integration file missing from the repository: $src"
  mediabox_scp -q "$here/$src" "$MEDIABOX_TARGET:$stage/rootfs/$dst"
  state=LIVE_MATCHES_REPO
  # </dev/null on every ssh in a loop: without it the client reads the loop's
  # own input and the second iteration never happens.
  if mediabox_ssh "test -e /$dst" </dev/null; then
    mediabox_ssh "cmp -s '$stage/rootfs/$dst' '/$dst'" </dev/null || { state=LIVE_DIFFERS_FROM_REPO; drift=$((drift + 1)); }
  else
    state=ABSENT_ON_GOLDEN
  fi
  printf '%s\trepo:%s\t%s\n' "$dst" "$src" "$state" >>"$integration_manifest"
  printf '  repo   %-58s %s\n' "$dst" "$state"
done <<<"$repo_integration"

for dst in $golden_integration; do
  mediabox_ssh "test -e /$dst" </dev/null || die "integration file missing on the golden board: /$dst"
  mediabox_ssh "install -m \"\$(stat -c %a '/$dst')\" '/$dst' '$stage/rootfs/$dst'" </dev/null
  printf '%s\tgolden:/%s\tNOT_IN_REPO\n' "$dst" "$dst" >>"$integration_manifest"
  printf '  golden %-58s %s\n' "$dst" NOT_IN_REPO
done
mediabox_scp -q "$integration_manifest" "$MEDIABOX_TARGET:$stage/meta/integration-manifest.tsv"
rm -f "$integration_manifest"
echo "  integration files differing from the repository: $drift"

# ------------------------------------------------------------ the inventory

say "inventory, closure and capability baseline"
mediabox_ssh "PREFIX='$MEDIABOX_PREFIX' STAGE='$stage' VERSION='$version' \
  GOLDEN_KERNEL='$g_kernel' GOLDEN_OS='$g_os' GOLDEN_ARCH='$g_arch' GOLDEN_MODEL='$g_model' \
  GOLDEN_HOST='$MEDIABOX_HOST' bash -s" <<'REMOTE_EOF'
set -euo pipefail
M="$STAGE/meta"
export LD_LIBRARY_PATH="$PREFIX/mali-g24p0-runtime/lib:$PREFIX/media-runtime/lib"

# What the archive will and will not carry. The backup binary is a copy of a
# previous interface left beside the current one; the caches are generated.
is_excluded() {
  case "$1" in
    */bin/mediabox-tv.pre-account) return 0 ;;
    */__pycache__/*|*.pyc)         return 0 ;;
    *) return 1 ;;
  esac
}

# artifact-manifest.tsv: one line per object, with the mode, the ownership as
# it will be written, the size and -- for regular files -- the digest.
: >"$M/artifact-manifest.tsv"
: >"$M/SHA256SUMS"
elf_list="$(mktemp)"
find "$PREFIX" -xdev -mindepth 1 \( -type f -o -type l -o -type d \) -printf '%y\t%m\t%s\t%p\n' |
  sort -k4 |
  while IFS=$'\t' read -r kind mode size path; do
    is_excluded "$path" && continue
    rel="rootfs${path}"
    case "$kind" in
      d) printf 'dir\t%s\t-\t-\t%s\n' "$mode" "$rel" >>"$M/artifact-manifest.tsv" ;;
      l) printf 'link\t%s\t-\t%s\t%s\n' "$mode" "$(readlink "$path")" "$rel" >>"$M/artifact-manifest.tsv" ;;
      f) sum="$(sha256sum "$path" | cut -d' ' -f1)"
         printf 'file\t%s\t%s\t%s\t%s\n' "$mode" "$size" "$sum" "$rel" >>"$M/artifact-manifest.tsv"
         printf '%s  %s\n' "$sum" "$rel" >>"$M/SHA256SUMS"
         if [ "$(head -c4 "$path" 2>/dev/null | od -An -tx1 | tr -d ' ')" = 7f454c46 ]; then
           printf '%s\n' "$path" >>"$elf_list"
         fi
         ;;
    esac
  done

# The integration files staged a moment ago belong in the same digest list.
( cd "$STAGE" && find rootfs/etc -type f -exec sha256sum {} + ) >>"$M/SHA256SUMS" || true
( cd "$STAGE" && find rootfs/etc \( -type f -o -type d \) -printf '%y\t%m\t%s\t%p\n' |
  sed -E 's/^f\t([0-9]+)\t([0-9]+)\t(.*)$/file\t\1\t\2\t-\t\3/; s/^d\t([0-9]+)\t[0-9]+\t(.*)$/dir\t\1\t-\t-\t\2/' ) \
  >>"$M/artifact-manifest.tsv"

# elf-closure.txt: every shared object each product binary resolves, and where
# it resolves it from. A library the product cannot find, or finds under the
# other product's prefix, is the whole reason this file exists.
{
  echo "# binary<TAB>soname<TAB>resolved<TAB>class"
  while read -r f; do
    ldd "$f" 2>/dev/null | sed 's/^[[:space:]]*//' | while read -r line; do
      case "$line" in
        *"not found"*) printf '%s\t%s\t-\tINVALID\n' "$f" "${line%% *}" ;;
        *"=>"*)
          so="${line%% =>*}"; res="${line#*=> }"; res="${res%% (*}"
          [ -n "$res" ] || continue
          case "$res" in
            "$PREFIX"/*)            cls=PRIVATE_RUNTIME ;;
            /opt/rk3588-screenbridge/*) cls=SCREENBRIDGE ;;
            /*)                     cls=OS_RUNTIME ;;
            *)                      continue ;;
          esac
          printf '%s\t%s\t%s\t%s\n' "$f" "$so" "$res" "$cls" ;;
      esac
    done
  done <"$elf_list"
} >"$M/elf-closure.txt"

# runpath.tsv: the linkage the two players carry, as the golden board reads it.
#
# The verifier reads the same thing out of the archived binaries itself -- it
# has to, since a release cut before this file existed does not carry it -- and
# this is the second opinion it checks that reading against. It is written here
# because the capture runs on a development machine where readelf exists; the
# target is a clean Minimal image where it does not, and the archive is checked
# there before a single package is installed.
{
  echo "# binary<TAB>runpath"
  for b in player/bin/mpv kodi/lib/kodi/kodi-gbm; do
    [ -f "$PREFIX/$b" ] || continue
    printf '%s\t%s\n' "$b" "$(readelf -d "$PREFIX/$b" 2>/dev/null |
      sed -n 's/.*R\(UN\)\?PATH).*\[\(.*\)\]/\2/p' | head -1)"
  done
} >"$M/runpath.tsv"

# systemd-closure.txt: every absolute path a unit names, classified. A unit
# that starts something the release does not carry is a release that does not
# start.
{
  echo "# unit<TAB>directive<TAB>path<TAB>class"
  for u in "$STAGE"/rootfs/etc/systemd/system/*.service; do
    un="$(basename "$u")"
    grep -hoE '^(ExecStart|ExecStartPre|ExecStartPost|ExecStop|ExecStopPost|ExecReload|EnvironmentFile|WorkingDirectory)=[^ ]*' "$u" |
      while IFS='=' read -r directive value; do
        p="${value#-}"; p="${p#+}"
        case "$p" in /*) ;; *) continue ;; esac
        case "$p" in
          "$PREFIX"/*) cls=RELEASE_ARTIFACT ;;
          /etc/*)      cls=REPO_INSTALL_FILE ;;
          /var/*|/run/*|/tmp/*) cls=RUNTIME_STATE ;;
          /usr/*|/bin/*|/sbin/*|/lib/*) cls=OS_RUNTIME ;;
          *) cls=UNCLASSIFIED ;;
        esac
        printf '%s\t%s\t%s\t%s\n' "$un" "$directive" "$p" "$cls"
      done
  done
} >"$M/systemd-closure.txt"

# runtime-packages.txt: the Debian packages that own the libraries and the
# commands the product actually resolves. Derived from the closure above, not
# copied from a deploy script's apt line. No compiler and no -dev package: the
# installer has no build step to feed them to.
{
  awk -F'\t' '$4=="OS_RUNTIME"{print $3}' "$M/elf-closure.txt" |
    sed 's|^/lib/|/usr/lib/|' | sort -u |
    while read -r l; do dpkg-query -S "$l" 2>/dev/null | cut -d: -f1; done |
    tr ',' '\n' | tr -d ' '
  # ffprobe is not linked into anything, so no ELF closure finds it -- and it is
  # the first thing the worker runs on every source. The board this was first
  # captured from happened to have Debian's ffmpeg installed; the clean board
  # did not, and the television's own player never started. It is named here so
  # a clean install gets it, and gets a probe that can open https, which the
  # appliance's own Rockchip build cannot.
  #
  # bluetoothctl, btattach and rfkill are named for the same reason: the
  # product does not link against bluetoothd, so no ELF closure reaches it,
  # and a board without them has radios it cannot switch on. Both boards ship
  # the radios blocked at boot -- "[BT_RFKILL]: bt shut off power" -- and the
  # unblock needs rfkill present; on the Ultra the vendor's own attach unit
  # even fails with 203/EXEC when it is missing, because its ExecStartPre is
  # /usr/sbin/rfkill. The firmware, the patchram tool and that unit come from
  # the board's BSP and are already on a clean image; these two are not.
  for c in python3 ffprobe ffmpeg sway swaymsg chromium amixer modetest chvt openvt setterm kbd_mode fc-list fc-match bluetoothctl btattach rfkill; do
    p="$(command -v "$c" 2>/dev/null)" || continue
    dpkg-query -S "$(readlink -f "$p")" 2>/dev/null | cut -d: -f1
  done | tr ',' '\n' | tr -d ' '

  # The fonts the interface actually draws with.
  #
  # No ELF closure finds these: the interface asks fontconfig at run time and
  # takes what the board has. A clean board had DejaVu and nothing else, which
  # covers Turkish perfectly and covers none of the emoji a catalogue is full
  # of -- every stream row drew its seeders and its size and its language flags
  # as empty boxes. So the packages behind the body face and behind the emoji
  # coverage are read off this board and named, rather than assumed to be part
  # of a base image.
  for ch in 0041 1F4BE 1F464 1F1F9; do
    fc-list ":charset=$ch" file 2>/dev/null | head -1 | cut -d: -f1
  done | sed '/^$/d' | sort -u |
    while read -r f; do dpkg-query -S "$f" 2>/dev/null | cut -d: -f1; done |
    tr ',' '\n' | tr -d ' '
} | sed '/^$/d' | sort -u >"$M/runtime-packages.txt"

# capability-baseline.txt: what the kernel underneath the golden binaries
# offered them. The installer checks a clean board against this; it does not
# demand the same kernel version.
{
  echo "kernel	$GOLDEN_KERNEL"
  echo "arch	$GOLDEN_ARCH"
  echo "os	$GOLDEN_OS"
  for n in /dev/mpp_service /dev/rga /dev/mali0 /dev/dri/card0 /dev/dri/renderD128; do
    printf 'device\t%s\t%s\n' "$n" "$([ -e "$n" ] && echo present || echo absent)"
  done
  for d in /sys/class/drm/card*/device/driver; do
    [ -e "$d" ] || continue
    printf 'drm-driver\t%s\t%s\n' "${d%/device/driver}" "$(basename "$(readlink -f "$d")")"
  done
  for s in /sys/class/drm/card*-*/status; do
    [ -e "$s" ] || continue
    printf 'connector\t%s\t%s\n' "$(basename "$(dirname "$s")")" "$(cat "$s")"
  done
  awk '/^ *[0-9]+ \[/{print "alsa-card\t" $0}' /proc/asound/cards 2>/dev/null || true
  printf 'platform-inspect\t%s\n' \
    "$("$PREFIX/bin/mediabox-platform" inspect >/dev/null 2>&1 && echo ok || echo unavailable)"
  printf 'mpv-vo-mediabox\t%s\n' \
    "$("$PREFIX/player/bin/mpv" --vo=help 2>/dev/null | grep -qw mediabox && echo present || echo absent)"
  printf 'mpv-hwdec-rkmpp\t%s\n' \
    "$("$PREFIX/player/bin/mpv" --hwdec=help 2>/dev/null | grep -q rkmpp && echo present || echo absent)"
} >"$M/capability-baseline.txt"

rm -f "$elf_list"

# release-info: what can be proved, and only that.
cat >"$M/release-info" <<EOF
version=$VERSION
artifact_source=live-golden-board
opt_source=live-golden-board
golden_model=$GOLDEN_MODEL
etc_source=repo-packaging+golden-only-config
golden_capture_host=$GOLDEN_HOST
golden_capture_time=$(date -u +%Y-%m-%dT%H:%M:%SZ)
golden_kernel=$GOLDEN_KERNEL
golden_os=$GOLDEN_OS
golden_arch=$GOLDEN_ARCH
prefix=$PREFIX
reference_kernel=$GOLDEN_KERNEL
exact_kernel_required=false
kernel_family=Rockchip vendor 6.1
capability_gate_required=true
ownership_policy=normalized-root
# The binaries below were captured from a running appliance. Whether they were
# produced from the packaging repository's current revision is not something a
# capture can establish, so it is not claimed here; the repository head is
# recorded as the revision of the installer and the manifest, nothing more.
EOF
REMOTE_EOF

# Provenance of the packaging side is local knowledge, so it is added here.
repo_head="$(git -C "$here" rev-parse HEAD)"
origin_main="$(git -C "$here" rev-parse origin/main 2>/dev/null || echo unknown)"
mediabox_ssh "printf 'packaging_repo_head=%s\norigin_main=%s\n' '$repo_head' '$origin_main' \
  >>'$stage/meta/release-info'"

# -------------------------------------------------------------- the gates
#
# A release that fails any of these is not a release. There is no fallback to
# building the missing piece: the point of this archive is that nothing is built
# on the target, and a capture that papers over a gap would move the build there.

say "release gates"
gate_rc=0
gate_report="$(mediabox_ssh "PREFIX='$MEDIABOX_PREFIX' STAGE='$stage' bash -s" <<'REMOTE_EOF'
set -euo pipefail
M="$STAGE/meta"
fail=0
g() { printf '  %-34s %s\n' "$1" "$2"; [ "$2" = PASS ] || fail=$((fail + 1)); }

test -x "$PREFIX/player/bin/mpv" && g "mpv present" PASS || g "mpv present" GOLDEN_RELEASE_NOT_READY
grep -q '^mpv-vo-mediabox	present$'  "$M/capability-baseline.txt" && g "mpv vo_mediabox" PASS || g "mpv vo_mediabox" FAIL
grep -q '^mpv-hwdec-rkmpp	present$' "$M/capability-baseline.txt" && g "mpv hwdec rkmpp" PASS || g "mpv hwdec rkmpp" FAIL
readelf -d "$PREFIX/player/bin/mpv" 2>/dev/null | grep -q "$PREFIX/media-runtime/lib" \
  && g "mpv RUNPATH" PASS || g "mpv RUNPATH" FAIL
test -x "$PREFIX/kodi/lib/kodi/kodi-gbm" && g "kodi present" PASS || g "kodi present" FAIL
readelf -d "$PREFIX/kodi/lib/kodi/kodi-gbm" 2>/dev/null | grep -q "$PREFIX/media-runtime/lib" \
  && g "kodi RUNPATH" PASS || g "kodi RUNPATH" FAIL

grep -q "^player/bin/mpv	.*$PREFIX/media-runtime/lib" "$M/runpath.tsv" \
  && g "mpv RUNPATH recorded" PASS || g "mpv RUNPATH recorded" FAIL
grep -q "^kodi/lib/kodi/kodi-gbm	.*$PREFIX/media-runtime/lib" "$M/runpath.tsv" \
  && g "kodi RUNPATH recorded" PASS || g "kodi RUNPATH recorded" FAIL

n="$(awk -F'\t' '$4=="SCREENBRIDGE"' "$M/elf-closure.txt" | wc -l)"
[ "$n" -eq 0 ] && g "ScreenBridge linkage ($n)" PASS || g "ScreenBridge linkage ($n)" FAIL
n="$(awk -F'\t' '$4=="INVALID"' "$M/elf-closure.txt" | wc -l)"
[ "$n" -eq 0 ] && g "unresolved libraries ($n)" PASS || g "unresolved libraries ($n)" FAIL
n="$(awk -F'\t' '$4=="UNCLASSIFIED"' "$M/systemd-closure.txt" | wc -l)"
[ "$n" -eq 0 ] && g "unresolved unit paths ($n)" PASS || g "unresolved unit paths ($n)" FAIL

# Every RELEASE_ARTIFACT path a unit names has to be in the manifest.
missing=0
while IFS=$'\t' read -r _ _ p cls; do
  [ "$cls" = RELEASE_ARTIFACT ] || continue
  grep -qF "	rootfs$p" "$M/artifact-manifest.tsv" || { echo "  unit path absent from manifest: $p"; missing=$((missing + 1)); }
done < <(tail -n +2 "$M/systemd-closure.txt")
[ "$missing" -eq 0 ] && g "unit paths in manifest ($missing)" PASS || g "unit paths in manifest ($missing)" FAIL

n="$(find "$PREFIX" -xdev -xtype l | wc -l)"
[ "$n" -eq 0 ] && g "broken symlinks ($n)" PASS || g "broken symlinks ($n)" FAIL

# A symlink inside the prefix may point outside it only if the target is also
# carried; an absolute link into a path the archive does not have is a link
# that breaks on a clean board.
dangle=0
while IFS=$'\t' read -r kind _ _ target rel; do
  [ "$kind" = link ] || continue
  case "$target" in
    /*) grep -qF "	rootfs$target" "$M/artifact-manifest.tsv" || { echo "  link outside the archive: ${rel#rootfs} -> $target"; dangle=$((dangle + 1)); } ;;
  esac
done <"$M/artifact-manifest.tsv"
[ "$dangle" -eq 0 ] && g "absolute links resolved ($dangle)" PASS || g "absolute links resolved ($dangle)" FAIL

# Nothing that belongs to a person rather than to the product.
leak="$( { grep -rIlE 'authKey|"password"|BEGIN (RSA |EC |OPENSSH )?PRIVATE KEY' \
  "$STAGE/rootfs/etc" 2>/dev/null || true; } | wc -l)"
[ "$leak" -eq 0 ] && g "credentials in integration ($leak)" PASS || g "credentials in integration ($leak)" FAIL

# And no compiler in the runtime package list.
bad="$(grep -cE '^(build-essential|gcc|g\+\+|rustc|cargo|cmake|meson|ninja-build|.*-dev)$' "$M/runtime-packages.txt" || true)"
[ "$bad" -eq 0 ] && g "build packages in manifest ($bad)" PASS || g "build packages in manifest ($bad)" FAIL

# And nothing staged, in a prefix that is supposed to be only what was installed.
#
# An accepted A/B candidate of Kodi was once copied over the production binary
# while its data tree stayed behind under .../kodi-candidate. It ran for as long
# as that tree was on the disk and died one second into every start once the
# tree was tidied away. A capture taken in between would have shipped a release
# that breaks the first time anybody cleans up, so the check is on the prefix
# the binary was configured with rather than on whether the tree is there today.
leftovers="$(
  { find "$PREFIX" -xdev -mindepth 1 \
      \( -name '*.orig' -o -name '*.bak' -o -name '*candidate*' \
         -o -name 'staging' -o -name 'kodi-src' \) -printf '%P\n' 2>/dev/null
    find "$PREFIX" -xdev -mindepth 1 -maxdepth 2 -name 'build' -printf '%P\n' 2>/dev/null
  } | sort -u)"
n="$(printf '%s' "$leftovers" | grep -c . || true)"
[ "$n" -eq 0 ] && g "staging leftovers ($n)" PASS \
  || { printf '%s\n' "$leftovers" | sed 's/^/    /'; g "staging leftovers ($n)" FAIL; }

baked="$(LC_ALL=C grep -aoE '/opt/rk3588-mediabox/[A-Za-z0-9._+-]+/share/kodi' \
  "$PREFIX/kodi/lib/kodi/kodi-gbm" 2>/dev/null | sort -u || true)"
[ "$baked" = "/opt/rk3588-mediabox/kodi/share/kodi" ] \
  && g "Kodi data prefix" PASS || g "Kodi data prefix (${baked:-none})" FAIL
test -f "$PREFIX/kodi/share/kodi/system/settings/settings.xml" \
  && g "Kodi data files" PASS || g "Kodi data files" FAIL

echo "GATE_FAILURES=$fail"
REMOTE_EOF
)" || gate_rc=$?
printf '%s\n' "$gate_report"
[ "$gate_rc" -eq 0 ] || die "the gate script itself failed on the golden board (exit $gate_rc)"
gate_failures="$(printf '%s\n' "$gate_report" | sed -n 's/^GATE_FAILURES=//p')"
[ "${gate_failures:-1}" -eq 0 ] || die "GOLDEN_RELEASE_NOT_READY: $gate_failures gate(s) failed on the golden board"

# --------------------------------------------------------------- the archive
#
# One pass, straight off the board. The prefix is not copied into the staging
# directory first: it is 1.2 GB and the copy would buy nothing. tar reads it
# where it lives and renames it into the archive's rootfs as it goes.

say "archive"
mkdir -p "$out_dir"
command -v zstd >/dev/null || die "zstd is needed on this workstation to write the archive"
# The tar is built on the board and compressed here. The golden board has no
# zstd and it is not going to get one: installing a package to make a release is
# a change to the appliance the release is supposed to be a copy of.
mediabox_ssh "cd '$stage' && tar --numeric-owner --owner=0 --group=0 --xattrs --acls \
    --exclude='*/__pycache__' --exclude='*.pyc' \
    --exclude='opt/rk3588-mediabox/bin/mediabox-tv.pre-account' \
    -cf - meta rootfs/etc \
    -C / --transform='s,^opt/,rootfs/opt/,' opt/rk3588-mediabox" \
  | zstd -T0 -6 -c >"$out_dir/$asset"

[ -s "$out_dir/$asset" ] || die "the archive came back empty"
sha="$(sha256sum "$out_dir/$asset" | cut -d' ' -f1)"
printf '%s  %s\n' "$sha" "$asset" >"$out_dir/${asset%.tar.zst}.sha256"

size="$(stat -c %s "$out_dir/$asset")"
say "written"
printf '  %s\n  %s bytes (%s)\n  %s\n' \
  "$out_dir/$asset" "$size" "$(numfmt --to=iec "$size")" "$sha"

cat <<EOF

  Pin it with:
    scripts/release/create-mediabox-release.sh --version $version
    MEDIABOX_RELEASE_TAG=appliance-golden-$version
EOF
