# MBSR on a real 1080p release: QUALITY NOT ESTABLISHED

Every earlier quality result (MBSR.md, VALIDATION.md) used 1080p sources made
by Lanczos-downscaling the very 4K frame used as the reference. That is also
how MBSR was trained. A real 1080p release is a separate encode: different
compression, grain partly gone. This test puts MBSR against a real release
pair. October 2026, frozen weights (sha256 99b9f8fe...), same RKNN INT8 build
as VALIDATION.md.

## Pair

*In the Mood for Love* (2000), two UHD Blu-ray-sourced HDR10 releases:

- source: "UHD BluRay 1080p ... DoVi HDR10 x265", 1792x1080, flagged 24 fps
- reference: "2160p UHD BluRay x265", 3584x2160, 23.976 fps

The frames are the same and only the timing flag differs (duration ratio
exactly 1.001). Ten frames, every 10 min from 5 min, were taken by rule
(`hdr_pairs.py`). Each 2160p frame was matched among +-3 frames by its 2x box
downscale against the 1080p frame (47-60 dB, 10-bit): the same picture.

Both were tone-mapped to SDR BT.709 with the same simple mapping as
aisr-demo, since MBSR is an SDR model. Reference, VOP2 and MBSR (INT8 on the
NPU) then went through the same writeback path. Luma was measured with the
pillarbox excluded (columns 136-3704).

## Result: PSNR / SSIM against the 2160p release

| scene | VOP2 | MBSR INT8 | gain | detail VOP2 / MBSR (ref = 1) |
|---|---|---|---|---|
| 300 s, room, cheongsam, fan | 46.84 / 0.9875 | 46.58 / 0.9860 | -0.26 | 0.886 / 0.935 |
| 900 s, kitchen, hands, thermos | 47.38 / 0.9880 | 46.91 / 0.9860 | -0.47 | 0.813 / 0.905 |
| 1500 s, very dark corridor | 48.77 / 0.9900 | 47.99 / 0.9878 | -0.78 | 0.804 / 0.965 |
| 2100 s, face (Maggie Cheung), patterned dress | 47.28 / 0.9880 | 47.01 / 0.9863 | -0.27 | 0.826 / 0.907 |
| 2700 s, bed, patterned cheongsam | 49.20 / 0.9912 | 48.41 / 0.9890 | -0.79 | 0.878 / 1.064 |
| 3300 s, face, red coat, floral collar | 48.33 / 0.9895 | 47.67 / 0.9873 | -0.66 | 0.895 / 1.053 |
| 3900 s, stairs, floral cheongsam, patterned wall | 43.00 / 0.9726 | 43.11 / 0.9709 | +0.11 | 0.850 / 0.881 |
| 4500 s, haze, foliage shadows | 48.11 / 0.9888 | 48.14 / 0.9887 | +0.03 | 0.936 / 0.944 |
| 5100 s, two faces, cheongsam, textured wall | 45.74 / 0.9826 | 45.49 / 0.9810 | -0.25 | 0.787 / 0.823 |
| 5700 s, end credits (text) | 46.93 / 0.9984 | 51.81 / 0.9991 | +4.88 | 1.093 / 1.181 |

- **Natural frames:** MBSR is below VOP2 on seven of nine (-0.25 to -0.79
  dB) and level on two (+0.11, +0.03). Only the credits gain (+4.9 dB).
- **Why:** the reference carries fine film grain. VOP2 smooths it away.
  MBSR does not restore it; it sharpens what the 1080p encode left, blotchy
  compression texture. That brings detail closer to the reference's level
  (0.82-1.06 against VOP2's 0.79-0.94), but it is the wrong detail, so the
  error grows. To the eye the difference is very small.
- **Ringing and geometry:** clean on natural frames (shift <= 0.10 px,
  <= 0.01% of pixels > 4 codes).

## What this means

The earlier PASS holds for clean downscales and does not transfer to real
1080p releases. MBSR was trained only on Lanczos and bicubic downscales.
Beating VOP2 on real content needs training on real degradation: encoder
artifacts (x264/x265 at release bitrates), grain loss and denoising, or
pairs of real 1080p/2160p releases. That is a training-data change; the
network shape and its NPU timing (29 ms e2e) stay as measured.

Out of scope here and unsolved: HDR10/Dolby Vision. MBSR is SDR-only. The
tone mapping used for this test and for the TV demo is a crude
approximation, visibly worse than the player's own HDR output.
