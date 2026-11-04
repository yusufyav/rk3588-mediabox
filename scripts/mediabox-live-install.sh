#!/bin/sh
#
# On the appliance: install mediabox-live's bundle (scripts/build-mediabox-live.sh
# makes it and copies it here), or say what installing it would do.
#
#   install.sh --plan    <live sha256> <launcher sha256>   read only
#   install.sh --install <live sha256> <launcher sha256>
#
# The two hashes are what /opt/rk3588-mediabox/bin/mediabox-live and
# bin/mediabox-player were when the plan was made; anything else, and it
# refuses. So does a live channel playing now, or a runtime already at the
# versioned directory.
#
# Install, in order; any failure before step 6 undoes what was done:
#   1. the bundle's own hashes, and every file of the runtime once unpacked
#   2. a rollback copy (cp -a) of the binary, the launcher and the media
#      runtime's mppvideodec, with their hashes and rollback.sh, in
#      /opt/rk3588-mediabox/rollback/mediabox-live-<time>
#   3. the runtime unpacked into a temporary directory beside its place,
#      mppvideodec added to its plugin directory, renamed to
#      gstreamer-1.26.11; gstreamer-1.26 made a symlink to it by rename
#   4. checked before anything uses it: version, every element mediabox-live
#      builds, and the loader -- no Debian GStreamer library, no plugin from
#      the media runtime, in a process of the new runtime
#   5. mediabox-live checked by the loader from its staging name
#   6. mediabox-live and the launcher renamed into place
# No service restarts: the launcher is started per live channel by mediaboxd
# (systemd-run), so the next channel opened is the first on the new one.
# Debian's GStreamer and the media runtime's plugin stay as they are.
set -eu

P=/opt/rk3588-mediabox
G=$P/gstreamer-1.26
V=$P/gstreamer-1.26.11
ELEMENTS="tsdemux h264parse h265parse mpegvideoparse mpegaudioparse aacparse ac3parse
  mpg123audiodec avdec_aac avdec_ac3 avdec_eac3 appsrc appsink input-selector queue
  alsasink audioconvert audioresample volume identity fakesink typefind mppvideodec"

mode="${1:-}"; expect_live="${2:-}"; expect_player="${3:-}"
case "$mode" in --plan|--install) ;; *) echo "usage: install.sh --plan|--install <live sha256> <launcher sha256>" >&2; exit 2 ;; esac
[ ${#expect_live} = 64 ] && [ ${#expect_player} = 64 ] || { echo "two sha256 hashes are needed" >&2; exit 2; }

refusals=0
refuse() { echo "REFUSE: $*" >&2; refusals=$((refusals + 1)); }
sha() { sha256sum "$1" | cut -c1-64; }

# --- the state the plan was made for -----------------------------------------
[ "$(sha $P/bin/mediabox-live)" = "$expect_live" ] || refuse "bin/mediabox-live is $(sha $P/bin/mediabox-live), not $expect_live"
[ "$(sha $P/bin/mediabox-player)" = "$expect_player" ] || refuse "bin/mediabox-player is $(sha $P/bin/mediabox-player), not $expect_player"
for p in /proc/[0-9]*; do
  if [ "$(readlink "$p/exe" 2>/dev/null)" = "$P/bin/mediabox-live" ]; then refuse "mediabox-live is playing (pid ${p#/proc/})"; fi
done
# The runtime may be in place from an earlier install: then it is checked
# file by file against this bundle's, mppvideodec included, and kept -- never
# unpacked over. Anything else there is refused.
runtime_kept=no
# The bundle beside this script, when it is run from it (--install); a plan
# read from stdin has none here and says what the install will check.
case "$0" in */install.sh) BUNDLE=$(cd "$(dirname "$0")" && pwd) ;; *) BUNDLE="" ;; esac
if [ -e "$V" ] && [ -z "$BUNDLE" ] && [ "$mode" = --plan ]; then
  runtime_kept=yes
  echo "note: $V is in place; --install checks it file by file against the bundle and refuses if it differs"
elif [ -e "$V" ]; then
  if [ "$(readlink "$G" 2>/dev/null)" = gstreamer-1.26.11 ] \
     && [ -n "$BUNDLE" ] && ( cd "$V" && sha256sum -c --quiet "$BUNDLE/runtime.files.sha256" ) \
     && [ "$(sha "$V/lib/gstreamer-1.0/libgstrockchipmpp.so")" = "$(sha "$BUNDLE/libgstrockchipmpp.so")" ] \
     && [ "$(( $(cd "$V" && find . -type f | wc -l) - 1 ))" = "$(wc -l < "$BUNDLE/runtime.files.sha256")" ]; then
    runtime_kept=yes
  else
    refuse "$V exists and is not this bundle's runtime (or $G does not point at it)"
  fi
elif [ -e "$G" ] || [ -L "$G" ]; then
  [ -L "$G" ] || refuse "$G exists and is not a symlink"
fi
link_before=$( [ -L "$G" ] && echo yes || echo no )
avail=$(df -Pk $P | awk 'NR==2 {print $4}')
[ "$avail" -gt 1048576 ] || refuse "less than 1 GiB free under $P"

stamp=$(date +%Y%m%dT%H%M%S)
R=$P/rollback/mediabox-live-$stamp

if [ "$mode" = --plan ]; then
  echo "appliance now:"
  echo "  bin/mediabox-live    $(sha $P/bin/mediabox-live)"
  echo "  bin/mediabox-player  $(sha $P/bin/mediabox-player)"
  echo "  media-runtime mppvideodec $(sha $P/media-runtime/lib/gstreamer-1.0/libgstrockchipmpp.so) (kept)"
  echo "  gstreamer-1.26       $( [ -L $G ] && readlink $G || { [ -e $G ] && echo 'a directory' || echo absent; })"
  echo "  free under $P        $((avail / 1024)) MiB"
  echo "writes, in order:"
  echo "  mkdir $R; cp -a bin/mediabox-live bin/mediabox-player media-runtime/lib/gstreamer-1.0/libgstrockchipmpp.so -> it; SHA256SUMS, rollback.sh"
  if [ $runtime_kept = yes ]; then echo "  runtime: $V in place, every file and mppvideodec the bundle's -- kept, nothing written there"; else
  echo "  $P/.gstreamer-1.26.11.tmp <- runtime.tar.xz (strip opt/rk3588-mediabox/gstreamer-1.26), each file checked against runtime.files.sha256"
  echo "  $P/.gstreamer-1.26.11.tmp/lib/gstreamer-1.0/libgstrockchipmpp.so <- libgstrockchipmpp.so (0755 root:root)"
  echo "  mv -T .gstreamer-1.26.11.tmp $V"
  echo "  ln -s gstreamer-1.26.11 $P/.gstreamer-1.26.link; mv -T it $G"; fi
  echo "  /var/cache/mediabox-live/ (registry, built by the check)"
  echo "  check: gst-inspect $(echo $ELEMENTS | wc -w) elements, version 1.26.11, LD_DEBUG=libs: no /usr/lib/aarch64-linux-gnu/libgst*, no media-runtime/lib/gstreamer-1.0"
  echo "  $P/bin/.mediabox-live.new, .mediabox-player.new (install -m 0755 -o root -g root); LD_TRACE_LOADED_OBJECTS on the new binary; mv -f into bin/"
  echo "services: none restarted; the next live channel opened starts the new binary"
  echo "rollback: sh $R/rollback.sh (restores both files by rename; the $G symlink $( [ $link_before = yes ] && echo 'stays, as it was before' || echo 'removed, $V left passive'))"
  [ $refusals = 0 ] && echo "state: as expected; --install would proceed" || echo "state: $refusals refusal(s); --install would refuse"
  exit 0
fi

[ $refusals = 0 ] || { echo "nothing installed" >&2; exit 1; }
B=$(cd "$(dirname "$0")" && pwd)
cd "$B"
sha256sum -c --quiet SHA256SUMS || { echo "bundle damaged in transfer; nothing installed" >&2; exit 1; }

# --- 2. rollback copy ------------------------------------------------------------
mkdir -p "$R"
cp -a $P/bin/mediabox-live $P/bin/mediabox-player $P/media-runtime/lib/gstreamer-1.0/libgstrockchipmpp.so "$R/"
( cd "$R" && sha256sum mediabox-live mediabox-player libgstrockchipmpp.so > SHA256SUMS )
cat > "$R/rollback.sh" <<ROLLBACK
#!/bin/sh
# Back to what was there before mediabox-live-$stamp. The new runtime stays
# at $V, unused.
set -eu
R=\$(cd "\$(dirname "\$0")" && pwd)
for p in /proc/[0-9]*; do
  if [ "\$(readlink "\$p/exe" 2>/dev/null)" = "$P/bin/mediabox-live" ]; then echo "mediabox-live is playing (pid \${p#/proc/}); stop the channel first" >&2; exit 1; fi
done
( cd "\$R" && sha256sum -c --quiet SHA256SUMS )
cp -a "\$R/mediabox-live" $P/bin/.mediabox-live.rollback && mv -f $P/bin/.mediabox-live.rollback $P/bin/mediabox-live
cp -a "\$R/mediabox-player" $P/bin/.mediabox-player.rollback && mv -f $P/bin/.mediabox-player.rollback $P/bin/mediabox-player
# The symlink was there before this install ($link_before): kept if so.
if [ $link_before = no ] && [ -L $G ]; then rm -f $G; fi
cd $P/bin && sha256sum mediabox-live mediabox-player | sed 's/^/restored /'
grep -E ' (mediabox-live|mediabox-player)\$' "\$R/SHA256SUMS" | (cd $P/bin && sha256sum -c --quiet -)
if [ $link_before = no ]; then echo "rolled back; $V left in place, not used"; else echo "rolled back; $G -> $V kept, as it was before"; fi
ROLLBACK
chmod 0755 "$R/rollback.sh"
echo "rollback copy: $R"

undo() {
  echo "FAILED: $*; undoing" >&2
  rm -rf "$P/.gstreamer-1.26.11.tmp" "$P/.gstreamer-1.26.link" "$P/bin/.mediabox-live.new" "$P/bin/.mediabox-player.new"
  if [ $link_before = no ] && [ -L "$G" ] && [ "$(readlink "$G")" = gstreamer-1.26.11 ]; then rm -f "$G"; fi
  echo "bin/ untouched; $V (if made) left passive; rollback copy $R" >&2
  exit 1
}

# --- 3. the runtime, beside its place, then renamed -----------------------------
T=$P/.gstreamer-1.26.11.tmp
if [ $runtime_kept = yes ]; then
  echo "runtime: $G -> $(readlink $G), checked against the bundle, kept"
else
rm -rf "$T" && mkdir "$T"
tar -C "$T" --strip-components=3 -xJf runtime.tar.xz || undo "unpacking the runtime"
[ "$(sed -n 1p "$T/BUILD-INFO")" = "$(cat runtime.build-info)" ] || undo "BUILD-INFO is not the bundle's"
( cd "$T" && sha256sum -c --quiet "$B/runtime.files.sha256" ) || undo "runtime files differ from the bundle's list"
[ "$(cd "$T" && find . -type f | wc -l)" = "$(wc -l < runtime.files.sha256)" ] || undo "runtime has files not in the bundle's list"
install -m 0755 -o root -g root libgstrockchipmpp.so "$T/lib/gstreamer-1.0/libgstrockchipmpp.so"
mv -T "$T" "$V" || undo "renaming the runtime"
ln -s gstreamer-1.26.11 "$P/.gstreamer-1.26.link" && mv -T "$P/.gstreamer-1.26.link" "$G" || undo "switching $G"
echo "runtime: $G -> $(readlink $G), $(sed -n 1p $G/BUILD-INFO)"
fi

# --- 4. the runtime checked before anything uses it -----------------------------
gstenv() {
  env -u GST_PLUGIN_PATH -u GST_PLUGIN_PATH_1_0 -u GST_PLUGIN_SYSTEM_PATH \
    GST_PLUGIN_SYSTEM_PATH_1_0="$G/lib/gstreamer-1.0" "$@"
}
mkdir -p /var/cache/mediabox-live
check=$(mktemp -d)
version=$(gstenv GST_REGISTRY_1_0="$check/registry.bin" "$G/bin/gst-inspect-1.0" --version | sed -n 's/^GStreamer //p')
[ "$version" = 1.26.11 ] || undo "gst-inspect says GStreamer $version"
for e in $ELEMENTS; do
  gstenv GST_REGISTRY_1_0="$check/registry.bin" "$G/bin/gst-inspect-1.0" "$e" > "$check/$e.txt" 2>&1 || undo "element $e missing"
done
sed -n 's/^ *Filename *//p' "$check/mppvideodec.txt" | grep -qx "$V/lib/gstreamer-1.0/libgstrockchipmpp.so\|$G/lib/gstreamer-1.0/libgstrockchipmpp.so" ||
  undo "mppvideodec is not the runtime's: $(sed -n 's/^ *Filename *//p' "$check/mppvideodec.txt")"
# Every plugin of the runtime loaded in one process (no scanner fork), and
# every object the loader brought in listed.
gstenv GST_REGISTRY_1_0="$check/loader.bin" GST_REGISTRY_FORK=no LD_DEBUG=libs \
  "$G/bin/gst-inspect-1.0" mppvideodec > /dev/null 2> "$check/ld.txt" || undo "gst-inspect under LD_DEBUG"
sed -n 's/.*calling init: //p' "$check/ld.txt" | sort -u > "$check/loaded.txt"
echo "loaded by one process of the runtime: $(wc -l < "$check/loaded.txt") objects, $(grep -c 'libgst' "$check/loaded.txt") GStreamer"
if grep -E '^/usr/lib/aarch64-linux-gnu/(libgst|gstreamer-1\.0/)' "$check/loaded.txt"; then undo "Debian GStreamer loaded"; fi
if grep -E 'media-runtime/lib/gstreamer-1\.0/' "$check/loaded.txt"; then undo "the media runtime's plugin loaded"; fi
if grep 'libgst' "$check/loaded.txt" | grep -vE "^($G|$V)/"; then undo "a GStreamer library from outside the runtime"; fi
grep -E 'librockchip_mpp|libgstrockchipmpp' "$check/loaded.txt" || undo "mppvideodec or MPP not loaded"
# The product's registry, as the launcher will use it.
gstenv GST_REGISTRY_1_0=/var/cache/mediabox-live/gstreamer-registry.aarch64.bin "$G/bin/gst-inspect-1.0" mppvideodec > /dev/null

# --- 5. mediabox-live by the loader, from its staging name ----------------------
install -m 0755 -o root -g root mediabox-live $P/bin/.mediabox-live.new
install -m 0755 -o root -g root mediabox-player $P/bin/.mediabox-player.new
LD_TRACE_LOADED_OBJECTS=1 $P/bin/.mediabox-live.new > "$check/live-ldd.txt" 2>&1 || undo "loader cannot resolve mediabox-live"
if grep -E 'not found' "$check/live-ldd.txt"; then undo "mediabox-live has unresolved libraries"; fi
if grep -E 'libgst' "$check/live-ldd.txt" | grep -vE "=> ($G|$V)/"; then undo "mediabox-live resolves GStreamer outside the runtime"; fi
grep -E 'libgst' "$check/live-ldd.txt" | sed 's/^[[:space:]]*/  /'

# --- 6. into place, by rename ----------------------------------------------------
mv -f $P/bin/.mediabox-live.new $P/bin/mediabox-live
mv -f $P/bin/.mediabox-player.new $P/bin/mediabox-player
[ "$(sha $P/bin/mediabox-live)" = "$(grep ' mediabox-live$' SHA256SUMS | cut -c1-64)" ] || { echo "bin/mediabox-live is not the bundle's after install: sh $R/rollback.sh" >&2; exit 1; }
[ "$(sha $P/bin/mediabox-player)" = "$(grep ' mediabox-player$' SHA256SUMS | cut -c1-64)" ] || { echo "bin/mediabox-player is not the bundle's after install: sh $R/rollback.sh" >&2; exit 1; }
rm -rf "$check"
echo "installed:"
( cd $P && sha256sum bin/mediabox-live bin/mediabox-player gstreamer-1.26/lib/gstreamer-1.0/libgstrockchipmpp.so ) | sed 's/^/  /'
echo "rollback: sh $R/rollback.sh"
