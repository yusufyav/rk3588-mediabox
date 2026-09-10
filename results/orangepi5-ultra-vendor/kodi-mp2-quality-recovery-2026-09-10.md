# Gate MP2 — quality recovery

Orange Pi 5 Ultra, vendor kernel 6.1.115. 2026-09-10.

Gate MP2 stopped with three open problems: a GUI on software rendering, a
colour chain whose three descriptions of one signal disagreed, and a Kodi
ALSA sink throwing `snd_pcm_writei -77`. All three are closed. Two of the
three turned out to have a different cause than the one recorded.

**The display changed under this gate and that limits one conclusion.** The
sink is now a **BenQ RD280UG**, a 28" productivity monitor, not the Sony
BRAVIA KD-65XE9005 that Gates MP1a and MP1b were judged on. It reaches
3840x2160@23.976, advertises BT.2020 and ST2084, and reports itself in HDR
mode, so every *signal* requirement is testable and was tested. HDR *picture
quality* is not judgeable on it — see question 9.

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

## 9. Does Kodi's picture match the raw MP1b reference?

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

**Kodi is therefore not a regression — it reproduces the proven baseline
exactly.** The washed-out HDR appearance belongs to the sink: a 28"
productivity monitor that advertises ST2084 and reports HDR mode, but has
neither the peak luminance nor the tone mapping to render a PQ grade, and PQ
shown flat is exactly "washed out".

`FAULT DOMAIN: sink (BenQ RD280UG HDR10 rendering)`, not the RK3588 pipeline
and not Kodi. Re-judging HDR picture quality needs the Sony back on the input;
that is a sink swap, not a code change. The operator is separately checking
the same file on the same monitor from Windows + Kodi, which will confirm or
refute this independently.

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

## What was NOT done, and why

* **HDR picture-quality judgement.** Deferred, with a reason: the sink cannot
  render HDR10 well enough to judge by. The signal is proven correct and
  identical to the MP1b reference; the picture is not judgeable here.
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
| 4K 23.976 | `PASS` — 3840x2160p24, whitelist matched exactly |
| YCbCr 4:2:2 10-bit | `PASS` — `bus_format 200d YUYV10_1X20` |
| HDR10 | `PASS` — HDR10[2], BT.2020, `BT2020_YCC`, `30bit` |
| Physical PCM audio | `PASS` — operator: "Evet, ses geliyor" |
| xrun = 0 | `PASS` |
| EBADFD = 0 | **`PASS with a caveat`** — 4 occurred, 4 recovered, 0 unrecoverable, 0 audio dropouts. The count is not zero and cannot be: the vendor driver stops the PCM on every HDMI modeset. What is zero is unrecovered ones. |
| No gross A/V sync issue | `PASS` — 121 s wall = 121 s player; operator confirms lip sync |
| Seek / OSD / stop / replay stable | `PASS` |
| Kernel errors 0 | `PASS` — every class zero |
| Kodi picture matches MP1b reference | `PASS` — identical measured, identical seen |
| **HDR picture quality** | **`NOT_JUDGEABLE_WITH_CURRENT_SINK`** |

### Classification

**`MP2 = PASS`**, with one item deferred rather than failed.

Every machine-checkable requirement is met. The one requirement that is not
answered — "does HDR look right" — is not answerable on this display, and the
reason is proven rather than assumed: the raw MP1b reference, which *is* the
proven-good baseline, looks exactly as washed out as Kodi on this monitor.
That is a sink limitation, and it is the same limitation for any source.

The EBADFD caveat is stated rather than smoothed over. Four occurred and four
were recovered; a literal "EBADFD = 0" is unreachable while the driver stops
the PCM on every modeset, and the honest measure is that none survived.

### If the operator's Windows + Kodi test on this monitor also looks washed out

That confirms the sink-limitation finding independently and closes the
question. If it instead looks correct, the finding is wrong and the next step
is to compare the HDR metadata blob and quantisation-range signalling between
the two sources — not to change anything in the pipeline, which has already
been shown to match its own reference.

## Recommended next single Gate

**Re-run the MP1b/MP2 picture-quality judgement on the Sony BRAVIA.**

Not a code gate. Every signal-level requirement is already proven; what is
missing is a display that can render HDR10 well enough to judge picture
quality on, and that is the only thing blocking a full MP2 sign-off. It is
also the cheapest possible gate: reconnect the TV, play the same asset at the
same timestamp, compare against `kodi-vs-mp1b.txt`.

Hold `MA1` (compressed passthrough) and Stremio until that is closed, per the
gate's own stopping rule.
