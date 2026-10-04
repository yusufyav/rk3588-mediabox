#!/usr/bin/env python3
"""Synthetic 4K text frames (credits, subtitles, signs) as 3840x2160 NV12 training targets.

The film's own credits are held out for testing, so text comes from here:
random words in the system's Latin fonts, 20-110 px at 4K, light on dark
and dark on light, antialiased as a renderer would.

  text_frames.py OUTDIR COUNT
"""
import os
import random
import subprocess
import sys

import numpy as np
from PIL import Image, ImageDraw, ImageFont

WORDS = ('director producer camera sound editor music design light grip assistant visual effects '
         'Thom Celia Barley Djenghis Amsterdam studio color pipeline render composite animation '
         'Ağustos şehir İstanbul Çağrı Öykü Günay ışık müzik 2026 1080p 4K HDR Dolby').split()


def fonts():
    out = subprocess.run(['fc-list', ':lang=en', 'file'], capture_output=True, text=True).stdout
    fs = [l.split(':')[0] for l in out.splitlines() if l.split(':')[0].lower().endswith(('.ttf', '.otf'))]
    return [f for f in fs if not any(k in f.lower() for k in ('emoji', 'symbol', 'math', 'braille', 'music'))]


def frame(rng, fl):
    dark = rng.random() < 0.7
    bg = rng.integers(0, 40) if dark else rng.integers(180, 256)
    img = Image.new('RGB', (3840, 2160), (int(bg),) * 3)
    d = ImageDraw.Draw(img)
    y = int(rng.integers(20, 200))
    while y < 2050:
        size = int(rng.choice([20, 26, 32, 40, 52, 64, 80, 110]))
        try:
            font = ImageFont.truetype(str(rng.choice(fl)), size)
        except OSError:
            continue
        x = int(rng.integers(20, 600))
        line = ' '.join(rng.choice(WORDS) for _ in range(rng.integers(3, 12)))
        fg = int(rng.integers(200, 256)) if dark else int(rng.integers(0, 60))
        tint = tuple(int(np.clip(fg + rng.integers(-30, 30), 0, 255)) for _ in range(3))
        d.text((x, y), line, font=font, fill=tint)
        y += int(size * rng.uniform(1.2, 2.2))
    return img


def to_nv12(img):
    rgb = np.asarray(img).astype(np.float64) / 255
    r, g, b = rgb[..., 0], rgb[..., 1], rgb[..., 2]
    yy = 0.2126 * r + 0.7152 * g + 0.0722 * b
    u, v = (b - yy) / 1.8556, (r - yy) / 1.5748
    sub = lambda p: p.reshape(1080, 2, 1920, 2).mean(axis=(1, 3))
    Y = np.clip(np.rint(16 + 219 * yy), 0, 255).astype(np.uint8)
    UV = np.clip(np.rint(128 + 224 * np.stack([sub(u), sub(v)], -1)), 0, 255).astype(np.uint8)
    return Y.tobytes() + UV.tobytes()


def main():
    out, n = sys.argv[1], int(sys.argv[2])
    os.makedirs(out, exist_ok=True)
    rng = np.random.default_rng(42)
    fl = fonts()
    for i in range(n):
        open(f'{out}/text-{i:03d}.nv12', 'wb').write(to_nv12(frame(rng, fl)))
    print(n, 'text frames from', len(fl), 'fonts')


if __name__ == '__main__':
    main()
