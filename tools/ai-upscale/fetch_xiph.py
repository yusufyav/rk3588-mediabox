#!/usr/bin/env python3
"""A few frames of each Xiph/Netflix El Fuente 4K sequence, as 3840x2160 8-bit NV12.

The .yuv files are raw 4096x2160 10-bit 4:2:0 (little-endian 16-bit
samples), so single frames come by HTTP range request: no multi-GB download.
Centre-cropped to 3840 wide, rounded to 8 bits.

  fetch_xiph.py OUTDIR [frames_per_sequence]
"""
import os
import sys
import urllib.request

import numpy as np

BASE = 'https://media.xiph.org/video/derf/ElFuente/'
SEQS = ['Boat', 'BoxingPractice', 'Crosswalk', 'FoodMarket', 'FoodMarket2', 'Narrator',
        'RitualDance', 'SquareAndTimelapse', 'Tango', 'TunnelFlag']
W, H, CW = 4096, 2160, 3840
FB = W * H * 3 // 2 * 2


def main():
    out, n = sys.argv[1], int(sys.argv[2]) if len(sys.argv) > 2 else 12
    os.makedirs(out, exist_ok=True)
    for s in SEQS:
        url = f'{BASE}Netflix_{s}_4096x2160_60fps_10bit_420.yuv'
        size = int(urllib.request.urlopen(urllib.request.Request(url, method='HEAD')).headers['Content-Length'])
        total = size // FB
        for k in np.linspace(0, total - 1, n).astype(int):
            path = f'{out}/xiph-{s}-{k:04d}.nv12'
            if os.path.exists(path):
                continue
            req = urllib.request.Request(url, headers={'Range': f'bytes={k * FB}-{(k + 1) * FB - 1}'})
            a = np.frombuffer(urllib.request.urlopen(req).read(), '<u2')
            y = a[:W * H].reshape(H, W)
            u = a[W * H:W * H * 5 // 4].reshape(H // 2, W // 2)
            v = a[W * H * 5 // 4:].reshape(H // 2, W // 2)
            x0 = (W - CW) // 2
            y8 = ((y[:, x0:x0 + CW].astype(np.uint32) + 2) >> 2).clip(0, 255).astype(np.uint8)
            uv = np.stack([u[:, x0 // 2:(x0 + CW) // 2], v[:, x0 // 2:(x0 + CW) // 2]], -1)
            uv8 = ((uv.astype(np.uint32) + 2) >> 2).clip(0, 255).astype(np.uint8)
            with open(path, 'wb') as f:
                f.write(y8.tobytes())
                f.write(uv8.tobytes())
        print(s, total, 'frames in the sequence', flush=True)


if __name__ == '__main__':
    main()
