# QuickSRNet Small 2x W8A8 on the RK3588 NPU: QUICKSRNET_NPU_ARCH_FAIL

The network is exactly what it claims to be: four 3x3 convs with Hardtanh and
a pixel shuffle, 47.18 GMAC per 1080p frame, every op on the NPU. It still
does not fit. Every conv runs at full 1080p resolution, and the official graph
with a 4K output needs 107.8 M NPU cycles per frame by the compiler's own
count. On this board's measured three-core scaling that is 38-54 ms of NPU
time alone, above the 41.7 ms budget before any colour conversion or output
handling. Admission gate only, no board benchmark. October 2026, same
toolchain as [README.md](README.md). Research only.

## Checkpoint

| | |
|---|---|
| source | github.com/quic/aimet-model-zoo, model card `aimet_zoo_torch/quicksrnet/model/model_cards/quicksrnet_small_2x_w8a8.json` at 1bd2bf5b |
| weights | release `phase_2_january_artifacts`, `quicksrnet_small_2x_checkpoint_int8.pth` (the card's post-optimisation weights), sha256 `c4401830...9f4b66b208` |
| W8 encodings | `quicksrnet_small_2x_checkpoint_int8_params_only.encodings`, sha256 `e2e5223c...9a51063b`: per-channel symmetric INT8, offset -128 |
| A8 | not shipped (`url_aimet_encodings: null`); AIMET computes them from calibration data, RKNN did the same here on 10 real film frames |
| config | AIMET `default_config_per_channel.json` (release-aimet-1.23) |

Read with `torch.load(weights_only=True)`. The float32 checkpoint pickles an
Adam optimizer and is refused by that loader, but W8A8 does not use it. The
int8 weights sit up to 0.66 LSB off the official grid. As AIMET does, they
are snapped to the frozen per-channel encodings before export
(`quicksrnet.py`).

## Graph

`quicksrnet.py` is a copy of the official `QuickSRNetSmall` (32 channels, two
intermediate layers, no input-to-output connection):

    Conv3x3(3->32) Hardtanh  Conv3x3(32->32) Hardtanh  Conv3x3(32->32) Hardtanh
    Conv3x3(32->12) Hardtanh  PixelShuffle(2)

22 860 parameters; 22 752 MAC per input pixel, **47.18 GMAC per 1080p frame**.
There is no LayerNorm, GELU or attention.

## Admission gate (rknn-toolkit2 2.3.2, rk3588, INT8, 1x3x1080x1920)

| graph | ops | non-NPU compute ops | output | est. 1 core | internal tensors | traffic / frame | at 23.976 fps |
|---|---|---|---|---|---|---|---|
| official, shuffle on NPU | 7 | none | 1x3x2160x3840 | **107.8 M cycles** | 388.8 MB | 481 MB | 11.5 GB/s |
| shuffle left to the CPU | 6 | none | 1x12x1080x1920 | 74.6 M cycles | 135.7 MB | 259 MB | 6.2 GB/s |

Per op (compiler, one core, 1 GHz):

| op | cycles |
|---|---|
| Conv3x3 3->32 + Clip | 18.66 M |
| Conv3x3 32->32 + Clip | 18.66 M |
| Conv3x3 32->32 + Clip | 18.66 M |
| Conv3x3 32->12 + Clip | 18.66 M |
| PixelShuffle as ConvTranspose 12->3, 2x2 | 33.18 M |

Each conv costs one cycle per pixel per tap (2.07 M px x 9), whatever its
channels: at full 1080p the NPU is bound by pixels, not MACs. The pixel
shuffle is lowered to a deconvolution whose 3-channel 4K output is stored
NC1HWC2 as (1,2,2160,3840,16): 265 MB written every frame. Weights are 29 KB.
The compiler builds the graph for all three cores (multi-core-model-mode 7).

## Why the gate stops here

- On this board the compiler's single-core cycles turned into 3-core time
  at 1/2.0 (ESPCN plain INT8: 236 -> 118 ms) to 1/2.7 (FSRCNN-small INT8:
  141 -> 51 ms), with 1 GHz NPU and 1/3 the ideal. The official graph's
  107.8 M cycles therefore come to 38-54 ms of NPU time per frame. Even a
  perfect three-way split would be 35.9 ms, leaving 5.8 ms for everything
  else.
- That else is real work here: the model wants RGB 0..1, so NV12 -> RGB at
  1080p on the way in. With the shuffle on the NPU, a 265 MB padded tensor
  has to be turned into 4K RGB -> NV12 on the way out. With the shuffle on
  the CPU (24.9 M samples per frame), the NPU estimate is 27.6-37 ms,
  and the CPU shuffle plus RGB -> NV12 at 4K comes on top. A simpler
  luma-only 16-channel scatter plus chroma upscale already measured
  4.6 ms median and 17 ms p95 on four A76 cores (README.md), so a p95 at or
  under 41.7 ms is out of reach.

## Decision: QUICKSRNET_NPU_ARCH_FAIL

Quality, temporal behaviour and the board run were not measured: the gate
said stop.
