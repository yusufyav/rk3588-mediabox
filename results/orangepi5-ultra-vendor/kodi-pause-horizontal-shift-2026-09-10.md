# Kodi pause/resume horizontal shift

Orange Pi 5 Ultra, vendor kernel 6.1.115. 2026-09-10.

The picture moves horizontally when Kodi's OSD appears over HDR video: a black
band opens at the left edge and content is lost off the right. It returns to
place when the OSD goes away. The gate's brief was to find the cause and
remove the behaviour without disturbing the picture quality accepted in Gate
0009.

**Classification: `PARTIAL_FAULT_DOMAIN_ISOLATED`.**

The triggering condition is found, reproduced without Kodi, and reduced to
three things that must hold together. It is not in Kodi, not in the television,
and not in plane geometry. It is in the RK3588 VOP2 HDR composition path.

Three fixes were built and physically tested, and all three were rejected:

* a Kodi change that tags the GUI plane traditional-HDR instead of PQ -- removes
  the displacement, gives back 0009's washed-out OSD blue;
* a kernel change that forces RGB mixing for HDR10 output -- built from the
  appliance's own kernel source and booted; does not remove the displacement
  at all;
* a Kodi workaround that keeps one tiny invisible non-PQ layer in the
  composition -- removes the displacement completely, 0 of 20 cycles, and gives
  back the washed-out OSD blue in both of its variants.

Every route that removes the displacement costs the OSD colour fix, and the one
route that preserves the colours does not remove the displacement. The
appliance is back on the accepted 0009 binary and the accepted kernel, with the
0009 baseline verified intact.

---

## 1. Root cause

The displacement needs three things to hold at once:

1. the video port drives an **HDR10 output**;
2. **two or more layers** are composited;
3. **every one of those layers is tagged `EOTF = 2`** (SMPTE ST 2084 / PQ).

Break any one of the three and the picture stays where it belongs. That is the
whole of what is established; the mechanism inside the VOP2 is not.

Kodi meets that condition because patch `0009-gbm-tag-hdr-composited-gui-plane-with-eotf`
calls `SetGuiPlaneEotf()`, which tags the GUI plane with `m_eotf` — PQ during
HDR10 playback — whenever the HDR GUI compositor is active. The GUI plane is
attached to the CRTC only while the OSD is drawn, so the condition switches on
and off exactly with the OSD, which is what makes the fault look like a
pause/resume behaviour.

It is not a pause behaviour. Pausing merely raises the OSD.

Two candidate mechanisms were proposed for *why* the hardware does this, and
direct measurement falsified both. They are recorded in sections 3 and 4
because each one cost a build and a reboot, and because the next attempt should
not start by repeating them.

## 2. How the fault domain was narrowed

Each step below removed a candidate. The order matters, because two of the
steps produced answers that later had to be thrown out.

### 2.1 Kodi never moves the video plane

The VOP2 register file and the committed DRM atomic state were sampled at
25 Hz across three full pause/resume cycles while the reference asset played.
Seven distinct states were recorded; the video window's geometry is
bit-identical in all of them:

```
Esmart0-win0   DSP_ST=002a0000  DSP_INFO=081b0eff  ACT_INFO=081f0eff
               SCL_CTRL=00000000 SCL_FACTOR_YRGB=00005564
               atomic: crtc=3840x2076+0+42  src=3840x2080+0+0
```

`DSP_ST=0x002a0000` is x=0, y=42. It never changes. Neither does any scaler
register, and neither does the video port's timing: `H: 3840 5116 5204 5500`
in both states.

Diffing the whole register file between playing and paused leaves three
changes: the GUI window (`Cluster0`) turning on, the mixer's alpha-blend
controls at `0xfdd90650`/`0x658` following it, and the video buffer pointer.
`playing` versus `resumed` differ only in a free-running status word and that
buffer pointer — every other bit is equal.

This retires the pre-research hypothesis the gate handed over. The concern was
that `UpdateVideoPlane()` writes only `FB_ID` and `CRTC_ID` on a repeated
frame, leaving geometry un-replayed. It does write only those two, and it does
not matter: the hardware holds the correct geometry throughout, so replaying it
would write the values already there. No patch was built for it.

`fdd90034` was briefly mistaken for a free-running counter; it tracks the GUI
plane's presence (`0x7f` off, `0x7e` on) and is a consequence, not a cause.

### 2.2 The television is not involved

The operator reproduced the fault on a second display, about fifty times, and
states the cause is in the source. That retires the entire sink branch — the
Pixel Shift / Display Area / Wide Mode A/B the gate's Phase 0 asks for cannot
explain a fault that follows the source to another panel, so it was not run.

Two sink-side hypotheses died with it. Static-image panel protection requires a
frozen picture, and the operator confirmed the displacement also happens with
the OSD raised **while the film is still playing**. Auto-framing and overscan
scale rather than translate, and the displacement bands only the left edge
while cutting the right.

### 2.3 A negative result that did not count

A synthetic reproduction was built to test the composition change without
Kodi: `tools/drm-writeback-probe` sets the mode, puts a static test pattern on
the video plane at Kodi's exact rectangle, and toggles a GUI-like plane on and
off with no modeset between. Writeback measured `dx = 0` across four
configurations, and the operator confirmed on the panel that nothing moved.

That arm was invalid. Checking the output state against Kodi's showed the
probe had been running a different pipeline entirely:

| | probe | Kodi |
| --- | --- | --- |
| `bus_format` | `RGB888_1X24` | `YUYV10_1X20` |
| `overlay_mode` | 0 | 1 |
| HDR | `SDR[0]` | `HDR10[2]` |

The probe was not asserting connector HDR state. Once it drives
`HDR_OUTPUT_METADATA` and `Colorspace` properly, its output matches Kodi's
exactly — `YUYV10_1X20`, `overlay_mode[1]`, `output_mode[9]`, `HDR10[2]`,
BT.2020 limited, 3840x2160p24 — and the fault reproduces. Connector properties
persist between processes, so the probe now asserts the SDR case as well
rather than leaving it unset.

### 2.4 A measurement that was withdrawn

In the corrected pipeline, the first captures appeared to show the ruler
displaced by exactly six pixels. That reading was withdrawn. The pattern in
use was periodic with a sixteen-pixel period, and only one row had been
inspected; a capture torn during readback still looks periodic on any single
row. Sampling five rows of the same capture showed the bar positions differ in
every row — the capture disagrees with itself, so no displacement is
recoverable from it.

`scripts/writeback-registration.py` now measures that self-agreement first and
refuses to report `dx` when a capture is torn. The pattern was also rebuilt to
be aperiodic, and low-frequency: a per-pixel random signature sits at Nyquist,
where the 10-bit YUV conversion alters the values themselves and the two
captures stop being shifted copies of one another.

### 2.5 The condition, isolated

With a trustworthy capture and an integrity check in front of it, the GUI
plane's EOTF tag is the only variable:

| GUI plane EOTF | capture self-agreement | `dx` |
| --- | --- | --- |
| 0 — SDR | 1.0000 | 0 |
| 1 — traditional HDR | 1.0000 | 0 |
| **2 — PQ / ST 2084** | **-0.0333, torn** | not measurable |
| 3 — HLG | 1.0000 | 0 |

The operator's eyes agree with the instrument, in both directions and with no
player running: at `EOTF=2`, "Evet, kayma var"; at `EOTF=0`, "Kayma kayboldu".

It is not bandwidth. The same split appears at 1920x1080, at a quarter of the
pixel rate: `EOTF=0` clean with `dx=0`, `EOTF=2` torn.

It also is not an output FIFO underflow, which was the attractive mechanism
for a frame that arrives late and lands to the right. 140 `POST_BUF_EMPTY`
events were in the ring buffer earlier in the session, but the counter is zero
in every controlled arm — including the `EOTF=2` arm that reproduces the
fault, the same arm with a writeback client added, and real Kodi HDR playback
with the OSD drawn. Those earlier events are unattributed and are not claimed
as the cause.

What VOP2 does differently is visible in its own classification of the plane.
Only `EOTF=2` moves the port into `overlay_mode[1]` and engages an RGB-to-YUV
conversion on the GUI plane:

| GUI EOTF | plane class | csc | `overlay_mode` |
| --- | --- | --- | --- |
| 0 | `SDR[0]` | mode[0], r2y[0] | 0 |
| 1 | `GAMMA HDR[1]` | mode[0], r2y[0] | 0 |
| 2 | `HDR10[2]` | mode[3], r2y[1] | **1** |
| 3 | `HLG[3]` | mode[0], r2y[0] | 0 |

Both halves are needed. Running the identical planes with a PQ-tagged GUI
plane but an SDR output leaves the plane at `csc mode[0]` and `overlay_mode 0`,
and nothing moves.

## 3. Rejected fix 1: tag the GUI plane traditional-HDR

The only lever inside Kodi is which EOTF that plane is tagged with. Kodi was
rebuilt with `SetGuiPlaneEotf()` downgrading PQ to `TRADITIONAL_HDR`, which
still tells the hardware the plane is not SDR — patch 0009's stated purpose —
without the PQ classification.

It works, and it costs too much. The displacement is gone; the washed-out OSD
blue that 0009 was accepted for fixing comes back. Video quality itself was
reported unchanged. Measured with the OSD drawn, it fails three of the gate's
five 0009 regression criteria:

| criterion | 0009 baseline | candidate |
| --- | --- | --- |
| `overlay_mode` | 1 | 0 — fail |
| video `csc mode` | 0 | 3, `y2r[1]` — fail |
| GUI EOTF | 2 | 1 — fail |
| video EOTF | 2 | 2 — pass |
| VP | `HDR10[2]` | `HDR10[2]` — pass |

`FIX_REJECTED`. The experiment was reverted on the target and no patch file
was added to `patches/kodi/`; the gate's commit policy is explicit that
rejected experiments do not stay on main.

Two other in-scope ideas were considered and not pursued. Keeping the GUI
plane permanently attached removes the transition but parks the picture in the
displaced position, which is worse than a fault that only appears with the
OSD. Moving the GUI to an Esmart window instead of a Cluster window could not
be measured cleanly — the reference captures in that arm were themselves
incoherent — and it is a substantial change, since Kodi's GUI plane is the EGL
scanout buffer's plane.

## 4. Rejected fix 2: force RGB mixing in the kernel

Only `EOTF=2` on the GUI plane put the port into YUV mixing — `overlay_mode 1`,
with an RGB-to-YUV conversion on that plane — and that was the configuration
that displaced the picture. The driver already forces RGB mixing whenever
sdr2hdr or hdr2sdr is active, so YUV mixing is reachable only in the all-PQ
corner. That made it the obvious mechanism.

The kernel was rebuilt to test it: `armbian/linux-rockchip` at
`fd9f82366e235b8afbdf516765210e97d24dce93`, the same commit and the same
cross toolchain that produced the kernel already on the appliance, with the
ScreenBridge HDMI-RX patch kept and one change added in `vop2_setup_hdr10()`:

```c
if (vop2->version == VOP_VERSION_RK3588 && vp->hdr_out)
        vcstate->yuv_overlay = false;
```

Installed beside the working kernel as
`6.1.115-vendor-rk35xx-screenbridge-hdmirx-audio-vop2rgb` and booted.

It does what it says: `overlay_mode` is 0 in both OSD states, the output stays
HDR10 / `YUYV10_1X20` / BT.2020, both planes keep `EOTF=2`, OSD colours and
video quality are unaffected. **The displacement is unchanged.** YUV mixing is
not the mechanism. Rolled back by re-pointing `/boot/Image`; both images remain
installed and `scripts/deploy-kernel.sh` switches between them.

Scan timing was measured across the same configurations and rules out the other
obvious candidate, the port's pre-scan delay:

| state | `pre_scan_htiming` | `BG_DLY` | picture |
| --- | --- | --- | --- |
| video plane only | `07b40058` | 53 | reference |
| video + GUI plane tagged PQ | `07b40058` | 53 | **displaced** |
| video + GUI plane tagged SDR | `07b40058` | 53 | in place |
| video + GUI PQ + extra SDR layer | `07b60058` | 55 | in place |

The first three are bit-identical and only the second displaces; the fourth is
the one row whose timing differs and it does not displace. Neither the scan
start nor the `bg_dly` the driver computes from the layer allocation explains
the fault.

## 5. Rejected fix 3: an invisible non-PQ sentinel layer

With both mechanisms falsified, the remaining lever above the kernel is the
condition itself: keep one layer in the composition that is not PQ.

Implemented in `CDRMAtomic` as a 4x4 fully transparent ARGB plane, attached only
while the GUI layer is composited and both the GUI and video planes read back
`EOTF=2`, parked at the video plane's own origin so it never depends on a
letterbox bar existing, and detached on stop or whenever the composition stops
being all-PQ.

Its invisibility is by construction rather than by being small. This SoC
reports `pixel blend mode` as `None=2, Pre-multiplied=0, Coverage=1` and the
planes sit at 0, Pre-multiplied, so an all-zero buffer contributes exactly
nothing. The operator confirmed nothing is visible where it sits.

It works, and it costs the same thing as fix 1:

| | |
| --- | --- |
| horizontal shift | **0 of 20** pause/resume, 0 of 10 OSD open/close |
| sentinel visible | no |
| display mode over 3 seeks | `3840x2160p24` throughout |
| stop / replay | detaches on stop, HDR10 restored on replay |
| `POST_BUF_EMPTY` | 0 |
| `drm *ERROR*` | 0 |
| Kodi atomic failures | 0 |
| TV HDR10 / video plane PQ / GUI plane PQ | all retained |
| `overlay_mode` | 0 (0009 baseline is 1) |
| **OSD colours** | **regressed — 0009's washed-out blue is back** |

Two tags were tried. `EOTF=0` puts the port on its sdr2hdr path, which is what
forces RGB mixing. `EOTF=3` was tried next because the driver counts only
non-PQ, non-HLG layers as an SDR layer:

```c
if (vpstate->eotf != HDMI_EOTF_SMPTE_ST2084 &&
    vpstate->eotf != HDMI_EOTF_BT_2100_HLG)
        have_sdr_layer = true;
```

so an HLG sentinel should have stayed out of that count and left the OSD colour
path alone. It did not — `overlay_mode` still dropped to 0 and the washed blue
came back.

`overlay_mode = 1` was one of the gate's PASS criteria and this workaround
cannot meet it: the mechanism that removes the displacement *is* the driver
switching away from YUV mixing. That was flagged before the work started. The
criterion that decided it was the colour, not the mode.

## 6. 0009 baseline, after the reverts

The target runs the `kodi-gbm.with0009` binary, byte-identical to the accepted
one, and the source tree carries the unmodified 0009 patch. Measured during
HDR playback with the OSD drawn:

```
overlay_mode[1] output_mode[9] HDR10[2] color-encoding[BT.2020] color-range[Limited]
Cluster0-win0   color: HDR10[2]   csc: y2r[0] r2y[1] csc mode[3]
Esmart0-win0    color: HDR10[2]   csc: y2r[0] r2y[0] csc mode[0]
                src: 3840x2080+0+0   dst: 3840x2076+0+42
```

All five criteria hold: `overlay_mode = 1`, video `csc mode[0]`, GUI EOTF 2,
video EOTF 2, VP `HDR10[2]`. No quality regression was introduced by this
gate.

## 7. Where this leaves the fault

The horizontal displacement is **not fixed**. What is fixed is the cost of
working on it: the condition is known, it reproduces on demand in under a
minute without Kodi, and `scripts/writeback-probe.sh` flips it on and off.

Two mechanisms are now ruled out by measurement rather than by argument — YUV
versus RGB mixing, and the port's pre-scan delay — and a kernel built against
the appliance's own source proves the first of those is not it. Whatever the
VOP2 does differently when it mixes two PQ layers for an HDR10 output is not
visible in the register file: plane geometry, scaler state, scan timing, layer
delays and the overlay mode are all identical between a frame that is displaced
and one that is not. Going further needs Rockchip's own documentation for that
block, not more black-box bisection.

The practical position for the appliance is a choice between two accepted
results, and it is the operator's to make:

* keep 0009 and live with the picture shifting while the OSD is on screen —
  what is installed now;
* take any of the three rejected fixes and live with a washed-out OSD blue.

Both are one command away. Nothing in this gate needs to be redone to switch.

## 8. Evidence

`logs/orangepi5-ultra-vendor/kodi-pause-horizontal-shift-2026-09-10/`

| file | what it holds |
| --- | --- |
| `kodi-pause-path.log`, `kodi-pause-marks.log` | 25 Hz VOP2 geometry watch across three pause/resume cycles, with the transport commands timestamped against the same clock |
| `state-{playing,paused,resumed}.txt` | full DRM/VOP2 state in each transport state |
| `vop-regs-{playing,paused,resumed}.txt` | raw VOP2 register dumps |
| `vop-regs-diff-*.txt` | the register differences, reduced to the three that exist |
| `writeback-capabilities.txt` | writeback connector, its CRTC set, its formats |
| `writeback-registration.txt` | `dx` per GUI-plane EOTF at 4K and at 1080p, with self-agreement scores |
| `writeback-raw/*.nv12.gz` | the decisive captures, raw NV12, 3840x2160, pitch 3840 |
| `vop-eotf-classification.txt` | how VOP2 classifies the plane for each EOTF |
| `output-pipeline-sdr-vs-hdr.txt` | the control that invalidated the first negative result |
| `vop-underflow-vs-eotf.txt` | `POST_BUF_EMPTY` against every arm, and why it is not the mechanism |
| `kodi-eotf1-candidate.txt` | the rejected candidate, measured against the 0009 criteria |
| `operator-observations.txt` | the physical reports, kept separate from the instrument readings |
| `scan-timing-vs-layers.txt` | `pre_scan_htiming` and `bg_dly` per layer configuration |
| `kernel-rgb-composition-attempt.txt` | the kernel build, what it changed, and what it did not fix |
| `sentinel-workaround.txt` | the sentinel's design, invisibility proof, and the full acceptance matrix result (20 pause/resume, 10 OSD toggles, 3 seeks, stop/replay) |
| `dmesg-delta.txt`, `SHA256SUMS` | kernel log, checksums |
