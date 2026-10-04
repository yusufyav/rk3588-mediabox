# NPU super resolution 1080p -> 2160p: AI_SR_QUALITY_NOT_USEFUL

Follow-up with RT4KSR x2, a stronger model built for 1080p -> 4K:
[RT4KSR.md](RT4KSR.md), AI_UPSCALE_PERFORMANCE_FAIL. SPAN x2, the NTIRE 2024
efficient-SR winner: [SPAN.md](SPAN.md), SPAN_NPU_ARCH_FAIL. QuickSRNet Small
2x W8A8 from the AIMET model zoo: [QUICKSRNET.md](QUICKSRNET.md),
QUICKSRNET_NPU_ARCH_FAIL.

Can the RK3588 NPU turn a 1080p SDR frame into a 2160p frame with a 2x SR
network, at video rate, and look better than VOP2's own upscale? Measured on
the Orange Pi 5 Plus, October 2026. **No.** The one configuration that fits a
23.976 fps frame budget is worse than VOP2 on natural pictures; the
configurations whose output is clean do not fit the budget. Research only:
nothing here is installed, deployed or run by the product.

## Environment

| | |
|---|---|
| board | Orange Pi 5 Plus, RK3588, 16 GB |
| kernel | 6.1.115-vendor-rk35xx, Armbian 26.8.2 trixie |
| RKNPU driver | 0.9.8 (20240828), iommu mode, 3 cores |
| runtime | librknnrt 2.3.2 (429f97ae6b), copied to /tmp for the run, sha256 `d31fc19c...a738e8`; the board has no RKNN runtime of its own |
| toolkit | rknn-toolkit2 2.3.2 (latest release; repo HEAD 59a913d only touches LICENSE), Python 3.11 venv, onnx 1.16.1 |
| NPU | rknpu_ondemand, 1000 MHz during every run (300..1000 available) |
| DDR | dmc_ondemand, 2112 MHz throughout |
| CPU | ondemand on all policies; benchmark pinned to the A76 cores 4-7 |
| SRAM | `CONFIG_ROCKCHIP_RKNPU_SRAM` is not set: the NPU's 956 KB SRAM option is unavailable without a kernel change, so it was not tested |
| idle | 45..48 °C, NPU load 0% |

rknn_model_zoo 2.3.2 has no super-resolution model.

## Candidates

All luma-only (Y in, Y out), which suits NV12: the Y plane goes in as it is,
chroma is upscaled conventionally. Weights are the OpenCV dnn_superres
TensorFlow GraphDefs (data, no pickle), rebuilt in PyTorch and checked
against TensorFlow's own output (max |diff| 1.8e-7).

| model | params | MAC / input px | GMAC / 1080p frame | 2x native | INT8 build | non-NPU ops |
|---|---|---|---|---|---|---|
| FSRCNN x2 (d56 s12 m4, conv + depth-to-space) | 8 500 | 8 152 | 16.9 | yes | ok | none |
| FSRCNN-small x2 (d32 s5 m1) | 1 625 | 1 473 | 3.05 | yes | ok | none |
| ESPCN x2 (64-32, pixel shuffle) | 21 284 | 21 184 | 43.9 | yes | ok | none |

| source | revision | file | sha256 |
|---|---|---|---|
| github.com/Saafke/FSRCNN_Tensorflow | 6a4812c4 | models/FSRCNN_x2.pb | 366b33f0...b380d5c |
| github.com/Saafke/FSRCNN_Tensorflow | 6a4812c4 | models/FSRCNN-small_x2.pb | 429e4793...76b2b0d9 |
| github.com/fannymonori/TF-ESPCN | 5c628eca | export/ESPCN_x2.pb | 59f77351...a1823221d |

Not taken: Real-ESRGAN and other generative restorers (out of brief);
RT4KSR x2 (eduardzamfir/RT4KSR), the NTIRE 2023 real-time 4K baseline, ships
only a pickle checkpoint, which was not loaded, and needs LayerNorm and GELU.

## RKNN conversion

The naive export is unusable: a one-channel full-resolution tensor is stored
NC1HWC2 with C2 = 8/16 lanes, so the output of a model that ends in
depth-to-space becomes (1,16,2160,3840,8) FP16 -- 530 MB, written by the NPU
every frame. What was deployed instead (`models.py`):

- no one-channel full-resolution tensor on the NPU: the graph ends at the
  2x2 (or 4x4) sub-pixel channels and the CPU does the pixel shuffle;
- limited-range Y in and out: `(y-16)/219` stays an elementwise op (folding it
  into conv1 changes the border, and an explicit Pad runs on the **CPU** in
  RKNN 2.3.2 -- tried, rejected); the output mapping is folded into the last
  conv;
- `s2d`: the whole net rewritten on 2x2 polyphase components (4ch 540x960 in,
  16ch 540x960 out), exactly equivalent including borders (max 1.7e-4 Y code
  vs the published net). It costs 2.6-3.8x the MACs but replaces the
  one-input-channel 5x5 layer the NPU runs at about 1/8 of its rate.

Every deployed build has all compute ops on the NPU (`build_report.py`):
InputOperator/OutputOperator are the runtime's layout conversion and are
replaced here by the benchmark's own zero-copy pre/post. No CPU or GPU
fallback.

## Performance (real NPU, 300 frames after 20 warm-up, CPU stages on 4 A76 threads)

`npu` = `rknn_run` wall time (the driver's RKNN_QUERY_PERF_RUN matched it to
the 0.01 ms). `e2e` = pre + npu + post + uv, one frame after the other.

| model | variant | dtype | npu 1 core median | npu 3 cores median | e2e 3 cores median / p95 / max |
|---|---|---|---|---|---|
| FSRCNN-small | s2d | INT8 | 58.9 | **21.8** | **26.9 / 42.9 / 44.0** |
| FSRCNN-small | s2d | FP16 | 102.1 | 38.0 | 45.0 / 65.1 / 66.7 |
| FSRCNN-small | plain | INT8 | 140.6 | 51.3 | 57.3 / 77.4 / 79.6 |
| FSRCNN-small | plain | FP16 | 204.2 | 72.8 | 80.5 / 107.1 / 108.0 |
| FSRCNN | plain | INT8 | 286.2 | 100.1 | 119.2 / 130.3 / 131.0 |
| FSRCNN | plain | FP16 | 431.5 | 160.6 | 189.8 / 192.8 / 208.4 |
| ESPCN | plain | INT8 | 236.2 | 118.4 | 139.0 / 141.3 / 151.6 |
| ESPCN | plain | FP16 | 418.5 | 180.6 | 208.0 / 210.9 / 229.5 |
| ESPCN | s2d | INT8 | 277.6 | 167.3 | 186.3 / 188.2 / 198.0 |
| ESPCN | s2d | FP16 | 638.6 | 330.3 | 369.8 / 372.0 / 387.9 |

FSRCNN s2d was stopped unmeasured: the compiler estimates it above FSRCNN
plain. Three cores give 2.0-2.8x over one; core-split is the only reason
anything comes near the budget.

Stages of the fastest configuration (FSRCNN-small s2d INT8, 3 cores, 300 s):

| stage | median | p95 | max |
|---|---|---|---|
| pre: Y -> 2x2 phases, INT8, into rknn_create_mem | 0.51 | 1.79 | 3.45 |
| npu | 21.81 | 23.47 | 24.23 |
| post: NC1HWC2 INT8 -> LUT -> 3840x2160 Y | 2.74 | 10.14 | 12.96 |
| uv: 960x540 -> 1920x1080 bilinear | 1.85 | 7.21 | 10.57 |
| **e2e** | **26.92** | **41.46** | **45.75** |

The input reaches the NPU as `NHWC INT8, zp -128, scale 1` (Y XOR 0x80) and
the output leaves as `NC1HWC2 (1,2,540,960,16) INT8, zp -128, scale 1`, so
neither side can be the decoder's or the display's buffer as it is: the
2x2 unshuffle, the 4x4 shuffle and the chroma are CPU work in any pipeline.
NPU memory for that model: 96.7 MB DMA (94.9 MB internal tensors).

## Sustained (FSRCNN-small s2d INT8, 3 cores, 300 s, 10 316 frames)

| | first 30 s | 135-165 s | last 30 s |
|---|---|---|---|
| e2e median / p95 | 26.90 / 40.87 | 26.92 / 41.35 | 26.93 / 42.66 |
| npu median | 21.80 | 21.81 | 21.81 |

- frames over 41.7 ms: 509 (4.9%); over 33.3 ms: 1 370 (13.3%). The tail is
  CPU (post and uv p95 3-4x their median), not the NPU.
- NPU 1000 MHz and DDR 2112 MHz throughout, NPU load 72-86% per core, DDR
  controller load up to 70% (45% idle with the 2160p60 desktop scanned out),
  GPU 0%, CPU at most 26% of 8 cores.
- Temperature 47-52 °C (NPU 50.8 max), trip point 75 °C: no throttling, no drift.

## Quality

Six frames of Tears of Steel 4K (3840x1714 H.264, letterboxed into 2160p):
face 60 s, dark face/metal 150 s, foliage 300 s, hair/glasses/fabric 420 s,
credits with diagonal artwork 610 s, small credits 680 s. The 1080p source is
a Lanczos downscale. VOP2 is the production scaler read back through
writeback; to keep the path equal, the AI output was also shown 1:1 and read
back (`wb`). Luma, letterbox and an 8 px border excluded; no picture was
displaced (sub-pixel phase correlation within ±0.2 px).

PSNR (dB) against the 4K reference:

| frame | VOP2 (wb) | FSRCNN-small INT8 (wb) | bicubic CPU | FSRCNN-small FP16 | FSRCNN FP16 | ESPCN FP16 | ESPCN INT8 |
|---|---|---|---|---|---|---|---|
| face | 48.40 | 43.64 | 51.21 | 49.78 | 50.17 | 50.26 | 45.58 |
| dark face | 41.45 | 40.16 | 41.84 | 41.63 | 41.70 | 41.78 | 40.93 |
| foliage | 41.11 | 41.05 | 43.73 | 43.04 | 43.82 | 44.06 | 42.34 |
| hair/fabric | 47.83 | 43.23 | 50.88 | 49.57 | 49.72 | 49.77 | 44.83 |
| credits + art | 25.88 | 27.09 | 26.46 | 26.88 | 27.22 | 27.13 | 27.10 |
| small credits | 26.87 | 30.66 | 28.62 | 30.37 | 31.83 | 32.30 | 32.28 |

(The last five columns are raw NPU/CPU output without writeback; the
writeback path itself moved FSRCNN-small INT8 by 0.1-0.4 dB.)

- Natural pictures: no model beats a plain bicubic, even in FP32 (FP16 is
  within 0.15 dB of FP32). The models were trained on bicubic-degraded
  stills; on a Lanczos-made film source their "detail" is not there.
- Text and credits: real gain over VOP2, +1.2 to +3.8 dB, fewer >4-code
  excursions for ESPCN.
- INT8 adds a fixed sub-pixel pattern: high-frequency energy 1.35-2.9x the
  reference (FP16: 0.95), visible as a fine checker on skin at the glasses
  crop; SSIM 0.97 vs VOP2's 0.99. That is quantisation of the 16 output
  phases, not detail.
- No hallucinated structure and no oversharpening in the FP16 outputs;
  overshoot stays below VOP2's on text for ESPCN, above it for FSRCNN-small.
- Geometry: AI output is centre-aligned (shift ≤ 0.05 px); VOP2 within 0.2 px.

INT8 vs FP16 (FSRCNN-small s2d): 21.8 vs 38.0 ms NPU, 2.0 vs 3.5 MB model
file; NPU memory 96.7 MB for INT8 s2d (FP16 plain: 257 MB); INT8 loses up
to 6.1 dB to FP16 on natural frames, nothing on credits.

## Temporal (foliage, 48 consecutive frames)

Mean |(candidate frame difference) - (reference frame difference)|; a linear
scaler only passes the source's own change on.

| | mean | max | worst 5% blocks |
|---|---|---|---|
| bicubic (stand-in for any fixed linear scaler) | 2.016 | 2.953 | 5.19 |
| FSRCNN-small FP16 | 2.042 | 2.905 | 4.93 |
| ESPCN INT8 | 2.149 | 2.945 | 4.85 |
| FSRCNN-small INT8 | 2.567 | 3.286 | 5.22 |

FP16 is as stable as a linear scaler. INT8 adds 27% frame-to-frame error:
the quantisation pattern changes with the content under it.

## Decision: AI_SR_QUALITY_NOT_USEFUL

- Only FSRCNN-small s2d INT8 on all three cores fits 23.976 fps on its
  median (26.9 ms), and even it misses 41.7 ms on 4.9% of frames; it misses
  33.3 ms on 13%.
- That configuration is worse than VOP2 on natural pictures (-1.3 to -4.8 dB,
  a visible pattern, +27% temporal error) and better only on credits.
- Every configuration with clean output (FP16, or a stronger model) needs
  45-370 ms per frame.
- Even unlimited, these spatial models do not beat bicubic on film content;
  a faster NPU would not change the verdict.

## Tools

| file | runs on | |
|---|---|---|
| `extract.py` | workstation, TensorFlow venv | GraphDef -> npz + TF reference output |
| `models.py` | workstation, torch | rebuild, check against TF, export `plain` / `s2d` ONNX |
| `convert.py` | workstation, rknn-toolkit2 2.3.2 | ONNX -> RKNN FP16, or INT8 calibrated on 18 real Y planes |
| `build_report.py` | anywhere | op targets and the compiler's cycle estimate from a verbose build log |
| `frames.sh` | workstation, ffmpeg | 4K reference + 1080p source NV12 from a film |
| `aisr-bench.c` | appliance | per-stage timing, sustained mode, NV12 dump |
| `run-matrix.sh` | appliance | every model, one core and three |
| `telemetry.sh` | appliance | NPU/DDR/GPU/CPU/thermal once a second |
| `reference.py` | workstation, torch | FP32 output of the same model, same chroma path |
| `quality.py` | workstation | PSNR, SSIM, detail, hf, overshoot, geometry; `--temporal` |
| `rt4ksr.py` | workstation, torch | RT4KSR x2 from its state dict: FP32 frames, NV12 deploy graph to ONNX |
| `span.py` | workstation, torch | SPAN x2 eval graph (collapsed Conv3XC) from its state dict, to ONNX |
| `quicksrnet.py` | workstation, torch | QuickSRNet Small 2x, official W8 grid, to ONNX; FP32 frames |

```sh
# workstation
python extract.py                                    # in a tensorflow-cpu venv, run from a dir with w/*.pb
python models.py FSRCNN-small_x2 w/FSRCNN-small_x2.npz fs-s2d.onnx --variant s2d
python convert.py fs-s2d.onnx fs-s2d-int8.rknn --int8 calib-s2d.txt   # .npy 1x4x540x960 per line
aarch64-linux-gnu-gcc -O3 -mcpu=cortex-a76 -o aisr-bench aisr-bench.c -I<rknpu2 include> -L<dir with librknnrt.so> -lrknnrt -lpthread -lm
# appliance, everything under /tmp, librknnrt.so 2.3.2 from rknn-toolkit2/rknpu2/runtime/Linux/librknn_api/aarch64
LD_LIBRARY_PATH=lib taskset -c 4-7 ./aisr-bench fs-s2d-int8.rknn s2d 7 src.nv12 -n 300
LD_LIBRARY_PATH=lib taskset -c 4-7 ./aisr-bench fs-s2d-int8.rknn s2d 7 src.nv12 -s 300 -l sustain.csv & sh telemetry.sh > tele.csv
```

VOP2 frames come from `../rga-upscale/wbcap.c` with mediabox-tv-ui stopped;
check VP0 names HDMI-A-1 alone afterwards. Model weights and frames are not
in the repository.
