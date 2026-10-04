# The SR shape the RK3588 NPU can run at 1080p23.976

No published SR model is built for this NPU: RT4KSR needs LayerNorm and GELU,
and SPAN and QuickSRNet work at full 1080p. The measurements in this directory
fix the shape instead, and a random-weight sweep
(`npu_sweep.py`) confirms it on the Orange Pi 5 Plus (INT8, 3 cores, 1 GHz,
300 frames after 20 warm-up). The timings include the CPU work of taking NV12
in and producing a 3840x2160 NV12 frame (`aisr-bench ... nv12`).

## Rules, from earlier runs

- No full-resolution convs. At 1080p every 3x3 conv costs one NPU cycle per
  pixel per tap, 18.7 M cycles whatever its channels (QUICKSRNET.md).
- No 1- or 3-channel full-resolution tensors: NC1HWC2 pads them 16-32x
  (README.md).
- Conv + ReLU/Clip/PReLU only. LayerNorm costs 56 ms and GELU 10 ms per
  layer, both on one core; Sqrt/Div fall back to the CPU in INT8 (RT4KSR.md).
- NV12 in and NV12 out: 4 Y phases + U + V at 540x960 in, 16 Y phases + 2x2
  U/V out. The colour handling then costs 1.3 ms pre and 3.6 ms post on the
  CPU (median).

## Sweep: conv3x3(6->C) ReLU, (D-2) x conv3x3(C->C) ReLU, conv3x3(C->24), at 540x960

| C x D | GMAC | compiler, 1 core | NPU median | e2e median | e2e p95 | fps |
|---|---|---|---|---|---|---|
| 32 x 6 | 23.6 | 28.0 M | 14.0 ms | 19.0 ms | 25.9 ms | 50.8 |
| 24 x 8 | 19.5 | 37.3 M | 18.6 ms | 23.5 ms | 33.4 ms | 40.9 |
| 32 x 8 | 33.1 | 37.3 M | 18.6 ms | 23.5 ms | 33.3 ms | 39.9 |
| **32 x 10** | 42.7 | 46.7 M | 23.2 ms | **28.1 ms** | **37.8 ms** | 34.0 |
| 48 x 6 | 49.7 | 93.3 M | 39.6 ms | 51.3 ms | 54.9 ms | 20.0 |

- Up to 32 channels a conv costs the same, 4.67 M cycles, about 2.3 ms on
  three cores. 48 channels costs 3.3x.
- Three cores are effective here: 32 x 8 runs in 53.1 ms on one core and
  18.6 ms on three (2.86x).
- The CPU p95 tail (post 9-10 ms against a 3.6 ms median) is the largest
  remaining variance. Pipelining CPU post of frame n with NPU of frame n+1
  would hide it.

## What this means

A 32-channel, 8-10 conv, ReLU body at half resolution fits the 41.7 ms
budget at p95, with 4-14 ms to spare. It has more capacity than QuickSRNet
Small (47 GMAC spent at full resolution on 4 convs) and a wider receptive
field, about 42 source pixels against 9. RT4KSR, the one model that beat
VOP2 on film frames, is 24 channels and 7 convs at the same resolution;
only its LayerNorm and GELU were in the way. No weights of this shape exist,
so it has to be trained, for example RepVGG/ECBSR-style multi-branch blocks
that collapse to these plain convs.
