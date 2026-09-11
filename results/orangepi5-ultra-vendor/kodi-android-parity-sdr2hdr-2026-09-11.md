# Android-parity HDR composition on Linux

Orange Pi 5 Ultra, vendor kernel 6.1.115, Kodi 22.0b2-Piers. 2026-09-11.

The Android oracle captured the day before said the working stack keeps its GUI
genuinely SDR — plain sRGB pixels on a plane tagged `EOTF=0` — and lets VOP2's
hardware SDR-to-HDR block lift it into the HDR10 output. Linux did the opposite:
Kodi PQ-encoded the GUI in software and tagged its plane PQ, leaving every
composited layer PQ and the hardware block bypassed. This gate reproduced the
Android contract on Linux.

**Classification: `ROOT_CAUSE_CONFIRMED`. `MP2_DISPLAY = PASS`.**

One semantic change, entirely in userspace, no kernel or device-tree work:
during HDR direct-to-plane playback Kodi no longer runs its software HDR GUI
compositor, so the GUI stays in its ordinary SDR render path and its plane stays
tagged SDR. `SDR2HDR_CTRL` came up `0x0000000b` — byte-identical to Android —
and both symptoms cleared at once for the first time in this project.

Evidence: `logs/orangepi5-ultra-vendor/kodi-android-parity-sdr2hdr-2026-09-11/`
(75 files, `SHA256SUMS` verified).

---

## 1. What was changed, and where

The switch is a single call in `CRendererDRMPRIME::Configure()`:

```
CRendererDRMPRIME::Configure(picture)                RendererDRMPRIME.cpp:107
  ├─ winSystem->SetColorimetry(&picture)             -> connector Colorspace
  ├─ winSystem->SetHDR(&picture)          :126       -> HDR_OUTPUT_METADATA
  └─ if (passthroughHDR)
       winSystem->SetGuiCompositing(picture.color_transfer)          :131
         └─ CWinSystemGbmGLESContext::SetGuiCompositing()   GLESContext.cpp:195
              m_guiCompositing = (colorTransfer != 0)
              ├─ CGuiCompositeShaderGLES::CompileAndLink()      <- PQ shader
              ├─ m_compositeShader->CreateLUTs(colorTransfer)   <- PQ LUT
              │    └─ GeneratePQLUT(m_sdrPeak)   GuiCompositeShaderGLES.cpp:152
              └─ SetGuiPlaneEotf(m_guiCompositing)  [patch 0009]  <- plane EOTF=2

per frame:  Application.cpp:913 BeginGuiComposite() -> GUI draws into m_guiFbo
            Application.cpp:946 CompositeGui()      -> FBO through PQ LUT
                                EndGuiComposite()

teardown:   ~CRendererDRMPRIME()                     RendererDRMPRIME.cpp:29
              ├─ SetGuiCompositing(false)  -> FBO cleanup, shader reset,
              │                               SetGuiPlaneEotf(false) -> EOTF=0
              ├─ SetHDR(nullptr)           -> HDR_OUTPUT_METADATA cleared
              └─ SetColorimetry(nullptr)   -> Colorspace cleared
```

Answering the brief's four structural questions directly:

* **Where `SetGuiCompositing()` is called** — enabled once per playback at
  `RendererDRMPRIME.cpp:131`, guarded by `SetHDR()` having returned true;
  disabled in the destructor at `RendererDRMPRIME.cpp:37`.
* **HDR GUI FBO lifecycle** — created lazily in `BeginGuiComposite()`
  (`GLESContext.cpp:235`), which returns immediately when `m_guiCompositing` is
  false; recreated on resolution change; destroyed in the `else` branch of
  `SetGuiCompositing()`.
* **Where the PQ LUT is built** — `CreateLUTs()` is reached only from
  `SetGuiCompositing()`; the PQ curve itself is `GeneratePQLUT()`, built only
  when `colorTransfer == AVCOL_TRC_SMPTE2084`.
* **How the GUI plane became `EOTF=2`** — patch 0009's `SetGuiPlaneEotf()`
  tail call, which derives the tag from `m_guiCompositing`.

That last point is what makes this one change rather than two. The brief
required the GUI's pixels and the GUI plane's tag to move together; because
0009 already derives the tag from the compositor's state, declining to enable
the compositor sets both. Patch `0010` adds one early-out:

```cpp
  m_guiCompositing = (colorTransfer != 0);

  if (m_guiCompositing && CanDisplayComposeSdrGuiOverHdrVideo())
  {
    CLog::Log(LOGDEBUG, "...: GUI left SDR for hardware SDR-to-HDR composition");
    m_guiCompositing = false;
  }
```

`CanDisplayComposeSdrGuiOverHdrVideo()` is a capability test, not a board name:
direct-to-plane must be running with a GUI plane distinct from the video plane,
and both planes must carry the per-plane `EOTF` property. That property is a
Rockchip vendor addition with no mainline equivalent, so the test is narrow by
construction, and every other platform keeps Kodi's software HDR compositing
untouched.

`0009` is not reverted. RKMPP, DRM PRIME, NV15, video BT.2020, limited range,
`BT2020_YCC`, `color_depth=30bit`, `HDR_OUTPUT_METADATA`, TV HDR10, 10-bit
output and the accepted video CSC state are all still in force; the only thing
removed is the GUI's software PQ encoding and the PQ promotion of its plane.

## 2. A/B

Same binary tree, same asset, same scene, same output mode. `A` is the accepted
0009 build; `B` adds only patch 0010.

| measured, OSD visible | A (0009) | B (Android-parity) | Android oracle |
| --- | --- | --- | --- |
| `SDR2HDR_CTRL` `0xfdd92010` | `0x00000100` | **`0x0000000b`** | **`0x0000000b`** |
| `sdr2hdr_eotf_en` | 0 | 1 | 1 |
| `sdr2hdr_r2r_en` | 0 | 1 | 1 |
| `sdr2hdr_r2r_mode` | – | 0 = `BT709_TO_BT2020` | `BT709_TO_BT2020` |
| `sdr2hdr_oetf_en` | 0 | 1 | 1 |
| `sdr2hdr_bypass_en` | **1** | **0** | **0** |
| `hdr2sdr_en` | 0 | 0 | 0 |
| GUI plane (`Cluster0-win0`, AR24) | `HDR10[2]` | **`SDR[0]`** | `SDR[0]` |
| video plane (`Esmart0-win0`, NV15) | `HDR10[2]` | `HDR10[2]` | `HDR10[2]` |
| video csc | `y2r[0] r2y[0] mode[0]` | `y2r[1] r2y[0] mode[3]` | `y2r[1] mode[3]` |
| `overlay_mode` | 1 | **0** | 0 |
| output | 3840x2160@23.976, HDR10, YUYV10_1X20 | unchanged | 1920x1080p60 |

The Linux value is not merely semantically equivalent to Android's, it is the
same number. The `RK3568_SDR2HDR_CTRL` field layout is identical in the 5.10
tree the oracle was read from and in this appliance's 6.1.115 tree, so the
question of reconciling a different register ABI did not arise.

Kodi's own log for arm B:

```
CRendererDRMPRIME::Configure: HDR passthrough: on
CWinSystemGbmGLESContext: GUI left SDR for hardware SDR-to-HDR composition
CWinSystemGbm::SetGuiPlaneEotf: plane=57 hdr_encoded=false eotf=0
CVideoLayerBridgeDRMPRIME::Configure: plane=73 plane_fourcc=NV15
    encoding=ITU-R BT.2020 YCbCr range=YCbCr limited range eotf=2
```

against arm A's `hdr_encoded=true eotf=2` on the same plane 57.

One cosmetic artefact is worth recording so it is not mistaken for a fault
later: `SetGuiCompositing()` now returns false on this hardware, so
`RendererDRMPRIME.cpp:133` still logs `HDR passthrough active but GUI
compositing not supported by windowing system`. That warning is upstream text
written for platforms that cannot composite HDR at all. Here it is the intended
outcome, and the line above it says so explicitly. It was left alone to keep the
diff to the windowing system.

## 3. Lifecycle

Eight states, captured in `lifecycle-s0.txt` … `lifecycle-s7.txt`.

| state | `SDR2HDR_CTRL` | GUI plane | video plane | VP | mode |
| --- | --- | --- | --- | --- | --- |
| S0 SDR GUI | `0x00000100` bypassed | `SDR[0]` | – | `SDR[0]`, RGB888_1X24 | 1920x1080p60 |
| S1 HDR, OSD hidden | `0x0000000b` | `SDR[0]` | `HDR10[2]` | `HDR10[2]` | 3840x2160p24 |
| S2 HDR, OSD visible | `0x0000000b` | `SDR[0]` | `HDR10[2]` | `HDR10[2]` | 3840x2160p24 |
| S3 paused, OSD visible | `0x0000000b` | `SDR[0]` | `HDR10[2]` | `HDR10[2]` | 3840x2160p24 |
| S4 paused, OSD hidden | `0x0000000b` | `SDR[0]` | `HDR10[2]` | `HDR10[2]` | 3840x2160p24 |
| S5 resumed | `0x0000000b` | `SDR[0]` | `HDR10[2]` | `HDR10[2]` | 3840x2160p24 |
| S6 stopped | `0x00000100` bypassed | `SDR[0]` | – | `SDR[0]`, UYYVYY8 | 3840x2160p60 |
| S7 replayed | `0x0000000b` | `SDR[0]` | `HDR10[2]` | `HDR10[2]` | 3840x2160p24 |

S1 through S5 are identical. That is the property the Android capture measured
and Linux did not have: the composition state no longer changes when the OSD
appears or when playback pauses. The displacement was never a pause behaviour —
pausing raises the OSD, the OSD attached a PQ-tagged GUI plane, and the all-PQ
multilayer state displaced the picture. With the GUI tagged SDR the transition
has nothing left to switch.

S6 clears correctly: hardware conversion bypassed, `HDR_OUTPUT_METADATA` blob
empty, the panel back to SDR. S7 rebuilds the HDR state from scratch with no
residue.

## 4. Stability

120 s of continuous playback, media position 00:45 to 03:00 at speed 1.

| | |
| --- | --- |
| unacceptable drops | none observed |
| atomic commit errors | 0 |
| VOP underflow / `POST_BUF_EMPTY` | 0 |
| kernel fatal errors | 0 |
| Kodi crashes | 0 |
| new kernel lines during the window | 0 |

Kodi publishes no frame counters over JSON-RPC, so `decoded / presented /
dropped` are not quoted as numbers the way the standalone MP1b probe reported
them. What is claimed is what was measured: the player clock advanced in real
time and the log recorded no drop, skip, stall or late event. The two kernel
lines matching an error pattern are a boot-time `no regulator (vop)` OPP message
and an informational monitor line, both present since boot and unrelated; the
seven `Oops` matches are the `ramoops` pstore driver's own name.

## 5. Physical acceptance

The operator drove the cycles at the appliance and reported both judgements
directly. Verbatim, with the reading kept separate from the words:

> "Ben baslattim izledim ve sorunun giderildi."

Horizontal shift: gone. No displacement on pause, no reverse displacement on
resume, none on OSD open or close. This is the same operator who rejected three
earlier fixes at exactly this step, so it is a comparison against a fault known
by sight.

> "Su ana kadarki en iyi ve sorunsuz OSD/GUI renklerini aliyoruz su an."

OSD colour: better than 0009, not merely equal. No washed-out look, no pale
blue, no double-transfer appearance, no degradation of the video's own colour.

The operator ran the cycles themselves and reported the outcome as a whole, so
no `0/20` or `0/10` tally is claimed here — this session did not watch the
screen. What is established is that the fault the gate was opened on is no
longer present, and that the colour cost every previous fix incurred was not
paid this time.

**This is not the experiment that was rejected before.** The rejected fix was
*PQ buffer + SDR label*: Kodi converted the pixels, then the hardware converted
them again, and the colour washed out. This is *SDR buffer content + SDR label*:
the hardware performs the only conversion there is. The tag and the pixels agree
for the first time on Linux, which is why both symptoms resolve together instead
of trading against each other.

## 6. Answers to the brief

1. **Kodi software PQ GUI compositing off?** Yes. `SetGuiCompositing()` returns
   false before the shader or the LUTs are built; `IsHdrComposite()` is false,
   so the GUI's blend and range handling revert to the ordinary SDR path.
2. **GUI pixel content on the normal SDR/sRGB path?** Yes. No FBO is created, no
   degamma or PQ LUT exists, and `CompositeGui()` returns at its first guard.
3. **GUI plane `EOTF` really 0?** Yes — `hdr_encoded=false eotf=0` on plane 57
   in Kodi's log, and VOP2 classifies the window `SDR[0]`.
4. **Video plane `EOTF` still 2?** Yes — plane 73, NV15, BT.2020, limited,
   `HDR10[2]`.
5. **`SDR2HDR` raw value?** `0x0000000b`.
6. **Decoded bits?** `eotf_en=1`, `r2r_en=1`, `r2r_mode=0`, `oetf_en=1`,
   `bypass_en=0`.
7. **`r2r_mode` = `BT709_TO_BT2020`?** Yes, bit 2 clear.
8. **`HDR2SDR` off?** Yes, `hdr2sdr_en=0`.
9. **OSD colour as good as 0009?** Better, per the operator.
10. **pause/resume shift?** None reported across the operator's cycles; see the
    caveat in section 5.
11. **OSD transition shift?** None reported, same caveat.
12. **4K23.976 preserved?** Yes — 3840x2160 @ 23.976 Hz.
13. **HDR10 / YCbCr 4:2:2 10-bit preserved?** Yes — `YUYV10_1X20`,
    `Colorspace=BT2020_YCC`, `color_depth=30bit`, `HDR_OUTPUT_METADATA` blob
    populated, panel in HDR10.
14. **Android contract reproduced?** Yes, to the register value.

## 7. What is left open

Plane assignment is still the mirror of Android's: Linux puts HDR video on
`Esmart0` and the SDR GUI on `Cluster0`, Android the reverse. The oracle listed
this third of three differences and predicted the first two would be sufficient.
They were, so it was not touched, per the brief's CASE C reservation. It remains
the one known structural difference from the Android reference and the place to
start if a future fault points back at plane classes.
