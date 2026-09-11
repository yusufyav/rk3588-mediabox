# Architecture

## Target product

```text
Phone / Laptop
      |
      v
Stremio Web / Stremio UI
      |
      | resolved stream
      v
mediabox-bridge                REST/JSON control plane      NOT BUILT YET
      |
      | Kodi JSON-RPC
      v
Kodi GBM/DRM                   no desktop compositor        BUILT, ACCEPTED
      |
      +--> RKMPP hardware decode
      |
      +--> DRM PRIME dma-buf
      |
      +--> DRM/KMS / VOP2
      |
      +--> HDMI OUT
      |
      +--> ALSA HDMI audio     PCM accepted; passthrough is MA1
```

Everything from Kodi down exists and is accepted. The selection front end and
the control bridge above it are not built, and each waits for its own gate.

## What exists today

```text
Kodi 22.0b2-Piers + patches/kodi/0001..0010      /opt/rk3588-mediabox/kodi
    CDVDVideoCodecDRMPRIME
      -> hevc_rkmpp decode           hardware, DRM PRIME output
      -> NV15 (10-bit) dma-buf
      -> CRendererDRMPRIME           direct to plane, no GL texture import
      -> atomic KMS commit on VOP2 video_port0
      -> HDMI-A-1
    CWinSystemGbmGLESContext         GUI, on the private libmali G610 runtime
      -> ordinary SDR/sRGB render path
      -> GUI plane tagged EOTF=0

tools/hdr-signaling-probe.cpp        MP1a instrument, kept byte-for-byte
tools/hdr-playback-probe.cpp         MP1b instrument, built on src/
tools/drm-writeback-probe.c          objective displacement measurement
tools/vop2-sdr2hdr-capture/          the VOP2 composition-state capture
tools/android-hdr-oracle/            the Android golden-oracle capture
```

The probes are measurement instruments, not players. They are kept because the
gates they closed have to stay reproducible, and because the writeback probe is
the only thing in the project that can measure a horizontal displacement
objectively.

## HDR mixed composition: the part that is board-specific

This is the decision that took the longest to reach and the one most likely to
be undone by accident.

VOP2 can either composite in PQ and pass through, or composite in SDR and run
its hardware SDR-to-HDR block. Kodi's upstream answer for an HDR direct-to-plane
path is to PQ-encode the GUI in software so every layer is PQ. On RK3588 that
choice puts the port into an all-PQ multilayer state with `overlay_mode[1]` and
an RGB-to-YUV conversion on the GUI plane, and that configuration both washes
out OSD colour and displaces the picture horizontally when the OSD appears.

Android on the same silicon does the opposite, and the capture in
`results/orangepi5-ultra-android/android-hdr-golden-reference-2026-09-11.md`
proves it: the GUI stays genuinely SDR, in pixels and in tag, and one non-PQ
plane switches VOP2 onto its hardware SDR-to-HDR path.

Patch `0010` reproduces that contract. During HDR direct-to-plane playback Kodi
declines to enable its software HDR GUI compositor, so the GUI keeps its
ordinary SDR render path and patch `0009` derives an `EOTF=0` tag from the same
state. The result is byte-identical to Android:

| | Linux | Android |
| --- | --- | --- |
| `SDR2HDR_CTRL` (`0xfdd92010`) | `0x0000000b` | `0x0000000b` |
| GUI plane | `SDR[0]` | `SDR[0]` |
| video plane | `HDR10[2]` | `HDR10[2]` |
| `r2r_mode` | `BT709_TO_BT2020` | `BT709_TO_BT2020` |
| `hdr2sdr_en` | 0 | 0 |
| `overlay_mode` | 0 | 0 |

`CanDisplayComposeSdrGuiOverHdrVideo()` is a capability test rather than a board
name: direct-to-plane must be running with a GUI plane distinct from the video
plane, and both planes must carry the per-plane `EOTF` property. That property
is a Rockchip vendor addition with no mainline equivalent, so the test is narrow
by construction and every other platform keeps Kodi's software HDR compositing
untouched.

## Hardware constraints this design has to respect

These were established in Gate MP0 on the ScreenBridge reference repository and
are treated as inputs here rather than rediscovered.

**Single CRTC.** VOP2 binds four video ports but only `vp0` has a routed output
in this DTB, so DRM exposes one CRTC. Everything shares it.

**Split plane capabilities on `vp0`.** Only two planes are usable:

| Plane | Type | Formats |
| --- | --- | --- |
| `Cluster0-win0` (57) | Primary | RGB, `YU08`, `YU10`, `Y210`; AFBC and linear. **No NV12/NV15.** |
| `Esmart0-win0` (73) | **Cursor** | RGB, `NV12`, `NV15`, `NV20`, `NV30`, packed YUV; linear only. |

RKMPP emits `NV12`/`NV15`, so the only plane that can scan out decoder output
directly is the one the vendor driver registered as the cursor plane. Nothing
here filters planes by `type`; an atomic `TEST_ONLY` commit decides. A player
that searches for `DRM_PLANE_TYPE_OVERLAY` finds nothing on this CRTC.

**No standard `max bpc`.** The connector has no `max bpc` property. The vendor
exposes `color_depth` (`Automatic` / `24bit` / `30bit`) instead. Any user space
that only knows `max bpc` will silently never request 10 bpc. Patch `0001`
resolves the vendor enum by name, and that is what produces the 10-bit link.

**Sink ceiling.** The attached Sony TV is HDMI 1.4 / 300 MHz TMDS. It declares
ST2084, HLG and BT.2020, but 4K50/60 exists only as YCbCr 4:2:0 8-bit. True
10-bit at 4K is reachable only at 23.976–30 Hz over YCbCr 4:2:2. This is why
the HDR baseline runs at 3840x2160p23.976 and why 4K60 HDR is not tested.

**No system-wide Mali.** The upstream `libmali` package drops an
`/etc/ld.so.conf.d` entry that puts the vendor blob ahead of Mesa for every
process on the machine. `scripts/install-mali-runtime.sh` only ever *extracts*
the package and builds one directory of symlinks from it, which consumers reach
through `LD_LIBRARY_PATH`. Nothing under `/usr/lib`, `/etc/ld.so.conf.d` or the
glvnd vendor directory is touched, so llvmpipe stays available as a comparison
and as a fallback.

## Dependency on the ScreenBridge runtime, and how it ends

Kodi is configured with `-DENABLE_INTERNAL_FFMPEG=OFF` against the RKMPP-enabled
FFmpeg installed at `/opt/rk3588-screenbridge`, selected through
`MEDIABOX_FFMPEG_PREFIX`. Kodi 22 bundles FFmpeg 9.0.1, which has no Rockchip
MPP decoder; the ScreenBridge build is the exact RKMPP FFmpeg that Gates MP1a
and MP1b proved end to end. `librockchip_mpp` and `librga` are loaded from the
same prefix at run time.

This is a dependency on a path, not on the ScreenBridge daemon, and it is meant
to end in one of two ways, to be decided in a later gate:

1. this project builds and ships its own pinned RKMPP FFmpeg under its own
   prefix, or
2. the decode path drops libav\* entirely and drives `librockchip_mpp` directly.

Until then, `MEDIABOX_FFMPEG_PREFIX` is the single point where the dependency is
expressed.

## Kernel

The appliance runs `6.1.115-vendor-rk35xx-screenbridge-hdmirx-audio`, an
out-of-tree build kept from the ScreenBridge work. **No kernel, device-tree or
bootloader change belongs to this project.** The Android-parity fix is entirely
in user space, and an attempt to solve the same fault by forcing RGB mixing in
the VOP2 driver was built, booted and rejected — see section 4 of
`results/orangepi5-ultra-vendor/kodi-pause-horizontal-shift-2026-09-10.md`.

The stock Armbian kernel `6.1.115-vendor-rk35xx` is kept installed alongside as
the rollback. `/boot/Image` is the only thing that selects between them.
