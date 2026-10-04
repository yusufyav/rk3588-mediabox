# SPAN x2 on the RK3588 NPU: SPAN_NPU_ARCH_FAIL

SPAN x2 maps cleanly onto the NPU: no LayerNorm, no GELU, no CPU fallback.
It is far too large for a 1080p frame. The network runs at the full input
resolution with 48 channels and 21 3x3 convolutions: 850 GMAC per 1080p
frame. The RKNN compiler estimates 1.5 s per frame on one core in INT8 and
6.3 GB of memory traffic per frame. Nothing was run on the board. October 2026,
same toolchain as [README.md](README.md). Research only.

## Checkpoint

| | |
|---|---|
| model | SPAN (Wan et al., CVPRW 2024; NTIRE 2024 ESR, 1st place) |
| code | github.com/hongyuanyu/SPAN at c77a5917, `basicsr/archs/span_arch.py` (sha256 `9ff5a8d5...3118cd1`) |
| weights | the authors' release linked from that README ("Release checkpoints", Google Drive `1iYUA2TzKuxI0vzmA-UXr_nB43XgPOXUg`, zip sha256 `675b653c...b5f747dc`) |
| file | `spanx2_ch48.pth`, sha256 `561fd5cf...1847bc`, a native x2 model; `params_ema` used |
| provenance | the same zip carries the authors' x2 test log: `feature_channels: 48`, `upscale: 2`, Set5 38.06 / Urban100 32.20 dB |

Read with `torch.load(weights_only=True)`, which does not execute the pickle.

## Eval graph (`span.py`)

Each Conv3XC (1x1 -> 3x3 -> 1x1 + 1x1 skip) is collapsed into one 3x3 conv
with the official `update_params()` arithmetic. On a film crop the result
matches the training graph to 9.5e-7. The `eval_conv` tensors stored in the
checkpoint are stale (up to 4.9 off) and are not used. The official eval
forward recomputes them on every call, as this does.

22 convs in total: 21 3x3 (conv_1, 6 x 3 in the SPABs, conv_2, upsampler)
and the 192->48 1x1 concat conv. Parameters 410.7k (the paper's 0.43M for
x4 differs by the upsampler). 409 680 MAC per input pixel: **849.5 GMAC per
1080p frame**. RT4KSR, which unshuffles to 960x540 first, needs 16.8.

## RKNN profile (rknn-toolkit2 2.3.2, rk3588, 1x3x1080x1920)

| build | ops | non-NPU compute ops | LayerNorm / GELU | est. 1 core | memory traffic / frame |
|---|---|---|---|---|---|
| INT8 (10 real RGB frames) | 52 | none | 0 / 0 | 1526 M cycles = 1.53 s @ 1 GHz | 6.3 GB |
| FP16 | 52 | none | 0 / 0 | 2267 M cycles = 2.27 s | 12.5 GB |

INT8 op list: Conv 12, ConvExSwish 11 (3x3 conv with SiLU fused), Sigmoid 6,
Mul 6, Add 13, Concat 1, exSwish 1. InputOperator/OutputOperator are the
runtime's layout conversion. There are 23 conv ops: the 21 collapsed 3x3
convs, the 1x1 concat conv, and the 1x1 the compiler makes for the mean
subtraction. The training Conv3XC would have been 4 convs each, 80 or more
in all. Every 48->48 3x3 conv costs 74.6 M cycles on its own.

Even split perfectly over three cores at the compiler's own rate, INT8 needs
about 509 ms per frame, 12x the 41.7 ms budget. At 24 fps its traffic would
be about 150 GB/s; the board's LPDDR has a theoretical ceiling of about 34 GB/s.

## Decision: SPAN_NPU_ARCH_FAIL

The brief's stop condition, an estimate clearly above 41.7 ms, holds by more
than an order of magnitude. No board benchmark and no quality run: neither
could change the verdict.
