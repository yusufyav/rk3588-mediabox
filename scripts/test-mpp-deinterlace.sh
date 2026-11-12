#!/usr/bin/env bash
#
# MPP's deinterlacer on the appliance, against streams whose every field
# shows its own time (tests/mpp): the decoder the product plays with,
# tsdemux -> h264parse/mpegvideoparse -> mppvideodec (fast-mode off, as the
# live engine sets it), its every picture checked for the field it shows
# against the timestamp it carries -- from the key frame of an open GOP on
# (a channel's opening), and across 480 ms lost from the middle of a
# picture on (a transport loss) to the key frame after it.
#
# open-gop-576i-mpeg2-bar.ts is the MPEG-2 opening, top field first, its
# key frame 46.12 s into the stream's running time: from 39.77 s on, and
# for every other 39.77 s, twice that in microseconds at 27 MHz no longer
# fits in 32 bits (packaging/mpp-patches/0003). Made with
#   ffmpeg -f lavfi -i "color=black:s=720x576:r=50:d=1.76[b];
#     color=white:s=8x576:r=50:d=1.76[w];
#     [b][w]overlay=x=8*(n+1):y=0:eval=frame,format=yuv420p"
#     -vf tinterlace=mode=interleave_top,setfield=tff -c:v mpeg2video
#     -flags +ilme+ildct -g 12 -bf 2 -sc_threshold 1000000000 -q:v 2
#     -muxdelay 46 -muxpreload 46 -f mpegts full.ts
# and cut at the second key frame (its PAT and PMT kept): its two B
# pictures before it in time need the GOP cut away; field n shows frame
# n + 1 (tinterlace starts at the second), so the key frame's first is 25.
#
#   scripts/test-mpp-deinterlace.sh            the installed MPP
#   scripts/test-mpp-deinterlace.sh <libdir>   another build of it, e.g. one
#                                              without packaging/mpp-patches/0002
#
# Passes with 0002 (the opening: 50 pictures, fields 28 to 77, each at
# its own time; the loss: from the key frame's first field, 112, on);
# fails without it. The MPEG-2 opening passes with 0003 (61 pictures,
# fields 25 to 85); without it the key frame's two fields and the next P
# picture's are missing (57 pictures from field 27, a 60 ms step).
# Nothing on the appliance is changed; it needs no television, only the
# decoder.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=env.sh
source "$here/scripts/env.sh"

libdir="${1:-}"
work=/var/tmp/mediabox-mpp-deinterlace
mediabox_ssh "rm -rf $work && mkdir -p $work"
mediabox_scp "$here/tests/mpp/open-gop-576i-bar.ts" "$here/tests/mpp/open-gop-576i-bar-hole.ts" \
  "$here/tests/mpp/open-gop-576i-mpeg2-bar.ts" \
  "$here/tests/mpp/deinterlace-check.py" "$MEDIABOX_TARGET:$work/"
mediabox_ssh "bash -s" <<REMOTE
set -euo pipefail
cd $work
gst=/opt/rk3588-mediabox/gstreamer-1.26
export GST_PLUGIN_SYSTEM_PATH_1_0=\$gst/lib/gstreamer-1.0 GST_REGISTRY_1_0=$work/registry.bin
${libdir:+export LD_LIBRARY_PATH=$libdir}
decode() {
  timeout 60 \$gst/bin/gst-launch-1.0 -v filesrc location=\$1 ! tsdemux ! \${2:-h264parse} \
    ! mppvideodec fast-mode=false ! identity silent=false ! filesink location=frames.yuv > launch.log 2>&1
  grep -o 'identity0:sink.*pts: [0-9:.]*' launch.log | sed 's/.*pts: //' \
    | awk -F: '{ printf "%d\\n", (\$1 * 3600 + \$2 * 60 + \$3) * 1e9 }' > pts.txt
}
failed=0
echo "opening (open-gop-576i-bar.ts)"
decode open-gop-576i-bar.ts
python3 deinterlace-check.py frames.yuv pts.txt 720 576 8 28 || failed=1
echo "a loss, then the key frame (open-gop-576i-bar-hole.ts)"
decode open-gop-576i-bar-hole.ts
python3 deinterlace-check.py frames.yuv pts.txt 720 576 4 0 112 || failed=1
echo "MPEG-2 opening, its key frame 46.12 s in (open-gop-576i-mpeg2-bar.ts)"
decode open-gop-576i-mpeg2-bar.ts mpegvideoparse
python3 deinterlace-check.py frames.yuv pts.txt 720 576 8 25 || failed=1
exit \$failed
REMOTE
