# RGA3 upscaling: rejected, and the tools that showed why

**RGA3 is rejected as the MediaBox Enhanced Upscaling production path.** Do
not re-open RGA upscale unless new Rockchip driver/hardware documentation
exposes sampling-phase/origin control.

Measured on the Orange Pi 5 Plus, 6.1.115-vendor-rk35xx, October 2026. The
experimental player integration that was measured has been removed; nothing
here is installed, deployed or run by the product.

## Result

What works:

- RKMPP NV12 -> ffmpeg-rockchip `scale_rkrga` -> 3840x2160 NV12 runs in
  real time on one RGA3 core: 1080p23.976 for 250 s with no dropped frame at
  14% RGA load, 1080p30 at 17%, the player's CPU unchanged (about 9% of one
  core), GPU and NPU idle. The plane then scans the frame out 1:1.
- The production path was unaffected while the experiment was switched off:
  argv, `vf=[]`, no `/dev/rga`, plane, mode, colour and counters identical to
  the baseline before and after every run.

Why it is rejected:

- The RGA output sits about one source pixel right of and below where VOP2 puts
  the same frame. Model `j = x / f + c` (`geom.py`, residual sd < 0.01 px):

  | path | c | at x=0 | at x=1919 |
  |---|---|---|---|
  | source -> RKMPP decode | | 0.000 | 0.000 |
  | VOP2 1920 -> 3840 (production) | +0.004 dst px | -0.50 | +0.56 |
  | RGA3 `scale_rkrga`, output buffer | +2.005 dst px = **+1.002 src px** | +1.50 | +2.57 |
  | the same frame after DRM/writeback | +2.005 dst px | +1.50 | +2.57 |

  At 1.5x, 3x and 4x `c` is +1.5, +3.0 and +4.0 destination pixels: one
  source pixel at every ratio. Chroma: -0.75 chroma source pixel.
- The displacement exists before DRM and display: FFmpeg hands librga the
  whole frame (`x:0 y:0 w:1920 h:1080` -> `x:0 y:0 w:3840 h:2160`); vo_mediabox,
  the plane and writeback add nothing.
- The kernel driver (`rga3_reg_info.c`, multicore 1.3.7) programs the step as
  `(sw - 1) / (dw - 1)`, align-corners -- the same as VOP2, so not a
  difference. The RGA3 then samples source position `j * f - 1`.
- `RGA_BIC_MODE` (`RGA3_SYS_CTRL[10:9]`, never set by the driver) was forced
  to each value with a temporary kprobe on the job-start store; read back as
  0x000/0x200/0x400/0x600, mode 0 byte-identical to the stock driver:

  | BIC_MODE | origin, 2x | 1.5x | 3x | impulse peak | undershoot |
  |---|---|---|---|---|---|
  | 0 (driver default) | +1.002 src px | +1.001 | +1.002 | 117.9 | -12.4 |
  | 1 | +1.002 | +1.001 | +1.002 | 58.0 | 0 |
  | 2 | +1.002 | +0.995 | +1.002 | 114.3 | -8.1 |
  | 3 | +1.002 | +1.008 | +1.002 | 93.6 | -2.3 |

  It selects the interpolation kernel and ringing; it does not change the
  sampling origin.
- No clean hardware control was found to correct the geometry: the RGA3
  register map has a step (`SCL_FAC`), an integer window offset (`ACT_OFF`,
  NV12 in steps of two) and `RGA_BIC_MODE`, and no phase. The RGA2 core, which
  does take an interpolation choice, cannot map the decoder's buffers
  (`rga_job_commit: failed to map job info`; 32-bit addressing). Shifting,
  cropping or padding the output to hide the offset was ruled out.
- Quality figures measured before the offset was known (+0.2..0.7 dB PSNR
  against VOP2 after undoing the shift by hand) describe a picture the product
  cannot produce and are not a result.

The kprobe module itself is not kept: it patched one instruction by offset
with no check of its own against the running kernel. Building one for a
vendor kernel needs headers whose `Armbian-Original-Hash` matches the running
kernel and pahole in the build environment -- without it `struct module`
comes out 64 bytes short, loads, and corrupts its own refcount.

## Tools

| file | runs on | what it answers |
|---|---|---|
| `measure-playback.sh` | appliance | what a film costs and how it is shown: argv, `/dev/rga`, `vf`, plane, mode, CPU/GPU/RGA/NPU, drops, A/V sync |
| `wbcap.c` | appliance | the composed picture, read back through DRM writeback |
| `geom.py` | anywhere | where a source pixel lands after an upscale: impulses, lines, corner markers |
| `compare.py` | anywhere | PSNR, SSIM, detail and overshoot against a reference, refused if the picture is displaced |

`geom.py` and `compare.py` need numpy and scipy (`--crops` also Pillow); a
venv on the workstation is enough. All frames are tightly packed NV12.

## Playback telemetry

```sh
# on the appliance; the clip must be under /opt/rk3588-mediabox/assets/
sh measure-playback.sh before /opt/rk3588-mediabox/assets/<clip> 30
sh measure-playback.sh after  /opt/rk3588-mediabox/assets/<clip> 30
```

Diff the two files: for an unchanged path only the session id and the film
counter differ. Use a clip with an audio track, or A/V sync reads as
unavailable.

## Geometry

```sh
python3 geom.py pattern luma p.nv12 1920 1080          # chroma, vlines, hlines, corners too
# appliance: ffmpeg-rockchip's RGA scaler on its own
ffmpeg -init_hw_device rkmpp=rk -filter_hw_device rk \
  -f rawvideo -pix_fmt nv12 -s 1920x1080 -i p.nv12 \
  -vf "hwupload,scale_rkrga=w=3840:h=2160:format=nv12,hwdownload,format=nv12" \
  -f rawvideo rga.nv12
python3 geom.py measure luma p.nv12 1920 1080 rga.nv12 3840 2160
```

For VOP2's own scaler, show `p.nv12` with `wbcap p.nv12 1920 1080 wb.nv12 73`
on a 2160p mode and measure `wb.nv12` the same way. Use the media runtime's
ffmpeg (`/opt/rk3588-mediabox/media-runtime/bin/ffmpeg`); `-v debug` prints
the rectangles handed to librga (`RGA src | ... x:0 y:0 w:1920 h:1080`).

## Quality

A 4K reference card, downscaled to the 1080p source both scalers start from:

```sh
# reference: photo | zone plate / credits | testsrc2, 3840x2160, then NV12
ffmpeg -i reference.png -vf "scale=3840:2160:out_color_matrix=bt709:out_range=tv,format=nv12" -f rawvideo gt4k.nv12
ffmpeg -i reference.png -vf "scale=1920:1080:flags=lanczos+accurate_rnd+full_chroma_int:out_color_matrix=bt709:out_range=tv,format=nv12" -f rawvideo src1080.nv12
# appliance, UI stopped: the three through the same output path
./wbcap gt4k.nv12 3840 2160 wb-gt.nv12 73
./wbcap src1080.nv12 1920 1080 wb-vop2.nv12 73
./wbcap rga4k.nv12 3840 2160 wb-rga.nv12 73       # rga4k: src1080 through scale_rkrga as above
python3 compare.py wb-gt.nv12 VOP2=wb-vop2.nv12 RGA=wb-rga.nv12 --crops crops/
```

After writeback, check that VP0 in `/sys/kernel/debug/dri/0/summary` names
`HDMI-A-1` alone before judging the television.
