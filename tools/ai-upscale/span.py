#!/usr/bin/env python3
"""SPAN x2 (Wan et al., CVPRW 2024, NTIRE 2024 ESR winner), eval form, from its state dict.

Checkpoint: the authors' release linked from github.com/hongyuanyu/SPAN
(README, "Release checkpoints", Google Drive 1iYUA2TzKuxI0vzmA-UXr_nB43XgPOXUg),
file spanx2_ch48.pth. Read with the restricted loader, which never executes
code from the pickle:

  python -c "import torch, numpy as np; c = torch.load('spanx2_ch48.pth', map_location='cpu',
      weights_only=True); sd = c.get('params_ema', c.get('params', c));
      np.savez('spanx2_ch48.npz', **{k: v.float().numpy() for k, v in sd.items()})"

Architecture as basicsr/archs/span_arch.py at c77a5917: 48 features, 6 SPAB
blocks, every Conv3XC (1x1 -> 3x3 -> 1x1 + 1x1 skip) collapsed into the single
3x3 eval_conv the official update_params() computes; SiLU, sigmoid, no norm,
no GELU. RGB 0..1 in, x2 out.

  span.py NPZ --onnx OUT.onnx [--hw 1080x1920]   eval graph, checked against the training graph
"""
import argparse

import numpy as np
import torch
import torch.nn as nn
import torch.nn.functional as F


def collapse(sd, p):
    """Official Conv3XC.update_params(), on the state dict."""
    t = lambda k: torch.from_numpy(sd[p + k])
    w1, b1, w2, b2, w3, b3 = t('conv.0.weight'), t('conv.0.bias'), t('conv.1.weight'), t('conv.1.bias'), \
        t('conv.2.weight'), t('conv.2.bias')
    w = F.conv2d(w1.flip(2, 3).permute(1, 0, 2, 3), w2, padding=2).flip(2, 3).permute(1, 0, 2, 3)
    b = (w2 * b1.reshape(1, -1, 1, 1)).sum((1, 2, 3)) + b2
    W = F.conv2d(w.flip(2, 3).permute(1, 0, 2, 3), w3).flip(2, 3).permute(1, 0, 2, 3)
    B = (w3 * b.reshape(1, -1, 1, 1)).sum((1, 2, 3)) + b3
    W = W + F.pad(t('sk.weight'), [1, 1, 1, 1])
    B = B + t('sk.bias')
    c = nn.Conv2d(W.shape[1], W.shape[0], 3, padding=1)
    c.weight.data, c.bias.data = W.contiguous(), B
    return c


def conv3xc_train(sd, p, x):
    t = lambda k: torch.from_numpy(sd[p + k])
    y = F.conv2d(F.pad(x, (1, 1, 1, 1)), t('conv.0.weight'), t('conv.0.bias'))
    y = F.conv2d(y, t('conv.1.weight'), t('conv.1.bias'))
    y = F.conv2d(y, t('conv.2.weight'), t('conv.2.bias'))
    return y + F.conv2d(x, t('sk.weight'), t('sk.bias'))


MEAN = (0.4488, 0.4371, 0.4040)


class SPANEval(nn.Module):
    def __init__(self, sd, shuffle=True):
        super().__init__()
        self.register_buffer('mean', torch.tensor(MEAN).view(1, 3, 1, 1))
        self.conv_1 = collapse(sd, 'conv_1.')
        self.blocks = nn.ModuleList(
            nn.ModuleList(collapse(sd, f'block_{i}.c{j}_r.') for j in (1, 2, 3)) for i in range(1, 7))
        self.conv_2 = collapse(sd, 'conv_2.')
        self.conv_cat = nn.Conv2d(192, 48, 1)
        self.conv_cat.weight.data = torch.from_numpy(sd['conv_cat.weight'])
        self.conv_cat.bias.data = torch.from_numpy(sd['conv_cat.bias'])
        self.up = nn.Conv2d(48, 12, 3, padding=1)
        self.up.weight.data = torch.from_numpy(sd['upsampler.0.weight'])
        self.up.bias.data = torch.from_numpy(sd['upsampler.0.bias'])
        self.shuffle = shuffle

    def forward(self, x):
        x = (x - self.mean) * 255.0
        f = self.conv_1(x)
        h, o5_2, o1 = f, None, None
        for i, (c1, c2, c3) in enumerate(self.blocks):
            a = c1(h)
            b = c2(F.silu(a))
            c = c3(F.silu(b))
            h = (c + h) * (torch.sigmoid(c) - 0.5)
            if i == 0:
                o1 = h
            if i == 5:
                o5_2 = a
        out = self.up(self.conv_cat(torch.cat([f, self.conv_2(h), o1, o5_2], 1)))
        return F.pixel_shuffle(out, 2) if self.shuffle else out


def span_train(sd, x):
    """Training-form forward pass, for the equivalence check."""
    x = (x - torch.tensor(MEAN).view(1, 3, 1, 1)) * 255.0
    f = conv3xc_train(sd, 'conv_1.', x)
    h, o1, o5_2 = f, None, None
    for i in range(1, 7):
        p = f'block_{i}.'
        a = conv3xc_train(sd, p + 'c1_r.', h)
        b = conv3xc_train(sd, p + 'c2_r.', F.silu(a))
        c = conv3xc_train(sd, p + 'c3_r.', F.silu(b))
        h = (c + h) * (torch.sigmoid(c) - 0.5)
        o1 = h if i == 1 else o1
        o5_2 = a if i == 6 else o5_2
    t = lambda k: torch.from_numpy(sd[k])
    out = F.conv2d(torch.cat([f, conv3xc_train(sd, 'conv_2.', h), o1, o5_2], 1), t('conv_cat.weight'), t('conv_cat.bias'))
    return F.pixel_shuffle(F.conv2d(out, t('upsampler.0.weight'), t('upsampler.0.bias'), padding=1), 2)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('npz')
    ap.add_argument('--onnx', required=True)
    ap.add_argument('--hw', default='1080x1920')
    a = ap.parse_args()
    sd = dict(np.load(a.npz))
    m = SPANEval(sd).eval()
    # smooth, image-like input: on white noise both graphs amplify their
    # differing border handling (training pads before the first 1x1) into the interior
    x = F.interpolate(torch.rand(1, 3, 8, 12), size=(64, 96), mode='bicubic', align_corners=False).clamp(0, 1)
    with torch.no_grad():
        err = (m(x) - span_train(sd, x)).abs().max().item()
    print(f'eval (collapsed Conv3XC) vs training graph: max |diff| {err:.2e} (0..1 scale)')
    convs = [c for c in m.modules() if isinstance(c, nn.Conv2d)]
    k3 = sum(1 for c in convs if c.kernel_size == (3, 3))
    h, w = map(int, a.hw.split('x'))
    macs = sum(c.weight.numel() for c in convs)
    print(f'convs {len(convs)} ({k3} 3x3), params {sum(p.numel() for p in m.parameters())}, '
          f'MAC/input px {macs}, GMAC/frame({w}x{h}) {macs * h * w / 1e9:.1f}')
    dep = SPANEval(sd, shuffle=False).eval()
    torch.onnx.export(dep, torch.zeros(1, 3, h, w), a.onnx, input_names=['rgb'], output_names=['sub'],
                      opset_version=13)


if __name__ == '__main__':
    main()
