#!/usr/bin/env python3
"""QuickSRNet Small 2x W8A8 (Qualcomm AIMET Model Zoo), for RKNN.

Architecture as aimet_zoo_torch/quicksrnet/model/models.py at
quic/aimet-model-zoo 1bd2bf5b (QuickSRNetSmall: 32 channels, 2 intermediate
layers, no input-to-output connection):

  Conv3x3(3->32) Hardtanh(0,1) Conv3x3(32->32) Hardtanh Conv3x3(32->32) Hardtanh
  Conv3x3(32->12) Hardtanh(0,1) PixelShuffle(2)

RGB 0..1 in (the zoo's loader divides by 255), RGB 0..1 out.

Weights: the model card's post-optimisation W8A8 checkpoint
(phase_2_january_artifacts/quicksrnet_small_2x_checkpoint_int8.pth), whose
weights AIMET freezes with ..._int8_params_only.encodings: per-channel
symmetric INT8. The zoo ships no activation encodings; AIMET computes them
from calibration data, as RKNN does here.

  quicksrnet.py PTH ENC --onnx OUT.onnx [--no-shuffle]   1x3x1080x1920 -> 1x3x2160x3840
  quicksrnet.py PTH ENC SRC.nv12 OUT.nv12 [--w8]         FP32 (or W8 weights) on 1080p NV12 frames
"""
import argparse
import json
import sys

import numpy as np
import torch
import torch.nn as nn
import torch.nn.functional as F

SW, SH = 1920, 1080


class QuickSRNetSmall2x(nn.Module):
    def __init__(self, shuffle=True):
        super().__init__()
        self.cnn = nn.Sequential(
            nn.Conv2d(3, 32, 3, padding=1), nn.Hardtanh(0., 1.),
            nn.Conv2d(32, 32, 3, padding=1), nn.Hardtanh(0., 1.),
            nn.Conv2d(32, 32, 3, padding=1), nn.Hardtanh(0., 1.),
        )
        self.conv_last = nn.Conv2d(32, 12, 3, padding=1)
        self.clip_output = nn.Hardtanh(0., 1.)
        self.shuffle = shuffle

    def forward(self, x):
        x = self.clip_output(self.conv_last(self.cnn(x)))
        return F.pixel_shuffle(x, 2) if self.shuffle else x


def load(pth, enc, w8):
    sd = torch.load(pth, map_location='cpu', weights_only=True)['state_dict']
    e = json.load(open(enc))
    worst = 0.0
    for k, chans in e.items():
        s = torch.tensor([c['scale'] for c in chans]).view(-1, 1, 1, 1)
        q = torch.clamp(torch.round(sd[k] / s), -128, 127)
        worst = max(worst, ((sd[k] - q * s).abs() / s).max().item())
        if w8:
            sd[k] = q * s
    print(f'weights vs the official per-channel INT8 grid: worst {worst:.3f} LSB', file=sys.stderr)
    return sd


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('pth')
    ap.add_argument('enc')
    ap.add_argument('src', nargs='?')
    ap.add_argument('out', nargs='?')
    ap.add_argument('--onnx')
    ap.add_argument('--no-shuffle', action='store_true')
    ap.add_argument('--w8', action='store_true', help='snap weights to the official encodings')
    a = ap.parse_args()
    sd = load(a.pth, a.enc, a.w8 or bool(a.onnx))
    if a.onnx:
        m = QuickSRNetSmall2x(shuffle=not a.no_shuffle)
        m.load_state_dict(sd)
        convs = [c for c in m.modules() if isinstance(c, nn.Conv2d)]
        macs = sum(c.weight.numel() for c in convs)
        print(f'params {sum(p.numel() for p in m.parameters())}, MAC/input px {macs}, '
              f'GMAC/frame({SW}x{SH}) {macs * SW * SH / 1e9:.2f}')
        torch.onnx.export(m.eval(), torch.zeros(1, 3, SH, SW), a.onnx, input_names=['rgb'],
                          output_names=['out'], opset_version=13)
        return
    sys.path.insert(0, __file__.rsplit('/', 1)[0])
    from rt4ksr import nv12_to_rgb, rgb_to_nv12
    m = QuickSRNetSmall2x().eval()
    m.load_state_dict(sd)
    src = np.fromfile(a.src, np.uint8)
    n = len(src) // (SW * SH * 3 // 2)
    with open(a.out, 'wb') as o, torch.no_grad():
        for k in range(n):
            f = src[k * SW * SH * 3 // 2:(k + 1) * SW * SH * 3 // 2]
            y = m(torch.from_numpy(nv12_to_rgb(f, SW, SH))[None])[0].numpy()
            o.write(rgb_to_nv12(y))


if __name__ == '__main__':
    main()
