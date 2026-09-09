# rk3588-mediabox

Headless Linux media playback appliance for the Orange Pi 5 Ultra (RK3588):
4K, HEVC Main 10, HDR10/HLG, refresh-rate matched, eventually driven by Kodi
with Stremio as the selection front end.

```text
Stremio / web selection
        |
        v
      Kodi
        |
        v
      RKMPP            hardware decode, no software fallback
        |
        v
    DRM PRIME          dma-buf, no CPU copy, no colour conversion
        |
        v
 DRM/KMS / VOP2        atomic KMS, 10-bit scanout
        |
        v
      HDMI             BT.2020 + ST2084, mode matched to the content
```

Netflix, Prime Video, Widevine and any other licensed streaming application are
out of scope. The target content is local, network, HTTP and torrent sourced
media.

**Status: Gate MP1a.** Kodi is not implemented yet and is not part of the
current gate. What exists today is `hdr-signaling-probe`, a minimal tool that
drives the decode-to-HDMI chain directly in order to characterise the vendor
BSP's HDR behaviour. See [`docs/gates.md`](docs/gates.md).

## Target

| Item | Value |
| --- | --- |
| Board | Orange Pi 5 Ultra / RK3588 |
| Address | `root@10.27.27.25` |
| OS | Armbian trixie, vendor kernel `6.1.115-vendor-rk35xx-*` |
| HDMI connector | `HDMI-A-1`, DRM connector id 201, single CRTC `video_port0` |

Connection settings live in [`scripts/env.sh`](scripts/env.sh) and can be
overridden from the environment (`MEDIABOX_HOST`, `MEDIABOX_SSH_KEY`, …).

## Build, deploy, run, collect

Everything is built on the target; nothing is installed there.

```bash
# 1. build the test assets on the workstation (needs ffmpeg with libx265)
./scripts/make-test-assets.sh assets

# 2. hardware-free checks
./tests/run-host-tests.sh

# 3. sync + build on the board
./scripts/build-remote.sh

# 4. copy the assets across
./scripts/deploy-assets.sh

# 5. inventory only, performs no modeset
ssh root@10.27.27.25 /tmp/rk3588-mediabox/build/hdr-signaling-probe --probe

# 6. run the A/B ladder and collect evidence under logs/
./scripts/run-mp1a.sh "" 30
```

A single rung can be run on its own; each prints `PASS`, `FAIL` or `BLOCKED`:

```bash
ssh root@10.27.27.25 \
  /tmp/rk3588-mediabox/build/hdr-signaling-probe \
    --step a4 --asset /tmp/rk3588-mediabox/assets/hdr10-4k-2398-main10.mp4 --hold 30
```

While a step runs it takes the CRTC from the kernel console, and it gives it
back on exit, on `SIGINT`/`SIGTERM`, and on a watchdog timeout. No boot
configuration, kernel command line or console setting is ever changed.

## Provenance

The DRM/KMS discovery, atomic-commit and DRM PRIME import structure in
`tools/hdr-signaling-probe.cpp` is **derived from
[yusufyav/rk3588-screenbridge](https://github.com/yusufyav/rk3588-screenbridge)**
(`tools/hdmirx-direct-display.cpp` and `docs/direct-display.md`), reduced to
what this project needs and re-targeted from V4L2 capture to file playback.
That repository is a read-only engineering reference for this one.

The build currently also links against the RKMPP-enabled FFmpeg that lives at
`/opt/rk3588-screenbridge` on the target. That is a deliberate temporary
dependency for Gate MP1a; see [`docs/architecture.md`](docs/architecture.md)
for how it is meant to go away.

## Results

| Gate | Report |
| --- | --- |
| MP1a — HDR signalling isolation | [`results/orangepi5-ultra-vendor/hdr-signaling-mp1a-2026-09-09.md`](results/orangepi5-ultra-vendor/hdr-signaling-mp1a-2026-09-09.md) |

Raw evidence for each run is kept under `logs/orangepi5-ultra-vendor/`.
