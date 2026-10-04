# MBSR blind validation: unseen films, frozen weights

The question: does MBSR hold up on films it never saw, with no weight
changed? The bar is the same as in [MBSR.md](MBSR.md): a clear win over
VOP2 on natural pictures, no artifacts, correct geometry, temporal
stability, and 1080p23.976 at an end-to-end p95 of 41.7 ms or less.
October 2026, Orange Pi 5 Plus.

## Frozen

- Weights: `weights/mbsr-x2-c32d9-qat.pt`, sha256 `99b9f8fe14a5965030bb0fca882ad23874f83cc5b22cb39f6d952f628c398745`,
  as committed in 1f2dc01. No training and no fine-tuning took place.
- RKNN INT8 was rebuilt from those weights with the same 23 **training**
  frames for calibration, under the rule in MBSR.md: every 30th kept ToS
  frame, the first frame of each El Fuente sequence, the first two synthetic
  text frames. No validation frame was used. The .rknn's sha256 differs from
  the first build's (`1c9f4076...` vs `b0c61705...`): the file carries build
  metadata, while the weights and the calibration rule are the same.

## Blind set: never in training or calibration

| source | frames | content |
|---|---|---|
| Netflix *Chimera* (Xiph, 4096x2160 10-bit, centre crop to 3840) | 9, the middle frame (600) of every sequence, picked by rule, not by look | Aerial, BarScene, Dancers, DinnerScene, DrivingPOV, PierSeaside, RollerCoaster, ToddlerFountain, WindAndNature |
| *Sintel* 4K (Blender, CG, 4096x1744) | 5, picked by content before any result | face/hair/snow 60 s, fabric 245 s, sky/dragon 330 s, title 750 s, small credits 840 s |
| *Big Buck Bunny* 2160p (Blender, CG) | 3 | grass/fur 60 s, fur/face 200 s, foliage 330 s |
| *Chimera* WindAndNature 600-647 | 48 consecutive | temporal |

The training used El Fuente, a different Netflix production from Chimera,
and Tears of Steel. The 1080p source is a Lanczos downscale of the 4K
reference, as before.

## Quality: same writeback path for reference, VOP2 and MBSR INT8 (real NPU output)

PSNR / SSIM, luma, letterbox and an 8 px border excluded:

| frame | VOP2 | **MBSR INT8** | gain | overshoot >4 codes | shift dy,dx |
|---|---|---|---|---|---|
| Chimera Aerial | 39.79 / 0.9705 | **44.78 / 0.9858** | +4.99 | 0.01% | +0.00,-0.05 |
| Chimera BarScene | 39.65 / 0.9486 | **42.24 / 0.9549** | +2.59 | 0.00% | -0.05,-0.05 |
| Chimera Dancers | 42.33 / 0.9484 | **42.52 / 0.9518** | +0.19 | 0.00% | -0.05,-0.05 |
| Chimera DinnerScene | 44.46 / 0.9669 | **44.84 / 0.9695** | +0.38 | 0.00% | -0.10,-0.10 |
| Chimera DrivingPOV | 39.51 / 0.9690 | **43.72 / 0.9812** | +4.21 | 0.00% | -0.10,+0.00 |
| Chimera PierSeaside | 44.74 / 0.9796 | **46.78 / 0.9841** | +2.04 | 0.00% | -0.05,-0.05 |
| Chimera RollerCoaster | 45.52 / 0.9846 | **47.24 / 0.9865** | +1.72 | 0.00% | +0.00,-0.05 |
| Chimera ToddlerFountain | 43.63 / 0.9774 | **45.89 / 0.9813** | +2.26 | 0.00% | -0.05,-0.05 |
| Chimera WindAndNature | 41.22 / 0.9803 | **45.83 / 0.9848** | +4.61 | 0.00% | -0.05,+0.00 |
| Sintel face/hair/snow | 46.87 / 0.9884 | **48.60 / 0.9901** | +1.73 | 0.00% | +0.00,+0.00 |
| Sintel fabric | 46.57 / 0.9905 | **48.70 / 0.9923** | +2.13 | 0.00% | +0.00,+0.00 |
| Sintel sky/dragon | 41.42 / 0.9900 | **45.56 / 0.9921** | +4.14 | 0.01% | +0.00,+0.05 |
| Sintel title | 35.49 / 0.9929 | **43.33 / 0.9982** | +7.84 | 0.07% | +0.00,+0.00 |
| Sintel small credits | 24.99 / 0.9132 | **31.55 / 0.9610** | +6.56 | 0.15% | +0.00,+0.00 |
| BBB grass/fur | 42.67 / 0.9850 | **47.22 / 0.9915** | +4.55 | 0.01% | +0.00,+0.00 |
| BBB fur/face | 52.28 / 0.9960 | **52.70 / 0.9959** | +0.42 | 0.00% | +0.00,+0.00 |
| BBB foliage | 41.74 / 0.9831 | **45.63 / 0.9889** | +3.89 | 0.01% | +0.00,+0.00 |

- **Gain over VOP2:** MBSR wins all 17 blind frames. On the 13 natural and
  CG non-text frames the gain is +0.19 to +4.99 dB, median +2.13. SSIM is
  higher on 16; BBB fur/face is level (0.9959 vs 0.9960).
- **Smallest margins:** Dancers (+0.19), DinnerScene (+0.38) and BBB
  fur/face (+0.42). These are soft or dark pictures where VOP2 is already
  close.
- **Geometry:** within 0.10 px everywhere.
- **Ringing and halo:** at most 0.15% of pixels leave the reference's 5x5
  range by more than 4 codes, on small credits; 0.00-0.01% on natural
  frames.
- **Artifacts:** crops of the face, fabric, hair edge and credits show no
  checkerboard, contouring or halo. MBSR keeps the reference's fine grain
  where VOP2 smooths it.

Against a CPU bicubic (raw, no writeback), INT8 MBSR is better on 11 of 17
frames. It is 0.1-0.6 dB below on five calm Chimera frames and the Sintel
fabric frame, and 1.2 dB below on BBB fur/face. Against VOP2, the product's
real baseline, it is ahead everywhere: VOP2 is the weakest scaler of the three.

## Temporal (Chimera WindAndNature, 48 frames)

| | mean | max | worst 5% blocks |
|---|---|---|---|
| bicubic (any fixed linear scaler) | 0.913 | 0.920 | 1.606 |
| MBSR FP32 | 0.885 | 0.891 | 1.498 |
| **MBSR INT8 (NPU)** | **0.991** | **0.996** | **1.592** |

INT8 adds about 0.1 code of frame-to-frame change on average over a linear
scaler (+8.5%) and is level in the worst blocks. FP32 has less temporal
error than bicubic, so the extra comes from INT8 rounding moving with the
content. This is small but measurable, and it is the weakest of the five
criteria.

## Latency (real NPU, INT8, three cores, NV12 in -> 3840x2160 NV12 out)

| run | frames | median | p95 | p99 | max | over 41.7 ms |
|---|---|---|---|---|---|---|
| 17 blind stills, 300 iterations | 300 | 29.12 | 38.98 | | 41.27 | 0 |
| WindAndNature, 300 s sustained | 9 801 | 29.18 | 38.94 | 39.26 | 41.27 | **0** |

- Medians over the first, middle and last 30 s were 29.18 / 29.18 / 29.19 ms:
  no drift.
- NPU cores averaged 73/72/72% load. NPU 1000 MHz and DDR 2112 MHz held
  throughout. DDR controller load peaked at 66%, CPU at 27%; 44-52 °C.

## Television demo (`aisr-demo.c`)

Live on the TV at 23.976 fps, a blind Sintel clip alternates every 5 s
between MBSR's 4K output scanned out 1:1 (white square top left) and the
same 1080p frame scaled by VOP2. 24 frames were shown every second, with
the MBSR path at 29-31 ms per frame on average; the frame was copied into a
scan-out buffer, which a real integration would avoid. A split screen over two planes is not possible on the Plus: only
Cluster0 and Esmart0 reach VP0, and Cluster0 refuses NV12.

## Verdict against the five criteria

| criterion | result |
|---|---|
| clear win over VOP2 on natural pictures | **met**: 17/17, median +2.1 dB on non-text frames; three soft frames at +0.2 to +0.4 dB |
| no artifacts | **met**: no checkerboard or halo, ringing <= 0.15% |
| geometry correct | **met**: <= 0.10 px |
| temporal stability | **met, with a margin to watch**: worst blocks equal to a linear scaler, mean +8.5% from INT8 rounding |
| 1080p23.976 e2e p95 <= 41.7 ms | **met**: p95 38.9 ms, 0 of 9 801 frames over 41.7 ms |
