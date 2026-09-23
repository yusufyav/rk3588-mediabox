# What this appliance builds for itself, and why

Armbian ships an FFmpeg, a Mesa and a Kodi. None of them drives this board's
video hardware, so a few things are built here instead. This page is the list —
every non-stock component, the exact revision it is pinned to, where the pin was
read from, and what the appliance loses without it.

It is a living document. Change it when a pin changes; do not add a dated
snapshot beside it.

Last reconciled against the running Ultra: **2026-09-16**.

---

## The rule about prefixes

```text
/opt/rk3588-screenbridge/          rk3588-screenbridge owns this
/opt/rk3588-mediabox/              MediaBox owns this
    media-runtime/                     MPP, RGA, FFmpeg
    player/                            mpv
    kodi/                              Kodi
    mali-g24p0-runtime/                the vendor GL user space
```

These two products are both for RK3588 and can be installed on the same board.
They used to share `/opt/rk3588-screenbridge`, because the board this was
developed on had only one of them on it. That is over: MediaBox builds its own
media runtime, links against it by RPATH, and neither reads nor writes the other
prefix. `scripts/deploy-mediabox-v3.sh` hashes every file in that directory
before and after a deploy and fails if one moves; `tests/run-host-tests.sh`
checks that nothing in this repository names it as a library path, a pkg-config
path, an install prefix or an rpath.

---

## The inventory

### 1. Rockchip MPP — `librockchip_mpp`

| | |
| --- | --- |
| Upstream | `https://github.com/rockchip-linux/mpp.git`, branch `develop` |
| Pinned at | `0986d01294d5c2449c14cf13af9b740368c33967` (2026-08-26, "fix[vproc]: Fix calloc transposed args") |
| Where that pin came from | MPP stamps its build into the library and prints it on every start. `strings librockchip_mpp.so \| grep author:` on the appliance the display and audio gates were measured on reads `0986d01 author: Yandong Lin 2026-08-26 …`. The pin is the artefact's own claim about itself, not a note. |
| Why not the distribution's | Debian has no `librockchip_mpp` at all. This is the user-space half of the kernel's `mpp_service` driver and there is no other way to reach the hardware decoder. |
| Built by | `scripts/build-media-runtime.sh mpp` |
| Installed at | `$MEDIABOX_MEDIA_PREFIX/lib` |
| Consumers | FFmpeg's `hevc_rkmpp`/`h264_rkmpp` decoders, therefore mpv and Kodi |
| Patches | none |
| Validation | `strings` stamp matches the pin; `ffmpeg -decoders \| grep hevc_rkmpp` |

### 2. librga

| | |
| --- | --- |
| Upstream | `https://github.com/airockchip/librga.git` |
| Pinned at | `2b32edcb97b601b25683e2941d888c8515da6d55` ("Update librga version to 1.10.6_[3]") |
| Where that pin came from | Upstream ships this as a **prebuilt object**, not as sources. The pin was established by taking the SHA-256 of the installed `librga.so` on the appliance and finding the commit whose `libs/Linux/gcc-aarch64/librga.so` is the identical git blob `8e1010308767b158c7fc7b6a3e6fe99e87fb02c2`. The library also states its own version: `rga_api version 1.10.6_[3]`. |
| Why not the distribution's | Not packaged. FFmpeg's `--enable-rkrga` needs it to link. |
| Built by | `scripts/build-media-runtime.sh rga` — installed, not compiled; the `.pc` file is written by that script because upstream ships none |
| Installed at | `$MEDIABOX_MEDIA_PREFIX/lib/librga.so` |
| Consumers | FFmpeg's `rkrga` filters. **No MediaBox playback path uses RGA today** — the decoder's output goes to the display controller untouched — but FFmpeg will not configure with `--enable-rkrga` without it, and that flag is part of the configuration the gates were measured on. |
| Patches | none |
| Validation | `ffmpeg -filters \| grep rkrga` |

### 3. ffmpeg-rockchip

| | |
| --- | --- |
| Upstream | `https://github.com/nyanmisaka/ffmpeg-rockchip.git` |
| Pinned at | `d90e3a1c18d7929383cf88c1b3da2e2d1c966cbf` (2026-04-23) |
| Where that pin came from | The binary reports it: `ffmpeg version d90e3a1c18`. It also agrees with `rk3588-screenbridge`'s own `docs/encoder.md`, which records the same full hash. |
| Why not the distribution's | Debian's FFmpeg has no Rockchip MPP decoder and no RGA filters. Kodi 22 bundles FFmpeg 9.0.1, which has neither either — which is why the build is configured with `ENABLE_INTERNAL_FFMPEG=OFF`. |
| Configuration | `--disable-doc --enable-gpl --enable-version3 --enable-libdrm --enable-rkmpp --enable-rkrga --enable-libsrt`, plus an rpath naming this prefix |
| Built by | `scripts/build-media-runtime.sh ffmpeg` |
| Installed at | `$MEDIABOX_MEDIA_PREFIX` — `libav*` as **static** archives, which is why neither player has a `libavcodec.so` dependency and both still pull in MPP and RGA as shared libraries |
| Consumers | mpv, Kodi |
| Patches | none |
| Validation | version string matches the pin; `hevc_rkmpp` decoder, `rkrga` filters and the `srt` protocol all present |

`libsrt` is the distribution's (`libsrt-openssl-dev`, 1.5.4 on the appliance)
and is not pinned here: it is a transport this product's sources can arrive
over, and it is in the configuration the appliance was measured with.

### 3b. rockchip-vaapi — the browser's way in to MPP

| | |
| --- | --- |
| Upstream | `https://github.com/defcom5-rockchip/rockchip-vaapi.git` |
| Pinned at | `8e41d7853415a401984dc71521e9fc4fc5f7fe97`, tag `v2.2.0` |
| Licence | LGPL-2.1-or-later |
| What it is | A VA-API 1.20 backend that implements the driver vtable on top of `librockchip_mpp`. It is not a second decoder: it is a different doorway onto the one in entry 1. |
| Why it has to exist | This appliance's kernel exposes the VPU as `/dev/mpp_service` and exposes no V4L2 codec device at all — there is no `/dev/video*` on the board, `CONFIG_VIDEO_HANTRO` is unset and `/sys/class/video4linux` is empty. Chromium's two accelerated decode backends on Linux are V4L2 and VA-API; the first has nothing to bind to here, and the second needs a driver that did not exist. Without it the GPU process says `vaInitialize failed: unknown libva error`, `chrome://gpu` lists no decode profiles, and 4K is decoded by the CPU. |
| Why not upstream `woodyst/rockchip-vaapi` | That is the original and it is a one-tag prototype: its own `docs/DEVELOPMENT.md` records that the HEVC, VP9 and AV1 paths fall through to the H.264 stub. The HEVC assembler, the 10-bit surface export and the fix for Chromium's create-export-then-decode order all live in the fork. |
| Why that revision and not `main` | `v2.1.5` is the first release that reads a surface's bit depth from the surface rather than from the last decoded frame, which is the order Chromium uses; before it, 10-bit content exports as 8-bit NV12 and arrives garbled. `v2.2.0` is the current tag above that line. |
| Configuration | The shipped `Makefile` hard-codes `/usr/include/rockchip` and a bare `-lrockchip_mpp`, both of which are the distribution's MPP. The build overrides `CFLAGS`/`LDFLAGS` with `pkg-config` against this prefix and adds `-Wl,-rpath,$MEDIABOX_MEDIA_PREFIX/lib`, so there is one MPP on the box and the browser links the one the players link. |
| Built by | `scripts/build-vaapi-driver.sh` |
| Installed at | `$MEDIABOX_MEDIA_PREFIX/lib/dri/rockchip_drv_video.so` — deliberately **not** `/usr/lib/aarch64-linux-gnu/dri`, so nothing else on the machine acquires a Rockchip decoder it did not ask for. The browser is pointed at it by `LIBVA_DRIVERS_PATH` in its unit. |
| Consumers | the browser application, and nothing else: mpv and Kodi call MPP directly and have no use for a VA-API layer |
| Patches | none |
| Decode profiles | H.264 Constrained Baseline / Main / High / High10, HEVC Main / Main10, VP8, VP9 Profile 0 / Profile 2 — to 7680x4320. **No AV1**: VA-API hands the driver headerless tile data and MPP wants whole OBUs. No encode entrypoints at all. |
| Validation | `scripts/build-vaapi-driver.sh verify` and `mediabox-browser-verify`: the driver resolves against this prefix, never names the ScreenBridge prefix, `va_openDriver()` returns 0, and H.264 High and VP9 Profile 0 are advertised |

### 3c. browser-runtime — the V4L2 door onto MPP, and the one that carries AV1

| | |
| --- | --- |
| Upstream | `https://github.com/JeffyCN/libv4l-rkmpp.git` and `v4l-utils` |
| Pinned at | libv4l-rkmpp `c5bc0aef0bc571872eb67508dcd75e05247eb83a` (1.8.0 + 3); v4l-utils `1.30.1` tarball, SHA-256 `c1cf549c…bae197` |
| Licence | GPL-2.0-or-later (plugin), LGPL-2.1-or-later (libv4l2) |
| What it is | Rockchip's own V4L2 memory-to-memory decoder for Chromium, implemented as a libv4l plugin on top of `librockchip_mpp`, plus the `v4l2convert.so` wrapper that puts it in the path of a browser that was never linked against libv4l2. Not a second decoder: a different doorway onto the one in entry 1. |
| Why it has to exist | The VA-API door (entry 3b) cannot carry AV1, and not for want of silicon — RK3588 has an AV1 decoder of its own at `av1d@fdc70000` and the kernel advertises it as `DEVICE[ 4]:AV1DEC`. VA-API's AV1 entry point hands a driver tile data the browser has already parsed; MPP's AV1 decoder wants the stream. Chromium's V4L2 backend sends a stream, which is the shape MPP has, and its V4L2 codec table already contains `AV01` mapped to `AV1PROFILE_PROFILE_MAIN`. |
| Why no Chromium build | Debian's Chromium 153 carries both backends. `media/base/media_switches.h`: *"When both VA-API and V4L2 are compiled in, selects the active backend: disabled (default) => VA-API, enabled => V4L2. Toggle via `--enable-features=PreferV4L2VideoAcceleration`."* The launcher asks for V4L2; nothing is compiled. |
| Configuration | Built against this prefix's libv4l2 and `$MEDIABOX_MEDIA_PREFIX`'s MPP by `pkg-config`, with an RPATH naming both, so there is one MPP on the box and the browser links the one the players link. Decoder ceiling 4096x2304. |
| Built by | `scripts/build-browser-runtime.sh` |
| Installed at | `$MEDIABOX_PREFIX/browser-runtime` — the wrapper at `lib/libv4l/v4l2convert.so`, the plugin at `lib/libv4l/plugins/libv4l-rkmpp.so`, and the codec list at `etc/video-dec0`. Nothing goes to `/usr`. |
| The device node | `etc/video-dec0` is an ordinary **file**: libv4l-rkmpp reads its capabilities out of the node it is opened on and refuses any node that is a character device. The browser's unit bind-mounts it at `/dev/video0` inside its own `PrivateDevices` namespace, so this board's `/dev` never gains a video node and nothing is left behind by hand. |
| Consumers | the browser application, and nothing else |
| Patches | four, all in `packaging/` and all with their measurement in the header: `v4l-utils-patches/0001` interposes the fortified `__open_2`/`__open64_2`, without which the wrapper is loaded and interposes nothing; `v4l-utils-patches/0002` carries the resolution-change event to a client that waits on `POLLPRI`, which a libv4l plugin cannot raise; `libv4l-rkmpp-patches/0001` asks MPP for eight-bit output so a ten-bit stream decodes instead of tripping an `assert` inside the GPU process; `libv4l-rkmpp-patches/0002` logs to stderr rather than into Chromium's mojo socket; `libv4l-rkmpp-patches/0003` stops reporting `POLLIN` for a returning OUTPUT buffer, which made Chromium dereference a CAPTURE queue it had not created yet. Upstream's own dependency, JeffyCN's `0001-libv4l2-Support-mmap-to-libv4l-plugin.patch`, is fetched by hash and applied first. |
| Decode profiles | AV1 Main, VP9, VP8, H.264, H.265 — to 4096x2304, frames as NV12. AV1 is decoded by `fdc70000.av1d`; the rest by the rkvdec cores. |
| Validation | `scripts/build-browser-runtime.sh verify` asks the chain the questions Chromium asks (`tools/v4l2-probe.c`, run inside a mount namespace of its own so the host's `/dev` is untouched), and `mediabox-browser-verify` checks it again on the appliance. `tools/browser-video-probe.py` measures the running browser. |

### 4. Mali G610 user space — **not a source build**

| | |
| --- | --- |
| Upstream | `tsukumijima/libmali-rockchip`, release `v1.9-1-20260312-bd33ee2` |
| Artefact | `libmali-valhall-g610-g24p0-gbm_1.9-1_arm64.deb`, SHA-256 `32ffe853…85eb1a` |
| What it actually is | A **pinned vendor binary package, extracted privately**. Nothing is compiled, no maintainer script runs, and nothing is installed system-wide. |
| Why not the distribution's | Mesa on this board is llvmpipe. The interface is a 4K GLES client; software rasterisation at that size is not a fallback, it is a slideshow. |
| Installed by | `scripts/install-mali-runtime.sh` |
| Installed at | `/opt/rk3588-mediabox/mali-g24p0-runtime/lib`, a directory of symlinks |
| Consumers | `mediabox-tv`, Kodi, the browser application's compositor — each by putting that directory first on `LD_LIBRARY_PATH`, because they link the generic sonames |
| Why private | The upstream `.deb` drops `/etc/ld.so.conf.d/00-aarch64-mali.conf`, which puts the vendor blob ahead of Mesa for every process on the machine. If it then misbehaves there is no software stack left to debug it against. `MEDIABOX_GPU=mesa` exists for exactly that comparison and only works because this is per-process. |
| Validation | `scripts/mali-probe.sh`; at run time the interface refuses to start unless its GBM device reports the `armsoc` backend |

### 5. mpv — the embedded player

| | |
| --- | --- |
| Upstream | `mpv-player/mpv`, tag `v0.41.0` |
| Why not the distribution's | Debian's mpv links Debian's FFmpeg, which cannot reach the decoder; and it has no video output that can draw on a display somebody else holds DRM master on. |
| Built by | `scripts/build-mediabox-player.sh`, on the appliance |
| Installed at | `/opt/rk3588-mediabox/player` |
| Runtime binding | RPATH `$MEDIABOX_MEDIA_PREFIX/lib`. No `LD_LIBRARY_PATH` anywhere in the product. |
| Wayland | **off**. See the table below. |

| Patch | Purpose | Class | What happens without it |
| --- | --- | --- | --- |
| `0001-accept-rockchip-nv15` | Adds `nv15`/`nv20` to mpv's fork-specific pixel-format name list | PRODUCTION_REQUIRED | Ten-bit HEVC frames are refused by the DRM PRIME hwdec, so every HDR film falls back to software |
| `0002-open-the-decoder-device` | Lets zero-copy decoding create its own MPP device when the video output has none | PRODUCTION_REQUIRED | Measured: "Could not create device" at verbose level only, then software decode at 33% of eight cores instead of ~1% |
| `0003-register-the-mediabox-video-plane-output` | Two lines that put `video/out/vo_mediabox.c` in the build | PRODUCTION_REQUIRED | `--vo=mediabox` does not exist; there is no video output that can reach the panel without taking it from the interface |
| `packaging/mpv/vo_mediabox.c` | A whole video output, not a diff | PRODUCTION_REQUIRED | as above |

All three patches name `a/<path>` and `b/<path>` and apply with `patch -p1`.
They used to name an absolute path in the machine they were generated on —
`/var/tmp/<file>.orig` — which GNU patch refuses as a dangerous file name and
then cannot resolve; the series silently stopped applying.

**Wayland is disabled** (`-Dwayland=disabled`). This player was a Wayland client
once, drawing into the sway compositor the old Chromium TV shell ran in, with
`--vo=dmabuf-wayland`. There is no compositor on the appliance any more, and
`packaging/mpv-patches/0003`'s own header records what the Wayland output did on
a box without one: exit 2, every time. Building it pulled `libwayland-client`,
`libwayland-cursor`, `libwayland-egl`, `wayland-protocols` and `libxkbcommon`
into a player that cannot use any of them. Verified after the change:
`mpv --vo=help` lists `mediabox` and `drm` and no Wayland output at all.

### 6. Kodi — the handoff target

| | |
| --- | --- |
| Upstream | `xbmc/xbmc`, `e513e0ff4331fc25fd2454659a9dd3e6b7670146` (`22.0b2-Piers`) |
| Why pinned there | Gate MP2 audited this revision and proved by `git log` that the GBM/DRM/DRMPRIME/HDR paths were identical to master at audit time. "Latest master" would buy nothing and cost reproducibility. |
| Why not the distribution's | Debian's Kodi uses its own FFmpeg and carries none of the patches below. |
| Built by | `scripts/build-kodi.sh`, on the appliance, hours |
| Installed at | `/opt/rk3588-mediabox/kodi` |
| Runtime binding | RPATH `$MEDIABOX_MEDIA_PREFIX/lib` |
| Configuration | `CORE_PLATFORM_NAME=gbm`, `APP_RENDER_SYSTEM=gles`, `ENABLE_INTERNAL_FFMPEG=OFF`, `FFMPEG_PATH=$MEDIABOX_MEDIA_PREFIX`, `DISABLE_FFMPEG_SOURCE_PLUGINS=ON` (this FFmpeg has no `libpostproc`), ALSA on, PulseAudio/VAAPI/VDPAU/CEC/Blu-ray off |

#### The patch profiles

```sh
./scripts/build-kodi.sh                          # production — the default
KODI_PATCH_PROFILE=diagnostic ./scripts/build-kodi.sh
```

`patches/kodi/` is the production series. `patches/kodi-diagnostics/` is applied
on top of it only in the diagnostic profile. Both land in one directory on the
appliance so the numbering stays a single ordered series — which is what they
were generated against.

**`patches/kodi-diagnostics/` is empty today.** Both patches that are
instrumentation by intent turned out to be load-bearing, for two different
reasons, neither of which is visible from reading the patch. The table below
records which and why; `patches/kodi-diagnostics/README.md` records how each
was found.

| Patch | Purpose | Class | Measured consequence if absent |
| --- | --- | --- | --- |
| `0001-gbm-request-deep-colour-and-keep-ycc-for-direct-plane` | Stops Kodi tagging the wire as RGB while a YUV video plane is being scanned out | PRODUCTION_REQUIRED | The sink is told RGB and receives YCbCr. Every combination where the tag and the pixels disagree produced washed-out OSD colour or a displaced picture. |
| `0003-gbm-bound-the-locked-front-buffer-queue` | Bounds the locked front-buffer pool independently of `gbm_surface_has_free_buffers()` | PRODUCTION_REQUIRED | ARM's libmali returns non-zero unconditionally, so the queue never pops: one full GUI-sized buffer leaks per frame and the display IOMMU address space is exhausted within seconds. It surfaces as `EGL_BAD_ALLOC` out of `eglSwapBuffers`, which Kodi turns into a `std::runtime_error` and dies on. Mesa converges to three locked buffers and never reaches the cap, so this is not a tuning knob. |
| `0004-instrument-display-colour-state-transitions` | `MP2COLOR` log lines, and the `LogColorState()` function they are written from | DIAGNOSTIC_ONLY **by behaviour**, PRODUCTION-ANCHOR **by form** | Nothing, behaviourally: every line it adds is a `CLog` call. It is nevertheless in the production profile, because `0006` inserts its functions immediately after `LogColorState()` and `0008` needs the include this patch adds. Lifting it out means regenerating the colour-and-EOTF series against a different base — a change to the path the HDR baseline was measured on, not a cleanup of it. See "What is still owed", below. |
| `0005-alsa-instrument-sink-initialisation-and-first-write` | ALSA sink state, hw params and first-write logging | DIAGNOSTIC_ONLY **by intent**, PRODUCTION_REQUIRED **in fact** | It was moved to the diagnostic profile and moved back. The patches *apply* cleanly without it — 8 patches, 586 insertions — but the build then fails: `AESinkALSA.cpp:1003: error: 'MP2PcmStateName' was not declared in this scope`. `0007` is production-required and reports what state it recovered from by calling `MP2PcmStateName()`, which this patch defines. An instrumentation patch that production code calls into is not instrumentation. |
| `0006-gbm-choose-the-output-pixel-format-and-tag-it-truthfully` | Reads `color_format` live from the driver instead of from Kodi's cache, and requests RGB or YCbCr explicitly | PRODUCTION_REQUIRED | The driver negotiates the output format down to fit the link — a 4K24 10-bit request is transmitted as 4:2:2 — so Kodi's cached copy describes a signal that is not on the wire, and the colorimetry tag follows the cache. |
| `0007-alsa-recover-the-pcm-after-a-modeset-stops-it` | Handles `-EBADFD` by re-preparing the PCM | PRODUCTION_REQUIRED | Switching to a film's 23.976 mode re-trains the HDMI link; the vendor driver stops the HDMI PCM substream, leaving it in SETUP, and the next write returns `EBADFD`. `snd_pcm_recover()` passes that straight back, so nothing ever prepares the stream again: audio is dead for the whole of playback while video carries on. This is the failure Gate MP2 recorded and could not explain. |
| `0008-gbm-tag-direct-video-plane-with-eotf` | Sets the plane's atomic `EOTF` property from the picture | PRODUCTION_REQUIRED | VOP2 decides whether a plane contains HDR samples from this property alone. A PQ NV15 plane left at `EOTF=0` is classified as SDR and run through the SDR-to-HDR block before an already-PQ picture reaches an HDR output. |
| `0009-gbm-tag-hdr-composited-gui-plane-with-eotf` | Tells the display the GUI plane is already HDR-encoded when Kodi's own compositor encoded it | PRODUCTION_REQUIRED | A second SDR-to-HDR conversion on top of transfer-encoded pixels. |
| `0010-gbm-let-the-display-compose-the-sdr-gui-over-hdr-video` | Lets VOP2's SDR2HDR block lift an SDR GUI over HDR video instead of Kodi transfer-encoding it | PRODUCTION_REQUIRED | Kodi's software HDR GUI compositor runs instead. The Android golden model on the same silicon keeps its GUI genuinely SDR and lets the hardware lift it; that capture is the oracle this stack was made to match. |
| `0011-gbm-keep-the-hdr-link-rgb-and-never-let-it-drop-to-8-bit` | Holds ten bits when HDR ends instead of descending to eight | PRODUCTION_REQUIRED | The 6.1.172 HDMI driver carries a rule -- "We prefer use YCbCr422 to send hdr 10bit" -- that fires when the colorimetry is BT.2020, the depth is ten bits, and the previously committed link was 8 bit or already 4:2:2. A link that never drops to 8 bit never gives that rule a predecessor to fire on. Its other half, an unconditional RGB request, is what `0012` replaces. Where a display setting exists, `0013` sets the depth from it instead, so this holding-ten-bits rule applies only without one. |
| `0012-gbm-signal-hdr-only-where-the-link-can-carry-it` | Signals HDR only where the link can carry ten bits at the mode currently set; where it cannot, gives up BT.2020 and the tenth bit together and lets VOP2 convert | PRODUCTION_REQUIRED, **premise corrected by `0013`** | A sink reports its sockets separately: a Sony KD-65XE9005 declares HDMI 1 a 300 MHz port and HDMI 3 a 600 MHz one, and ten-bit RGB at 4K24 is 371 MHz. Over the ceiling the driver kept the depth, gave up the chroma and transmitted 4:2:2 without writing that back to `color_format`, while the tag said BT.2020 RGB -- HDR looked wrong on the slower socket. 0012 concluded that "a 4:2:2 link renders HDR wrong on this driver" and so allowed HDR only where ten-bit **RGB** fits, which on a 300 MHz input at 4K is never. **That conclusion was wrong.** Measured on 2026-09-23 on the same set's 300 MHz input: 4:2:2 ten-bit HDR, asked for by name (`color_format=ycbcr422`) and tagged `BT2020_YCC`, is correct on the television -- from the appliance's own player, and from Kodi once `0013` asks for it the same way. What remains of 0012 is the SDR fallback where no ten-bit format fits at all (4K60 on that input), and its RGB-only test, which is used only when there is no display setting to read.
| `0013-gbm-send-the-colour-the-display-setting-names` | Takes the output format and depth for SDR and for HDR from the appliance's display setting (`/run/mediabox/output-plan`, one line per mode keyed by size, clock, totals and interlace), names that format to the driver, sets the depth, and tags it truthfully | PRODUCTION_REQUIRED | Without it Kodi applies its own, narrower rule and disagrees with the appliance's player about the same mode: on the 300 MHz input an HDR film that the player shows as HDR10 in 4:2:2 was sent by Kodi as SDR ("this link cannot carry HDR at this mode"). With it, measured on the Plus: Kodi at 2160p23.976, plan line `3840x2160@296703/5500x2250 rgb:8 ycbcr422:10`, requested `ycbcr422` / `30bit` / `BT2020_YCC`, wire `YUYV10_1X20 HDR10[2] BT.2020`, colours correct on the television. The key needs the totals and the interlace flag: 2160p23.976 and 2160p29.97 share a clock, 1080i60 and 1080p30 share the totals. |

### 7. The kernel — **not built here, and MediaBox should not need it**

Both boards run a kernel called
`6.1.115-vendor-rk35xx-screenbridge-hdmirx-audio`. It is the stock Armbian
vendor kernel plus one patch, which opens the HDMI **receiver's** I2S capture
DAI. `rk3588-screenbridge` needs that patch; it is what its whole product is.

MediaBox uses HDMI **TX** and has no HDMI RX path in it, so the stock Armbian
vendor kernel should be enough. **That is an inference and not a measurement** —
MediaBox has never been booted on a stock kernel. Nothing in this gate changed
the kernel, the DTB or the boot chain on either board.

The two boards do not run the *same* kernel, only the same release string:
different build hosts, compilers, sizes and hashes. Treat that string as a
family name, not an identity.

---

## What is still owed

* `0004` is instrumentation that the production series is anchored on. Splitting
  it out means regenerating `0006` and `0008` against a base without it, then
  re-measuring the HDR baseline the series was accepted on. That is its own
  piece of work with its own evidence, not a tidy-up.
* `0005` is instrumentation that production code calls into. Separating them
  means moving `MP2PcmStateName()` into `0007` and taking it out of `0005`,
  which is patch-series surgery on an audio path a gate closed. Worth doing;
  not worth doing without re-running that gate.
* RGA is built and linked and no MediaBox playback path uses it. Measured
  during playback: all three RGA schedulers at 0% load with no sessions, and
  the player's open device handles are `/dev/mpp_service` and the DMA heaps and
  nothing else. It is there because the FFmpeg configuration the gates were
  measured with asks for it. Whether that flag can go is a measurement nobody
  has taken.

* **Kodi is built on the appliance and does not have to be.** It takes hours on
  eight Cortex cores, and the sysroot a cross build needs is already fetched —
  `scripts/build-mediabox-tv.sh` rsyncs the appliance's own `/usr/lib` and
  `/usr/include` into `.sysroot/aarch64-trixie` and cross-compiles the
  television interface against it. What stands in the way is not the libraries
  but Kodi's own build: it produces host tools during the build (TexturePacker,
  JsonSchemaBuilder, the SWIG wrappers), so a cross build needs a second, native
  build tree for those. Kodi supports that; nobody here has set it up.

  The same is true of mpv and the media runtime, where it matters much less: 45
  minutes and 10 minutes respectively, against hours.

  The argument *for* building on the appliance is real but weaker than it looks:
  the binary links exactly the libraries the appliance has, with no question
  about whether a copied sysroot has drifted. That is worth something. It is
  probably not worth hours per Kodi change.
