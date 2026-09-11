# Android RK3588 HDR composition oracle

Orange Pi 5 Ultra, the working Android 13 image, measured read-only on
2026-09-11 to answer one question: what exact VOP2 composition state does
Android program while Kodi draws its OSD over HDR10 video, given that the
colours are right, the OSD is right, and nothing moves horizontally.

**Classification: `ANDROID_MIXED_HDR_COMPOSITION = A — SDR_GUI_WITH_HARDWARE_SDR2HDR`.**

Android hands VOP2 a genuinely SDR GUI buffer on a plane tagged `EOTF=0` and
lets the hardware SDR-to-HDR block convert it. Linux/Kodi hands VOP2 a
PQ-encoded GUI buffer on a plane tagged `EOTF=2` and the hardware block stays
bypassed. Everything else that differs follows from that one choice.

Nothing was flashed, written or restarted. The single mutation was
`vendor.hwc.log`, set to `255` for one logging pass and restored to its
original empty value.

Evidence: `logs/orangepi5-ultra-android/android-hdr-golden-reference-2026-09-11/`
(118 files, `SHA256SUMS` verified twice on the host).

---

## 1. Identity

| | |
| --- | --- |
| build fingerprint | `rockchip/rk3588_t/rk3588_t:13/TQ3C.230805.001.B2/eng.yu.20250802.104257:userdebug/release-keys` |
| vendor fingerprint | `rockchip/rk3588_t/rk3588_t:13/TQ3C.230805.001.B2/yu08021041:userdebug/release-keys` |
| Android | 13, security patch 2023-08-05, `userdebug`, built 2025-08-02 |
| kernel | `5.10.157 #2 SMP PREEMPT Fri Aug 1 16:53:55 UTC 2025 aarch64`, Android clang 14.0.6 |
| SELinux | permissive (`androidboot.selinux=permissive`) |
| root | `/system/xbin/su`, legacy syntax `su 0 <cmd>` |
| composer HAL | `/vendor/bin/hw/android.hardware.graphics.composer@2.1-service`, PID 325 |
| HWC implementation | `/vendor/lib64/hw/hwcomposer.rk30board.so`, 879968 B, SHA256 `1a639074181aadc1d850c1d48db082aebc2bf292a3c9f610f573414d4d4ea09a`, build-id `81cd2e59581c5d6d07ece36237254b49` |
| HWC version | `vendor.ghwc.version = HWC2-1.5.143` (Rockchip drmhwc2) |
| Kodi | `org.xbmc.kodi` 21.3, versionCode 2103000, arm64-v8a, APK SHA256 `34e7526705a038fa12f70161d0c1cc38f10deb0fee305b46fbd46ff015d05b44` |
| test asset | `past.lives.2023.hdr.2160p.web.h265-huzzah.mkv`, SHA256 `f4e32b8d…96a` — **matches the Linux reference exactly** |

The public Rockchip Android13 drmhwc2 mirror is a valid map for this binary.
Every symbol the brief asked to look for is present in the running library:
`CollectVPHdrInfo`, `android::DrmHdrParser` (`Init`, `InitVividHdr`,
`InitNextHdr`, `MetadataHdrParser`, `NextHdrParser`),
`DrmPlane::get_sdr2hdr`, `DrmPlane::get_hdr2sdr`,
`DrmDevice::is_plane_support_hdr2sdr`, `vendor.hwc.log`, and the
`s2h_sm_ratio` / `s2h_scale_ratio` / `s2h_sdr_color_space` user-config string.
Runtime behaviour, not the mirror, is what is reported below.

## 2. The golden state

Five states were captured in one Kodi session (PID 3531 throughout).

**G1, G2, G3 and G4 are identical.** `10-drm-summary.txt` diffs clean between
all four once the video buffer address is masked, and the VOP2 `HDR:` register
block at `0xfdd92000` is byte-identical across all four.

```
Video Port0: ACTIVE
    Connector: HDMI-A-1
        bus_format[200d]: YUYV10_1X20
        overlay_mode[0] output_mode[9] color_space[10], eotf:2
    Display mode: 1920x1080p60
    Cluster0-win0: ACTIVE                 <- HDR video
        format: YU10 [AFBC] HDR[2] color_space[10] glb_alpha[0xff]
        csc: y2r[1] r2y[0] csc mode[3]
        zpos: 0
        src: pos[0, 4] rect[3840 x 2080]
        dst: pos[0, 21] rect[1920 x 1038]
    Esmart0-win0: ACTIVE                  <- SDR GUI / OSD
        format: AB24 SDR[0] color_space[0] glb_alpha[0xff]
        csc: y2r[0] r2y[0] csc mode[0]
        zpos: 1
        src: pos[0, 0] rect[1920 x 1080]
        dst: pos[0, 0] rect[1920 x 1080]
```

G0 (GUI only, no video) is the only state that differs: both Kodi surfaces sit
on `Cluster0-win0` and `Cluster0-win1` as AFBC RGBA, the port is
`RGB888_1X24`, `eotf:0`, and SDR2HDR is bypassed.

The composer says the same thing itself, once per frame, in the verbose log:

```
plane=Cluster0-win0 ... zpos=0 ... blend mode=2 eotf=2 colorspace=a
plane=Esmart0-win0  ... zpos=1 ... blend mode=0 eotf=0 colorspace=0
```

and classifies the layers `hdr=1` (YU10, AFBC) and `hdr=0` (AB24).

### Composition type

SurfaceFlinger reports, in every playback state:

| layer | dataspace | HWC composition |
| --- | --- | --- |
| `SurfaceView[org.xbmc.kodi/…](BLAST)` | `BT2020_ITU_PQ (298188800)` | **DEVICE** |
| `org.xbmc.kodi/org.xbmc.kodi.Main` | `UNKNOWN (0)` | **CLIENT** |

So the answer to the brief's question 10 is **C**: the HDR video is a hardware
plane fed straight from the decoder, and the GUI is GPU-composited by
SurfaceFlinger into one client-target buffer that is itself scanned out on a
second hardware plane. Two hardware planes, one HDR, one SDR. It is not a
single composited framebuffer (B), and the GUI never becomes PQ.

`ro.surface_flinger.has_HDR_display = false` and `colorMode = NATIVE (0)`:
SurfaceFlinger does no HDR rendering at all. The GUI buffer holds ordinary
sRGB pixels.

## 3. Hardware SDR-to-HDR is on, and that is the whole mechanism

VOP2 `SDR2HDR_CTRL` at `0xfdd92010`:

| state | value | decode |
| --- | --- | --- |
| G0, GUI only | `0x00000100` | `sdr2hdr_bypass_en=1`, everything else 0 |
| **G1 / G2 / G3 / G4** | **`0x0000000b`** | **`sdr2hdr_eotf_en=1`, `sdr2hdr_r2r_en=1`, `sdr2hdr_oetf_en=1`, `bypass_en=0`** |

The bit layout is not inferred. It is read from the driver that is running:
`RK3568_SDR2HDR_CTRL = 0x2010` with `sdr2hdr_eotf_en` bit 0, `sdr2hdr_r2r_en`
bit 1, `sdr2hdr_r2r_mode` bit 2, `sdr2hdr_oetf_en` bit 3, `sdr2hdr_bypass_en`
bit 8 (`drivers/gpu/drm/rockchip/rockchip_vop2_reg.c`). The Linux 6.1.115 tree
on this host defines the identical fields.

`HDR2SDR` is off everywhere — the output is PQ in every arm, so `hdr2sdr_en`
is never set. The HDR LUT path is engaged: the kernel loads the sdr2hdr curve
(`SDR2HDR_FOR_HDR`) only when `sdr2hdr_en`, and `hdr_lut_mode` is written in
the same branch. The vendor `DrmHdrParser` / Vivid metadata path is present in
the binary but idle here: every `vendor.hwc.vivid_*` property is unset and the
log carries no Vivid parse line — this is plain HDR10, not HDR Vivid.

### What turns it on

`vop2_setup_hdr10()` in the running driver:

```c
for_each_set_bit(phys_id, &win_mask, ROCKCHIP_MAX_LAYER) {
        ...
        if (vpstate->eotf != HDMI_EOTF_SMPTE_ST2084) {
                have_sdr_layer = true;
                break;
        }
}

if (have_sdr_layer && vp->hdr_out) {
        sdr2hdr_en = 1;
        sdr2hdr_r2r_mode = BT709_TO_BT2020;
        sdr2hdr_tf = SDR2HDR_FOR_HDR;
}
```

One non-PQ plane in the composition is the entire condition. Android's GUI
plane is `EOTF=0`, so `have_sdr_layer` is true and the block runs. Linux 0009's
GUI plane is `EOTF=2`, so `have_sdr_layer` is false and the block stays
bypassed — which is exactly what the Linux registers show: `0x00000100` in
`vop-regs-playing.txt`, `vop-regs-paused.txt` and `vop-regs-resumed.txt`, all
three.

`overlay_mode` follows from the same branch and is not an independent variable:

```c
if (hdr2sdr_en || sdr2hdr_en) {
        vcstate->yuv_overlay = false;     /* -> overlay_mode 0 */
        VOP_MODULE_SET(vop2, vp, hdr_lut_mode, lut_mode);
}
```

Android measures `overlay_mode[0]`, Linux 0009 measures `overlay_mode[1]`, and
the previous gate already falsified YUV mixing as the cause by booting a kernel
that forced `yuv_overlay = false` and watching the displacement survive.

## 4. Geometry and timing are not the difference

Measured, not assumed:

| | Android G1–G4 | Linux playing / paused / resumed |
| --- | --- | --- |
| `VP0_PRE_SCAN_HTIMING` (`0xfdd90c30`) | `0x03f4002c` | `0x07b40058` |
| decoded `pre_scan_dly` | 1012 | 1972 |
| decoded `hsync_len` | 44 | 88 |
| `BG_DLY` (`0xfdd906e0[31:24]`) | `0x35` = 53 | `0x35` = 53 |

Both sides check out against `pre_scan_dly = bg_dly + (hdisplay >> 1) - 1`:
`53 + 960 - 1 = 1012` at 1920, `53 + 1920 - 1 = 1972` at 3840. `bg_dly` is 53
on both, which is `pre_scan_max_dly[2] = 65` minus `bg_ovl_dly = 12` for two
active layers. The scan start is identical in the only sense that matters, and
this confirms the earlier gate's finding rather than reopening it.

Per-window delays differ, but only as a consequence of which plane class
carries the HDR layer:

| | Cluster0 | Esmart0 |
| --- | --- | --- |
| Android | `0x11` = 17 = HIHO_H(29) − 12 | `0x17` = 23 = DEFAULT |
| Linux | `0x04` = 4 = DEFAULT | `0x24` = 36 = HIHO_H(48) − 12 |

Both are what `vop2_setup_dly_for_window()` computes for their own topology,
from identical `dly` tables (`{4,26,29,…}` Cluster, `{23,45,48,…}` Esmart) in
both the 5.10 and 6.1 trees. This is a symptom of the swapped plane
assignment, not a separate fault.

## 5. Why every Linux attempt so far has failed

`sdr2hdr` is a single per-video-port module, not a per-window one. Every
register it drives is written with `VOP_MODULE_SET(vop2, vp, …)`. When it is
on, it converts the non-HDR side of the overlay as a whole.

That resolves the contradiction in the record:

| arm | GUI buffer | GUI plane tag | sdr2hdr | shift | OSD colour |
| --- | --- | --- | --- | --- | --- |
| **Android** | **plain sRGB** | **`EOTF=0`** | **on** | **none** | **correct** |
| Linux 0009 (accepted) | PQ-encoded by Kodi | `EOTF=2` | off | **displaced** | correct |
| Linux rejected fix 1 | PQ-encoded by Kodi | `EOTF=1` | on | none | washed out |
| Linux sentinel (rejected) | PQ-encoded by Kodi | `EOTF=2` + SDR sentinel | on | none | washed out |
| Linux kernel fix 2 (rejected) | PQ-encoded by Kodi | `EOTF=2` | off | **displaced** | correct |

Patch 0009 says what Kodi is doing, in its own comment:

> The HDR GUI compositor writes transfer-encoded pixels into the scanout
> buffer. Tell display hardware that the output plane is already HDR so it does
> not apply a second SDR-to-HDR conversion.

Both rejected colour regressions are the same event: the hardware converted a
buffer that Kodi had already converted. Neither experiment changed the buffer,
only the tag. Android is the only arm in the table where the tag and the pixels
agree, and it is the only arm that gets both results at once.

## 6. The three differences that matter

1. **The GUI buffer's encoding.** Android's GUI buffer holds plain sRGB;
   Kodi's holds PQ-encoded pixels produced by `SetGuiCompositing()`'s composite
   shader and LUTs. This is the one difference no Linux experiment has touched.
2. **The GUI plane's EOTF tag.** `SDR[0]` on Android, `HDR10[2]` on Linux 0009.
   This is the bit the kernel reads to decide `have_sdr_layer`, and therefore
   whether `sdr2hdr` runs and whether the composition enters the all-PQ corner
   that displaces the picture.
3. **Which plane class carries the video.** Android puts HDR video on
   `Cluster0` and the SDR GUI on `Esmart0`; Linux does the opposite. The
   previous gate considered moving Kodi's GUI to an Esmart window and could not
   measure it cleanly. It is a real difference and it should be held in
   reserve, but it is third: differences 1 and 2 fully explain both symptoms on
   their own.

## 7. Answers to the brief

1. **Build fingerprint** — `rockchip/rk3588_t/rk3588_t:13/TQ3C.230805.001.B2/eng.yu.20250802.104257:userdebug/release-keys`, kernel 5.10.157.
2. **Composer** — `android.hardware.graphics.composer@2.1-service` over `hwcomposer.rk30board.so`, `HWC2-1.5.143`.
3. **Kodi** — 21.3 (2103000), arm64-v8a.
4. **HDR video composition type** — `DEVICE`, hardware plane `Cluster0-win0`, AFBC YU10.
5. **OSD composition type** — `CLIENT`: GPU-composited by SurfaceFlinger, then scanned out on hardware plane `Esmart0-win0`.
6. **OSD dataspace / EOTF** — SurfaceFlinger `UNKNOWN (0)` (plain sRGB), plane `EOTF=0`, `color_space[0]`, no CSC. **SDR, not PQ.**
7. **Video dataspace / EOTF** — `BT2020_ITU_PQ (298188800)`, plane `EOTF=2`, `color_space[10]` = BT.2020, `csc y2r[1] mode[3]`.
8. **What G1→G2 adds** — nothing. No plane is added, no register changes. The GUI plane is attached in both; only its buffer contents change.
9. **SDR2HDR** — **YES.** `0x2010 = 0x0000000b`, eotf+r2r+oetf enabled, bypass off, `r2r_mode = BT709_TO_BT2020`.
10. **HDR2SDR** — **NO.** Output is PQ, so `hdr2sdr_en` is 0 in every state.
11. **HDR parser / LUT** — the sdr2hdr LUT path is used (`hdr_lut_mode` written, curve loaded). `DrmHdrParser` / Vivid is present in the binary but idle: no Vivid properties set, no parser output in the log.
12. **G2/G3 topology** — two hardware planes on VP0: `Cluster0-win0` HDR video at zpos 0, `Esmart0-win0` SDR GUI at zpos 1.
13. **Horizontal shift on Android** — **NO**, in every state. This is the premise the gate was opened on and the operator confirms it; no measurement in this capture contradicts it, and the composition state never changes across the transitions that displace the picture on Linux.
14. **Top three differences vs Linux 0009** — section 6.
15. **Android's mechanism** — keep the GUI genuinely SDR in both pixels and tag, let one non-PQ plane switch VOP2 onto its hardware SDR-to-HDR path, and let that hardware do the BT.709→BT.2020 and SDR→PQ conversion. Correct colour comes from the hardware conversion; correct geometry comes from never entering the all-PQ multilayer corner.
16. **Next Linux change** — section 8.

## 8. The single next Linux experiment

Stop Kodi PQ-encoding the GUI, and tag the GUI plane `EOTF=0`.

Concretely: while HDR10 video is on its own plane, do not enable the HDR GUI
compositor (`SetGuiCompositing()` → no composite shader, no LUT, GUI rendered
in plain sRGB as Kodi does for SDR output), and let `SetGuiPlaneEotf(false)`
tag the GUI plane `TRADITIONAL_SDR` as patch 0009 already does on its false
path. Change nothing about the video plane: it stays `EOTF=2`, BT.2020, PQ.

Predicted result, from the kernel path and the Android measurement:
`have_sdr_layer` becomes true, `SDR2HDR_CTRL` goes from `0x00000100` to
`0x0000000b`, `overlay_mode` drops to 0, the hardware performs the
BT.709→BT.2020 and SDR→PQ conversion the shader used to do, the OSD colour is
correct because the buffer is converted exactly once, and the displacement does
not occur because the composition is no longer all-PQ — the previous gate
already measured `dx = 0` with a writeback capture at `GUI EOTF = 0`.

This is not rejected fix 1. That one kept the PQ-encoded buffer and changed
only the tag, which is precisely the double conversion that washed out the
blue. Failure criterion: if the OSD is still washed out with the shader
disabled, the hardware sdr2hdr curve is not equivalent to Kodi's and the third
difference (moving the video to a Cluster window) becomes the next arm.

Do not write this patch before the target is back on Linux; the Android image
is intact and nothing here has been flashed.
