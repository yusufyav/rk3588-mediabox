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
mediabox-bridge                REST/JSON control plane
      |
      | Kodi JSON-RPC
      v
Kodi GBM/DRM                   no desktop compositor
      |
      +--> RKMPP hardware decode
      |
      +--> DRM PRIME dma-buf
      |
      +--> DRM/KMS / VOP2
      |
      +--> HDMI OUT
      |
      +--> ALSA HDMI audio
```

None of this exists yet. Gate MP1a builds only the thin vertical slice needed
to answer whether the vendor display stack can signal HDR10 at all, because
every layer above it is wasted work if it cannot.

## What exists today

```text
tools/hdr-signaling-probe.cpp
    libavformat demux
      -> hevc_rkmpp decode          hardware, DRM PRIME output
      -> NV12 (8-bit) / NV15 (10-bit) dma-buf
      -> drmPrimeFDToHandle + AddFB2WithModifiers
      -> atomic KMS commit on VOP2 video_port0
      -> HDMI-A-1
```

The probe is a measurement instrument, not a player. It has no seeking, no
audio, no A/V sync and no UI. It exists to make one differential visible.

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
directly is the one the vendor driver registered as the cursor plane. The probe
therefore never filters planes by `type`; it runs an atomic `TEST_ONLY` commit
and lets the kernel decide. A player that searches for
`DRM_PLANE_TYPE_OVERLAY` finds nothing on this CRTC.

**No standard `max bpc`.** The connector has no `max bpc` property. The vendor
exposes `color_depth` (`Automatic` / `24bit` / `30bit`) instead. Any user space
that only knows `max bpc` will silently never request 10 bpc.

**Sink ceiling.** The attached Sony TV is HDMI 1.4 / 300 MHz TMDS. It declares
ST2084, HLG and BT.2020, but 4K50/60 exists only as YCbCr 4:2:0 8-bit. True
10-bit at 4K is reachable only at 23.976–30 Hz over YCbCr 4:2:2. This is why
the HDR baseline runs at 3840x2160p23.976 and why 4K60 HDR is not tested.

## Dependency on the ScreenBridge runtime, and how it ends

`hdr-signaling-probe` links the RKMPP-enabled FFmpeg installed at
`/opt/rk3588-screenbridge` on the target, selected through
`MEDIABOX_FFMPEG_PREFIX`. That build is currently the only RKMPP-capable user
space on the board: the distribution FFmpeg has no RKMPP, and Kodi, mpv,
`libmali`, Mesa, `libEGL`, `libGLESv2` and `libgbm` are all absent.

This is a build-time dependency on a path, not a runtime dependency on the
ScreenBridge daemon, and it is temporary. It ends in one of two ways, to be
decided in a later gate:

1. this project builds and ships its own pinned RKMPP FFmpeg under its own
   prefix, or
2. the probe drops libav* entirely and drives `librockchip_mpp` directly, which
   is plausible because it only needs decode-to-dma-buf.

Until then, `MEDIABOX_FFMPEG_PREFIX` is the single point where the dependency
is expressed, and nothing else in the tree references ScreenBridge at runtime.
