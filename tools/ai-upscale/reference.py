#!/usr/bin/env python3
"""FP32 output of a published model on 1080p NV12 frames, as 2160p NV12.

  reference.py NAME NPZ SRC.nv12 OUT.nv12

Same luma maths as the exported graphs, in float32 on the workstation; the
chroma goes through the same bilinear 2x as aisr-bench, so the only
difference to the NPU's output is the NPU's arithmetic (FP16 or INT8).
"""
import sys

import numpy as np
import torch

from models import reference

SW, SH, DW, DH = 1920, 1080, 3840, 2160


def uv2x(c):
    """aisr-bench's uv_rows: centre-aligned bilinear, edge-clamped."""
    c = c.astype(np.int32)
    up = np.concatenate([c[:1], c[:-1]], 0)
    dn = np.concatenate([c[1:], c[-1:]], 0)
    v = np.empty((c.shape[0] * 2,) + c.shape[1:], np.int32)
    v[0::2] = 3 * c + up
    v[1::2] = 3 * c + dn
    lf = np.concatenate([v[:, :1], v[:, :-1]], 1)
    rt = np.concatenate([v[:, 1:], v[:, -1:]], 1)
    o = np.empty((v.shape[0], v.shape[1] * 2, 2), np.int32)
    o[:, 0::2] = (3 * v + lf + 8) >> 4
    o[:, 1::2] = (3 * v + rt + 8) >> 4
    return o.astype(np.uint8)


def main():
    name, npz, src, out = sys.argv[1:5]
    model, _ = reference(name, npz)
    dev = 'cuda' if torch.cuda.is_available() else 'cpu'
    model = model.to(dev).eval()
    a = np.fromfile(src, np.uint8)
    n = len(a) // (SW * SH * 3 // 2)
    with open(out, 'wb') as o, torch.no_grad():
        for k in range(n):
            f = a[k * SW * SH * 3 // 2:(k + 1) * SW * SH * 3 // 2]
            y = torch.from_numpy(f[:SW * SH].reshape(1, 1, SH, SW).astype(np.float32)).to(dev)
            z = model((y - 16) / 219) * 219 + 16
            o.write(np.clip(np.rint(z.cpu().numpy()[0, 0]), 0, 255).astype(np.uint8).tobytes())
            o.write(uv2x(f[SW * SH:].reshape(SH // 2, SW // 2, 2)).tobytes())


if __name__ == '__main__':
    main()
