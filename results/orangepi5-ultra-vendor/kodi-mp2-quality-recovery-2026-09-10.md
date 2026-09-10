# Gate MP2 — quality recovery

Orange Pi 5 Ultra, vendor kernel 6.1.115. 2026-09-10.

Gate MP2 stopped with three open problems: a GUI on software rendering, a
colour chain whose three descriptions of one signal disagreed, and a Kodi
ALSA sink throwing `snd_pcm_writei -77`. All three are closed. Two of the
three turned out to have a different cause than the one recorded.

**Post-run correction.** The original report inferred from Kodi and the raw
probe both looking washed out that the BenQ RD280UG could not render HDR10
well. Windows + Kodi on that panel looks correct, and a second HP X27q shows
the same washed RK3588 output, disproving the sink attribution twice. A
physical A/B also disproved the proposed 4000→418-nit metadata clamp.

The actual fault is missing userspace programming of Rockchip's atomic-only
plane `EOTF` property. Tagging the direct NV15 video plane `EOTF=2` produced a
clear physical improvement while preserving the required 10-bit direct path.
The remaining washed Kodi control colours were a separate instance of the same
bug on the PQ-composited GUI plane, and tagging that plane closed them too.
Both fixes are now installed and physically accepted. Question 9 records the
evidence.

---

## 1. Does g24p0 user space work cleanly with a g25p0 kernel?

Yes, and it was proved before anything was installed.

| | |
| --- | --- |
| package | `libmali-valhall-g610-g24p0-gbm_1.9-1_arm64.deb` |
| release | `v1.9-1-20260312-bd33ee2`, tsukumijima/libmali-rockchip |
| SHA256 expected | `32ffe853…85eb1a` |
| SHA256 actual | `32ffe853…85eb1a` — **match** |
| kernel DDK | `g25p0-00eac0` |
| user-space DDK | `g24p0-00eac0` |
| GPU | arch 10.8.6 r0p0, CSF |

The package was **extracted, never installed**. It ships no maintainer
scripts at all, and the generic sonames (`libEGL.so.1`, `libGLESv2.so.2`,
`libgbm.so.1`) are real 5928-byte shims in the archive that define no symbols
of their own — every entry point resolves through `libmali.so.1`, with
`libmali-hook.so.1` interposing a few. That is what makes the whole approach
work: `kodi-gbm`'s `DT_NEEDED` entries are already those sonames, so the GL
stack is selected by `LD_LIBRARY_PATH` ordering with no Kodi rebuild.

`/usr/lib`, `/etc/ld.so.conf.d` and the glvnd vendor directory are untouched;
`dpkg -l | grep -c libmali` is 0 and Mesa still works, which is what the
llvmpipe half of every A/B in this report runs on.

Kernel evidence across probe init, render, teardown and three repeats: **no
oops, no GPU reset, no IOMMU fault, no DRM hang**. The kernel loaded its own
built-in CSF firmware; the `.deb`'s `mali_csffw.bin` was never installed and
never used, so g25p0 firmware and g24p0 user space coexist.

`GPU_G24P0 = COMPATIBLE`.

## 2. Is Kodi really using the Mali-G610?

Yes, from Kodi's own log:

```
EGL_VERSION = 1.5 Valhall-"g24p0-00eac0"
EGL_VENDOR  = ARM
GL_VENDOR   = ARM
GL_RENDERER = Mali-G610
GL_VERSION  = OpenGL ES 3.2 v1.g24p0-00eac0.071111a8920e863c7ed9b7ffed7fc78e
```

## 3. Is llvmpipe out of the picture?

Completely. `/proc/<kodi>/maps` lists `libmali.so.1.9.0`, `libmali-hook`, and
the three shims from the private tree. **No Mesa object is mapped at all** —
no `libgallium`, no `libEGL_mesa`, no swrast DRI module. llvmpipe is not
merely unused, it is not loaded.

## 4. Did GUI smoothness physically improve?

Yes, and by more than "noticeably". Same binary, same patches, same profile,
same scripted navigation, same 3840x2560 GUI; the only variable is
`LD_LIBRARY_PATH`:

| | llvmpipe | Mali-G610 |
| --- | --- | --- |
| GUI frame rate (Kodi's own `System.FPS`) | **1.6 fps** avg | **30.8 fps** avg |
| worst sample | 0.2 fps | 20.0 fps |
| best sample | 2.0 fps | 47.0 fps |
| process CPU | 7.5% of one core | 8.7% of one core |
| RSS | 265 MiB | 266 MiB |

About **19x**. CPU and memory barely move, which is the point: llvmpipe was
not saturating the CPU, it simply could not fill a 3840x2560 frame in
anything like a frame's time. At 1.6 fps the GUI is not slow, it is unusable.

Operator's assessment of the Mali run: *"belirgin şekilde akıcı"*.

### The crash that had to be fixed first

Hardware Mali alone did not give a working GUI. Kodi came up, rendered, and
died within 20 s to 3 minutes with an unhandled
`std::runtime_error("eglSwapBuffers failed")`, `EGL_BAD_ALLOC`, and thousands
of `rockchip_gem_iommu_map: out of I/O virtual memory: -28` in the kernel log.
It reads as a Mali incompatibility. It is not one.

Sampling the process and the display IOVA arena every five seconds:

```
t=+5s   fds=3    used 39,321,600
t=+10s  fds=543  used 4,045,516,800
t=+15s  fds=569  used 4,294,348,800   (arena total 4,294,967,296 — full)
t=+40s  dead
```

513 of those fds were `/dmabuf`, each exactly 3,686,400 bytes = Kodi's own
"GUI format 1280x720" at four bytes per pixel. One whole scanout buffer per
frame, never released.

libmali was cleared first: 200 swaps with the documented lock/release pairing
recycle two buffers and hold the arena flat, identically to Mesa; so does the
per-bo user-data cache Kodi keeps its DRM framebuffers in.

The fault is `CGBMUtils::CGBMSurface::LockFrontBuffer()`. It locks a new front
buffer every frame and releases the oldest **only** when
`gbm_surface_has_free_buffers()` reports the pool exhausted — that predicate is
its single release trigger. Upstream's own comment allows that it "may vary
across other implementations"; ARM's libmali does not vary, it never reports
full at all:

| frames | Mesa | Mali |
| --- | --- | --- |
| first `has_free_buffers()==0` | frame 4 | **never** |
| max locked-buffer queue depth | 3 | **120 of 120** |

`patches/kodi/0003` bounds the queue a second way that needs no cooperation
from the GBM implementation, at 4 — above the 3 a correct implementation
settles on, so Mesa never trips it and is bit-for-bit unchanged.

Result at the untouched default `RLIMIT_NOFILE` of 1024: **513 dmabufs → 5**,
**4.29 GB of IOVA → 57.7 MB**, flat, **0 iommu errors**, Kodi stays up.
Raising the fd limit to 65536 beforehand did *not* prevent the crash — the
binding constraint was the IOMMU arena and fd exhaustion was a second symptom
of the same leak.

`tools/gbm-buffer-recycle-probe.c` checks both contracts directly so a future
GL stack can be tested before Kodi's GUI is trusted to it.

---

## 5. What is the SDR GUI's actual HDMI output format?

After the fix: **RGB 4:4:4, 8-bit, full range** — `bus_format[100a]
RGB888_1X24`, `overlay_mode[0]`, `csc: y2r[0] r2y[0] csc mode[0]`, i.e. no
colour conversion at all. Connector `Colorspace = Default`, `color_depth =
Automatic`, HDR metadata clear.

Before the fix it was YCbCr 4:4:4 8-bit **limited** range with the connector
saying `Default`.

## 6. Why was the GUI doing a BT.601 CSC?

**It was not.** That reading was wrong, and proving it is what unblocked the
rest.

Two isolated experiments with Kodi stopped (`color-csc-semantics.txt`; the
connector writes must go through the *atomic* path — the legacy one returns
"Invalid argument" and silently leaves the old value):

*Move the plane's `COLOR_ENCODING`, watch the matrix:*

```
plane COLOR_ENCODING = BT.601   -> window color-encoding[BT.601]  csc mode[1]
plane COLOR_ENCODING = BT.709   -> window color-encoding[BT.709]  csc mode[1]
plane COLOR_ENCODING = BT.2020  -> window color-encoding[BT.2020] csc mode[1]
```

*Move the output colorimetry, plane property untouched at BT.601:*

```
connector Colorspace = Default(0)     -> VP BT.709   csc mode[1]
connector Colorspace = BT709_YCC(2)   -> VP BT.709   csc mode[1]
connector Colorspace = BT2020_YCC(10) -> VP BT.2020  csc mode[3]
connector Colorspace = Default(0)     -> VP BT.709   csc mode[1]
```

The vendor VOP2 selects the RGB→YCbCr matrix from the **output** state, as the
task's own hypothesis allowed. The plane's `COLOR_ENCODING` is stored,
reported by debugfs, and **ignored** on that path. `csc mode[1]` is this
driver's BT.709; `csc mode[3]` is its BT.2020 — the same mode[3] the NV15
video window uses during HDR playback, where BT.2020 is independently known
to be right.

So the GUI was already converted with BT.709, agreeing with the video port.
The previous gate's "BT.601" was a DRM property the driver does not use.

## 7. What was the fix?

The real fault was one step further out, and it did have a visible symptom:
the operator reported the GUI as *washed out*, blacks lifted.

Kodi decides the transmitted colorimetry from the **scanned-out plane's
fourcc**. That is a different question from what is on the wire: this display
engine composes an RGB GUI plane to a YCbCr bus format with no video plane
involved. Kodi saw RGB, sent `Default`, and left the sink to infer both the
matrix and the quantisation range from the resolution. On a PC monitor at a
mode it does not recognise — this panel's native 3840x2560 — that inference
goes wrong, and limited-range YCbCr read as full range is exactly "washed
out".

`patches/kodi/0006`:

* asks the **driver** what the link carries, reading the connector's
  `color_format` live rather than from `CDRMObject`'s cache (which only tracks
  values Kodi itself set and cannot see a driver-side negotiation);
* and rather than only describing the link, **chooses** it: with only the GUI
  on screen Kodi asks for RGB, which removes the matrix and the range from the
  conversation entirely;
* where a YCbCr link is genuinely wanted, tags it truthfully — `BT709_YCC` for
  HD and larger, `SMPTE_170M_YCC` below, instead of `Default` plus an
  inference. Measured to be pure signalling: `Default` and `BT709_YCC` produce
  an identical BT.709 conversion in the display engine.

Every enum is resolved **by name**; a connector without `color_format` falls
back to the old plane heuristic. Nothing is keyed to a board.

**Operator confirmation:** *"Ana menü güzel görünüyor"* — the washed-out GUI
is fixed.

The video path deliberately keeps YCbCr. Requesting RGB there was measured
too: the link comes up `RGB888_1X24`, which is **8-bit**, while `ycbcr444` +
30-bit yields a real 10-bit `YUYV10_1X20`. Trading HDR10 down to 8 bits to win
a range argument is the wrong trade.

### Patch 0002 was dead and is removed

It re-applied the connector colorimetry once a YUV buffer was really bound, on
the theory that the renderer's earlier call could only see the RGB GUI plane.
The instrumentation shows the premise is false — at the renderer's own
`SetColorimetry` the video plane already exists as `73/NV15`, `yuv_scanout` is
already 1, colorimetry is already `BT2020_YCC`, and the re-apply logs a
byte-identical state. It changed nothing.

### A mode-selection bug this sink exposed

Kodi's default mode whitelist admits only modes **at least as large as the
current one**. This panel's native mode is 3840x2560 (3:2), so 3840x2160 is
*shorter* than the desktop and every 23.976 film mode was silently excluded —
playback stayed at the desktop's 49.98 Hz and the cadence Gate MP1b proved was
lost.

`run-kodi-rk3588.sh whitelist-modes` fills the whitelist from the option list
**Kodi itself publishes for the attached connector**. No mode strings are
hard-coded; a different monitor produces a different whitelist. The log then
reads `[WHITELIST] Matched an exact resolution with an exact refresh rate
3840x2160 @ 23.976000 Hz`.

## 8. Is the colour state correct during HDR video and with the OSD up?

Yes. Four states, one session:

| | S0 SDR GUI | S1 HDR video | S2 HDR + OSD | S3 GUI restored |
| --- | --- | --- | --- | --- |
| `bus_format` | `100a RGB888_1X24` | `200d YUYV10_1X20` | `200d YUYV10_1X20` | `100a RGB888_1X24` |
| wire | RGB 4:4:4 8-bit | YCbCr 4:2:2 **10-bit** | YCbCr 4:2:2 **10-bit** | RGB 4:4:4 8-bit |
| mode | 3840x2560p50 | **3840x2160p24** | **3840x2160p24** | 3840x2560p50 |
| VP range/encoding | SDR, BT.709, Full | **HDR10[2]**, BT.2020, Limited | **HDR10[2]**, BT.2020, Limited | SDR, BT.709, Full |
| video plane | — | 73 `NV15`, csc mode[3] | 73 `NV15`, csc mode[3] | — |
| GUI plane | 57 `AR24`, csc mode[0] | — | 57 `AR24`, csc mode[0] | 57 `AR24`, csc mode[0] |
| `Colorspace` | `Default` | **`BT2020_YCC`** | **`BT2020_YCC`** | `Default` |
| `color_depth` | `Automatic` | **`30bit`** | **`30bit`** | `Automatic` |
| `HDR_OUTPUT_METADATA` | clear | set | set | clear |

**S2 meets every OSD criterion:** opening the OSD moves nothing about the
video state — same bus format, same 10 bits, same HDR10 tag, same
`BT2020_YCC`, same `csc mode[3]` on the video window. Three open/close cycles,
all identical.

**S3 is identical to S0 in every field.** The whole HDR acquisition is
released on stop.

Requested and actual are kept apart throughout, on purpose. `color_format`
reads `ycbcr444` while the wire is 4:2:2 — the driver negotiating down to fit
4K24 at 10 bits — which is why `bus_format` is the authority in this gate and
the property readback is not.

## 9. Does Kodi's picture match the raw MP1b reference, and what does that prove?

**Yes — they are indistinguishable, measured and seen.**

Same file, same 30:00 timestamp, same sink, same picture mode, under two
minutes apart:

| | Kodi | MP1b raw probe |
| --- | --- | --- |
| `bus_format` | `200d YUYV10_1X20` | `200d YUYV10_1X20` |
| mode | 3840x2160p24 | 3840x2160p24 |
| VP | HDR10[2], BT.2020, Limited | HDR10[2], BT.2020, Limited |
| video plane | Esmart0-win0 `NV15` | Esmart0-win0 `NV15` |
| `Colorspace` | `BT2020_YCC` | `BT2020_YCC` |
| `color_depth` | `30bit` | `30bit` |

Identical in every field.

The operator reported Kodi's HDR picture as *"oldukça soluk"*. The raw MP1b
reference at the same timestamp: *"bu da aynı şekilde soluk"*.

Kodi is not a regression *against the raw probe*: it reproduces that baseline
exactly. That comparison does **not** establish that either output is correct,
because both paths also reproduce the same bad HDR static metadata.

The later Windows + Kodi comparison on this exact panel looks correct. The
panel can therefore render HDR10, and the old `FAULT DOMAIN: sink` conclusion
is false.

The first follow-up hypothesis was static metadata. An experimental patch
decoded panel code 98 as `50 * 2^(98/32) = 417.71` nits and changed only the
emitted mastering maximum from 4000 to 418 nits. Readback of the committed DRM
blob proved `max mastering=418`, `min mastering=50` (0.005 nit), `MaxCLL=401`,
`MaxFALL=77`; the NV15 direct plane and YCbCr 4:2:2 10-bit link were unchanged.
The operator still reported the film and GUI as washed out. **The mastering-
luminance-clamp hypothesis is therefore rejected**, and the patch was removed.

Two more controlled tests were also negative: explicit connector quantisation
`Limited` versus `Full`, and RGB/BT.2020 10-bit versus YCbCr/BT.2020 10-bit.
The RGB leg additionally lost the GUI text and was reverted. HDR10+ is not a
candidate: the EDID advertises only PQ + Static Metadata Type 1, the connector
has no dynamic-HDR property, and Kodi does not carry FFmpeg's per-frame HDR10+
payload into the GBM output path.

The surviving fault domain was the VOP input EOTF. During HDR output the NV15
video window was consistently reported as `SDR[0]`. Rockchip's HDR10 path
requires that plane to carry `EOTF=2` (PQ); otherwise, with an HDR video port
and an SDR-labelled plane, the driver enables SDR→HDR processing.

The first inspection used legacy `modetest -p`, which hides atomic-only
properties, and incorrectly concluded that the kernel did not expose `EOTF`.
`modetest -M rockchip -a -p` later found property 31, range 0–18, on both active
planes. No kernel or device-tree change is required.

Patch `0008-gbm-tag-direct-video-plane-with-eotf.patch` sets that property from
Kodi's `VideoPicture` transfer function. It was built, installed and exercised
on the HP X27q at the same 30:00 scene. The path remained NV15 direct scanout,
`YUYV10_1X20`, HDR10/PQ, BT.2020 Limited at 2560x1440p60. Readback changed from
plane `EOTF=0` / `color: SDR[0]` to `EOTF=2` / `color: HDR10[2]`; with no OSD,
the unwanted conversion disappeared (`overlay_mode 1`, `y2r=0`, `csc mode=0`).
The operator reported: *"Şu an film renklerinden belirgin bir düzelme var"*.

Opening the player controls exposed the remaining half of the issue. The
direct video plane stays `EOTF=2`, but the AR24 GUI plane remains `EOTF=0`.
Kodi's existing HDR GUI compositor has already converted that plane from sRGB
to BT.2020/PQ; VOP therefore mistakes the PQ-coded GUI buffer for SDR and runs
a second SDR→HDR conversion. The measured transition is `overlay_mode 1→0`,
with the video CSC path also returning to `y2r=1`, `csc mode=3`. The operator's
physical result matches it: film colours are better, while the controls' blue
is still washed out.

Patch `0009-gbm-tag-hdr-composited-gui-plane-with-eotf.patch` implements the
corresponding GUI-plane tag and restores SDR on teardown. It is now installed
and accepted; `gui-plane-eotf-ab.txt` carries the full readback.

With the OSD up, the state no longer moves at all: `overlay_mode` stays 1
where it previously fell to 0, and the video window stays at `csc mode[0]`
where it previously returned to `csc mode[3]`. Both planes read `EOTF=2` and
both report `HDR10[2]`. S2 is identical to S1 in every measured field, stop
returns the GUI plane to `EOTF=0` and the output to S0, and replay restores
`EOTF=2` on both planes.

**Operator confirmation, with the controls visible:** *"Ne yaptın sen böyle?
Şu ana kadarki en iyi görüntüyü aldık"* — the first time in this gate that the
player controls have been called correct.

One asymmetry survives. `CVideoLayerBridgeDRMPRIME::Disable()` does not write
the video plane's `EOTF` back to 0, so it stays latched at 2 after playback
stops. The plane is disabled at that point (`crtc=0, fb=0`) and no visual
effect was observed; Kodi is self-consistent because 0008 rewrites the
property on every `Configure()`. The exposure is another DRM client
inheriting a stale PQ tag. Recorded as open below.

---

## 10. What state was the PCM in at `snd_pcm_writei -77`?

**`SETUP`.** Logged directly by the instrumentation, by name.

## 11. What was the proven root cause of -77?

Not initialization, and not the driver. Every init is healthy and the trace
says so in full:

```
MP2ALSA open:request device="sysdefault:CARD=rockchiphdmi1" rate=48000 channels=2
MP2ALSA open:result  snd_pcm_name="sysdefault:CARD=rockchiphdmi1" state=OPEN
MP2ALSA hw:after     state=PREPARED rate=48000 channels=2 periodSize=2400 frameSize=8
MP2ALSA sw:params    ret=0 start_threshold=2147483647 avail_min=2400 state=PREPARED
MP2ALSA chmap:set    requested="FL FR" ret=0 state=PREPARED
MP2ALSA prepare      ret=0 state=PREPARED
MP2ALSA write:first:before frames=2400 state=PREPARED
MP2ALSA write:first:after  frames=2400 ret=2400 (ok) state_after=PREPARED
```

The failure arrives later, and the log pins it to the frame:

```
10:30:52.500  Display resolution ADJUST : 3840x2160 @ 23.976000 Hz
10:30:53.331  SetMode: Found crtc mode: 3840x2160 @ 24 Hz
10:30:53.338  FlipPage: Execute modeset at next commit
10:30:53.424  snd_pcm_writei(-77) File descriptor in bad state ... state=SETUP
```

**Switching the display to the film's own 23.976 mode re-trains the HDMI link,
and the vendor driver stops the HDMI PCM substream while it does.** The stream
lands in `SETUP` — hw params still applied, simply no longer running — and the
next write returns `EBADFD`.

Kodi then has no way back. `snd_pcm_recover()` passes `EBADFD` straight
through; it only knows `EPIPE`, `ESTRPIPE` and `EINTR`. `HandleError` has
cases for exactly those two and a default that logs and returns. Nothing ever
calls `snd_pcm_prepare()` again, `AddPackets` gives up, and
`CActiveAESink::OutputSamples` fails for the rest of playback while video
carries on perfectly.

That is why Gate MA0 never saw it: the raw probe sets its mode once, before
playback, and never again.

`patches/kodi/0007` adds the missing case. On `EBADFD`, read the state: if it
is `SETUP` the stream was stopped underneath us and `prepare()` is the entire
recovery, after which the retry already in `AddPackets` succeeds. Any other
state is logged **by name** and left alone — a `DISCONNECTED` endpoint is a
real fault and must not be papered over.

```
CAESinkALSA::HandleError(snd_pcm_writei(1)) - the PCM was stopped underneath us (SETUP), preparing it again
CAESinkALSA::HandleError(snd_pcm_writei(1)) - prepared, state is now PREPARED
```

## 12. Does Kodi PCM play physically?

**Yes.** Operator confirmation: *"Evet, ses geliyor"*.

Device is named rather than left on "Default": `ALSA:sysdefault:CARD=rockchiphdmi1`.
Both resolve to the same card today, but Kodi rewrites an unresolvable device
setting to its own default and this appliance re-enumerates its audio devices
on every display mode change — naming it means a rewrite shows up as a diff
instead of quietly moving the stream to the board's ES8323 analogue codec.

## 13. What are Kodi's real buffer/period values?

```
access:      RW_INTERLEAVED
format:      S24_LE
channels:    2
rate:        48000
period_size: 2400      (50 ms)
buffer_size: 9600      (200 ms)
```

Kodi's own low-latency request, granted exactly. **Not** the uncontrolled
131072-frame / ~2.7 s default Gate MA0 warned about.

Channel map: requested `FL FR`, accepted (`ret=0`). Multichannel physical
mapping: `MULTICHANNEL_PHYSICAL_MAPPING = NOT_VERIFIABLE_WITH_CURRENT_SINK` —
this sink is a stereo monitor.


---

## 14. Is 120 s + OSD + seek + stop/replay fully stable?

Yes. One Kodi session, one file, hardware Mali throughout.

**Three OSD open/close cycles** — every sample identical:

```
open/close 1..3   YUYV10_1X20  HDR10[2] BT.2020 Limited  depth=30bit  cs=BT2020_YCC  alsa=RUNNING
```

**Seeks:**

| | position after | wire | HDR | depth | Colorspace | ALSA |
| --- | --- | --- | --- | --- | --- | --- |
| +30 s | 2m39s | `YUYV10_1X20` | HDR10[2] BT.2020 Ltd | 30bit | BT2020_YCC | RUNNING |
| +60 s | 3m51s | `YUYV10_1X20` | HDR10[2] BT.2020 Ltd | 30bit | BT2020_YCC | RUNNING |
| −30 s | 3m33s | `YUYV10_1X20` | HDR10[2] BT.2020 Ltd | 30bit | BT2020_YCC | RUNNING |

**120 s continuous:** wall clock **121 s**, player position **3m34s → 5m35s =
121 s**. Exact real time, no drift. State unchanged at every 30 s sample.

**Stop:** returns to `RGB888_1X24`, SDR, BT.709, **full range**, `color_depth
= Automatic`, `Colorspace = Default`, HDR metadata cleared, mode restored to
3840x2560p50 — identical to S0.

**Replay:** HDR reacquired correctly — `YUYV10_1X20`, HDR10[2], BT.2020
Limited, 30bit, BT2020_YCC. Kodi alive, no crash anywhere in the session.

**Audio across the whole session:**

```
snd_pcm_writei(-77)      : 4
recovered by prepare()   : 4
unrecoverable EBADFD     : 0
OutputSamples failed     : 0
xrun / underrun          : 0
```

Four EBADFDs, one per HDMI modeset (start, stop, replay, …), **every one
recovered**. Audio never dropped out and the sink never failed.

**Kernel across the whole session** — dmesg cleared immediately before the
run, 75 lines produced, every forbidden class at zero:

```
oops 0   panic 0   BUG 0   call trace 0   gpu fault 0   iommu 0
out of I/O virtual memory 0   reset 0   hang 0   underflow 0
atomic fail 0   MPP err 0   hdmi error 0   xrun 0
```

**A/V sync**, operator at 30:00: *"Senkron iyi"*.

---

## 15. Does the motion look right on this sink?

Not on its own, and the reason is the sink rather than the pipeline. The HP
X27q advertises 31 modes and **not one of them is 24, 23.976, 48 or 47.95 Hz**;
its lowest refresh rate is 59.94. Gate MP1b's cadence result was obtained on a
sink that did offer 3840x2160p23.976 and does not transfer here.

The desktop mode's real rate is 59.9506 Hz (241500 kHz / 2720 / 1481), and

    59.9506 / 23.976023 = 2.500437

3:2 pulldown averages 2.5, so the residual 0.000437 refresh per frame
accumulates to a whole refresh every ~95 s — one extra frame repeat, seen as a
periodic hitch. The operator reported exactly that: *"ekran kayıyor belirli
aralıklar ile"*.

Kodi could not switch its way out. Its whitelist search matches only x1, x2 and
x2.5, and only against the video's resolution or the desktop's: the x2.5 target
is 59.9400 and the desktop mode is 59.9506, missing a 0.01 Hz tolerance by
0.011. The modes that do divide exactly are all at 1920x1080 — 59.9402 (2.5x)
and 119.88 (a clean 5x) — which is neither resolution the algorithm considers.

`videoplayer.usedisplayasclock` was turned on. It makes the display's vblank
the master clock and resamples audio, locking the ratio at 2.5:

    CVideoReferenceClock: Detected refreshrate: 59.951 hertz
    CVideoPlayerAudio:: synctype set to 1: resample
    CVideoReferenceClock: Clock speed 100.02 %

100.02% is the 0.017% stretch the arithmetic predicts, it costs no resolution,
and HDR was unaffected. This is a profile setting, not a patch — nothing in the
source tree changed. It removes the periodic hitch; it does not remove 3:2
judder itself, which is inherent at 59.94 Hz. Details in `refresh-cadence.txt`.

`scripts/monitor-cadence-probe.sh` reports this for whatever sink is attached.
It recomputes each mode's rate from the mode timing rather than trusting
`modetest`, which rounds to two decimals and so shows a perfect 59.9400 and a
drifting 59.9506 identically.

## 16. Is there anything still unexplained?

Yes, and it is recorded rather than smoothed over. The operator reports that on
**both** sinks the picture shifts horizontally, once and stably, when the GUI
plane joins the composition — reproducible by toggling the OSD without pausing
at all.

Everything measurable on the RK3588 side says the video is programmed
identically in both states. A full DRM property dump diffed across the two
states shows the video plane's `CRTC_*` and `SRC_*` byte-identical; `pitch`
holds at 4864 over 25 samples; `overlay_mode` stays 1; Kodi logs no renderer
reconfiguration; Kodi's own screenshot with the GUI plane up is entirely black,
so that plane carries no copy of the video; four OSD toggles produce zero
kernel events, so the HDMI link never re-trains. At register level, the only
change inside the Esmart0 block is the two buffer addresses — the control word
and the stride word are unchanged.

Two rounds of operator photography could not settle it either: camera angle and
film scene moved together in the stills, and in a handheld video the logo's
position normalised to the panel edges ranges 0.545..0.571 with no discrete
step, so camera motion exceeds the signal.

The observation and the machine evidence are both recorded; neither is used to
overrule the other. The two surviving hypotheses — the sink re-applying its own
scaler, or VOP2 shifting the video when a Cluster window is enabled — are
separated by a Writeback capture, which this SoC supports and which needs a
small dedicated probe. `gui-plane-horizontal-shift.txt` carries the full
elimination list and the 0008/0009 A/B that is staged but not run.

---

## What was NOT done, and why

* **Restoring the video plane's `EOTF` on teardown.** 0009 restores the GUI
  plane; 0008 has no matching path, so the video plane stays at `EOTF=2` while
  disabled after playback stops. Measured, not visual — see
  `gui-plane-eotf-ab.txt`. `CVideoLayerBridgeDRMPRIME::Disable()` is where it
  belongs.
* **The horizontal shift when the GUI plane joins.** Open, on both sinks, with
  the RK3588 side cleared down to the registers and no instrument yet that can
  see further. See question 16 and `gui-plane-horizontal-shift.txt`.
* **Judder-free playback on this sink.** The periodic hitch is fixed by locking
  to the display clock, but 3:2 judder at 59.94 Hz remains, and the only clean
  mode this panel offers (1920x1080p119.88) would mean a 1080p desktop.
* **Multichannel physical mapping.** `NOT_VERIFIABLE_WITH_CURRENT_SINK` — a
  stereo monitor. Stereo PCM passes on its own evidence.
* **Everything in the gate's stated scope-out list**: no compressed
  passthrough, no TrueHD/DTS-HD/Atmos, no Stremio, no CEC, no HDR10+, no
  Dolby Vision.
* **No mainline kernel.** No panthor, no panfrost, no kernel migration. The
  vendor 6.1.115 HDMI/HDR stack is untouched, and g24p0 turned out to be
  compatible anyway, so the mainline gate never became necessary.
* **No package installation wave.** The GPU driver was extracted, never
  installed; nothing was upgraded; Mesa is intact and still runs the software
  half of every A/B in this report.

## Repository integrity note

The task brief expected ScreenBridge at `582e1d30c6762c134118445bf660c0784aaffe58`.
That commit **does not exist as an object in the clone** and never appears in
its reflog. Actual `HEAD` is `5db942fe9348b186f8c7d1b462b12e6e9077f21a`, equal
to `origin/main`, with a clean tree. Nothing in this session wrote to that
repository — it was read for its FFmpeg prefix only. Recorded as a stale
expectation in the brief rather than as local drift.


---

## Result

| Requirement | Result |
| --- | --- |
| Mali hardware rendering | `PASS` — GL_RENDERER = Mali-G610 |
| llvmpipe out | `PASS` — no Mesa object mapped at all |
| GUI quality acceptable | `PASS` — 1.6 → 30.8 fps; operator: "belirgin şekilde akıcı" |
| Correct S0/S1/S2/S3 colour states | `PASS` — S3 identical to S0, video state untouched by the OSD |
| RKMPP / NV15 direct-plane | `PASS` — plane 73, `NV15`, `CRendererDRMPRIME` |
| 4K 23.976 | `PASS` — 3840x2160p24, whitelist matched exactly (BenQ; the HP X27q offers no film-rate mode at all — question 15) |
| YCbCr 4:2:2 10-bit | `PASS` — `bus_format 200d YUYV10_1X20` |
| HDR10 | `PASS` — HDR10[2], BT.2020, `BT2020_YCC`, `30bit` |
| Physical PCM audio | `PASS` — operator: "Evet, ses geliyor" |
| xrun = 0 | `PASS` |
| EBADFD = 0 | **`PASS with a caveat`** — 4 occurred, 4 recovered, 0 unrecoverable, 0 audio dropouts. The count is not zero and cannot be: the vendor driver stops the PCM on every HDMI modeset. What is zero is unrecovered ones. |
| No gross A/V sync issue | `PASS` — 121 s wall = 121 s player; operator confirms lip sync |
| Seek / OSD / stop / replay stable | `PASS` |
| Kernel errors 0 | `PASS` — every class zero |
| Kodi picture matches MP1b reference | `PASS` — identical measured, identical seen |
| HDR film after video-plane EOTF | `PASS` — plane `EOTF=2`, `HDR10[2]`, NV15/10-bit retained |
| HDR player controls | `PASS` — GUI plane `EOTF=2`; OSD no longer moves the video state; operator: "en iyi görüntü" |
| Video-plane `EOTF` restored on stop | **`OPEN`** — GUI plane returns to 0, video plane stays latched at 2 while disabled |
| Cadence on this sink | `PASS with a caveat` — the panel offers no film-rate mode; the ~95 s hitch is removed by locking to the display clock, 3:2 judder remains |
| Horizontal shift when the GUI plane joins | **`OPEN`** — reported on both sinks; RK3588 cleared to register level, cause not yet located |

### Classification

**All of the gate's stated requirements are `PASS`.** The root cause —
userspace never programming Rockchip's atomic-only plane `EOTF` — is measured,
fixed on both planes, and physically confirmed by the operator on the state
that had been failing.

Two items are open and neither was in the gate's requirement list. Restoring
the video plane's `EOTF` on teardown is a hygiene defect with no measured
visual effect. The horizontal shift when the GUI plane joins is a live operator
complaint that the machine evidence cannot yet account for; it is carried
forward as the first thing the next session should instrument, not as something
this gate resolved.

The EBADFD caveat is stated rather than smoothed over. Four occurred and four
were recovered; a literal "EBADFD = 0" is unreachable while the driver stops
the PCM on every modeset, and the honest measure is that none survived.

### Windows + Kodi result

It looks correct on this monitor. That refutes the sink-limitation finding but
does not imply that Windows clamps mastering metadata: Kodi's Windows renderer
sends source mastering/CLL/FALL metadata, and its principal measured design
difference is an RGB Full PQ swap-chain. Matching that on RK3588 did not fix
the picture and broke the GUI overlay.

## Recommended next single Gate

That gate ran and passed; every one of its acceptance points was met and is
recorded in `gui-plane-eotf-ab.txt`.

**The next single Gate is to instrument the horizontal shift**, because it is
the only open item a viewer actually sees. Build a Writeback probe that
captures the composited VOP output with and without the GUI plane active and
differences the two: that separates a sink-side rescale from a VOP2
composition shift, which is the fork every remaining hypothesis hangs on. The
0008-only binary is already on the appliance as `kodi-gbm.pre0009`, so the
patch-attribution A/B can be run in the same session.

Restoring the video plane's `EOTF` on teardown is the smaller follow-up: add
the write to `CVideoLayerBridgeDRMPRIME::Disable()`, symmetrically with 0009's
GUI path. Acceptance is that after stop plane 73 reads `EOTF=0`, replay
restores `2`, the S1..S4 table is otherwise unchanged, and an SDR file played
straight after an HDR one is not tagged PQ.

`MA1` (compressed passthrough) no longer waits behind HDR picture quality,
which is now accepted; it waits only on whether the shift turns out to be ours.

The monitor hotplug observation is separate: Kodi retained the removed BenQ's
3840x2560 mode when the HP X27q was connected and the display stayed dark.
Restarting Kodi re-enumerated the EDID and restored 2560x1440p60. Treat a Kodi
restart as the current operational workaround; robust live-hotplug recovery is
a later, independent task.

The monitor is now the HP X27q at 2560x1440p60; the BenQ measurements earlier
in this report were taken at 3840x2560 and are kept as recorded rather than
restated for the new sink.
