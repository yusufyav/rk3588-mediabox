#!/bin/sh
# 4K reference and its 1080p source, tightly packed NV12, from a 4K SDR film.
# frames.sh <film> <outdir> <seconds> [count]
# Scope pictures are letterboxed into 3840x2160 (Y=16, UV=128) like a
# Blu-ray; the 1080p source is the same picture through a Lanczos downscale.
set -e
film=$1 out=$2 t=$3 n=${4:-1}
mkdir -p "$out"
tag=$(printf 't%04d' "$t")
ffmpeg -v error -y -ss "$t" -i "$film" -frames:v "$n" -an \
  -vf "scale=3840:-2:flags=lanczos,pad=3840:2160:(ow-iw)/2:(oh-ih)/2:color=black,format=nv12" \
  -f rawvideo "$out/$tag-ref4k.nv12"
ffmpeg -v error -y -f rawvideo -pix_fmt nv12 -s 3840x2160 -i "$out/$tag-ref4k.nv12" \
  -vf "scale=1920:1080:flags=lanczos+accurate_rnd+full_chroma_int,format=nv12" \
  -f rawvideo "$out/$tag-src1080.nv12"
