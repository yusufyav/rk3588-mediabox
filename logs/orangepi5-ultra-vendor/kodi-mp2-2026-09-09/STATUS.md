# Gate MP2 — status at stop (2026-09-10, stopped by operator)

MP2 is INCOMPLETE. This file records exactly where it stopped so it can be
resumed without repeating any of it. No result classification is claimed.

## What is proven working

Kodi 22.0b2-Piers (e513e0ff4331fc25fd2454659a9dd3e6b7670146) builds and runs
standalone GBM/DRM on this board, from `scripts/build-kodi.sh`, installed to
`/opt/rk3588-mediabox/kodi`. With the two patches in `patches/kodi/` and the
runtime settings below, the VIDEO path reaches the state Gate MP1b proved:

| Measurement | Value |
| --- | --- |
| Decoder | `CDVDVideoCodecDRMPRIME::Open - using decoder Rockchip MPP HEVC decoder` |
| Renderer | `CRendererDRMPRIME` (direct-to-plane, not the GLES variant) |
| Video plane | id **73**, `NV15`, `LINEAR` — the Cursor-typed plane MP1b uses |
| GUI plane | id 57 |
| Mode | `3840x2160p24` (23.976) |
| `bus_format` | `200d` `YUYV10_1X20` — **10-bit** |
| VP0 | `HDR10[2]`, `color-encoding[BT.2020]`, `color-range[Limited]` |
| connector `color_depth` | `30bit` (value 10) |
| TV | HDR active — `xtendedDynamicRange=high`, `lightSensor` unavailable |

## Two runtime settings that are NOT patches and are required

Found by inspection of the running instance, not by patching:

* `videoplayer.useprimedecoder` must be **true**. It defaults to false, and
  while it is false `videoplayer.useprimedecoderforhw` is *disabled* even
  though it reads true — so Kodi silently software-decodes HEVC.
* `videoplayer.useprimerenderer` must be **0 (Direct To Plane)**. The default
  is 1 (EGL), which imports the decoded buffer as a GL texture. On this board
  that means llvmpipe touches every 4K frame, and it is why Kodi felt slow
  during the session even after RKMPP was working.

Both need folding into `config/kodi/guisettings-appliance.xml` before the next
run; they were set live over JSON-RPC during this session.

## Two open problems, neither solved

1. **No GPU user space — the real cause of the slow GUI.** The Mali G610
   kernel driver reports DDK `g25p0-00eac0`, and no matching `libmali` exists
   in any configured repository: Debian trixie has none, and Armbian's
   `rockchip-multimedia` path is Ubuntu-noble only. There is no panfrost or
   panthor DRM node either, so Mesa has no hardware path. The GUI therefore
   runs on **llvmpipe software rendering** (`gbm-egl-probe` records this).
   This is a driver-availability problem; no Kodi change addresses it. Video
   is unaffected because the direct-to-plane path never touches the GPU.

2. **GUI colours are wrong (menu reads yellow).** Present with the Kodi GUI
   on screen and independent of the patches — the raw MP1a/MP1b probes never
   showed it because they scan out no GUI. debugfs shows the disagreement:
   the GUI window converts RGB to YCbCr with **BT.601** (`r2y[1] csc mode[1]`)
   while the video port is tagged **BT.709**, and the connector signals
   `Default`. Three descriptions of the same signal that do not match. Not
   investigated further.

## Patches in patches/kodi/ (applied and verified this session)

* `0001-...` — request deep colour via `max bpc` when the driver has it, else
  resolve the vendor `color_depth` enum **by name**. Without this Kodi leaves
  the link at 8-bit (`2025 YUV8_1X24`) while correctly signalling HDR10.
  **Verified: this is what produces the 10-bit `200d YUYV10_1X20` above.**
  Also stops forcing an RGB colorimetry when a YUV plane is scanned out.
* `0002-...` — re-apply the connector colorimetry from
  `CVideoLayerBridgeDRMPRIME::Configure`, once a YUV buffer is really bound.
  Upstream picks colorimetry in `CRendererDRMPRIME::Configure`, which runs
  *before* the video plane exists, so the window system can only see the RGB
  GUI plane. **Built and installed, but its effect was NOT verified**: the
  operator stopped the session before the confirming run. The connector still
  read `BT2020_RGB` on the last measured run, which was before 0002 was
  installed.

## Audit results that removed work rather than adding it

* Plane discovery needs no patch. Upstream Kodi 22 already accepts a
  Cursor-typed plane when it exposes `INPUT_WIDTH`/`INPUT_HEIGHT` range
  properties — the Rockchip hint. Plane 73 has them at 0..4096 and 0..4320.
* Plane `COLOR_ENCODING`/`COLOR_RANGE` need no patch;
  `VideoLayerBridgeDRMPRIME` sets both from the picture. Confirmed live:
  `csc mode[3]`, `color-encoding[BT.2020]` on the window.
* `HDR_OUTPUT_METADATA` needs no patch; `WinSystemGbm::SetHDR` builds the blob
  from the picture's own metadata.
* Refresh-rate matching needs no whitelist configuration. An empty whitelist
  makes Kodi consider every mode at or above the GUI resolution, so a 1080p
  GUI still reaches 3840x2160@23.976. Confirmed in the log.

## Not done

120 s acceptance run with frame statistics; audio (Kodi's ALSA sink threw
`snd_pcm_writei -77 File descriptor in bad state` repeatedly and was never
configured to a specific device or buffer); seek/stop/replay recovery; the
MP2 report proper.
