# Architecture

This describes the appliance as it runs today. Where something is a design
intention rather than a measured property, it says so.

## The system

```text
   Phone / laptop on the LAN                 Television + remote
            |                                        |
            v                                        v
   mediabox-ui  Rust -> wasm32             mediabox-tv  Rust + Slint
   served by the daemon below              FemtoVG/GLES on Mali G610
            |                              DRM master on card0
            |                                        |
            +----------> mediaboxd-rs <--------------+
                    Rust control plane               |
                    unix socket + HTTP               |
                              |                      |
                              v                      |
                    mediabox-media-worker            |
                    catalogue, policy, sessions      |
                              |                      |
                       stremio-server                |
                       torrent + stream resolution   |
                              |                      |
                       resolved stream --------------+
                                                     |
               +-------------------------------------+
               |                                     |
               v                                     v
      embedded playback                       Kodi handoff
      mpv 0.41 + vo_mediabox                  Kodi 22.0b2, GBM/DRM
      RKMPP decode                            RKMPP, DRM PRIME
      DMA-BUF over SCM_RIGHTS                 accepted HDR10 path
      film on Esmart0-win0                    takes the display
               |                                     |
               +------------> VOP2 --> HDMI <--------+
```

Both playback paths are built and both are in use. Which of the two runs is a
product decision, not a maturity one: the embedded player keeps the film inside
the interface, and Kodi is the accepted path for HDR10 and for the mixed SDR/HDR
composition described in section D.

## A. The television's render path

`rust/crates/mediabox-tv` is one process on the bare display controller. There
is no compositor under it, no browser engine in it and no launcher script in
front of it.

```text
Slint -> FemtoVG -> GLES on the private libmali G610 runtime
    render on /dev/dri/renderD128, GBM backend "armsoc"
    ARGB8888 scanout buffer, exported as dma-buf
      -> PRIME import on /dev/dri/card0
      -> ADDFB2
      -> atomic page flip on VOP2 video_port0
      -> HDMI-A-1
```

Three properties of this are load-bearing:

* **Split render/display.** The vendor Mali GBM implementation works on the
  render node; modesetting belongs to `card0`. Every frame crosses between them
  as a dma-buf and is kept alive until its page-flip event arrives. The platform
  refuses to start if GBM does not report the `armsoc` backend — a software
  fallback on a 4K panel is a slideshow, not a fallback.
* **ARGB8888, not XRGB8888.** The interface has to carry alpha for the film on
  the plane beneath it to show through the parts it does not draw.
* **It holds DRM master itself.** Handing the display to Kodi or to the browser
  application is an explicit transition driven by `mediaboxd-rs`, not a
  consequence of another process taking the panel.

The interface is not pinned to this board's plane map. `find_output` takes the
first connected `HDMI-A` connector, and the video plane is found by trying
`SetPlane` rather than by matching a name (section G).

## B. The embedded playback path

```text
mediabox-player (launcher)
  -> mpv 0.41, built on the appliance, three patches + one added output
     --hwdec=rkmpp --vo=mediabox
     links /opt/rk3588-screenbridge/lib for the Rockchip ffmpeg
       -> RKMPP hardware decode, NV12 / NV15
       -> packaging/mpv/vo_mediabox.c draws nowhere: it sends each frame's
          dma-buf descriptors over /run/mediabox-ui/video.sock with SCM_RIGHTS
            -> rust/crates/mediabox-tv/src/video.rs imports them on card0,
               gives them a framebuffer and calls SetPlane
            -> the buffer is released only once a later frame has replaced it
               on the wire, which is what stops MPP handing it back to the
               decoder while the panel is still reading it
```

The wire is one fixed-size message per frame, so a frame and its descriptors can
never be split across a read. It is written down in exactly two places —
`vo_mediabox.c` and `video.rs` — and a test asserts the size.

The plane layout while a film plays, measured on the appliance:

```text
Cluster0-win0   AR24        zpos 11    the interface, on top, with alpha
Esmart0-win0    NV12/NV15   zpos 0     the film, below, scaled in hardware
```

The driver publishes the opposite defaults — primary at 0, Esmart0 at 11 — which
puts a film over the interface and leaves nowhere to draw what is playing, how
far in it is, or which button the remote is on. The interface raises its own
window and sinks the video plane at run time, and restores both afterwards: the
stacking order belongs to the connector's state and outlives the process that
set it, so Kodi must not inherit a primary somebody else raised.

Two things this path does not do, and they are deliberate:

* It does not composite. Nothing draws the film into a texture; the decoder's
  own buffer is scanned out.
* It does not name the film. Title and duration come from the catalogue through
  `MediaPlayHere`, because a proxied session carries no container duration and
  mpv's estimate from one made a 99-minute film read as 3:45.

`mediabox-player.service` is transient. It exists while a film is playing and
not otherwise, and it is started by systemd rather than as a child of the
daemon: the daemon is sandboxed away from the GPU, the DMA heaps and the input
devices, and a player that inherited that sandbox could not open `renderD128` at
all.

## C. The Kodi handoff path

Kodi is a first-class application of this box, not a legacy one.
`MediaHandoffToKodi` is the equivalent of "send to an external player": the same
film, at the same second, in the application that is better at the rest of the
evening. The daemon stops the embedded player, takes the display, and opens the
source in Kodi over JSON-RPC at the position the player had reached.

Kodi is built from a pinned upstream revision (22.0b2-Piers) plus the patch
series in `patches/kodi/`; there is no vendored source dump. `kodi.service`
exists but has no `[Install]` section on purpose — enabled, it took DRM master
at boot and a cold box came up in Kodi instead of on the home screen. Who owns
the panel is `mediaboxd-rs`'s decision at run time.

Kodi runs unsandboxed where the daemon does not, for a measured reason: it
discovers keyboards and remotes through a udev netlink monitor, and a unit that
filtered address families left it with no input devices at all.

## D. HDR mixed composition: the part that is board-specific

This is the decision that took the longest to reach and the one most likely to
be undone by accident. It is established on the Kodi path, which is why Kodi is
the reference for HDR10.

VOP2 can either composite in PQ and pass through, or composite in SDR and run
its hardware SDR-to-HDR block. Kodi's upstream answer for an HDR
direct-to-plane path is to PQ-encode the GUI in software so every layer is PQ.
On RK3588 that choice puts the port into an all-PQ multilayer state with
`overlay_mode[1]` and an RGB-to-YUV conversion on the GUI plane, and that
configuration both washes out OSD colour and displaces the picture horizontally
when the OSD appears.

Android on the same silicon does the opposite, and the golden-reference capture
proves it: the GUI stays genuinely SDR, in pixels and in tag, and one non-PQ
plane switches VOP2 onto its hardware SDR-to-HDR path.

```sh
git show 710181b:results/orangepi5-ultra-android/android-hdr-golden-reference-2026-09-11.md
```

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

The colour state belongs to the connector and outlives the process that set it.
`packaging/mediabox-hdmi-prepare` runs in the one moment nobody holds DRM
master and puts the link back to SDR and RGB before an SDR interface is drawn on
it; without it the panel stayed in its HDR picture profile and drew the
interface through it.

**Not yet verified:** the embedded player's own behaviour against an HDR10 sink.
The panel currently attached to the appliance is an SDR monitor whose `edid`
reads 0 bytes. The HDR baseline above is the Kodi path's, measured on the HDR
television; nothing here claims it has been re-measured through `vo_mediabox`.

## E. The media and control plane

```text
mediaboxd-rs          Rust. The only authority.
  /run/mediabox/mediaboxd.sock    typed unix socket, one-line JSON per request
  127.0.0.1:8787                  loopback HTTP
  0.0.0.0:8788                    LAN HTTP, private-network peers only
  serves /opt/rk3588-mediabox/ui  the wasm product UI
  -> mediabox-media-worker        the only client of it
  -> kodi JSON-RPC
  -> mediabox-player              through a transient systemd unit
  -> mediabox-cec                 /dev/cec0, directly over the Linux CEC UAPI

mediabox-media-worker  Python, 127.0.0.1:8790, loopback only.
  catalogue and library, Stremio add-on bridge, ffprobe-driven policy,
  session proxy and transcode decisions
  -> stremio-server 127.0.0.1:11470   torrent and stream resolution
```

The wire protocol between the interfaces and the daemon is a closed enum in
`rust/crates/mediabox-core`. There is no generic call carrying a shell command,
an argv or a URL to execute.

The media worker is never exposed to the network. Anything a browser needs from
it — including session bytes for a preview — is relayed by the daemon that
already owns that client, so there is never a second authority on the LAN. The
LAN listener admits private-network peers only, which is what makes the UI
reachable from a phone without making the control plane reachable from the
internet.

The production web UI is `rust/crates/mediabox-ui` and there is no other one.
The earlier Python control plane has been removed from the tree; what remains
in Python is the media worker, and that is a current component rather than a
leftover.

## F. The ScreenBridge media-stack dependency

There is **no runtime dependency on the `rk3588-screenbridge` daemon**. There is
a dependency on a prefix: `/opt/rk3588-screenbridge`, which carries the
RKMPP-enabled FFmpeg, `librockchip_mpp` and `librga`.

Both players link against it. Kodi is configured with
`-DENABLE_INTERNAL_FFMPEG=OFF` and selects the prefix through
`MEDIABOX_FFMPEG_PREFIX`; `mediabox-player` and the television interface reach
it through `LD_LIBRARY_PATH`. Kodi 22 bundles FFmpeg 9.0.1, which has no
Rockchip MPP decoder; the ScreenBridge build is the exact RKMPP FFmpeg that
Gates MP1a and MP1b proved end to end.

It is meant to end in one of two ways, to be decided in a later gate:

1. this project builds and ships its own pinned RKMPP FFmpeg under its own
   prefix, or
2. the decode path drops libav\* entirely and drives `librockchip_mpp` directly.

Until then that prefix is the single point where the dependency is expressed.
`docs/temiz-imaj.md` records how the stack is produced — it is built by
`scripts/build-media-stack.sh` in the ScreenBridge repository, not here.

### Kernel

The appliance runs `6.1.115-vendor-rk35xx-screenbridge-hdmirx-audio`, an
out-of-tree build kept from the ScreenBridge work. **No kernel, device-tree or
bootloader change belongs to this project.** The Android-parity fix is entirely
in user space, and an attempt to solve the same fault by forcing RGB mixing in
the VOP2 driver was built, booted and rejected:

```sh
git show 710181b:results/orangepi5-ultra-vendor/kodi-pause-horizontal-shift-2026-09-10.md
```

That kernel differs from the stock Armbian vendor kernel by one patch, which
opens the HDMI **receiver's** I2S capture DAI; the transmitter side is
unchanged. MediaBox uses HDMI **TX**, so the stock vendor kernel should be
enough for it. **This is an inference and not a measurement** — MediaBox has
never been booted on the stock kernel, and `docs/temiz-imaj.md` is where that
test is written down. The stock kernel `6.1.115-vendor-rk35xx` is kept installed
alongside as the rollback; `/boot/Image` is the only thing that selects between
them.

## G. Board constraints this design has to respect

These were established in Gate MP0 on the ScreenBridge reference repository and
are treated as inputs here rather than rediscovered.

**Single CRTC.** VOP2 binds four video ports but only `vp0` has a routed output
in this DTB, so DRM exposes one CRTC. Everything shares it.

**Split plane capabilities on `vp0`.** Only two planes are usable:

| Plane | Type as reported | Formats |
| --- | --- | --- |
| `Cluster0-win0` (57) | Primary | RGB, `YU08`, `YU10`, `Y210`; AFBC and linear. **No NV12/NV15.** |
| `Esmart0-win0` (73) | **Cursor** | RGB, `NV12`, `NV15`, `NV20`, `NV30`, packed YUV; linear only; has a scaler. |

RKMPP emits `NV12`/`NV15`, so the only plane that can scan out decoder output
directly is the one the vendor driver registered as the cursor plane. Three
consequences, all of them live in the code:

* `DRM_CLIENT_CAP_UNIVERSAL_PLANES` must be requested or Esmart0 is not visible
  at all.
* `type` is not consulted when choosing a video plane. A player that searches
  for `DRM_PLANE_TYPE_OVERLAY` finds nothing on this CRTC.
* Which plane the video port will accept cannot be read from user space —
  `PLANE_MASK` and the plane `NAME` are both bitmask properties. Candidates are
  ordered by capability (10-bit first) and settled by trying `SetPlane`; a plane
  the port does not drive refuses, and the next is tried. That is one ioctl on
  the first frame of the first film, and it cannot be wrong about the hardware
  the way a name match can.

**No standard `max bpc`.** The connector has no `max bpc` property. The vendor
exposes `color_depth` (`Automatic` / `24bit` / `30bit`) instead. Any user space
that only knows `max bpc` will silently never request 10 bpc. Kodi patch `0001`
resolves the vendor enum by name, and that is what produces the 10-bit link.

**Sink ceiling.** The HDR television used for the display gates is HDMI 1.4 /
300 MHz TMDS. It declares ST2084, HLG and BT.2020, but 4K50/60 exists only as
YCbCr 4:2:0 8-bit. True 10-bit at 4K is reachable only at 23.976–30 Hz over
YCbCr 4:2:2. This is why the HDR baseline runs at 3840x2160p23.976 and why 4K60
HDR is not tested.

**No system-wide Mali.** The upstream `libmali` package drops an
`/etc/ld.so.conf.d` entry that puts the vendor blob ahead of Mesa for every
process on the machine. `scripts/install-mali-runtime.sh` only ever *extracts*
the package and builds one directory of symlinks from it, which consumers reach
through `LD_LIBRARY_PATH`. Nothing under `/usr/lib`, `/etc/ld.so.conf.d` or the
glvnd vendor directory is touched, so llvmpipe stays available as a comparison
and as a fallback.

**Second board (Orange Pi 5 Plus).** The differences are measured and they are
all in the boot chain and the peripheral indices — EFI/GRUB and NVMe rather than
U-Boot and eMMC, a different DTB, two HDMI outputs, a different HDMI-IN card
index. The SoC peripherals are identical. Four places in this repository are
pinned to the Ultra and are listed in `docs/temiz-imaj.md`; the television
interface is not one of them, because it discovers both its connector and its
video plane. **A clean-image MediaBox bring-up on the Plus has not been run**,
so everything about it is derived rather than accepted.

## H. Known debt and deferred work

The full list, with what is merely open versus what is broken, is in
[`../results/DURUM.md`](../results/DURUM.md). The items that are architectural
rather than cosmetic:

* **Per-frame import in the embedded player.** `vo_mediabox` does
  `PRIME_FD_TO_HANDLE` + `ADDFB2` for every frame, while the MPP pool cycles
  three or four buffers. The import could be done once per buffer and cached.
  This is the first place to look at the player's 6–9 % CPU, against Kodi's 2 %.
* **The television interface's `main.rs` is one file of ~2,400 lines.** Most
  changes land in it. It wants splitting per screen.
* **No in-process DRM re-modeset.** A display hot-plug is currently absorbed by
  restarting the unit rather than by re-modesetting in place.
* **The probes stay.** `src/`, `tools/` and `tests/run-host-tests.sh` are
  measurement instruments, not product, and are kept because the gates they
  closed have to stay reproducible — the writeback probe is the only thing here
  that can measure a horizontal displacement objectively.
* **`packaging/systemd/mediaboxd.service` is a leftover.** The Python control
  plane it starts was removed from the tree at `d757d17`; the unit file has not
  been.
* **The HDR baseline has not been re-measured through the embedded player**
  (section D), and the appliance currently has an SDR panel attached.
