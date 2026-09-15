# rk3588-mediabox

A television appliance for the Orange Pi 5 Ultra (RK3588). Not a player: the box
boots into an interface of its own, resolves what to watch, plays it on the
display controller's own video plane, and hands the evening to Kodi when Kodi is
the better answer. 4K, HEVC Main 10, HDR10, hardware decode throughout, and no
desktop compositor anywhere in it.

```text
  Phone / laptop on the LAN                 Television + remote
            |                                        |
            v                                        v
   mediabox-ui (Rust -> wasm)              mediabox-tv (Rust + Slint)
            |                                 holds DRM master on card0
            |                                        |
            +----------> mediaboxd-rs <--------------+
                        (control plane)              |
                              |                      |
                              v                      |
                    mediabox-media-worker            |
                              |                      |
                       stremio-server                |
                              |                      |
                       resolved stream --------------+
                                                     |
               +-------------------------------------+
               |                                     |
               v                                     v
      embedded player                          Kodi handoff
      mpv 0.41 + vo_mediabox               GBM/DRM standalone
      RKMPP -> DMA-BUF over SCM_RIGHTS     the accepted HDR10 path
      film on Esmart0, UI on Cluster0
               |                                     |
               +------------> VOP2 --> HDMI <--------+
```

Netflix, Prime Video, Widevine and any other licensed streaming application are
out of scope. The target content is local, network, HTTP and torrent sourced
media.

## Current state

| Gate | Subject | Result |
| --- | --- | --- |
| MP1a | HDR10 signalling isolation | `PASS` |
| MP1b | real HDR10 playback fidelity | `PASS` |
| MA0 | HDMI PCM audio bring-up | `PASS` |
| MP2 | Kodi bring-up, GPU user space, display quality | `PASS` |
| MP2-display | Android-parity SDR/HDR composition | `PASS`, `ROOT_CAUSE_CONFIRMED` |
| MA1 | HDMI compressed-audio passthrough | `PARTIAL`, accepted |

MA1 is `PARTIAL` rather than `PASS` for one reason, and it is not the box's:
AC-3 and E-AC-3 pass through bit-exact and Kodi never falls back to PCM decode,
but the sink's ELD declares DTS and the sink does not reproduce it. The gate is
closed and the result is accepted.

Two things the box does are not gate-named, and both are working on the
appliance:

| Subject | State |
| --- | --- |
| Native shell — Slint on bare KMS, no compositor, remote input, kiosk smoke 16/16 | working |
| Embedded player — mpv on the interface's own video plane | working |

What is open, what is deferred and what is merely known-broken is kept in one
place: [`results/DURUM.md`](results/DURUM.md). That is the living document; this
file is the way in.

## What runs on the appliance

| Unit | Source | What it is |
| --- | --- | --- |
| `mediabox-tv-ui.service` | `rust/crates/mediabox-tv` | The television's own interface. Slint on FemtoVG/GLES, Mali G610 on `renderD128`, scanout on Rockchip `card0`. No compositor, no Wayland; it holds DRM master itself. |
| `mediaboxd-rs.service` | `rust/crates/mediaboxd-rs` | The control plane. Typed Unix socket at `/run/mediabox/mediaboxd.sock`, loopback HTTP on `8787`, LAN HTTP on `8788` for private peers only. Decides which application owns the display. Serves the product UI from `/opt/rk3588-mediabox/ui`. |
| — | `rust/crates/mediabox-ui` | The production web UI: Rust compiled to `wasm32`, served by the daemon above. There is no other web interface. |
| `mediabox-media-worker.service` | `media/` | The media core: catalogue, Stremio bridge, `ffprobe` policy, session proxy. Python, bound to loopback; a browser reaches it only through the daemon's relay. |
| `stremio-server.service` | node, pinned in `packaging/upstream.env` | Torrent and stream resolution. |
| `mediabox-player.service` | `packaging/mediabox-player` | Transient. It exists while a film is playing and not otherwise. |
| `kodi.service` | `packaging/systemd`, `patches/kodi/` | The handoff target. Installed but deliberately not enabled at boot: who owns the panel is `mediaboxd-rs`'s decision at run time, not systemd's. |
| `mediaboxctl` | `rust/crates/mediaboxctl` | The operator CLI. It speaks the daemon's protocol and nothing else. |

## The two playback paths

**Embedded.** `mpv 0.41`, built on the appliance against the Rockchip
FFmpeg/RKMPP stack, with three patches and one added video output —
[`packaging/mpv/vo_mediabox.c`](packaging/mpv/vo_mediabox.c). It decodes through
MPP and then draws nowhere: the decoded frame's DMA-BUF descriptors go to the
interface over a Unix socket with `SCM_RIGHTS`, and the interface — which
already holds DRM master — imports them and calls `SetPlane`. Nothing is copied
and nothing is converted on the CPU. The film opens inside the application; the
catalogue stays behind it and Back returns to it.

The plane layout while a film is playing, measured on the appliance:

```text
Cluster0-win0   AR24        zpos 11    the interface, on top, with alpha
Esmart0-win0    NV12/NV15   zpos 0     the film, below, scaled in hardware
```

The driver's own default is the reverse — the primary at 0 and Esmart0 at 11,
which puts a film over the interface and leaves nowhere to draw what is playing.
The interface swaps them at run time and puts them back when it is done.

One measurement, in its own context — a local 4K NV15 file, on this appliance:
23.96 fps, no dropped frames, `mpv` at 6–9 % CPU and the interface at roughly
0 %. That is what that clip did on that run; it is not a guarantee about
arbitrary content.

**Kodi.** Kodi has not been removed and is not a legacy path. It is the accepted
reference for HDR10 and for the Android-parity mixed SDR/HDR composition below,
and `MediaHandoffToKodi` moves the film that is playing to it at the same
second. Kodi takes the display, and the interface takes it back when Kodi exits.

## The display baseline

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

Accepted, measured rather than assumed, on the Kodi path:

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
| Audio | HDMI PCM on `hw:0,0` (`rockchip-hdmi1 i2s-hifi-0`); AC-3 / E-AC-3 passed through bit-exact |

### Do not regress

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

See [`docs/gates.md`](docs/gates.md) for what each gate asked and answered, and
[`docs/architecture.md`](docs/architecture.md) for why the pipeline is shaped
this way.

Before touching anything that reaches the panel — the units that hand the
display between the interface, the player and Kodi, the browser application's
compositor config — read [`docs/display-pipeline.md`](docs/display-pipeline.md).
Every rule in it was learned by breaking it on the appliance, and each one names
the command that says whether it still holds.

## Target

| Item | Value |
| --- | --- |
| Board | Orange Pi 5 Ultra / RK3588 |
| OS | Armbian trixie |
| Kernel | `6.1.115-vendor-rk35xx-screenbridge-hdmirx-audio` |
| HDMI connector | `HDMI-A-1`, DRM connector id 201, single CRTC `video_port0` |
| Product prefix | `/opt/rk3588-mediabox` |
| Kodi prefix | `/opt/rk3588-mediabox/kodi` |
| Player prefix | `/opt/rk3588-mediabox/player` |
| GPU user space | `/opt/rk3588-mediabox/mali-g24p0-runtime` (private libmali G610) |
| RKMPP / FFmpeg | `/opt/rk3588-screenbridge` |

The address is not fixed in the repository. Connection settings live in
[`scripts/env.sh`](scripts/env.sh) and are overridden from the environment
(`MEDIABOX_HOST`, `MEDIABOX_SSH_KEY`, …).

## Build, deploy, run

Nothing is vendored. Kodi and mpv are built on the appliance from pinned
upstream revisions plus this repository's patch series; the Rust binaries and
the web bundle are cross-compiled on the workstation and installed by one
script.

```bash
export MEDIABOX_HOST=<the appliance's address>

# 1. the private Mali G610 user space (nothing is installed system-wide)
./scripts/install-mali-runtime.sh

# 2. the embedded player: mpv 0.41 + packaging/mpv-patches/ + vo_mediabox.c,
#    built on the board against its Rockchip ffmpeg        (~10 min)
./scripts/build-mediabox-player.sh

# 3. the product: control plane, television interface, web UI, media core,
#    units, udev rules, config — and a kiosk smoke that fails the deploy
./scripts/deploy-mediabox-v3.sh

# 4. Kodi, for the handoff path: deps, fetch, patch, configure, build, install
#    on the board                                          (hours)
./scripts/build-kodi.sh
```

Steps 1–3 are the product. Step 4 is needed for the Kodi handoff and for
reproducing the HDR baseline above; it is the slowest thing in the repository
and it is not on the path to a working interface.

Driving it afterwards:

```bash
mediaboxctl status                     # on the appliance
mediaboxctl surface switch ui          # who owns the panel
mediabox-kiosk-smoke                   # 16 checks against the running box
```

Kodi can also be driven directly, which is what the display gates did:

```bash
./scripts/run-kodi-rk3588.sh start
./scripts/run-kodi-rk3588.sh rpc '{"jsonrpc":"2.0","id":1,"method":"Player.Open","params":{"item":{"file":"/path/on/target/movie.mkv"}}}'
./scripts/run-kodi-rk3588.sh stop

# capture the VOP2 mixed SDR/HDR composition state for one named state
./tools/vop2-sdr2hdr-capture/capture-state.sh logs/<run> <label>
```

`MEDIABOX_GPU=mesa ./scripts/run-kodi-rk3588.sh start` runs the same build
against the untouched system Mesa (llvmpipe). That fallback is kept
deliberately: the Mali claim is only worth as much as the software run it is
compared against.

### Reproducing the MP1 gates

The standalone probes that established the display pipeline before any player
existed are still built and still reproducible. They are measurement
instruments, not part of the product. `hdr-signaling-probe` is left
byte-for-byte untouched because the MP1a report publishes its source hash.

```bash
./scripts/make-test-assets.sh assets   # needs ffmpeg with libx265
./tests/run-host-tests.sh              # hardware-free checks
./tests/run-media-core-tests.sh        # the media core's own suite
./scripts/build-remote.sh              # sync + build the probes on the board
./scripts/deploy-assets.sh
./scripts/run-mp1a.sh "" 30
./scripts/run-mp1b.sh --input /path/on/target/movie.mkv --duration 90 --start 1200
```

## The ScreenBridge dependency

There is **no runtime dependency on the `rk3588-screenbridge` daemon**. There
is a dependency on a path: the RKMPP-enabled FFmpeg, `librockchip_mpp` and
`librga` installed at `/opt/rk3588-screenbridge`. Both players link against it —
Kodi through `MEDIABOX_FFMPEG_PREFIX`, `mediabox-player` through
`LD_LIBRARY_PATH` — because it is the only build on the box that can drive the
hardware decoder. [`docs/architecture.md`](docs/architecture.md) records how
that dependency is meant to end.

The custom kernel is a separate question and a softer dependency. The patch that
distinguishes it opens the HDMI **receiver's** I2S capture DAI; MediaBox uses
HDMI **TX**, so the stock Armbian vendor kernel should be enough. **That is an
inference, not a measurement** — MediaBox has never been booted on the stock
kernel. [`docs/temiz-imaj.md`](docs/temiz-imaj.md) says where to test it.

The DRM/KMS discovery, atomic-commit and DRM PRIME import structure in
`tools/hdr-signaling-probe.cpp` is **derived from
[yusufyav/rk3588-screenbridge](https://github.com/yusufyav/rk3588-screenbridge)**
(`tools/hdmirx-direct-display.cpp` and `docs/direct-display.md`), reduced to what
this project needs and re-targeted from V4L2 capture to file playback. That
repository is a read-only engineering reference for this one.

## Documents

| Document | What it is for |
| --- | --- |
| [`results/DURUM.md`](results/DURUM.md) | The living state of the product: what runs, what is open, what is known-broken. Updated, never added to. |
| [`docs/temiz-imaj.md`](docs/temiz-imaj.md) | Blank card to working appliance, in order, for the Ultra and — derived, not yet measured — the Plus. |
| [`docs/architecture.md`](docs/architecture.md) | Why the pipeline is shaped this way, and the board constraints it has to respect. |
| [`docs/gates.md`](docs/gates.md) | What each gate asked, and what the evidence said. |
| [`docs/display-pipeline.md`](docs/display-pipeline.md) | The rules about the panel, each with the command that checks it. |

The per-gate reports — MP1a, MP1b, MP1b-CSC, MP1b-FINAL, MA0, MA1, MP2, the
Android oracle, the platform and native-shell gates — and every raw device
capture under `logs/` were taken out of the tree at `d757d17`; carrying a
thousand files of receipts made it expensive to find the twelve that are the
product. They are not lost:

```sh
git show 710181b:results/orangepi5-ultra-vendor/hdr-signaling-mp1a-2026-09-09.md
git show 710181b --stat -- results logs      # everything that was there
```

Raw captures are also kept outside the repository, one directory per gate, each
with a `SHA256SUMS` covering every file in it.
