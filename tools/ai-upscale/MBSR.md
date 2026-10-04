# MBSR: 1080p -> 2160p AI upscaling on the RK3588 NPU, in real time, better than VOP2

**MBSR_AI_UPSCALE_PASS.** MBSR is a 2x super-resolution network trained here,
shaped by the measurements in this directory ([NPU_SHAPE.md](NPU_SHAPE.md)).
On the Orange Pi 5 Plus it takes a 1080p NV12 frame to a 2160p NV12 frame:
INT8, three NPU cores, no CPU fallback, end to end 29.2 ms median and 39.0 ms
p95 over 300 s. It beats VOP2's upscale on every test frame through the same
display path. Research only: nothing is installed or run by the product.
October 2026.

## Shape

```
in   1x6x540x960   Y of each 2x2 block (2i+j), U, V: the NV12 planes as they are
     9 x conv3x3, 32 channels, LeakyReLU(0.1)            (38.6 GMAC / frame)
     + fixed anchor conv3x3(6->24): bilinear 2x, taps 127/225, 42/225, 14/225
out  1x24x540x960  Y of the 4x4 output block (4a+b), U and V of its 2x2 samples
```

- **NPU rules.** Nothing runs at full resolution. There is no norm, GELU or
  attention, and the input and output are the NV12 layout, so the CPU only
  scatters bytes.
- **Training.** Each conv is a 3x3 + 1x1 + expanded 1x1->3x3 + identity
  branch, composed into one kernel before every forward pass. The deployed
  graph matches the trained one to 2e-4 codes.
- **Anchor.** A learnt anchor's taps do not fit the per-channel INT8 grid
  and came out about one code wrong on every pixel (5-7 dB on smooth
  frames). The fixed anchor's taps sum to exactly 1 and sit on that grid, so
  RKNN computes them exactly. The body only learns the correction on top.
- **Two failures on the way.** Clip(0,1) and plain ReLU let whole layers
  die, and the body went silent: its output share was 0.02 codes, so the
  "model" was only the anchor. The fixes were a near-identity start and
  LeakyReLU. Training prints the body's share so this cannot pass unseen
  again.
- **INT8.** QAT fine-tuning (fake per-channel symmetric weights, per-tensor
  asymmetric activations) brings INT8 within 1 dB of FP32.

## Training (`mbsr.py`, RTX 4080, 100k + 20k QAT iterations, about 25 min)

| source | 4K frames | notes |
|---|---|---|
| Tears of Steel 4K (Blender Foundation) | 315 | every 40th frame; **the test scenes (+-8 s around 60, 150, 300, 420 s) and all credits (>= 590 s) excluded** |
| Xiph/Netflix El Fuente 4K (`fetch_xiph.py`) | 120 | 12 frames each of 10 live-action sequences, 10-bit -> 8-bit, centre crop |
| synthetic text (`text_frames.py`) | 40 | system fonts, 20-110 px |

The LR source is each frame through ffmpeg Lanczos or bicubic to 1080p
NV12. Patches are 128x128 LR, flipped and transposed, with L1 loss on the 24
output channels. Adam 3e-4 cosine, batch 48, gradient clip 1. Then QAT at
5e-5. INT8 is calibrated on 23 training frames (`convert.py --int8 ...
--std 255`).

Weights: `weights/mbsr-x2-c32d9-qat.pt` (998 kB, sha256 `99b9f8fe14a59650...`).
The RKNN build from it is 1.5 MB.

## Quality: the six film frames of the earlier tests, same writeback path as VOP2

PSNR (dB) against the 4K reference, luma, letterbox excluded:

| frame | VOP2 | **MBSR INT8** | gain | SSIM VOP2 / MBSR |
|---|---|---|---|---|
| face | 48.40 | **49.30** | +0.90 | 0.9919 / 0.9915 |
| dark face | 41.45 | **41.70** | +0.25 | 0.9414 / 0.9431 |
| foliage | 41.11 | **44.48** | +3.37 | 0.9811 / 0.9859 |
| hair, glasses, fabric | 47.83 | **49.25** | +1.42 | 0.9902 / 0.9909 |
| credits, diagonal art | 25.88 | **27.91** | +2.03 | 0.8721 / 0.9149 |
| small credits | 26.87 | **34.13** | +7.26 | 0.9688 / 0.9914 |

Without writeback, MBSR also beats a CPU bicubic and RT4KSR:

| frame | bicubic | RT4KSR FP32 | MBSR FP32 | MBSR INT8 (NPU) |
|---|---|---|---|---|
| face | 51.21 | 50.62 | 51.40 | 50.40 |
| dark face | 41.84 | 41.94 | 41.97 | 41.77 |
| foliage | 43.73 | 44.60 | 45.22 | 44.71 |
| hair, glasses, fabric | 50.88 | 50.37 | 51.20 | 50.29 |
| credits, diagonal art | 26.46 | 27.70 | 27.77 | 27.75 |
| small credits | 28.62 | 32.06 | 34.15 | 33.86 |

- **Geometry:** centre-aligned, sub-pixel shift 0.00. VOP2 shows up to -0.20.
- **Ringing and halo:** on natural frames 0.00-0.01% of pixels leave the
  reference's 5x5 range by more than 4 codes. On small credits that share is
  0.63%, against VOP2's 2.99%.
- **Artifacts:** at the glasses and foliage crops there is no checkerboard
  and no halo. The INT8 high-frequency ratio stays at FP32's level.
- **Remaining weakness:** on the face frame SSIM is 0.0004 below VOP2's while
  PSNR is 0.9 dB above. The INT8 loss against FP32 is still about 1 dB on the
  smoothest frames.

## Temporal (48 consecutive foliage frames)

| | mean | max | worst 5% blocks |
|---|---|---|---|
| bicubic (any fixed linear scaler) | 2.016 | 2.953 | 5.19 |
| **MBSR INT8** | **1.878** | **2.690** | **4.26** |

Less frame-to-frame error than a linear scaler: no shimmer added.

## Performance (Orange Pi 5 Plus, INT8, NPU 1000 MHz, A76 x4 for CPU stages)

| | median | p95 | max |
|---|---|---|---|
| pre: NV12 -> native INT8 input | 1.31 | 3.06 | 5.38 |
| NPU (rknn_run) | 24.29 | 26.15 | 26.97 |
| post: native output -> 3840x2160 NV12 | 3.62 | 9.80 | 18.16 |
| **end to end** | **29.22** | **39.01** | 46.70 |

- 300 s run, 9 804 frames: p99 39.52 ms. Three frames (0.03%) went over
  41.7 ms. The medians in the first, middle and last 30 s were 29.22 /
  29.23 / 29.23 ms: no drift.
- NPU cores 0/1/2 averaged 76/75/75% load. One core takes 67.4 ms, three
  cores 24.3 ms (2.8x).
- 13 ops, all on the NPU, no CPU fallback. 112 MB NPU DMA memory, about
  190 MB of DDR traffic per frame, DDR controller load at most 54%, CPU at
  most 28% of 8 cores, GPU idle.
- 46-52 °C throughout (trip point 75 °C), NPU and DDR frequencies fixed:
  thermally stable.

## Not done

- Production: no player, DRM, DMA-BUF or UI integration. The CPU p95 tail
  (post 9.8 ms against a 3.6 ms median) could be pipelined behind the next
  frame's NPU run.
- 1080p30 (33.3 ms) fits the median but not the p95 without that
  pipelining.
- One film was the test set; the training film is the same title, with the
  test scenes held out.

## Reproduce

```sh
python fetch_xiph.py hr 12 && python text_frames.py hr 40     # + ToS frames, see mbsr.py
python mbsr.py prep hr lr
python mbsr.py train hr lr fp.pt --iters 100000 --batch 48 --rate 3e-4 --fixed-anchor
python mbsr.py train hr lr qat.pt --iters 20000 --batch 48 --rate 5e-5 --init fp.pt --fixed-anchor --qat
python mbsr.py onnx qat.pt mbsr.onnx
python convert.py mbsr.onnx mbsr.rknn --int8 calib.txt --std 255         # .npy 1x6x540x960, codes
# board
AISR_IN_STD=255 LD_LIBRARY_PATH=lib taskset -c 4-7 ./aisr-bench mbsr.rknn nv12 7 src.nv12 -n 300
```
