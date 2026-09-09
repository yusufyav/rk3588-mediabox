# Gate MP1a: HDR signalling isolation on Orange Pi 5 Ultra

Date: 2026-09-09
Target: `root@10.27.27.25` (`orangepi5-ultra`, RK3588 OPi 5 Ultra)
Sink: Sony BRAVIA `KD-65XE9005` at `10.27.27.51`

## 1. Executive summary

**GATE MP1a RESULT: `PASS`.**

The vendor RK3588 Linux DRM stack does signal HDR10 correctly. Given a
3840x2160p23.976 HEVC Main 10 BT.2020/PQ stream decoded by RKMPP into `NV15`
DRM PRIME buffers and committed through atomic KMS with `Colorspace =
BT2020_YCC`, the vendor `color_depth = 30bit` and a `HDR_OUTPUT_METADATA` blob
built from the stream's own mastering metadata, the Sony TV enters HDR.

The A/B ladder ran to completion, one variable per rung, five rungs, all `PASS`,
with zero decode errors, zero commit errors and no error or warning in any
kernel-log delta. Each rung moved exactly the piece of state it was supposed to
move:

| Rung | Variable added | VOP2 `bus_format` | VP0 HDR state | VP0 encoding |
| --- | --- | --- | --- | --- |
| a0 | NV12 8-bit, SDR asset | `2025` `YUV8_1X24` | `SDR[0]` | BT.709 |
| a1 | NV15 10-bit frames | `2025` `YUV8_1X24` | `SDR[0]` | BT.709 |
| a2 | `Colorspace=BT2020_YCC` | `2025` `YUV8_1X24` | `SDR[0]` | **BT.2020** |
| a3 | `color_depth=30bit` | **`200d` `YUYV10_1X20`** | `SDR[0]` | BT.2020 |
| a4 | `HDR_OUTPUT_METADATA` | `200d` `YUYV10_1X20` | **`HDR10[2]`** | BT.2020 |

Three independent lines of evidence agree that a4 is the rung that turns HDR
on, and that nothing before it does:

1. **Source side.** `/sys/kernel/debug/dri/0/summary` reports `SDR[0]` for a0
   through a3 and `HDR10[2]` for a4.
2. **Sink side.** The TV's own IP-control API reports a different picture
   profile during a4 only: `xtendedDynamicRange` goes `medium` → `off`,
   `lightSensor` becomes unavailable, and `brightness` reads the HDR profile's
   value. All three revert after the connector is reset.
3. **Human observation.** During the ladder the operator reported "4k görüntü
   var ama hdr değil" while the earlier rungs were on screen and "şimdi hdr
   geldi" as the run reached its final rung.

Two results matter beyond the pass/fail. First, **a3 is where the link becomes
10-bit**: requesting the vendor `color_depth = 30bit` moved the HDMI bus format
from `YUV8_1X24` (4:4:4 8-bit) to `YUYV10_1X20` (**YCbCr 4:2:2 10-bit**), which
is precisely the only 10-bit path this HDMI 1.4 / 300 MHz sink can accept at
4K. The driver negotiated it without being told which subsampling to use.
Second, **the plane the vendor driver registers as the cursor plane is the one
that works**: `Esmart0-win0` (id 73, `type = Cursor`) accepted a full-screen
3840x2160 `NV15` scanout in an atomic `TEST_ONLY` commit and then in a real
commit, every rung, ~718 page flips each.

Nothing was installed on the target. No kernel, DTB, bootloader, boot
configuration or service was changed. Nothing on the TV was changed; only
getters were called.

## 2. Source revision

| Item | Value |
| --- | --- |
| Repository | `yusufyav/rk3588-mediabox` (new, private) |
| Branch | `main` |
| Probe source | `tools/hdr-signaling-probe.cpp` |
| Probe source SHA-256 | `c6e275e7c857ab418d69cf406b9c2e31a2722dd84e893b1d7e1b7a55e0f9ae90` |
| Build | CMake, C++17, built on the target |
| Reference repository | `yusufyav/rk3588-screenbridge` at `582e1d30c6762c134118445bf660c0784aaffe58`, read-only |

Provenance: the DRM discovery, atomic-commit and DRM PRIME import structure is
derived from `tools/hdmirx-direct-display.cpp` in the ScreenBridge repository,
reduced to what this gate needs and re-targeted from V4L2 capture to file
playback. ScreenBridge was read but never modified; its `HEAD` and clean
working tree were verified before and after this gate.

## 3. Target identity

| Item | Value |
| --- | --- |
| Model | `RK3588 OPi 5 Ultra`, `rockchip,rk3588-orangepi-5-ultra` |
| OS | Armbian 26.11.0-trunk.35 trixie (Debian 13.6) |
| Kernel | `6.1.115-vendor-rk35xx-screenbridge-hdmirx-audio` |
| DRM driver | `rockchip 4.0.0 20140818`, 1 CRTC, `HDMI-A-1` connector id 201 |
| CRTC | `video_port0`, id 89 |
| Decoder user space | `/opt/rk3588-screenbridge/bin` FFmpeg `d90e3a1`, `--enable-rkmpp` |
| MPP | `librockchip_mpp` `0986d01` |
| Sink | Sony BRAVIA `KD-65XE9005`, interface version 5.4.0, at `10.27.27.51` |

The sink is the same physical TV whose EDID was decoded in Gate MP0: EDID
product name `SONY TV`, image size 144 x 81 cm, which matches a 65-inch panel.

## 4. MP0 assumptions used

These were established in Gate MP0 and used here as inputs rather than
rediscovered. Each was re-confirmed in passing by `--probe`, whose full output
is in `00-probe.txt`.

| Assumption | Re-confirmed |
| --- | --- |
| HDMI OUT is `HDMI-A-1`, connector id 201 | yes |
| `HDR_OUTPUT_METADATA` exists (id 7) | yes |
| `Colorspace` exists (id 214) with `BT2020_YCC=10`, `BT2020_RGB=9` | yes |
| No standard `max bpc` property | yes — reported `ABSENT` |
| Vendor `color_depth` enum `Automatic=0 / 24bit=8 / 30bit=10` (id 202) | yes |
| `color_depth_caps = 7`, `color_format_caps = 15` | yes |
| VOP2 `vp0` advertises `HDR10` in its `FEATURE` bitmask | assumed, not re-read |
| `Cluster0-win0` has no NV12/NV15; `Esmart0-win0` has them but is the cursor plane | yes |
| RKMPP decodes HEVC Main 10 to `NV15` preserving BT.2020/PQ | yes |
| Sink is HDMI 1.4 / 300 MHz: 10-bit at 4K only at ≤30 Hz over 4:2:2 | **confirmed by a3** |

The last row is the one this gate turned from a prediction into a measurement.

## 5. Test asset metadata

No real HDR10 file exists on either machine — a search of the workstation and
the target found no media files at all — so the gate used the synthetic HDR10
stream option, which is the second preference in the task's ordering and which
was already validated in Gate MP0. Both assets are generated by
`scripts/make-test-assets.sh` and are reproducible from it.

| Asset | SHA-256 |
| --- | --- |
| `sdr-4k-2398-main.mp4` | `dddcf04a200932894d662a6c091cecc9c34971e4e085b87ebd7cca116de84a1c` |
| `hdr10-4k-2398-main10.mp4` | `2c105ada1080c817ebc3cbb5fdf29d414290602af1c02ec1749f511d33929e18` |

`ffprobe` on the HDR10 asset, and what the probe read back on the target:

| Field | Value |
| --- | --- |
| codec / profile | `hevc` / `Main 10` (`profile=2` as the decoder reports it) |
| resolution | 3840 x 2160 |
| pixel format | `yuv420p10le` (10-bit) |
| `color_primaries` | `bt2020` |
| `color_transfer` | `smpte2084` |
| `colorspace` | `bt2020nc` |
| frame rate | `24000/1001` = 23.976 fps |
| Mastering display metadata | present: `r(0.7080,0.2920) g(0.1700,0.7970) b(0.1310,0.0460) wp(0.3127,0.3290)`, `min 0.005`, `max 1000.0` cd/m² |
| MaxCLL / MaxFALL | 1000 / 400 |

The SDR baseline asset is the same generator, same geometry and same frame
rate, differing only in bit depth and colour tagging: HEVC Main 8-bit, BT.709.

The probe re-reads this metadata on the target from both the stream's coded
side data and the first decoded frame's side data, and logs what it found:

```text
asset hdr_side_data mastering=1 content_light_level=1 transfer=smpte2084
```

Nothing was invented. Had the mastering or content-light metadata been absent,
the probe would have logged `metadata unavailable` and left those infoframe
fields zero, which is the CTA-861.3 encoding for "unspecified"; the code path
exists and is exercised by the SDR asset, which reports `mastering=0
content_light_level=0`.

## 6. DRM topology

From `--probe`, which performs no modeset:

```text
card /dev/dri/card0   crtcs=1   connectors=1 (connected HDMI-A)
connector id=201 HDMI-A-1 status=connected modes=48
  property Colorspace           id=214 value=0
  property color_depth          id=202 value=8    enums: Automatic=0 24bit=8 30bit=10
  property color_depth_caps     id=204 value=7
  property color_format         id=203 value=1    enums: rgb=0 ycbcr444=1 ycbcr422=2 ycbcr420=3 ...
  property color_format_caps    id=205 value=15
  property HDR_OUTPUT_METADATA  id=7   value=0
  property max bpc              ABSENT
  property HDR_PANEL_METADATA   id=206 value=226
plane id=73  type=Cursor  possible_crtcs=0x1 nv12=1 nv15=1
plane id=146 type=Overlay possible_crtcs=0x1 nv12=1 nv15=1
plane id=162 type=Overlay possible_crtcs=0x1 nv12=1 nv15=1
plane id=178 type=Overlay possible_crtcs=0x1 nv12=1 nv15=1
```

### 6.1 The plane-type question, answered by the kernel

Four planes advertise `possible_crtcs = 0x1` and both NV12 and NV15. Three of
them are typed `Overlay`, which is what a conventional player looks for. But
VOP2's plane mask assigns only `Cluster0` and `Esmart0` to `vp0`; planes 146,
162 and 178 belong to video ports that have no output in this DTB.

The probe therefore does not filter by `type`. It builds the complete atomic
request for the step and issues a `DRM_MODE_ATOMIC_TEST_ONLY` commit for each
candidate plane in turn, taking the first the kernel accepts. On every rung:

```text
plane candidate id=73 type=Cursor test_only=ACCEPTED
plane selected id=73
```

The cursor-typed plane is the correct and only answer on this CRTC, and the
kernel says so. A player that rejects it on type grounds would find no video
plane at all. This is the single most important integration constraint this
gate produced for the later Kodi work.

### 6.2 Inherited console plane

`Cluster0-win0` (id 57) is left bound to CRTC 89 by the kernel fbdev client,
holding a 1920x1080 `XR24` SDR surface. The probe disables it as part of the
modeset:

```text
inherited plane id=57 is bound to CRTC 89; it will be disabled
```

This matters for correctness, not tidiness: VOP2 blends per-window and tracks
an HDR/SDR flag per window, so leaving an SDR RGB window active underneath the
surface under test would have put a second input into exactly the blending path
this gate is trying to characterise.

### 6.3 Mode selection

Every rung selected the same mode, logged explicitly:

```text
mode selected index=13 name=3840x2160 3840x2160 clock=296703 refresh=23.976
  (requested 23.976, delta -0.000) flags=0x100005
```

The exact 23.976 Hz mode exists in the connector's mode list (clock 296703 vs
297000 for the 24.000 Hz variant), so no fallback to 24.000 was needed and none
is being reported. VOP2 confirms it drove that clock:

```text
Update mode to 3840x2160p24, type: 0(if:HDMI1, flag:0x0) for vp0 dclk: 296703000
set dclk_vop0 to 296703000, get 296703000
final tmdsclk = 296703000
```

## 7. A0 result — SDR baseline

**`PASS`.**

| Item | Value |
| --- | --- |
| Asset | `sdr-4k-2398-main.mp4`, HEVC Main 8-bit, BT.709 |
| Decoder output | `drm_prime`, layer format `NV12`, modifier `0x0`, 2 planes |
| Framebuffer | `NV12` linear, pitch 3840, plane 1 offset 8294400, 3840x2160 |
| Plane | 73 (`Cursor`), `TEST_ONLY` accepted |
| Run | 700 frames decoded, 699 page flips, 0 decode errors, 0 commit errors |
| Imported framebuffers | 11 |
| `bus_format` | `2025` `YUV8_1X24` |
| VP0 | `overlay_mode[1] output_mode[f] SDR[0] color-encoding[BT.709]` |
| Requested vs actual | `Colorspace` Default/Default, `color_depth` Automatic/Automatic, `HDR_OUTPUT_METADATA` 0/0 |
| TV | SDR profile (`xtendedDynamicRange=medium`, `lightSensor` available) |

699 flips in a 30-second hold is 23.3 per second, which is the 23.976 Hz vblank
cadence less the decode start-up. The control rung establishes that 4K23.976
modeset, hardware decode, DRM PRIME import and atomic page-flipping on the
cursor-typed plane all work before any HDR variable is introduced.

## 8. A1 result — 10-bit frames

**`PASS`.**

Only the frame format changed.

| Item | Value |
| --- | --- |
| Asset | `hdr10-4k-2398-main10.mp4`, HEVC Main 10 |
| Decoder output | `drm_prime`, layer format **`NV15`**, modifier `0x0`, 2 planes |
| Framebuffer | `NV15` linear, **pitch 4864**, plane 1 offset 10506240, 3840x2160 |
| Run | 718 frames decoded, 717 page flips, 0 decode errors, 0 commit errors |
| `bus_format` | `2025` `YUV8_1X24` — unchanged |
| VP0 | `SDR[0] color-encoding[BT.709]` — unchanged |

The pitch is the tell: 4864 bytes for 3840 pixels is 10 bits per sample packed,
so the 10-bit data reaches the plane intact with no conversion anywhere. VOP2
scans `NV15` out of the cursor-typed plane without complaint.

Equally important is what did **not** change. The HDMI link stayed 8-bit 4:4:4
and the output stayed SDR. Feeding the pipeline 10-bit content is not by itself
enough to get 10 bits onto the wire; that requires an explicit request, which
is rung a3. This is the behaviour that would silently degrade a player that
assumes 10-bit content implies 10-bit output.

## 9. A2 result — BT.2020

**`PASS`.**

Only `Colorspace` changed, `Default` → `BT2020_YCC`.

| Item | Value |
| --- | --- |
| Run | 708 frames decoded, 707 page flips, 0 errors |
| Readback | `Colorspace` requested `BT2020_YCC`, actual `BT2020_YCC` (10) |
| `bus_format` | `2025` `YUV8_1X24` — unchanged |
| VP0 | `SDR[0]` **`color-encoding[BT.2020]`** — changed from BT.709 |
| TV | still SDR profile |

`BT2020_YCC` rather than `BT2020_RGB` because the sink cannot accept RGB at 4K
with 10 bits: Gate MP0's per-mode capability blob shows 3840x2160p23.976
permitting RGB and YCbCr 4:4:4 only at 8-bit, with 10-bit available solely over
YCbCr 4:2:2. Asking for `BT2020_RGB` would have forced the driver to choose
between honouring the colorimetry request and honouring the depth request one
rung later. `BT2020_YCC` is consistent with both, and with the stream's own
`bt2020nc` matrix.

The VP-level colour encoding follows the request immediately, while the link
stays 8-bit. Colorimetry signalling and bit depth are independent knobs on this
driver.

## 10. A3 result — 30-bit output

**`PASS`, and this is where the link becomes 10-bit.**

Only `color_depth` changed, `Automatic` → `30bit`.

| Item | Value |
| --- | --- |
| Run | 717 frames decoded, 716 page flips, 0 errors |
| Readback | `color_depth` requested `30bit`, actual **`30bit` (10)** |
| `bus_format` | `2025` `YUV8_1X24` → **`200d` `YUYV10_1X20`** |
| `output_mode` | `f` → `9` |
| VP0 | `SDR[0] color-encoding[BT.2020]` |
| TV | still SDR profile |

`MEDIA_BUS_FMT_YUYV10_1X20` is **YCbCr 4:2:2 at 10 bits per component**. The
probe never requested a subsampling — it left the vendor `color_format`
property untouched at its default — and the driver worked out on its own that
4:4:4 10-bit at 296.703 MHz would need 371 MHz of TMDS bandwidth against this
sink's 300 MHz ceiling, and dropped to 4:2:2, which carries 10 and 12 bits at
the pixel clock rate.

That is exactly the path Gate MP0 predicted from the EDID and from the vendor
`MODE_COLOR_CAPACITY` blob, whose bit-meaning was inferred there rather than
read from source. **This rung is independent confirmation of that inference.**

Note also what `color_format` reads back as: `ycbcr444`. That property reflects
a user *request* (never set here, so still at its default), not the negotiated
output. The negotiated output is only visible in `bus_format`. Any monitoring
or product code that reads `color_format` expecting to learn what is on the
wire will be wrong; `/sys/kernel/debug/dri/0/summary` is the source of truth.

## 11. A4 result — HDR10 signalling

**`PASS`. The TV enters HDR.**

Only `HDR_OUTPUT_METADATA` changed, unset → a blob built from the asset.

| Item | Value |
| --- | --- |
| Run | 719 frames decoded, 718 page flips, 0 decode errors, 0 commit errors |
| Blob | id 262, 32 bytes, accepted by `drmModeCreatePropertyBlob` |
| Readback | requested blob 262, actual blob **262** |
| `bus_format` | `200d` `YUYV10_1X20` — unchanged from a3 |
| `overlay_mode` | `1` → `0` |
| VP0 | `SDR[0]` → **`HDR10[2]`** |
| TV | **HDR profile** — see section 15 |

The metadata the probe built, logged field by field and as raw bytes:

```text
hdr_metadata blob_size=32 metadata_type=0 eotf=2 infoframe_metadata_type=0
hdr_metadata display_primaries(ST2086 order G,B,R)=[(8500,39850) (6550,2300) (35400,14600)]
             white_point=(15635,16450)
hdr_metadata max_display_mastering_luminance=1000 (cd/m2)
             min_display_mastering_luminance=50 (0.0001 cd/m2)
             max_cll=1000 max_fall=400
hdr_metadata raw=00 00 00 00 02 00 34 21 aa 9b 96 19 fc 08 48 8a 08 39 13 3d
                 42 40 e8 03 32 00 e8 03 90 01 00 00
```

Decoding the raw bytes against `struct hdr_output_metadata` from the kernel
UAPI header, which is what the probe fills in rather than a hand-rolled layout:

| Offset | Field | Bytes | Value | Source |
| --- | --- | --- | --- | --- |
| 0 | `metadata_type` (u32) | `00 00 00 00` | 0 = Static Metadata Type 1 | fixed |
| 4 | `eotf` | `02` | 2 = SMPTE ST2084 | stream `color_trc` |
| 5 | `metadata_type` | `00` | 0 | fixed |
| 6 | `display_primaries[0]` | `34 21 aa 9b` | (8500, 39850) = green | asset |
| 10 | `display_primaries[1]` | `96 19 fc 08` | (6550, 2300) = blue | asset |
| 14 | `display_primaries[2]` | `48 8a 08 39` | (35400, 14600) = red | asset |
| 18 | `white_point` | `13 3d 42 40` | (15635, 16450) = D65 | asset |
| 22 | `max_display_mastering_luminance` | `e8 03` | 1000 cd/m² | asset |
| 24 | `min_display_mastering_luminance` | `32 00` | 50 × 0.0001 = 0.005 cd/m² | asset |
| 26 | `max_cll` | `e8 03` | 1000 cd/m² | asset |
| 28 | `max_fall` | `90 01` | 400 cd/m² | asset |

Every value traces to the stream. The primaries are written in ST 2086 order
(green, blue, red), converted from FFmpeg's normalised red-green-blue order;
this ordering choice is called out again in section 17 as the one part of the
mapping that is a reading of the specification rather than a measurement.

## 12. Requested versus actual DRM state

Read back from the kernel after each commit, not assumed from what was sent.

| Rung | `Colorspace` req / actual | `color_depth` req / actual | `HDR_OUTPUT_METADATA` req / actual | `color_format` actual |
| --- | --- | --- | --- | --- |
| a0 | `Default` / `Default` (0) | `Automatic` / `Automatic` (0) | 0 / 0 | `ycbcr444` (1) |
| a1 | `Default` / `Default` (0) | `Automatic` / `Automatic` (0) | 0 / 0 | `ycbcr444` (1) |
| a2 | `BT2020_YCC` / `BT2020_YCC` (10) | `Automatic` / `Automatic` (0) | 0 / 0 | `ycbcr444` (1) |
| a3 | `BT2020_YCC` / `BT2020_YCC` (10) | `30bit` / `30bit` (10) | 0 / 0 | `ycbcr444` (1) |
| a4 | `BT2020_YCC` / `BT2020_YCC` (10) | `30bit` / `30bit` (10) | 262 / 262 | `ycbcr444` (1) |

Every request was honoured exactly. There is no case in this gate of the driver
silently substituting a different value.

`color_format` is the one field where "actual" is misleading, and it is
included precisely to record that: it stayed at `ycbcr444` on every rung
including a3 and a4, while the link was actually running 4:2:2. It is a request
channel, not a status channel.

## 13. VOP2 / debugfs differential

`/sys/kernel/debug/dri/0/summary`, video-port line, sampled at the end of each
rung's hold window:

```text
a0   bus_format[2025]: YUV8_1X24    overlay_mode[1] output_mode[f] SDR[0]    color-encoding[BT.709]
a1   bus_format[2025]: YUV8_1X24    overlay_mode[1] output_mode[f] SDR[0]    color-encoding[BT.709]
a2   bus_format[2025]: YUV8_1X24    overlay_mode[1] output_mode[f] SDR[0]    color-encoding[BT.2020]
a3   bus_format[200d]: YUYV10_1X20  overlay_mode[1] output_mode[9] SDR[0]    color-encoding[BT.2020]
a4   bus_format[200d]: YUYV10_1X20  overlay_mode[0] output_mode[9] HDR10[2]  color-encoding[BT.2020]
```

Each rung moves one thing:

- **a1 → a2**: `color-encoding` BT.709 → BT.2020. Colorimetry only.
- **a2 → a3**: `bus_format` 8-bit 4:4:4 → 10-bit 4:2:2, `output_mode` `f` → `9`.
  Depth and subsampling only.
- **a3 → a4**: `SDR[0]` → `HDR10[2]`, `overlay_mode` `1` → `0`. HDR state only;
  the link format is byte-identical to a3.

That a3 and a4 carry the *same* `bus_format` is what makes a4 a clean isolation
of HDR signalling: the electrical configuration of the link is unchanged, and
the only difference is the Dynamic Range and Mastering InfoFrame.

The per-window line adds one observation worth recording: the plane itself
stays tagged SDR even while the video port reports `HDR10[2]`.

```text
Esmart0-win0: ACTIVE  format: NV15  color: SDR[0] color-encoding[BT.601] color-range[Limited]
```

The probe never sets the plane's `COLOR_ENCODING` or `COLOR_RANGE` properties,
so they sit at their defaults. This did not prevent HDR output, but it is a
loose end; see section 17.

### Reversibility

The gate did not only observe the transition, it reversed it. A separate
confirmation run of a3 followed by a4, with everything else identical,
reproduced the toggle from the kernel side:

```text
a3  bus_format[200d]: YUYV10_1X20  overlay_mode[1] output_mode[9] SDR[0]    color-encoding[BT.2020]
a4  bus_format[200d]: YUYV10_1X20  overlay_mode[0] output_mode[9] HDR10[2]  color-encoding[BT.2020]
```

and the `--reset` action at the end of the canonical run returned the link to
its neutral state:

```text
reset connector 201 to Colorspace=Default color_depth=Automatic HDR_OUTPUT_METADATA=unset
bus_format[2025]: YUV8_1X24  overlay_mode[1] output_mode[f] SDR[0] color-encoding[BT.709]
```

## 14. Kernel log differential

A `dmesg` snapshot was taken immediately before and after every rung and the
delta filtered for `drm|vop|hdmi|dw-hdmi|dw_hdmi|phy|hdr|colorspace|bpc|color_depth|infoframe|edid`.

| Rung | Delta lines | Lines matching `hdr\|infoframe\|colorspace\|bpc\|error\|fail\|warn` |
| --- | --- | --- |
| a0 | 33 | 0 |
| a1 | 33 | 0 |
| a2 | 33 | 0 |
| a3 | 30 | 0 |
| a4 | 33 | 0 |

**No errors, no warnings, on any rung.** Every delta is the same benign
modeset-down / modeset-up sequence:

```text
rockchip-vop2 fdd90000.vop: [drm] vop disable intf:1000
rockchip-vop2 fdd90000.vop: [drm:vop2_crtc_atomic_disable] Crtc atomic disable vp0
rockchip-vop2 fdd90000.vop: [drm:vop2_crtc_atomic_enable] Update mode to 3840x2160p24, ... dclk: 296703000
rockchip-hdptx-phy-hdmi fed70000.hdmiphy: hdptx phy pll locked!
rockchip-vop2 fdd90000.vop: [drm:vop2_crtc_atomic_enable] set dclk_vop0 to 296703000, get 296703000
dwhdmi-rockchip fdea0000.hdmi: final tmdsclk = 296703000
dwhdmi-rockchip fdea0000.hdmi: don't use dsc mode
dwhdmi-rockchip fdea0000.hdmi: dw hdmi qp use tmds mode
rockchip-hdptx-phy-hdmi fed70000.hdmiphy: hdptx phy lane locked!
rockchip-vop2 fdd90000.vop: [drm] vop enable intf:1000
```

The a3 and a4 deltas are **identical once timestamps are stripped**. The vendor
driver emits nothing at all about HDR, colorimetry, bit depth or InfoFrames.
This is a finding in its own right: for this BSP, the kernel log is useless as
an HDR diagnostic, and `/sys/kernel/debug/dri/0/summary` is the only in-kernel
observability point. Any later monitoring must be built on the latter.

## 15. TV physical HDR result

**TV HDR10 trigger: `YES`.** Verified two ways, one of them machine-readable.

### 15.1 Sink-side telemetry

The TV is a Sony BRAVIA with the built-in IP-control REST API enabled. The gate
queries it read-only, halfway through each rung's hold window, through
`scripts/tv-state.sh`. Only getters are called; nothing on the TV was changed.

This generation has no documented "is the incoming signal HDR" getter, so the
evidence is taken as a differential instead: BRAVIA keeps a separate set of
picture values per input and per SDR/HDR profile, so the values and
availability flags reported under the same `pictureMode` differ between an SDR
and an HDR signal. Every field that changed across the run:

| TV setting | baseline | a0 | a1 | a2 | a3 | **a4** | after reset |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `xtendedDynamicRange` | medium | medium | medium | medium | medium | **off** | medium |
| `lightSensor` | available | available | available | available | available | **unavailable** | available |
| `brightness` | 30 | 30 | 30 | 30 | 30 | **40** | 30 |

`xtendedDynamicRange` is Sony's dynamic-range expansion for SDR content; the TV
turns it off for an HDR signal because HDR content is not expanded. `brightness`
reads the HDR profile's stored value. `lightSensor` becomes unavailable.

All three flip on a4 and only on a4, and all three revert once the connector is
reset. `pictureMode` itself stayed `graphics` throughout, `hdrMode` stayed
`auto` and `colorSpace` stayed `auto`; those are user settings, not detected
state, and their stability is what makes the three changed fields meaningful
rather than noise.

### 15.2 Human observation

During the ladder the operator, watching the TV, reported unprompted:

> "4k görüntü var ama hdr değil"

while the earlier rungs were on screen, and then:

> "şimdi hdr geldi"

as the run reached its final rung. This is an independent physical
confirmation, and it also confirms the TV accepted and displayed the
3840x2160p23.976 mode.

### 15.3 What is not claimed

The operator was not asked to, and did not, confirm the separate a3 → a4
confirmation toggle described in section 13. That toggle is reported on
kernel-side and sink-telemetry evidence only. Attribution of the HDR transition
to rung a4 specifically rests on the debugfs state, the TV telemetry
differential and the run ordering, which agree with each other; it does not
rest on a second human observation.

## 16. Failure localization

None required. The ladder ran to completion.

For the record, against the four scenarios the gate was designed to
distinguish:

| Scenario | Predicted symptom | Observed |
| --- | --- | --- |
| 1 — signalling path works | metadata set, debugfs HDR active, TV HDR | **this is what happened** |
| 2 — `dw-hdmi-qp` / InfoFrame / PHY / sink fault | metadata set, debugfs HDR active, TV SDR | not observed |
| 3 — DRM/VOP2 property handling fault | metadata set, debugfs still SDR | not observed |
| 4 — NV15 commit fails | atomic commit rejected | not observed; `TEST_ONLY` accepted on every rung |

The fault domain for HDR10 on this platform is therefore **empty at the kernel
and driver level**. Everything from `HDR_OUTPUT_METADATA` down through
`dw-hdmi-qp`, the Samsung HDPTX PHY and the sink works. Gate MP0's hypothesis —
that the first missing layer was user space, not the kernel — is confirmed.

What remains between here and a product is entirely above this line: a player
that (a) does not reject the cursor-typed plane, (b) knows to set the vendor
`color_depth` property because `max bpc` does not exist, and (c) constructs
`HDR_OUTPUT_METADATA` from stream metadata.

## 17. Remaining uncertainties

1. **Mastering primaries ordering.** The probe writes `display_primaries` in
   ST 2086 order (green, blue, red), converting from FFmpeg's normalised red,
   green, blue. This follows CTA-861.3, which carries the ST 2086 payload
   unchanged, but it was read from the specification rather than measured: HDR
   mode entry depends on `eotf`, not on the primaries, so a permutation here
   would not have shown up as a failure in this gate. Confirming it needs a
   sink or analyser that reports the received InfoFrame contents.

2. **The plane stays tagged SDR.** `Esmart0-win0` reports `color: SDR[0]
   color-encoding[BT.601]` while the video port reports `HDR10[2]`. The probe
   leaves the plane's `COLOR_ENCODING` and `COLOR_RANGE` properties at their
   defaults. Output was correct regardless, but whether VOP2 is passing the PQ
   data through untouched or applying some per-window conversion has not been
   established, and picture *fidelity* was not assessed — only HDR mode entry.

3. **Synthetic content.** The asset is a generated test pattern, not graded
   film. It proves the signalling path; it says nothing about tone mapping
   quality, banding, or how real 23.976 content looks.

4. **Sticky connector properties.** `Colorspace`, `color_depth` and
   `HDR_OUTPUT_METADATA` persist after a DRM master exits — the kernel fbdev
   client restores its own mode but never touches them. A run restores what it
   found, which across a ladder means later rungs inherit earlier rungs' values;
   `--reset` exists to return the link to neutral. A product will need an
   explicit policy here rather than relying on anything to clean up.

5. **Single sink.** Everything is measured against one HDMI 1.4 TV. The a3
   result (automatic fallback to 4:2:2 10-bit) is a consequence of that sink's
   300 MHz ceiling. On an HDMI 2.0/2.1 sink the driver would have other choices
   and its behaviour is untested.

6. **`autoPictureMode` moved once.** In an earlier run of the ladder the TV's
   `autoPictureMode` changed `off` → `auto` at rung a2 and stayed there. It did
   not recur in the canonical run and does not correlate with the HDR
   transition, but it is recorded as an uncontrolled change on the sink.

## 18. Recommended next Gate

**MP1b — real HDR10 content and frame-rate matching, still without Kodi.**

Rationale: MP1a proved the signalling path with a synthetic 2-second clip
looped at a single rate. Two things are now worth knowing, and both are cheap
because the instrument already exists:

1. whether a **real graded HDR10 file** looks correct, not merely whether the
   TV reports HDR — this is where uncertainty 2 above (per-window SDR tagging,
   possible tone mapping) would show itself; and
2. whether **mode switching between 23.976 / 24 / 25 / 50 / 59.94 / 60** works
   cleanly, since the product requirement is that 23.976 content plays at
   23.976 rather than being pulled to 60.

If only one experiment is run, it should be the first: **play a real
3840x2160p23.976 HDR10 file through `--step a4` and assess the picture**, with
`4K60 + HDR` deliberately still excluded, because this sink cannot carry 10-bit
at 4K60 and testing it would only re-measure the sink's limit.

That requires a real HDR10 asset, which neither machine currently has, so the
prerequisite is obtaining one.

Not authorised by this gate and not started: Kodi, GBM/Mesa/`libmali` user
space, Stremio, audio passthrough, CEC, HDR10+, Dolby Vision, and any kernel,
device-tree or boot configuration change.

## 19. Raw evidence paths

All under
[`logs/orangepi5-ultra-vendor/hdr-signaling-mp1a-2026-09-09/`](../../logs/orangepi5-ultra-vendor/hdr-signaling-mp1a-2026-09-09/):

| File | Contents |
| --- | --- |
| `00-probe.txt` | `--probe` inventory; no modeset performed |
| `01-summary-baseline.txt` | `/sys/kernel/debug/dri/0/summary` before the ladder |
| `01-dmesg-baseline.txt` | kernel log before the ladder |
| `01-tv-state-baseline.json` | sink state before the ladder |
| `a{0..4}-run.txt` | full per-rung log: asset metadata, mode selection, plane search, HDR blob, debugfs before/after/during, requested-vs-actual readback, run counters |
| `a{0..4}-tv-state.json` | sink state sampled mid-hold for that rung |
| `a{0..4}-dmesg-before.txt`, `-after.txt` | kernel log around each rung |
| `a{0..4}-dmesg-delta.txt` | the delta |
| `a{0..4}-dmesg-delta-filtered.txt` | delta filtered to display/HDMI/HDR terms |
| `98-reset.txt` | `--reset` run returning the connector to neutral |
| `99-summary-after.txt` | `summary` after the ladder |
| `99-connector-state.txt` | connector `status` and `enabled` after the ladder |
| `99-tv-state-after.json` | sink state after the reset |
| `ladder-summary.txt` | one line per rung: step, result, exit code |
| `confirm-a3-run.txt`, `confirm-a4-run.txt` | the separate a3 → a4 toggle confirmation |
| `98b-reset-after-confirm.txt` | `--reset` after the confirmation runs |
| `SHA256SUMS` | checksums for every file above |

Tooling that produced them, in this repository:
[`tools/hdr-signaling-probe.cpp`](../../tools/hdr-signaling-probe.cpp),
[`scripts/run-mp1a.sh`](../../scripts/run-mp1a.sh),
[`scripts/tv-state.sh`](../../scripts/tv-state.sh),
[`scripts/make-test-assets.sh`](../../scripts/make-test-assets.sh).

The generated assets are not committed — `.gitignore` excludes `assets/` — and
are reproducible from `scripts/make-test-assets.sh`; their checksums are in
section 5.

## 20. Checksums

`SHA256SUMS` in the evidence directory covers every raw output file.

Key artefacts:

| Artefact | SHA-256 |
| --- | --- |
| `tools/hdr-signaling-probe.cpp` | `c6e275e7c857ab418d69cf406b9c2e31a2722dd84e893b1d7e1b7a55e0f9ae90` |
| `assets/hdr10-4k-2398-main10.mp4` | `2c105ada1080c817ebc3cbb5fdf29d414290602af1c02ec1749f511d33929e18` |
| `assets/sdr-4k-2398-main.mp4` | `dddcf04a200932894d662a6c091cecc9c34971e4e085b87ebd7cca116de84a1c` |

## 21. What was and was not changed

**Target.** Nothing installed, nothing upgraded. No kernel, DTB, bootloader,
boot configuration, kernel command line, partition table or systemd unit was
touched. The board was not rebooted. Everything written to it lives under
`/tmp/rk3588-mediabox/` (source, CMake build tree, the two test assets).

The console framebuffer was taken over for the duration of each rung, as this
gate authorises, and given back on exit — by the normal exit path, by the
`SIGINT`/`SIGTERM`/`SIGHUP` handler, and by a watchdog `alarm()` sized to the
hold window. The watchdog was added after an early run wedged while holding DRM
master and had to be terminated by hand; that failure and its recovery are part
of this gate's history, and after it the tool cannot leave the console dark.
Connector colour properties are restored as found on exit, and `--reset`
returns them to neutral.

**Sink.** Only getters were called on the TV's IP-control API
(`getInterfaceInformation`, `getPowerStatus`, `getCurrentExternalInputsStatus`,
`getPictureQualitySettings`, `getMethodTypes`). No `set*` method was ever
invoked. The TV's own picture settings changed only as the TV itself switches
profiles between SDR and HDR signals.

**ScreenBridge repository.** Read only. `HEAD`
`582e1d30c6762c134118445bf660c0784aaffe58` and a clean working tree were
recorded at the start of this gate and verified again at the end.
