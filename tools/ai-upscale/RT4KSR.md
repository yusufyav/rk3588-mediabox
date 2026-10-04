# RT4KSR x2 on the RK3588 NPU: AI_UPSCALE_PERFORMANCE_FAIL

RT4KSR x2 beats VOP2 on real film frames, but the RK3588 NPU runs it at
about 3 fps. Its channel LayerNorm and its GELU execute on one NPU core at a
fraction of its rate, and no exact rewrite stays on the NPU and gets
faster. Measured on the Orange Pi 5 Plus, October 2026, with the same
frames, tools and runtime as [README.md](README.md). Research only; nothing
here is installed or run by the product.

## Model

RT4KSR (Zamfir et al., CVPRW 2023), github.com/eduardzamfir/RT4KSR at
fd6627a4, `code/checkpoints/rt4ksr_x2.pth` (sha256 `9ec75000...76b2124f`),
read with `torch.load(weights_only=True)`, which does not execute the
pickle. Configuration is the repository default: 24 features, 4
SimplifiedNAF blocks, GELU, channel LayerNorm, pixel unshuffle 2 /
pixel shuffle 4. It was chosen because it was designed for real-time 1080p ->
4K x2 (the NTIRE 2023 RTSR baseline) and works at 960x540 internally. 39k
parameters (training form), 32.8k after reparameterisation, 16.8 GMAC per
frame.

`rt4ksr.py` rebuilds it from the state dict, reparameterises each
three-conv block into one 3x3 conv (max |diff| 3.5e-6), and folds BT.709
colour into the first and last conv. The graph takes NV12 as it is (4 Y
phases + U + V at 540x960) and returns NV12 (16 Y phases + 2x2 U + 2x2 V),
which matches the training graph to 9e-4 codes on nearest-sampled chroma.

## Quality (FP32, the same six film frames, same writeback path as VOP2)

| frame | VOP2 | RT4KSR (RGB in) | gain |
|---|---|---|---|
| face | 48.40 | 49.69 | +1.29 |
| dark face, metal | 41.45 | 41.87 | +0.42 |
| foliage | 41.11 | 44.38 | +3.27 |
| hair, glasses, fabric | 47.83 | 49.42 | +1.59 |
| credits, diagonal art | 25.88 | 27.86 | +1.98 |
| small credits | 26.87 | 32.18 | +5.31 |

PSNR on luma, against the 4K reference read back the same way. SSIM is
higher on five frames and equal on the face (0.9913 vs 0.9919). Detail 1.02-1.03
of the reference, against VOP2's 0.92-0.93. Geometry is centre-aligned, within
0.05 px. Ringing is low on natural frames (0.00-0.04% of pixels leave the
reference's 5x5 range by more than 4 codes), but higher than VOP2's on
credits (1.6% vs 0.4%, 4.1% vs 3.0%). Against a CPU bicubic, without
writeback, RT4KSR is -0.6 to +0.9 dB on natural frames: most of the margin
over VOP2 is VOP2's own align-corners kernel. With NV12 input (nearest
chroma, no RGB clip) natural frames lose a further 0-0.4 dB and credits
overshoot more.

## RKNN (rknn-toolkit2 2.3.2, rk3588)

| build | compute ops not on NPU | rknn_run, 3 cores, median |
|---|---|---|
| NV12 graph, INT8 | none | 334 ms |
| NV12 graph, FP16 | none | 319 ms |
| LayerNorm as 1x1 convs + Mul/Sqrt/Div, FP16 | none | 986 ms |
| the same, INT8 | Sqrt, Div on CPU x5 (rejected) | - |
| GELU as x*sigmoid(1.702x) | - | rejected: output off by up to 19.8 codes |

Per-op times from RKNN_QUERY_PERF_DETAIL, INT8 NV12 graph:

| op | count | NPU time | cores used |
|---|---|---|---|
| exNorm (the fused LayerNorm) | 5 | 282 ms (56 ms each) | one |
| ConvExGelu | 4 | 41 ms (10 ms each) | one |
| other conv | 3 | 7 ms | three |
| input add | 1 | 0.9 ms | three |

In the decomposed FP16 build, Sqrt alone takes 798 ms (160 ms each). Even
with LayerNorm free, INT8 GELU + convs would be about 50 ms of NPU time per
frame, above the 41.7 ms budget before any CPU work.

## RK3588 measurement (INT8 NV12 graph, 3-core mask, 60 s)

| | |
|---|---|
| NPU (rknn_run) | 329.2 min / 333.3 median / 335.5 p95 / 336.2 max ms |
| end to end | 336.5 min / 366.4 median / 375.3 p95 ms, 2.7-3.0 fps |
| NPU cores | core0 90%, core1 2%, core2 2% |
| CPU | pre 3.6-21 ms, post 3.7-37 ms on 4 A76 threads (no chroma stage: the model makes it); at most 9.3% of 8 cores busy |
| DDR | at most 44% load, 2112 MHz |
| memory | 112 MB NPU DMA (INT8), 216 MB (FP16) |
| temperature | NPU at most 45.3 °C, 1000 MHz throughout |

The NPU did not heat up: 3 fps leaves it mostly idle. A sustained run was not
needed for the verdict.

## Decision: AI_UPSCALE_PERFORMANCE_FAIL

- Quality: better than VOP2 on every test frame through the same path.
- Real NPU, no CPU fallback in the INT8 NV12 build, geometry correct.
- 333 ms per frame on the NPU, about 8x the 41.7 ms budget. Three cores do
  not help: LayerNorm and GELU run on one core. No exact rewrite stays on
  the NPU and gets faster.

Getting under budget would mean changing the model itself, for example
retraining without LayerNorm and with ReLU, which makes it a different
network than the one measured here.
