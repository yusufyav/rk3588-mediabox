#!/usr/bin/env python3
"""Shape sweep: how large a plain NV12-native SR body can the RK3588 NPU run per 1080p frame?

Random weights: this measures time, not quality. The shape is the one the
earlier measurements point to:

  in   1x6x540x960   (Y of each 2x2 block + U + V, the NV12 planes as they are)
  body conv3x3(6->C) ReLU, (D-2) x conv3x3(C->C) ReLU, conv3x3(C->24)
  out  1x24x540x960  (Y of the 4x4 output block, U/V of its 2x2 chroma samples)

no norm, no GELU, no attention, nothing at full resolution. A trained model of
this shape is RepVGG/ECBSR-style multi-branch in training and collapses to
exactly this at inference.

  npu_sweep.py OUTDIR C,D [C,D ...]    -> OUTDIR/sweep-cC-dD.onnx + calib.txt
"""
import os
import sys

import numpy as np
import torch
import torch.nn as nn


def net(c, d):
    layers = [nn.Conv2d(6, c, 3, padding=1), nn.ReLU()]
    for _ in range(d - 2):
        layers += [nn.Conv2d(c, c, 3, padding=1), nn.ReLU()]
    layers.append(nn.Conv2d(c, 24, 3, padding=1))
    m = nn.Sequential(*layers)
    for x in m:
        if isinstance(x, nn.Conv2d):
            nn.init.kaiming_normal_(x.weight, nonlinearity='relu')
            x.weight.data *= 0.5
            nn.init.zeros_(x.bias)
    return m.eval()


def main():
    out = sys.argv[1]
    os.makedirs(out, exist_ok=True)
    rng = np.random.default_rng(0)
    cal = []
    for i in range(4):
        p = os.path.abspath(f'{out}/calib{i}.npy')
        np.save(p, rng.integers(16, 236, (1, 6, 540, 960)).astype(np.float32) / 255)
        cal.append(p)
    open(f'{out}/calib.txt', 'w').write('\n'.join(cal) + '\n')
    for spec in sys.argv[2:]:
        c, d = map(int, spec.split(','))
        m = net(c, d)
        macs = sum(x.weight.numel() for x in m if isinstance(x, nn.Conv2d)) * 540 * 960
        print(f'C={c} D={d}: {macs / 1e9:.1f} GMAC/frame')
        torch.onnx.export(m, torch.zeros(1, 6, 540, 960), f'{out}/sweep-c{c}-d{d}.onnx',
                          input_names=['x'], output_names=['y'], opset_version=13)


if __name__ == '__main__':
    main()
