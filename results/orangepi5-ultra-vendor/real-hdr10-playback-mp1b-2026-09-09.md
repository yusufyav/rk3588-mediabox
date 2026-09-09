# Gate MP1b: real HDR10 content and picture fidelity on Orange Pi 5 Ultra

Date: 2026-09-09
Target: `root@10.27.27.25` (`orangepi5-ultra`, RK3588 OPi 5 Ultra)
Sink: Sony BRAVIA `KD-65XE9005` at `10.27.27.51`
Reference source: Ugoos SK1 + Kodi, same TV, HDMI 3

## 1. Executive summary

**GATE MP1b RESULT: `PARTIAL` — specifically `PARTIAL_FIDELITY`.**

Everything the pipeline can be measured on passed, and one thing that can only
be judged by eye did not fully pass.

A real 1h45m 4K23.976 HEVC Main 10 HDR10 film — *Past Lives* (2023), 3840x2080,
BT.2020/PQ, mastered at 4000 nits — was decoded by RKMPP into `NV15` DRM PRIME
buffers and scanned out through the exact Gate MP1a A4 output state, unchanged.
Over 120 seconds of continuous playback the probe presented **2852 frames with
0 dropped, 0 repeated and 0 late**, at 23.9763 fps against a target of 23.9760,
with a p99 frame interval of 41.797 ms against the ideal 41.708 ms. Zero decode
errors, zero atomic commit errors, zero kernel errors. The result was
reproduced exactly on a second run.

The sink entered HDR10, and this is now confirmed against a commercial
reference rather than only against itself: **the Sony reports the identical HDR
picture profile for this board as it does for a Ugoos SK1 running Kodi** on the
same TV, same picture mode, same film, same scene.

What did not pass is the picture itself. The operator, watching the panel,
reported that HDR engaged but that "renklerin süper olduğunu söyleyemem" — the
colours are not right. Moving the TV out of its `graphics` picture mode into
`cinemaHome` made it "biraz daha iyi ama hâlâ tam değil": better, still not
right. That residual gap is real, is not explained by cadence, decode, mode,
metadata or HDR entry, and has one strong candidate cause identified below.

| Dimension | Result |
| --- | --- |
| Real HDR10 asset verified | `PASS` |
| HEVC Main 10 hardware decode, no software fallback | `PASS` |
| DRM PRIME / `NV15` / 10-bit preserved | `PASS` |
| BT.2020 + PQ preserved | `PASS` |
| Asset metadata → `HDR_OUTPUT_METADATA` | `PASS` |
| 3840x2160 @ 23.976 mode, no 60 Hz fallback | `PASS` |
| YCbCr 4:2:2 10-bit HDMI | `PASS` |
| TV HDR10 trigger | `PASS` |
| Continuous playback, 0 errors | `PASS` |
| Frame cadence (from local storage) | `PASS` — 0/0/0 |
| Frame cadence (from USB 2.0 storage) | `FAIL` — see §15 |
| Clean restore to SDR | `PASS` |
| **Visible colour fidelity** | **not clean — see §17** |

### The single most important finding

VOP2 tags the video window **`SDR[0] color-encoding[BT.601]` with `csc: y2r[1]`
enabled**, while the video port above it runs `HDR10[2]
color-encoding[BT.2020]`. The window is doing a YUV→RGB conversion, and it is
labelled with the wrong matrix for the content. BT.601 coefficients applied to
BT.2020 primaries produce exactly the class of error the operator described:
colour that is not obviously broken, but not right either. Gate MP1a recorded
this tag as uncertainty 2 and could not test it on a synthetic pattern. On real
graded film it is now the leading explanation, and testing it is a single
one-property experiment (§22).

Nothing was installed on the target. No kernel, DTB, bootloader, boot
configuration or service was changed. The board was not rebooted. Nothing on
the TV was changed by this gate; only getters were called. The operator changed
the TV picture mode themselves, once, deliberately, and that change is recorded
as such.

## 2. Repository revisions

| Item | Value |
| --- | --- |
| Repository | `yusufyav/rk3588-mediabox`, branch `main` |
| Parent commit | `1670406` (`docs: add MP1a HDR signaling results`) |
| New tool | `tools/hdr-playback-probe.cpp` |
| New modules | `src/common`, `src/drm`, `src/decode`, `src/media` |
| New runner | `scripts/run-mp1b.sh` |
| Build | CMake, C++17, built on the target |
| Reference repository | `yusufyav/rk3588-screenbridge` at `582e1d30c6762c134118445bf660c0784aaffe58`, read-only |

Source checksums:

| File | SHA-256 |
| --- | --- |
| `tools/hdr-playback-probe.cpp` | `05f7bda16288fe87722d1ffe4ef53eb0f4362df9daaf6609e4b6522190033470` |
| `src/drm/display.cpp` | `492d5e98e9bde503ef604f7f5b13c9e8cdac033b1ddbe8fc7a49079140a368a6` |
| `src/decode/decoder.cpp` | `ed8bd53176dc9510f03c755a8cb516d059bcb04b410d7bb0dcdda4085568d857` |
| `src/decode/framebuffer.cpp` | `a8c6c955f364cee9b6f2228647fec30a08da3b6cbcdee70842e6f14c6a5cfb25` |
| `src/media/hdr_metadata.cpp` | `7abcac83facca0798876bc48bdd6b3fc79af58d311fd280d74286a06a5a3cffe` |
| `src/media/cadence.cpp` | `d10f8309b305c8bb48ed343877eeef1522e52c5f98da416784947d85ec3da47c` |
| `src/common/log.cpp` | `e603c85f6f6b803018cd0b67a6a7021ed227141343dd6a57f6ef85a06186209b` |
| `scripts/run-mp1b.sh` | `0a99cc80a7074de208f18adf837dea3cfc9ddf20c1eb4e9e70e996c0f61a9256` |

### 2.1 Why Gate MP1a's probe was not refactored

`tools/hdr-signaling-probe.cpp` is untouched. Its SHA-256 is published in the
MP1a report and re-running that gate has to stay byte-for-byte reproducible, so
the DRM discovery, atomic-commit, DRM PRIME import and HDR metadata mapping
were **derived** into `src/` rather than shared with it. The cost is real
duplication between the MP1a probe and `src/`, and it is accepted deliberately:
a later gate that no longer needs MP1a reproducible can collapse the two.

Screenbridge was read but never modified. Its `HEAD` and clean working tree
were verified before and after this gate.

## 3. Target identity

| Item | Value |
| --- | --- |
| Model | `RK3588 OPi 5 Ultra`, `rockchip,rk3588-orangepi-5-ultra` |
| Kernel | `6.1.115-vendor-rk35xx-screenbridge-hdmirx-audio` |
| DRM | `rockchip`, 1 CRTC (`video_port0`, id 89), `HDMI-A-1` connector id 201 |
| Scanout plane | id 73, `Esmart0-win0`, DRM plane type **`Cursor`** |
| Decoder user space | `/opt/rk3588-screenbridge/bin` FFmpeg, `--enable-rkmpp` |
| Sink | Sony BRAVIA `KD-65XE9005`, HDMI 1.4 / 300 MHz TMDS ceiling |
| Reference player | Ugoos SK1 on the same TV, HDMI 3, seen over CEC as `SK1` |

## 4. MP1a baseline reused, not re-derived

These are inputs to this gate, taken as proven by Gate MP1a and held fixed:

| Held fixed | Value |
| --- | --- |
| frame format | `NV15`, DRM PRIME, RKMPP hardware decode |
| `Colorspace` | `BT2020_YCC` |
| `color_depth` | `30bit` (there is no standard `max bpc` property on this BSP) |
| `HDR_OUTPUT_METADATA` | set, built from the asset |
| plane selection | by atomic `TEST_ONLY`, never by plane type |

The single new variable is the content: synthetic test pattern → real HDR10
film. Nothing about the connector state, VOP2 configuration, bit depth,
colorimetry or HDR policy was varied in this gate.

## 5. Real HDR10 asset identity

Two candidate films were found on a USB disk the operator attached mid-gate.
Only one of them is usable, and why the other is not is a finding in itself.

### 5.1 Selected asset

```text
/mnt/usb-multiboot/Özel_yedek/Film/Past.Lives.2023.2160p.4K.WEB.x265.10bit.AAC5.1-[YTS.MX]/
  past.lives.2023.hdr.2160p.web.h265-huzzah.mkv
```

| Item | Value |
| --- | --- |
| Size | 12 081 238 172 bytes |
| **SHA-256** | **`f4e32b8d3feb7efdebaaefaf7338a25bcc86ee144bf82bd4b93da6b80906f96a`** |
| Duration | 6343.52 s (1h 45m 44s) |
| Bit rate | 15.24 Mbit/s |

### 5.2 Rejected asset, and why

```text
Decision.to.Leave.2022.KOREAN.2160p.WEB-DL.…DV.MKV.x265-DVSUX.mkv   27 383 730 770 bytes
```

`ffprobe` reports a Dolby Vision configuration record with **`dv_profile=5`**,
`bl_present_flag=1`, `rpu_present_flag=1` and — decisively —
**`dv_bl_signal_compatibility_id=0`**, with `color_space`, `color_transfer` and
`color_primaries` all `unknown` and `color_range=pc`.

Profile 5 carries an IPT-PQ-C2 base layer that is **not** an HDR10 base layer:
compatibility id 0 means "no cross-compatibility". Displaying it without Dolby
Vision RPU processing produces the well-known wrong-colour result. The gate's
own requirement — "HDR10 base layer bulunmalı" — is not met, so the file was
rejected rather than played. This is a useful product-level finding: a
significant share of DV releases in the wild are Profile 5 and will need
detection and refusal, not best-effort playback.

## 6. Asset provenance

The asset was supplied by the operator on a Western Digital Elements 1 TB USB
disk (`1058:25a2`), NTFS partition labelled `MultiBoot`, mounted **read-only**
by this gate at `/mnt/usb-multiboot` and unmounted cleanly afterwards. Nothing
was written to it.

The disk enumerated on `ehci-platform` — that is, at **USB 2.0 high speed**,
not USB 3.0 — which turns out to matter (§15).

No asset was downloaded. Before the operator supplied these files, a search of
both machines found no media at all, and an investigation of openly licensed
alternatives was under way; it is recorded here only because it produced one
reusable fact: the Jellyfin project's CC-BY-SA 4K HDR10 HEVC test clips are
**60 fps**, so they cannot serve a 23.976 cadence gate on an HDMI 1.4 sink.

## 7. Asset SHA-256

```text
f4e32b8d3feb7efdebaaefaf7338a25bcc86ee144bf82bd4b93da6b80906f96a  past.lives.2023.hdr.2160p.web.h265-huzzah.mkv
```

Computed over the USB source in a single streaming pass that simultaneously
wrote the eMMC working copy, then recomputed independently over that copy:

```text
f4e32b8d…f96a  /var/tmp/mp1b/past-lives.mkv        (eMMC copy)
```

The two agree, so the local copy used for the clean cadence runs is
bit-identical to the operator's file.

## 8. ffprobe metadata

Read on the target with the RKMPP-enabled build at
`/opt/rk3588-screenbridge/bin/ffprobe`.

| Field | Value |
| --- | --- |
| container | `matroska,webm` |
| codec / profile / level | `hevc` / `Main 10` / `150` (5.0) |
| width x height | **3840 x 2080** |
| pixel format | `yuv420p10le` |
| bit depth | 10 |
| chroma | 4:2:0, `chroma_location=left` |
| frame rate | `24000/1001` = **23.976024 fps** |
| time base | `1/1000` (Matroska millisecond timestamps) |
| `color_range` | `tv` (limited) |
| `color_primaries` | `bt2020` |
| `color_transfer` | **`smpte2084`** (PQ) |
| `color_space` | `bt2020nc` |
| sample aspect ratio | `481:480` |
| Mastering display primaries | R (0.6800, 0.3200) G (0.2650, 0.6900) B (0.1500, 0.0600) — **DCI-P3** |
| Mastering display white point | (0.31270, 0.32900) — D65 |
| Mastering display luminance | min **0.005** cd/m², max **4000** cd/m² |
| MaxCLL / MaxFALL | **401 / 77** |
| HDR10+ (ST 2094-40) | **present** on frames |
| Dolby Vision | absent |
| audio / subtitles | `eac3` 5.1, three `ass` tracks — irrelevant to this gate |

Two things here are worth carrying forward. The frame height is **2080, not
2160** — a 1.85:1 film with the framing baked in — which is handled below
without any scaler. And the file carries **HDR10+ dynamic metadata**; this gate
outputs HDR10 static metadata only, by scope, and the presence is reported
rather than acted on.

## 9. Decode path

```text
Matroska  →  demux  →  HEVC Main 10  →  hevc_rkmpp (RKMPP hardware)
          →  AV_PIX_FMT_DRM_PRIME  →  dma-buf  →  drmPrimeFDToHandle
          →  drmModeAddFB2WithModifiers  →  atomic KMS  →  VOP2  →  dw-hdmi-qp  →  HDPTX PHY  →  Sony
```

No software decode, no `libswscale`, no CPU colour conversion, no copy. The
probe does not merely avoid these — it **refuses to continue** if they would be
needed:

- a decoder that returns anything other than `AV_PIX_FMT_DRM_PRIME` →
  `BLOCKED_DECODE`, explicitly "refusing any software decode or CPU conversion";
- a DRM PRIME layer format other than `NV15` → `BLOCKED_DECODE`, because "NV12
  would mean the 10-bit samples were narrowed to 8-bit";
- a transfer characteristic other than SMPTE ST2084 → `BLOCKED_ASSET`;
- a bit depth other than 10 → `BLOCKED_ASSET`;
- a mid-run change of either → stop, "rather than converting".

Observed, every run:

```text
decoder drm_prime layer_format=NV15 modifier=0x0 objects=1 planes=2 frame=3840x2080
framebuffer format=NV15 modifier=0x0 planes=2 pitch0=4864 offset1=10117120 size=3840x2080
```

## 10. DRM PRIME / NV15 evidence

`pitch0 = 4864` for 3840 pixels is the tell: 4864 × 8 / 3840 = 10.13 bits per
sample, i.e. 10-bit samples packed, not padded to 16. The 10-bit data reaches
the plane intact. VOP2 confirms what it is scanning out:

```text
Esmart0-win0: ACTIVE
    format: NV15 little-endian (0x3531564e)
    src: pos[0, 0] rect[3840 x 2080]
    dst: pos[0, 40] rect[3840 x 2080]
    buf[0]: pitch: 4864 offset: 0
    buf[1]: pitch: 4864 offset: 10117120
```

### 10.1 No scaler in the path

The film is 3840x2080 and the mode is 3840x2160. The probe centres the frame at
its native size — `dst: pos[0, 40] rect[3840 x 2080]` — rather than scaling it:

```text
placement src=3840x2080+0+0 dst=3840x2080+0+40 scaling=none (1:1, centred)
```

Engaging the VOP2 scaler would put a resampler in the path of a gate about
picture fidelity, and the 40-pixel bars top and bottom are exactly the letterbox
a 1.85:1 film wants. Source and destination rectangles are equal, so the
mapping is 1:1 and every coded pixel reaches one panel pixel.

The plane selected is id 73, DRM plane type **`Cursor`**, accepted by an atomic
`TEST_ONLY` commit — the MP1a finding that a conventional player filtering
planes by type would find no video plane at all is reconfirmed on real content.

## 11. Mode and frame-rate selection

The output mode is derived from the asset, not configured:

```text
playback target_rate=23.9760 fps (source: asset frame rate)
mode selected index=13 name=3840x2160 3840x2160 clock=296703 refresh=23.9760
  (requested 23.9760, delta +0.0000) flags=0x100005
```

The exact 23.976 Hz mode exists (pixel clock 296703 kHz vs 297000 for the
24.000 Hz variant) and was matched with **zero error**. No 60 Hz fallback
occurred and none would have been accepted: the probe treats a rate error above
0.01 Hz as a `mode MISMATCH` and downgrades the verdict to `PARTIAL` unless
explicitly overridden. VOP2 drove the clock it was asked for:

```text
Update mode to 3840x2160p24, type: 0(if:HDMI1, flag:0x0) for vp0 dclk: 296703000
set dclk_vop0 to 296703000, get 296703000
final tmdsclk = 296703000
```

The `3840x2160p24` label in the driver log is the vendor driver rounding for
display; the 296703000 Hz dot clock is 23.976 Hz, not 24.000.

## 12. HDR metadata mapping

The `HDR_OUTPUT_METADATA` blob is built from this film's own metadata. Nothing
is hard-coded and nothing is invented; fields the asset does not carry are left
zero, which is the CTA-861.3 encoding for "unspecified".

```text
hdr_metadata blob_size=32 metadata_type=0 eotf=2 infoframe_metadata_type=0
hdr_metadata display_primaries(ST2086 order G,B,R)=[(13250,34500) (7500,3000) (34000,16000)]
             white_point=(15635,16450)
hdr_metadata max_display_mastering_luminance=4000 (cd/m2)
             min_display_mastering_luminance=50 (0.0001 cd/m2)
             max_cll=401 max_fall=77
hdr_metadata raw=00 00 00 00 02 00 c2 33 c4 86 4c 1d b8 0b d0 84 80 3e 13 3d
                 42 40 a0 0f 32 00 91 01 4d 00 00 00
```

| Field | Asset value | Coded value | Units |
| --- | --- | --- | --- |
| EOTF | `smpte2084` | `2` | HDMI\_EOTF\_SMPTE\_ST2084 |
| green primary | (0.2650, 0.6900) | (13250, 34500) | 0.00002 |
| blue primary | (0.1500, 0.0600) | (7500, 3000) | 0.00002 |
| red primary | (0.6800, 0.3200) | (34000, 16000) | 0.00002 |
| white point | (0.31270, 0.32900) | (15635, 16450) | 0.00002 |
| max mastering luminance | 4000 cd/m² | 4000 | 1 cd/m² |
| min mastering luminance | 0.005 cd/m² | 50 | 0.0001 cd/m² |
| MaxCLL | 401 | 401 | cd/m² |
| MaxFALL | 77 | 77 | cd/m² |

Every value traces to the film. This is a different blob from the MP1a
synthetic one in every field that can differ — different primaries (DCI-P3
rather than BT.2020), 4000 nits rather than 1000, MaxCLL 401 rather than 1000 —
which is itself the check that the mapping is data-driven.

Provenance was taken from the decoded frame's side data rather than only the
container: the container-level scan reported `mastering=0 content_light_level=0`
and the first decoded frame reported `mastering=1 content_light_level=1`. Had
the probe trusted the container alone it would have signalled HDR10 with zeroed
mastering fields.

### 12.1 Mid-stream metadata stability

Static metadata was monitored on every frame for the whole run:

```text
metadata_changes=0
```

over 2876 decoded frames. No re-signalling was needed and none was done.
`hdr10plus_frames` is counted and reported; dynamic metadata is not output, by
scope.

## 13. HDMI output state

`/sys/kernel/debug/dri/0/summary`, sampled mid-playback over a separate ssh
session so the measurement does not perturb the probe's own scheduling:

```text
Video Port0: ACTIVE
    Connector:HDMI-A-1  Encoder: TMDS-200
    bus_format[200d]: YUYV10_1X20
    overlay_mode[0] output_mode[9] HDR10[2] color-encoding[BT.2020] color-range[Limited]
    Display mode: 3840x2160p24
    dclk[296703 kHz] real_dclk[296703 kHz]
```

`MEDIA_BUS_FMT_YUYV10_1X20` is **YCbCr 4:2:2 at 10 bits per component** — the
only 10-bit format this HDMI 1.4 / 300 MHz sink can carry at 4K. Requested
versus actual, read back from the kernel after the commit:

| Property | Requested | Actual |
| --- | --- | --- |
| `Colorspace` | `BT2020_YCC` | `BT2020_YCC` (10) |
| `color_depth` | `30bit` | `30bit` (10) |
| `HDR_OUTPUT_METADATA` | blob 266 | blob 266 |
| `color_format` | *not set by probe* | `ycbcr444` (1) — see note |

`color_format` reads `ycbcr444` while the link runs 4:2:2. It is a request
channel, not a status channel; `bus_format` in debugfs is the only source of
truth for what is on the wire. This is recorded again because it is a trap any
later monitoring code will fall into.

## 14. TV HDR state

**TV HDR10 trigger: `YES`**, established three independent ways.

### 14.1 Sink telemetry, before / during / after

The BRAVIA has no "is the incoming signal HDR" getter on this generation, so
the evidence is a differential: the TV keeps separate picture values per input
and per SDR/HDR profile. Run `emmc-cinema`, with `pictureMode` held at
`cinemaHome` throughout so nothing else moves:

| TV setting | before | **during** | after |
| --- | --- | --- | --- |
| `xtendedDynamicRange` | medium | **high** | medium |
| `lightSensor` | available | **unavailable** | available |
| `brightness` | 35 | **50** | 35 |
| `pictureMode` | cinemaHome | cinemaHome | cinemaHome |
| `autoPictureMode` | off | off | off |
| `contrast` | 100 | 100 | 100 |
| `hdrMode` | auto | auto | auto |
| `colorSpace` | auto | auto | auto |

Three fields flip during playback and all three revert afterwards. The
unchanged rows are what makes the changed ones meaningful.

The earlier run `emmc`, taken with the TV in `graphics` mode, shows the same
three fields moving — `xtendedDynamicRange` medium → **off**, `lightSensor` →
unavailable, `brightness` 30 → 40 — so HDR entry is not an artefact of one
picture mode. (The absolute values differ because BRAVIA stores them per
profile.)

### 14.2 Operator observation

The operator, watching the panel: **"TV'de HDR açıldı"** — HDR engaged.

### 14.3 Reference cross-check

See §19. The TV reports the *same* HDR profile for this board as for a Ugoos
SK1 running Kodi.

### 14.4 Restore

After playback the probe returns the connector to what it found, and
`--reset` returns it to neutral. Verified from the board and from the sink:

```text
bus_format[2025]: YUV8_1X24  overlay_mode[1] output_mode[f] SDR[0] color-encoding[BT.709]
Display mode: 1920x1080p60
Colorspace=Default(0)  color_depth=Automatic(0)  HDR_OUTPUT_METADATA=0
connector status: connected
```

No sticky HDR state was left behind, on either side, after any run — including
after the deliberately-interrupted early runs.

## 15. Playback cadence metrics

Cadence is measured from the **DRM page-flip event's vblank sequence counter**,
not from wall-clock jitter. A sequence delta of one between consecutive flips
means the previous frame occupied exactly one refresh interval, so repeated
frames are counted rather than inferred.

Startup — the seek, RKMPP filling its buffer pool, the storage building
read-ahead — is excluded: the first 24 frames run free, then the playback clock
is anchored and measurement begins. Warmup figures are reported separately.

Expected frame period at 23.976024 fps: **41.708 ms**.

| Run | Source | Duration | decoded | presented | dropped | repeated | late | effective fps |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `emmc` | eMMC | 120 s | 2876 | **2852** | **0** | **0** | **0** | **23.9763** |
| `emmc-cinema` | eMMC | 120 s | 2876 | **2852** | **0** | **0** | **0** | **23.9763** |
| `usb` | USB 2.0 NTFS | 120 s | 2871 | 2816 | 31 | 35 | 2365 | 23.6818 |

Frame interval distribution, in milliseconds:

| Run | mean | p50 | p95 | p99 | min | max |
| --- | --- | --- | --- | --- | --- | --- |
| `emmc` | **41.708** | 41.708 | 41.772 | 41.797 | 41.567 | **41.835** |
| `emmc-cinema` | **41.708** | 41.707 | 41.766 | 41.789 | 41.560 | **41.850** |
| `usb` | 42.226 | 41.708 | 41.780 | 41.826 | 41.562 | **375.358** |

Lateness against the scheduled presentation time, in milliseconds:

| Run | mean | p50 | p95 | p99 | max |
| --- | --- | --- | --- | --- | --- |
| `emmc` | −0.623 | −0.623 | 0.162 | 0.360 | **0.513** |
| `emmc-cinema` | −0.599 | −0.600 | 0.176 | 0.371 | **0.536** |
| `usb` | 139.561 | 166.052 | 166.823 | 167.013 | 207.314 |

The vblank counter closes the argument for the local-storage runs:

```text
cadence vblank_sequence first=30376 last=33227 delta=2851 presented=2852
        (delta-presented+1=0 extra refresh intervals)
```

2851 vblanks elapsed while 2852 frames were presented. **Every frame occupied
exactly one refresh interval, for two minutes, twice.** Mean interval equals the
theoretical period to three decimal places; the worst single interval in 2851 is
0.13 ms from ideal.

### 15.1 The storage finding

The three runs differ in exactly one variable — which disk the identical bytes
are read from — and the result is unambiguous. The USB 2.0 path produces a
375 ms stall (nine refresh intervals) and 35 repeated frames; the eMMC path
produces none. The film averages 15.24 Mbit/s ≈ 1.9 MB/s, and the measured USB
2.0 NTFS throughput was ≈ 7 MB/s, so average bandwidth is not the problem —
peak-rate stalls are.

**The cadence defect is in the storage path, not in the display pipeline.** An
earlier 40-second USB run reproduces it independently (458 ms stall). This is a
product-level constraint worth carrying: 4K HDR playback on this board wants
local or USB 3.0 storage, and a player will need a read-ahead buffer sized in
seconds rather than frames — a stall of this size cannot be absorbed by any
frame queue that fits in the MPP buffer pool.

## 16. Kernel log results

`dmesg` was captured before and after each run and diffed. Filtered for
`drm|vop|vop2|hdmi|dw-hdmi|phy|hdr|colorspace|bpc|color_depth|infoframe|edid|underflow|timeout|iommu|mpp|rkvdec|error|fail|warn`:

| Run | delta lines | matching `error\|fail\|warn\|underflow\|timeout\|iommu\|fault` |
| --- | --- | --- |
| `emmc` | 36 | **0** |
| `emmc-cinema` | 36 | **0** |
| `usb` | 36 | **0** |

Every delta is the same benign modeset-down / modeset-up pair:

```text
rockchip-vop2 fdd90000.vop: [drm] vop disable intf:1000
rockchip-vop2 …: Update mode to 3840x2160p24, … for vp0 dclk: 296703000
rockchip-hdptx-phy-hdmi …: hdptx_ropll_cmn_config bus_width:2d45f6 rate:2967030
rockchip-hdptx-phy-hdmi …: hdptx phy pll locked!
dwhdmi-rockchip fdea0000.hdmi: final tmdsclk = 296703000
dwhdmi-rockchip fdea0000.hdmi: don't use dsc mode
dwhdmi-rockchip fdea0000.hdmi: dw hdmi qp use tmds mode
rockchip-hdptx-phy-hdmi …: hdptx phy lane locked!
rockchip-vop2 fdd90000.vop: [drm] vop enable intf:1000
… (mode down, 1920x1080p60 back up, on --reset)
```

**No VOP2 underflow, no atomic commit failure, no IOMMU fault, no MPP or
`rkvdec` error, no PHY or link error, in any run.** Notably the USB run — the
one with 35 repeated frames — has a kernel-log delta *identical* to the clean
runs: the stalls are invisible to the kernel log, which reconfirms the MP1a
finding that for this BSP `dmesg` is useless as a playback diagnostic and
debugfs plus flip telemetry is the only observability that works.

## 17. Visual fidelity assessment

This is the part of the gate a program cannot answer, and the probe says so
explicitly in its own output rather than implying otherwise:

```text
PLAYBACK NOTE: this verdict covers pipeline behaviour only. Colour, tone,
highlight and shadow fidelity are assessed on the physical sink and reported
separately.
```

### 17.1 What the operator observed

| Condition | Observation (verbatim) |
| --- | --- |
| TV in `graphics` mode | "Tv de HDR açıldı ama renklerin süper olduğunu söyleyemem." |
| TV in `cinemaHome` mode | "Biraz daha iyi ama hâlâ tam değil" |

HDR engagement is confirmed by eye and matches the telemetry. Colour is not
right, and moving the TV to a film-appropriate picture mode improved it without
fixing it.

The operator stated they are not experienced at classifying HDR artefacts, so
the structured questions about banding, highlight clipping, shadow crush and
chroma error were **not** answered and are recorded as unassessed rather than
as passes. This is a real limitation of this gate and is stated as one.

### 17.2 What can and cannot be concluded

Ruled **out** as causes, by measurement:

- frame cadence — 0 dropped, 0 repeated, 0 late for two minutes, twice;
- wrong output rate — 23.976 matched exactly, no 60 Hz fallback;
- bit-depth loss — `NV15` end to end, pitch 4864, `YUYV10_1X20` on the wire;
- scaling — 1:1, no VOP2 scaler engaged;
- wrong or missing HDR metadata — every field traced to the film, verified byte
  by byte;
- HDR mode entry — confirmed by telemetry, by eye, and against a reference;
- decode or commit errors — zero, in every run;
- kernel-level faults — zero, in every run.

**Not** ruled out, and the leading candidate: §18.

### 17.3 Deliberately not claimed

No screenshot or frame capture was used as colour evidence. Capturing an HDR
scanout and rendering it into an SDR PNG would misrepresent exactly the thing
under test, so it was not done, and the fidelity verdict rests only on the
physical panel plus pipeline telemetry.

## 18. VOP2 `SDR[0]` interpretation

Gate MP1a saw the video window tagged `SDR[0]` while the video port ran
`HDR10[2]`, and recorded it as an untested uncertainty. On real content the
full picture is worse than the label alone suggested:

```text
Video Port0:  bus_format[200d]: YUYV10_1X20
              overlay_mode[0] output_mode[9] HDR10[2] color-encoding[BT.2020] color-range[Limited]

Esmart0-win0: format: NV15
              color: SDR[0] color-encoding[BT.601] color-range[Limited]
              csc: y2r[1] r2y[0] csc mode[0]
```

Three facts, all read from the kernel, all reproduced in every run:

1. the window is tagged **`SDR[0]`** while the port is `HDR10[2]`;
2. the window is tagged **`color-encoding[BT.601]`** while the port and the
   content are BT.2020; and
3. **`csc: y2r[1]`** — a YUV→RGB conversion is *enabled* on this window.

Point 3 is the one MP1a could not see the significance of. The window is not
passing the samples through untouched; it is converting them. If that
conversion uses the BT.601 coefficients the window is labelled with, while the
port converts back to YUV using BT.2020, the round trip is mismatched and the
result is a systematic colour error — wrong saturation and wrong hue rotation,
strongest on saturated colours and skin tones. That is a good match for
"colours are not right, but not obviously broken".

Following the gate's own instruction, this label was **not** treated as a
failure on its own, and the picture was judged on the panel. But the picture on
the panel is also not clean, and these are the only two facts pointing the same
way.

### 18.1 What the probe deliberately did not do

The scanout plane exposes the standard DRM properties that describe the input
to that conversion, and this gate **left them untouched**, because changing
them departs from the locked MP1a A4 state:

```text
plane 73 property COLOR_ENCODING id=75 value=0
   enums: ITU-R BT.601 YCbCr=0  ITU-R BT.709 YCbCr=1  ITU-R BT.2020 YCbCr=2
plane 73 property COLOR_RANGE    id=76 value=0
   enums: YCbCr limited range=0  YCbCr full range=1
```

`COLOR_ENCODING` sits at its default **0 = BT.601**, which is exactly what
debugfs reports, for BT.2020 content. `COLOR_RANGE` is `limited`, which does
match the film's `color_range=tv`.

This is a one-property, one-run experiment and it is the recommended next
single experiment (§22).

## 19. Ugoos reference comparison

**Status: `AVAILABLE`, and performed.**

The Ugoos SK1 is on the same Sony TV, HDMI 3 — visible over CEC in the TV's own
input list as `extInput:cec?type=player&port=3 title='SK1'`. The same USB disk
was unmounted from the RK3588, moved to the Ugoos, and the same file played in
Kodi from the same point, with the TV in the same `cinemaHome` picture mode.

Sink telemetry sampled during each source's playback:

| TV setting | RK3588 (`emmc-cinema`, during) | **Ugoos SK1 + Kodi (during)** |
| --- | --- | --- |
| `xtendedDynamicRange` | high | **high** |
| `lightSensor` | unavailable | **unavailable** |
| `brightness` | 50 | **50** |
| `contrast` | 100 | **100** |
| `pictureMode` | cinemaHome | **cinemaHome** |
| `hdrMode` | auto | auto |
| `colorSpace` | auto | auto |
| `colorTemperature` | expert1 | expert1 |
| `autoLocalDimming` | medium | medium |
| `autoPictureMode` | off | **auto24p** |

**Every field the TV derives from the incoming signal's dynamic range is
identical.** The Sony puts this board into the same HDR10 picture profile as it
puts a shipping commercial media player. The HDR *signalling* produced by this
pipeline is therefore not merely "accepted" — it is indistinguishable, from the
sink's point of view, from the reference.

Two qualifications, both stated rather than smoothed over:

- `autoPictureMode` differs (`off` here, `auto24p` on the Ugoos input). That is
  a per-input user setting, not detected state, but it is not identical between
  the two inputs and a strict A/B would equalise it.
- The operator's verbal comparison of the two pictures was **"fena değil"** —
  "not bad". That is not a sufficiently discriminating statement to close the
  fidelity question, and it is not treated as one. The structured side-by-side
  comparison of the same scene was interrupted before it could be completed.

So the reference comparison settles the *signalling* question decisively and
leaves the *picture* question open.

## 20. Conditions held constant

Across the runs that are compared:

| Held constant | Value |
| --- | --- |
| media file | byte-identical (SHA-256 verified between USB source and eMMC copy) |
| scene / timestamp | `--start 1200` (20:00) every run |
| duration | 120 s every run |
| TV | same physical Sony, same HDMI input for all RK3588 runs |
| TV picture mode | `cinemaHome` for `emmc-cinema` and for the Ugoos comparison |
| output state | MP1a A4, unchanged |

The one TV setting change during the gate — `graphics` → `cinemaHome` — was
made by the operator, deliberately, between runs `emmc` and `emmc-cinema`, and
is treated as the variable it is: those two runs are a picture-mode A/B, not a
repeat. Nothing on the TV was changed by this gate; only getters were called.

## 21. Remaining uncertainties

1. **The residual colour error is not localised.** The operator reports the
   picture is still not right and the `BT.601` / `y2r[1]` window tagging is the
   leading candidate, but that is inference from two agreeing facts, not a
   measurement. Nothing in this gate proves the window CSC is what is wrong.

2. **The artefact classes were not assessed.** Banding, posterization, highlight
   clipping, shadow crush and chroma error were not evaluated, because the
   available observer said they were not able to classify them. These are
   recorded as *unassessed*, not as passes. Closing them needs either a trained
   observer or test patterns designed for each artefact.

3. **The reference A/B was not completed side by side.** The telemetry
   comparison is complete and decisive for signalling; the visual comparison
   produced only "not bad" before it was cut short.

4. **Mastering primaries ordering is still unverified.** The probe writes
   `display_primaries` in ST 2086 order (G, B, R), converted from FFmpeg's
   R, G, B. This follows CTA-861.3 but is read from the specification, not
   measured. HDR mode entry depends on `eotf`, not on the primaries, so a
   permutation here would still not have shown up as a failure. Confirming it
   needs an HDMI analyser. Carried forward unchanged from MP1a.

5. **`autoPictureMode` differed between the two inputs** in the reference
   comparison (`off` vs `auto24p`).

6. **HDR10+ was present and ignored.** The film carries ST 2094-40 dynamic
   metadata. This gate outputs static HDR10 only, by scope. How much of the
   remaining visual gap is simply the absence of dynamic tone mapping — which
   the Ugoos may well be applying — is not known and is a genuine confound in
   the reference comparison.

7. **Single film, single scene, single sink.** One 120-second window of one
   1.85:1 drama on one HDMI 1.4 TV.

8. **The TV selects `graphics` picture mode for this input.** Sony does that
   when the source signals a graphics content type in the AVI InfoFrame or the
   input is labelled as PC. The probe sets no content type. Whether the vendor
   driver emits a content type at all was not investigated, and it is a real
   product-level concern for a media appliance.

## 22. Gate result

```text
MP1b = PARTIAL   (classification: PARTIAL_FIDELITY)
```

Everything measurable passed, and passed cleanly and reproducibly. The picture
is not confirmed correct, and the operator's direct observation is that it is
not. The gate does not claim a `PASS` it cannot evidence, and it does not claim
a `FAIL_COLOR` it has not localised.

Per the gate's own rule — if MP1b is not a pass, do not proceed to Kodi; isolate
the real-content fidelity problem first — **MP2 (Kodi GBM/DRM bring-up) is not
recommended yet.**

## 23. Recommended next single experiment

**Set the scanout plane's `COLOR_ENCODING` property to `ITU-R BT.2020 YCbCr`
(value 2) and replay the identical scene.**

Everything else stays exactly as it is in this gate: same file, same
`--start 1200`, same 120 s, same eMMC copy, same A4 connector state, same TV in
`cinemaHome`. One property changes. It is a two-line change to
`src/drm/display.cpp` and one extra `add_prop` in the plane block.

Why this one:

- it is the only identified mechanism that can produce a systematic colour error
  while every other measurement is clean;
- the property exists, is standard DRM, is currently at its BT.601 default, and
  debugfs reports that default is what VOP2 is acting on;
- the outcome is decisive either way. If `color-encoding[BT.601]` becomes
  `BT.2020` in debugfs **and** the picture improves, the fidelity question is
  answered and MP1b can be re-run for a clean `PASS`. If the label changes and
  the picture does not, the window CSC is exonerated and the search moves to
  VOP2's HDR/SDR per-window handling; and
- it costs one 120-second run.

Run it as a strict A/B: `COLOR_ENCODING` default, then `COLOR_ENCODING` =
BT.2020, same scene, back to back, with the operator watching both.

Two cheaper things worth doing in the same session, neither of which is the
experiment:

- equalise `autoPictureMode` between the RK3588 and Ugoos inputs and repeat the
  side-by-side comparison that was cut short; and
- check whether the vendor driver emits an AVI InfoFrame content type, since the
  TV's choice of `graphics` mode for this input is a product problem regardless
  of how the colour question resolves.

Explicitly **not** recommended next: Kodi, mpv, Mesa/`libmali`/GBM, Stremio,
audio passthrough, CEC, HDR10+, Dolby Vision, and any kernel, device-tree or
boot configuration change. None of them are needed to answer the open question,
and all of them would add variables to it.

## 24. Raw evidence paths

All under
[`logs/orangepi5-ultra-vendor/real-hdr10-playback-mp1b-2026-09-09/`](../../logs/orangepi5-ultra-vendor/real-hdr10-playback-mp1b-2026-09-09/),
prefixed by run label (`emmc`, `emmc-cinema`, `usb`):

| File | Contents |
| --- | --- |
| `<run>-00-ffprobe-full.txt` | full container and stream metadata |
| `<run>-00-ffprobe-frame0.txt` | first frame's side data: mastering display, content light level, HDR10+ |
| `<run>-01-summary-before.txt` | VOP2 state before the run |
| `<run>-01-dmesg-before.txt` | kernel log before the run |
| `<run>-01-tv-state-before.json` | sink state before the run |
| `<run>-10-playback.txt` | the full run log: asset metadata, HDR mapping, plane search, plane properties, readback, cadence |
| `<run>-11-summary-during.txt`, `<run>-12-summary-during2.txt` | VOP2 state sampled twice during playback |
| `<run>-11-tv-state-during.json` | sink state sampled during playback |
| `<run>-20-dmesg-after.txt`, `-delta.txt`, `-delta-filtered.txt` | kernel log after, the delta, and the filtered delta |
| `<run>-30-reset.txt` | `--reset` returning the connector to neutral |
| `<run>-31-summary-after.txt` | VOP2 state after the reset |
| `<run>-31-tv-state-after.json` | sink state after the reset |
| `<run>-31-connector-state.txt` | connector `status` and `enabled` after |
| `ugoos-40-tv-state-during.json` | sink state during Ugoos SK1 + Kodi playback of the same file |
| `usb-smoke-40s-earlier-run.txt` | the earlier 40 s USB run that first exposed the storage stall |
| `asset-source-sha256.txt` | asset checksum, USB source and eMMC copy |
| `run-summary.txt` | one line per run: label, result, exit code |
| `SHA256SUMS` | checksums for every file above |

Tooling that produced them, in this repository:
[`tools/hdr-playback-probe.cpp`](../../tools/hdr-playback-probe.cpp),
[`src/`](../../src),
[`scripts/run-mp1b.sh`](../../scripts/run-mp1b.sh),
[`scripts/tv-state.sh`](../../scripts/tv-state.sh).

The film is not committed and never will be; only its checksum and metadata are.

## 25. Checksums

`SHA256SUMS` in the evidence directory covers every raw output file.

| Artefact | SHA-256 |
| --- | --- |
| `past.lives.2023.hdr.2160p.web.h265-huzzah.mkv` | `f4e32b8d3feb7efdebaaefaf7338a25bcc86ee144bf82bd4b93da6b80906f96a` |
| `tools/hdr-playback-probe.cpp` | `05f7bda16288fe87722d1ffe4ef53eb0f4362df9daaf6609e4b6522190033470` |
| `src/drm/display.cpp` | `492d5e98e9bde503ef604f7f5b13c9e8cdac033b1ddbe8fc7a49079140a368a6` |
| `src/media/hdr_metadata.cpp` | `7abcac83facca0798876bc48bdd6b3fc79af58d311fd280d74286a06a5a3cffe` |
| `src/media/cadence.cpp` | `d10f8309b305c8bb48ed343877eeef1522e52c5f98da416784947d85ec3da47c` |

## 26. What was and was not changed

**Target.** Nothing installed, nothing upgraded. No kernel, DTB, bootloader,
boot configuration, kernel command line, partition table or systemd unit was
touched. The board was not rebooted.

Two things were written to the board, both outside any system path:
`/tmp/rk3588-mediabox/` (source and build tree) and `/var/tmp/mp1b/`
(the 12 GB working copy of the film and its checksum). The working copy should
be deleted when it is no longer needed; it is not deleted automatically because
the next experiment will want it.

The operator's USB disk was mounted **read-only** at `/mnt/usb-multiboot`,
read, and unmounted cleanly. Nothing was written to it.

The console framebuffer was taken over for the duration of each run and given
back on exit — by the normal path, by the `SIGINT`/`SIGTERM`/`SIGHUP` handler,
and by an `alarm()` watchdog sized to the run. Connector colour properties are
restored as found, and `--reset` returns them to neutral.

**Sink.** Nothing was changed by this gate; only getters were called. The
operator changed the picture mode once, deliberately, and that is recorded in
§20 as a variable rather than as noise.

**Reference repository.** `yusufyav/rk3588-screenbridge` was read and never
written. `HEAD` verified as `582e1d30c6762c134118445bf660c0784aaffe58` with a
clean working tree, before and after.
