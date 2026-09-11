# rk3588-mediabox

Headless Linux media playback appliance for the Orange Pi 5 Ultra (RK3588):
4K, HEVC Main 10, HDR10/HLG, refresh-rate matched, driven by Kodi with Stremio
as the intended selection front end.

```text
Stremio / web selection          future gate, not built yet
        |
        v
      Kodi  GBM/DRM standalone, no desktop compositor
        |
        +--> RKMPP              hardware decode, no software fallback
        |
        +--> DRM PRIME / NV15   dma-buf, no CPU copy, no colour conversion
        |
        +--> VOP2               atomic KMS, 10-bit scanout
        |
        +--> HDMI               BT.2020 + ST2084, mode matched to the content
        |
        +--> ALSA HDMI audio    PCM today, passthrough in MA1
```

HDR mixed composition is the part of this that is specific to the board, and it
is settled:

```text
video plane   PQ / HDR10, EOTF=2, NV15, BT.2020 limited
      +
GUI plane     ordinary SDR / sRGB pixels, EOTF=0
      |
      v
VOP2 hardware SDR-to-HDR block   SDR2HDR_CTRL = 0x0000000b, BT709_TO_BT2020
      |
      v
HDMI HDR10 output                YCbCr 4:2:2 10-bit, YUYV10_1X20, BT2020_YCC
```

Netflix, Prime Video, Widevine and any other licensed streaming application are
out of scope. The target content is local, network, HTTP and torrent sourced
media.

## Do not regress

Three properties were expensive to establish and are easy to undo:

* **Do not software-PQ encode the GUI on this Rockchip direct-to-plane HDR
  path.** Kodi's built-in software HDR GUI compositor must stay off here; the
  hardware performs the only conversion there is.
* **Do not label a PQ GUI buffer as SDR**, or an SDR one as PQ. The tag and the
  pixels have to agree. Every combination where they disagree was tried and
  produced either washed-out OSD colour or a displaced picture.
* **The Android golden model is SDR GUI + VOP2 SDR2HDR**, not a PQ GUI. Android
  on the same silicon keeps its GUI genuinely SDR and lets VOP2 lift it. That
  capture is the oracle this stack was made to match.

The full history, including the three rejected fixes and why each failed, is in
the result reports; it is deliberately not repeated here.

## Current state

| Gate | Subject | Result |
| --- | --- | --- |
| MP1a | HDR10 signalling isolation | `PASS` |
| MP1b | real HDR10 playback fidelity | `PASS` |
| MA0 | HDMI PCM audio bring-up | `PASS` |
| MP2 | Kodi bring-up, GPU user space, display quality | `PASS` |
| MP2-display | Android-parity SDR/HDR composition | `PASS`, `ROOT_CAUSE_CONFIRMED` |
| MA1 | HDMI compressed-audio passthrough | next, not started |

Accepted display baseline, measured rather than assumed:

| Property | Value |
| --- | --- |
| Player | Kodi 22.0b2-Piers, GBM/DRM standalone |
| Decode | RKMPP, `CDVDVideoCodecDRMPRIME`, direct to plane |
| Frame format | DRM PRIME `NV15` |
| Mode | 3840x2160 @ 23.976 |
| Video plane | `Esmart0-win0` (73), `HDR10[2]`, BT.2020, limited range |
| GUI plane | `Cluster0-win0` (57), `SDR[0]`, ordinary sRGB |
| VOP2 | `SDR2HDR_CTRL = 0x0000000b`, `BT709_TO_BT2020`, `hdr2sdr_en = 0` |
| HDMI | `BT2020_YCC`, `color_depth = 30bit`, `YUYV10_1X20` |
| Sink | HDR10 active |
| Audio | HDMI PCM on `hw:0,0` (`rockchip-hdmi1 i2s-hifi-0`) |

See [`docs/gates.md`](docs/gates.md) for what each gate asked and answered, and
[`docs/architecture.md`](docs/architecture.md) for why the pipeline is shaped
this way.

## Target

| Item | Value |
| --- | --- |
| Board | Orange Pi 5 Ultra / RK3588 |
| Address | `root@10.27.27.25` |
| OS | Armbian trixie |
| Kernel | `6.1.115-vendor-rk35xx-screenbridge-hdmirx-audio` |
| HDMI connector | `HDMI-A-1`, DRM connector id 201, single CRTC `video_port0` |
| Kodi prefix | `/opt/rk3588-mediabox/kodi` |
| GPU user space | `/opt/rk3588-mediabox/mali-g24p0-runtime` (private libmali G610) |
| RKMPP / FFmpeg | `/opt/rk3588-screenbridge` |

Connection settings live in [`scripts/env.sh`](scripts/env.sh) and can be
overridden from the environment (`MEDIABOX_HOST`, `MEDIABOX_SSH_KEY`, …).

## Build, deploy, run

Kodi is built on the target from a pinned upstream revision plus the patch
series in [`patches/kodi/`](patches/kodi); no vendored source dump is kept.

```bash
# 1. the private Mali G610 user space (nothing is installed system-wide)
./scripts/install-mali-runtime.sh

# 2. build Kodi on the board: deps, fetch, patch, configure, build, install
./scripts/build-kodi.sh

# 3. run it
./scripts/run-kodi-rk3588.sh start
./scripts/run-kodi-rk3588.sh rpc '{"jsonrpc":"2.0","id":1,"method":"Player.Open","params":{"item":{"file":"/path/on/target/movie.mkv"}}}'
./scripts/run-kodi-rk3588.sh stop

# 4. capture the VOP2 mixed SDR/HDR composition state for one named state
./tools/vop2-sdr2hdr-capture/capture-state.sh logs/<run> <label>
```

`MEDIABOX_GPU=mesa ./scripts/run-kodi-rk3588.sh start` runs the same build
against the untouched system Mesa (llvmpipe). That fallback is kept
deliberately: the Mali claim is only worth as much as the software run it is
compared against.

### Reproducing the MP1 gates

The standalone probes that established the display pipeline before Kodi existed
are still built and still reproducible. `hdr-signaling-probe` is left
byte-for-byte untouched because the MP1a report publishes its source hash.

```bash
./scripts/make-test-assets.sh assets   # needs ffmpeg with libx265
./tests/run-host-tests.sh              # hardware-free checks
./scripts/build-remote.sh              # sync + build the probes on the board
./scripts/deploy-assets.sh
./scripts/run-mp1a.sh "" 30
./scripts/run-mp1b.sh --input /path/on/target/movie.mkv --duration 90 --start 1200
```

## Provenance

The DRM/KMS discovery, atomic-commit and DRM PRIME import structure in
`tools/hdr-signaling-probe.cpp` is **derived from
[yusufyav/rk3588-screenbridge](https://github.com/yusufyav/rk3588-screenbridge)**
(`tools/hdmirx-direct-display.cpp` and `docs/direct-display.md`), reduced to
what this project needs and re-targeted from V4L2 capture to file playback.
That repository is a read-only engineering reference for this one.

Kodi also links the RKMPP-enabled FFmpeg installed at `/opt/rk3588-screenbridge`
on the target, selected through `MEDIABOX_FFMPEG_PREFIX`. That is a dependency
on a path, not on the ScreenBridge daemon; see
[`docs/architecture.md`](docs/architecture.md) for how it is meant to end.

## Results

| Gate | Report |
| --- | --- |
| MP1a — HDR signalling isolation | [`hdr-signaling-mp1a-2026-09-09.md`](results/orangepi5-ultra-vendor/hdr-signaling-mp1a-2026-09-09.md) |
| MP1b — real HDR10 playback fidelity | [`real-hdr10-playback-mp1b-2026-09-09.md`](results/orangepi5-ultra-vendor/real-hdr10-playback-mp1b-2026-09-09.md) |
| MP1b-CSC — plane colour encoding A/B | [`mp1b-plane-color-encoding-ab-2026-09-09.md`](results/orangepi5-ultra-vendor/mp1b-plane-color-encoding-ab-2026-09-09.md) |
| MP1b-FINAL — blind fidelity comparison | [`mp1b-final-blind-fidelity-2026-09-09.md`](results/orangepi5-ultra-vendor/mp1b-final-blind-fidelity-2026-09-09.md) |
| MA0 — HDMI PCM audio | [`hdmi-audio-ma0-2026-09-09.md`](results/orangepi5-ultra-vendor/hdmi-audio-ma0-2026-09-09.md) |
| MP2 — Kodi quality recovery | [`kodi-mp2-quality-recovery-2026-09-10.md`](results/orangepi5-ultra-vendor/kodi-mp2-quality-recovery-2026-09-10.md) |
| MP2 — pause horizontal shift, three rejected fixes | [`kodi-pause-horizontal-shift-2026-09-10.md`](results/orangepi5-ultra-vendor/kodi-pause-horizontal-shift-2026-09-10.md) |
| Android golden oracle | [`android-hdr-golden-reference-2026-09-11.md`](results/orangepi5-ultra-android/android-hdr-golden-reference-2026-09-11.md) |
| MP2-display — Android-parity composition | [`kodi-android-parity-sdr2hdr-2026-09-11.md`](results/orangepi5-ultra-vendor/kodi-android-parity-sdr2hdr-2026-09-11.md) |

Raw evidence for each run is kept under `logs/`, one directory per gate, each
with a `SHA256SUMS` covering every file in it.
