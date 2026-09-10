# Kodi pause/resume horizontal shift

Orange Pi 5 Ultra, vendor kernel 6.1.115. 2026-09-10.

The picture moves horizontally when Kodi's OSD appears over HDR video: a black
band opens at the left edge and content is lost off the right. It returns to
place when the OSD goes away. The gate's brief was to find the cause and
remove the behaviour without disturbing the picture quality accepted in Gate
0009.

**Classification: `PARTIAL_FAULT_DOMAIN_ISOLATED`.**

The cause is found, reproduced without Kodi, and reduced to a single binary
condition. It is not in Kodi, not in the television, and not in plane geometry.
It is in the RK3588 VOP2 HDR composition path, which this gate is forbidden to
patch. The one lever available inside Kodi was built and physically tested; it
removes the displacement by giving up the OSD colour fix that Gate 0009 was
accepted for, so it was rejected and reverted. The appliance is back on the
0009 binary with the 0009 baseline verified intact.

---

## 1. Root cause

With the video port in HDR10 output, **tagging a second, composited plane
`EOTF = 2` (SMPTE ST 2084 / PQ) displaces the whole output frame
horizontally.** Any other EOTF tag on that plane — SDR, traditional HDR, HLG —
leaves the frame where it belongs.

Kodi meets that condition because patch `0009-gbm-tag-hdr-composited-gui-plane-with-eotf`
calls `SetGuiPlaneEotf()`, which tags the GUI plane with `m_eotf` — PQ during
HDR10 playback — whenever the HDR GUI compositor is active. The GUI plane is
attached to the CRTC only while the OSD is drawn, so the condition switches on
and off exactly with the OSD, which is what makes the fault look like a
pause/resume behaviour.

It is not a pause behaviour. Pausing merely raises the OSD.

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

## 3. The candidate fix, and why it was rejected

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

## 4. 0009 baseline, after the revert

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

## 5. What this gate did not do

The horizontal displacement is **not fixed**. It is understood, reproducible
on demand in under a minute without Kodi, and reduced to one property value.
The fix belongs in the VOP2 driver's handling of a PQ-tagged plane in an
HDR10 composition, and kernel, DT and PHY patches are outside this gate.

The 20-cycle stability run the gate's PASS criteria call for was not run:
with the accepted binary restored, the fault is present by construction, so
the run could only confirm what is already established.

Two things are worth carrying forward. The reproduction is now a tool rather
than a procedure — `tools/drm-writeback-probe run --hdr --hdr-out --nv12
--gui --gui-eotf {0,2}` flips the fault on and off — which is what a driver
investigation needs. And the writeback path itself is only trustworthy in the
arms where it agrees with itself; the integrity check exists because it caught
a wrong answer that had already been reported.

## 6. Evidence

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
| `dmesg-delta.txt`, `SHA256SUMS` | kernel log, checksums |
