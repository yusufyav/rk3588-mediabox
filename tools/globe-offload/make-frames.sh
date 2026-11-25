#!/usr/bin/env bash
# The globe experiment's frames (rust/crates/mediabox-tv/src/globe.rs), from
# the scene itself: drawn by the product's renderer on this machine's GLES,
# then HEVC, every frame an IDR of its own so any of them can be decoded next.
#
#   tools/globe-offload/make-frames.sh <directory>
#
# Leaves globe.h265 (decode, cache), globe.xrgb (still) and quality.txt: what
# the encoding and 4:2:0 cost, over the ground the light is added to.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
out="$(mkdir -p "$1" && cd "$1" && pwd)"
crf="${CRF:-6}"
(cd "$here/rust/crates/mediabox-tv" && MEDIABOX_TV_GLOBE_OUT="$out" cargo test hit::gl::globe::frames -- --ignored --exact)
side="$(cat "$out/side")"
raw=(-f rawvideo -pix_fmt rgb24 -s "${side}x${side}" -r 144)
ffmpeg -v error -y "${raw[@]}" -i "$out/frames.rgb" \
  -vf "scale=out_color_matrix=bt601:out_range=pc:flags=accurate_rnd+full_chroma_int+lanczos,format=yuv420p" \
  -c:v libx265 -preset slow -crf "$crf" \
  -x265-params "keyint=1:min-keyint=1:repeat-headers=1:aud=1:range=full:colorprim=bt709:transfer=bt709:colormatrix=smpte170m:log-level=error" \
  -color_range pc -f hevc "$out/globe.h265"
ffmpeg -v error -y -color_range pc -i "$out/globe.h265" \
  -vf "scale=in_color_matrix=bt601:in_range=pc:flags=accurate_rnd+full_chroma_int,format=rgb24" -f rawvideo "$out/decoded.rgb"
python3 -I - "$out" "$side" <<'PY'
import math, os, sys
out, side = sys.argv[1], int(sys.argv[2])
n = side * side * 3
src, dec = open(out + "/frames.rgb", "rb").read(), open(out + "/decoded.rgb", "rb").read()
ground, drawn = open(out + "/ground.rgb", "rb").read(), open(out + "/rest-drawn.rgb", "rb").read()
def psnr(a, b):
    e = sum((x - y) ** 2 for x, y in zip(a, b)) / len(a)
    return 10 * math.log10(255 * 255 / e) if e else 99.0
frames = len(src) // n
assert len(dec) == len(src), (len(dec), len(src))
half = frames // 2
rest = dec[half * n:(half + 1) * n]
sums = bytes(min(255, g + d) for g, d in zip(ground, rest))
worst = min(psnr(src[i * n:(i + 1) * n], dec[i * n:(i + 1) * n]) for i in range(0, frames, 8))
off = [abs(a - b) for a, b in zip(sums, drawn)]
open(out + "/globe.xrgb", "wb").write(b"".join(bytes((src[o + 2], src[o + 1], src[o], 0)) for o in range(half * n, (half + 1) * n, 3)))
text = (f"frames {frames} of {side}x{side}, stream {os.path.getsize(out + '/globe.h265')} bytes\n"
        f"light: worst PSNR of every 8th frame {worst:.1f} dB\n"
        f"at rest, ground + decoded light against Slint's own: PSNR {psnr(sums, drawn):.1f} dB, "
        f"largest difference {max(off)}/255, over 8/255 in {sum(o > 8 for o in off) / len(off) * 100:.2f} % of samples\n")
open(out + "/quality.txt", "w").write(text)
print(text, end="")
PY
