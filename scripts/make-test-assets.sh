#!/usr/bin/env bash
# Builds the Gate MP1a test assets on the workstation.
#
# Two clips, identical geometry and frame rate, differing only in what the A/B
# ladder needs to differ in:
#
#   sdr-4k-2398-main.mp4     HEVC Main   8-bit  BT.709          -> NV12
#   hdr10-4k-2398-main10.mp4 HEVC Main10 10-bit BT.2020 + PQ    -> NV15
#
# Synthetic content is used deliberately: no real HDR10 file exists on either
# machine, and a generated clip carries exactly the metadata we assert on.
set -euo pipefail

out_dir="${1:-assets}"
duration="${MEDIABOX_ASSET_SECONDS:-3}"
mkdir -p "$out_dir"

command -v ffmpeg >/dev/null || { echo "ffmpeg not found on this workstation" >&2; exit 1; }
# Capture first: piping into `grep -q` under `pipefail` fails the pipeline when
# grep exits early and ffmpeg takes SIGPIPE.
encoders="$(ffmpeg -hide_banner -encoders 2>/dev/null || true)"
case "$encoders" in
  *libx265*) ;;
  *) echo "ffmpeg has no libx265; cannot build a Main 10 asset" >&2; exit 1 ;;
esac

sdr="$out_dir/sdr-4k-2398-main.mp4"
hdr="$out_dir/hdr10-4k-2398-main10.mp4"

echo "== building SDR baseline asset: $sdr"
ffmpeg -hide_banner -loglevel error -y \
  -f lavfi -i "testsrc2=size=3840x2160:rate=24000/1001:duration=${duration},format=yuv420p" \
  -c:v libx265 -preset ultrafast -crf 28 -pix_fmt yuv420p \
  -color_primaries bt709 -color_trc bt709 -colorspace bt709 -color_range tv \
  -x265-params "colorprim=bt709:transfer=bt709:colormatrix=bt709:range=limited:repeat-headers=1" \
  -f mp4 "$sdr"

echo "== building HDR10 asset: $hdr"
ffmpeg -hide_banner -loglevel error -y \
  -f lavfi -i "testsrc2=size=3840x2160:rate=24000/1001:duration=${duration},format=yuv420p10le" \
  -c:v libx265 -preset ultrafast -crf 28 -pix_fmt yuv420p10le \
  -color_primaries bt2020 -color_trc smpte2084 -colorspace bt2020nc -color_range tv \
  -x265-params "colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc:range=limited:master-display=G(8500,39850)B(6550,2300)R(35400,14600)WP(15635,16450)L(10000000,50):max-cll=1000,400:hdr10=1:hdr10-opt=1:repeat-headers=1" \
  -f mp4 "$hdr"

for f in "$sdr" "$hdr"; do
  echo "== $f"
  ffprobe -hide_banner -v error -select_streams v:0 \
    -show_entries stream=codec_name,profile,width,height,pix_fmt,color_primaries,color_transfer,color_space,r_frame_rate,nb_frames \
    -of default=noprint_wrappers=1 "$f"
  ffprobe -hide_banner -v error -select_streams v:0 -show_frames -read_intervals "%+#1" \
    -show_entries frame_side_data=side_data_type,red_x,red_y,green_x,green_y,blue_x,blue_y,white_point_x,white_point_y,min_luminance,max_luminance,max_content,max_average \
    -of default=noprint_wrappers=1 "$f"
done

sha256sum "$sdr" "$hdr"
