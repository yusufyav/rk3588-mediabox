# Gate MP1b-CSC: plane `COLOR_ENCODING` A/B on Orange Pi 5 Ultra

Date: 2026-09-09
Target: `root@10.27.27.25` (`orangepi5-ultra`, RK3588 OPi 5 Ultra)
Sink: Sony BRAVIA `KD-65XE9005` at `10.27.27.51`, HDMI 1, `cinemaHome`
Asset: *Past Lives* (2023), 4K23.976 HEVC Main 10 HDR10, scene at 20:00, 120 s

## 1. Executive summary

**GATE MP1b-CSC RESULT: `INCONCLUSIVE_VISUAL`.**
**CSC hypothesis: `UNRESOLVED`.**
**MP1b final classification: `PARTIAL` (`PARTIAL_FIDELITY`) — unchanged.**

The gate asked one question: *if the active video plane is explicitly set to
`COLOR_ENCODING = ITU-R BT.2020 YCbCr`, does real HDR10 picture fidelity
improve?* It produced a clean answer to the **mechanical half** of that
question and no usable answer to the **perceptual half**.

The mechanical half is settled, and it is a new fact:

> The DRM plane property is **not** cosmetic. Requesting
> `ITU-R BT.2020 YCbCr` on plane 73 is accepted by an atomic `TEST_ONLY`,
> reads back as `ITU-R BT.2020 YCbCr (2)`, **and VOP2 actually reprograms the
> window**: debugfs moves from `color-encoding[BT.601]` / `csc mode[0]` to
> `color-encoding[BT.2020]` / `csc mode[3]`.

Gate MP1b's §23 laid out three outcomes. The one this rules out is Durum 3
(property accepted but VOP2 ignoring it): the property *is* applied, and a
different CSC matrix *is* loaded. What remains unanswered is whether that
different matrix is visible on this content.

The perceptual half did not resolve. The operator, watching the panel across
both 120-second runs, reported that they could not reliably tell the two
apart — explicitly **not** "they looked the same", but "I was not in a
position to judge the difference". Classified as `INCONCLUSIVE`, which is a
statement about the measurement protocol, not about the picture. Because the
gate's own rule is that a fidelity verdict without a usable visual result is
not a verdict, **no fidelity conclusion is drawn and MP1b stays `PARTIAL`.**

The A/B itself was as clean as this kind of experiment gets. The entire
difference between the two runs, across every instrument, is two lines of
debugfs:

```diff
-	color: SDR[0] color-encoding[BT.601] color-range[Limited]
+	color: SDR[0] color-encoding[BT.2020] color-range[Limited]
-	csc: y2r[1] r2y[0] csc mode[0]
+	csc: y2r[1] r2y[0] csc mode[3]
```

Everything else — asset bytes, mode, plane, format, modifier, placement,
connector `Colorspace`, `color_depth`, `HDR_OUTPUT_METADATA` blob content,
HDMI `bus_format`, video-port HDR state, cadence, kernel log, TV picture
mode and TV HDR entry — is identical between A and B. The `COLOR_RANGE`
property was never written in either run, as the gate requires.

| Dimension | A (default) | B (BT.2020) | Verdict |
| --- | --- | --- | --- |
| `TEST_ONLY` before modeset | `0 (ACCEPTED)` | `0 (ACCEPTED)` | not blocked |
| plane `COLOR_ENCODING` readback | `BT.601 (0)` | `BT.2020 (2)` | **the variable** |
| debugfs window encoding | `BT.601`, `csc mode[0]` | `BT.2020`, `csc mode[3]` | **applied** |
| HDMI `bus_format` | `YUYV10_1X20` | `YUYV10_1X20` | unchanged |
| VP0 HDR state | `HDR10[2]` / `BT.2020` | `HDR10[2]` / `BT.2020` | unchanged |
| Cadence | 2852 / 0 / 0 / 0 | 2852 / 0 / 0 / 0 | unchanged |
| TV HDR entry | yes | yes | unchanged |
| Operator visual | — | — | **`INCONCLUSIVE`** |

Nothing was installed on the target. No kernel, DTB, bootloader, boot
configuration or service was changed. The board was not rebooted. Only
read-only getters were called on the TV; the operator set `cinemaHome`
themselves, before both runs, and it was held there throughout.

**Audio status: `NOT IMPLEMENTED / NOT TESTED`.** See §19.

## 2. Repository revision

| Item | Value |
| --- | --- |
| Repository | `yusufyav/rk3588-mediabox`, branch `main` |
| Parent commit at gate start | `aebb157` (`docs: add MP1b real HDR10 playback results`) |
| Working tree at gate start | clean |
| Implementation branch | `mp1b-csc`, in a separate `git worktree` |
| Changed files | `src/drm/display.{h,cpp}`, `tools/hdr-playback-probe.cpp`, `tests/run-host-tests.sh` |
| Diff size | 4 files, +276 / −15 |
| Build | CMake, C++17, built on the target, no warnings |

`tools/hdr-signaling-probe.cpp` (Gate MP1a) is untouched, so the source hash
published in the MP1a report still stands.

## 3. Remote / main state

The previous handoff reported the push state inconsistently, so it was
verified from the remote rather than from any narrative:

```text
git rev-parse HEAD          aebb157450d4990395a61166240459c90887b1c4
git rev-parse origin/main   aebb157450d4990395a61166240459c90887b1c4
git fetch origin            (no change)
git status --short          (empty)
```

`HEAD` and `origin/main` were already identical at gate start and the tree was
clean: **`main` was not behind, and nothing from MP1b was unpushed.** The
earlier textual inconsistency was a reporting artefact, not a repository state.

## 4. Screenbridge integrity

`~/Projeler/rk3588-screenbridge` was read for provenance only and never
written. Verified at gate start and gate end:

| Check | Start | End |
| --- | --- | --- |
| `git rev-parse HEAD` | `582e1d30c6762c134118445bf660c0784aaffe58` | `582e1d30c6762c134118445bf660c0784aaffe58` |
| `git status --short` | empty | empty |

Matches the expected HEAD. **Integrity intact.**

## 5. Asset identity

The same eMMC copy MP1b used, re-verified rather than assumed:

```text
f4e32b8d3feb7efdebaaefaf7338a25bcc86ee144bf82bd4b93da6b80906f96a  /var/tmp/mp1b/past-lives.mkv
12081238172 bytes
```

Identical to the SHA-256 published in the MP1b report (§7), which was itself
verified against the operator's USB source. `ffprobe` output was captured
separately for both runs and is **byte-identical** between them
(`diff -q` clean): container `matroska,webm`, `hevc` / `Main 10`,
3840x2080, `yuv420p10le`, `24000/1001` fps, `color_range=tv`,
`color_primaries=bt2020`, `color_transfer=smpte2084`, `color_space=bt2020nc`,
mastering display DCI-P3 / D65 / 0.005–4000 cd/m², MaxCLL/MaxFALL 401/77.

## 6. A/B invariant matrix

Held identical across both runs, and verified from the run logs rather than
from the intent:

| Invariant | Value in both A and B |
| --- | --- |
| Asset file | `/var/tmp/mp1b/past-lives.mkv` (eMMC) |
| Asset SHA-256 | `f4e32b8d…f96a` |
| Scene start | `--start 1200` (20:00) |
| Duration | `--duration 120` |
| Decoder | `hevc_rkmpp`, DRM PRIME |
| Frame format | `NV15`, modifier `0x0`, 3840x2080 |
| Mode | index 13, `3840x2160`, clock 296703, refresh **23.9760 Hz**, delta `-0.0000` |
| Plane | id **73**, type `Cursor`, forced with `--plane 73` |
| Placement | `src=3840x2080+0+0 dst=3840x2080+0+40`, scaling **none** |
| Connector `Colorspace` | requested `BT2020_YCC`, actual `BT2020_YCC (10)` |
| Connector `color_depth` | requested `30bit`, actual `30bit (10)` |
| `HDR_OUTPUT_METADATA` | blob content byte-identical (raw hex below) |
| Plane `COLOR_RANGE` | **never written**; readback `YCbCr limited range (0)` |
| TV picture mode | `cinemaHome`, before / during / after, both runs |
| TV input, HDMI path, brightness/HDR settings | unchanged |

The HDR blob content is identical; only the kernel's allocation handle
differs (277 in A, 279 in B), which is a per-run object id, not content:

```text
hdr_metadata blob_size=32 metadata_type=0 eotf=2 infoframe_metadata_type=0
hdr_metadata display_primaries(ST2086 order G,B,R)=[(13250,34500) (7500,3000) (34000,16000)] white_point=(15635,16450)
hdr_metadata max_display_mastering_luminance=4000 min_display_mastering_luminance=50 max_cll=401 max_fall=77
hdr_metadata raw=00 00 00 00 02 00 c2 33 c4 86 4c 1d b8 0b d0 84 80 3e 13 3d 42 40 a0 0f 32 00 91 01 4d 00 00 00
```

### 6.1 Two deliberate deviations from MP1b, both neutral

* **`--plane 73` was forced in both runs.** MP1b let the probe pick the first
  NV15-capable plane, which was 73. Forcing it removes a failure mode specific
  to this gate: had the BT.2020 `TEST_ONLY` been rejected on plane 73, an
  unforced search would have moved to a *different plane* and the A/B would
  have silently become two-variable. Same plane is selected either way.
* **The probe now restores the plane's `COLOR_ENCODING` on exit.** Atomic
  plane state is sticky exactly as the connector's is. Without this, run B
  would leave plane 73 at BT.2020 and any later "default" run — including the
  confirmation repeat this gate's own decision table calls for — would be
  sitting on BT.2020 while claiming to be the A leg. Run A's readback
  confirms it worked: it found and reported `BT.601 (0)`, not B's value.

## 7. `COLOR_ENCODING` property discovery

Discovered at runtime from the plane object; no property id and no enum value
is hard-coded anywhere in the source (guarded by a host test):

```text
plane 73 property COLOR_ENCODING  id=75 value=0
  enums: ITU-R BT.2020 YCbCr=2  ITU-R BT.601 YCbCr=0  ITU-R BT.709 YCbCr=1
plane 73 property COLOR_RANGE     id=76 value=0
  enums: YCbCr full range=1  YCbCr limited range=0
```

`ITU-R BT.2020 YCbCr` is present, so the gate is not `BLOCKED_DISPLAY` on
enum availability. The CLI carries the DRM *enum name*, and the value behind
it is resolved against this map at runtime.

## 8. A run — `TEST_ONLY` and commit

```text
plane COLOR_ENCODING request: <default, property left untouched>
plane candidate id=73 type=Cursor color_encoding_requested=<default, untouched> test_only=ACCEPTED
TEST_ONLY plane=73 color_encoding=<default, untouched> color_range=<not set> result=0 (ACCEPTED)
atomic modeset committed
PLAYBACK RESULT: PASS
```

## 9. B run — `TEST_ONLY` and commit

```text
plane COLOR_ENCODING request: ITU-R BT.2020 YCbCr
plane candidate id=73 type=Cursor color_encoding_requested=ITU-R BT.2020 YCbCr test_only=ACCEPTED
TEST_ONLY plane=73 color_encoding=ITU-R BT.2020 YCbCr color_range=<not set> result=0 (ACCEPTED)
atomic modeset committed
PLAYBACK RESULT: PASS
```

The `TEST_ONLY` runs against the exact property set that is about to be
committed, and a non-zero return would have ended the run as
`BLOCKED_DISPLAY` with the kernel's errno and no modeset attempted. It
returned 0, so no such block occurred.

## 10. Requested vs readback

Sampled twice per run — after the modeset and after playback — and identical
at both points.

| Property | A requested | A actual | B requested | B actual |
| --- | --- | --- | --- | --- |
| plane `COLOR_ENCODING` | *(untouched)* | `ITU-R BT.601 YCbCr (0)` | `ITU-R BT.2020 YCbCr` | **`ITU-R BT.2020 YCbCr (2)`** |
| plane `COLOR_RANGE` | *never set* | `YCbCr limited range (0)` | *never set* | `YCbCr limited range (0)` |
| `Colorspace` | `BT2020_YCC` | `BT2020_YCC (10)` | `BT2020_YCC` | `BT2020_YCC (10)` |
| `color_depth` | `30bit` | `30bit (10)` | `30bit` | `30bit (10)` |
| `HDR_OUTPUT_METADATA` | blob 277 | blob 277 | blob 279 | blob 279 |
| `color_format` | *not set by probe* | `ycbcr444 (1)` | *not set by probe* | `ycbcr444 (1)` |

`color_format` reads `ycbcr444` while the link runs 4:2:2 in both runs. As MP1b
recorded, it is a request channel, not a status channel; `bus_format` in
debugfs is the only truth about the wire.

## 11. Debugfs A/B diff

Video Port 0, mid-run, with buffer addresses stripped (they differ per run by
construction). This is the **complete** diff:

```diff
--- a-default-11-vp0-during.txt
+++ b-bt2020-11-vp0-during.txt
@@ -10,9 +10,9 @@
     Esmart0-win0: ACTIVE
 	win_id: 8
 	format: NV15 little-endian (0x3531564e) pixel_blend_mode[0] glb_alpha[0xff]
-	color: SDR[0] color-encoding[BT.601] color-range[Limited]
+	color: SDR[0] color-encoding[BT.2020] color-range[Limited]
 	rotate: xmirror: 0 ymirror: 0 rotate_90: 0 rotate_270: 0
-	csc: y2r[1] r2y[0] csc mode[0]
+	csc: y2r[1] r2y[0] csc mode[3]
 	zpos: 11
 	src: pos[0, 0] rect[3840 x 2080]
 	dst: pos[0, 40] rect[3840 x 2080]
```

Reproduced identically at the second mid-run sample (`12-summary-during2`), so
it is steady state and not a transient.

### 11.1 This is the gate's most important finding

MP1b's §23 said the outcome would be decisive either way, and on the
mechanical axis it was:

* the **label** changed (`BT.601` → `BT.2020`), and
* the **CSC mode changed with it** (`csc mode[0]` → `csc mode[3]`).

The second is what makes this more than a cosmetic property write. `csc mode`
is VOP2's selector for which YUV→RGB coefficient set the window loads. It
moved, so the hardware is genuinely running a different matrix on the same
pixels. The "DRM property accepted but VOP2 ignores it" failure (MP1b §23's
Durum 3) **did not occur** and is now ruled out.

Note also what did *not* change: `SDR[0]`. The window is still tagged SDR
while the video port above it is `HDR10[2]`. `COLOR_ENCODING` addresses the
matrix, not the window's HDR/SDR tag; those are separate pieces of VOP2 state
and only the first has a standard DRM property. This is the leading remaining
uncertainty (§20).

## 12. HDMI bus-format comparison

| Field | A | B |
| --- | --- | --- |
| `bus_format` | `[200d]: YUYV10_1X20` | `[200d]: YUYV10_1X20` |
| `overlay_mode` / `output_mode` | `[0]` / `[9]` | `[0]` / `[9]` |
| VP0 HDR state | `HDR10[2]` | `HDR10[2]` |
| VP0 colour encoding | `BT.2020` | `BT.2020` |
| VP0 colour range | `Limited` | `Limited` |
| Display mode | `3840x2160p24` | `3840x2160p24` |
| `dclk` | `296703 kHz` | `296703 kHz` |

The wire state is unchanged: YCbCr 4:2:2 10-bit, BT.2020, HDR10, same pixel
clock. **The A/B is therefore genuinely single-variable and the fidelity
comparison is valid** by this gate's own §"HDMI state" rule.

## 13. TV state comparison

Read-only Sony IP-control getters. Nothing on the TV was changed by this gate.

```text
run        phase     pictureMode  autoPictureM  xtendedDynam    brightness      contrast       hdrMode    colorSpace         lightSensor
a-default  before     cinemaHome           off        medium            35           100          auto          auto           available
a-default  during     cinemaHome           off          high            50           100          auto          auto         UNAVAILABLE
a-default  after      cinemaHome           off        medium            35           100          auto          auto           available
b-bt2020   before     cinemaHome           off        medium            35           100          auto          auto           available
b-bt2020   during     cinemaHome           off          high            50           100          auto          auto         UNAVAILABLE
b-bt2020   after      cinemaHome           off        medium            35           100          auto          auto           available
```

Both runs show the same three-field HDR differential MP1b established
(`xtendedDynamicRange` medium→high, `brightness` 35→50, `lightSensor`
available→unavailable) and both revert cleanly. **TV HDR10 state: active in
both A and B, identically.** This was not a HDR-trigger test and it did not
need to be; it is recorded to prove the sink was in the same state for both
halves of the visual comparison.

The operator set `cinemaHome` before run A, at this gate's request, and it
held through both runs and both resets.

## 14. Cadence comparison

| Run | decoded | presented | dropped | repeated | late | flip err | timeouts | eff. rate |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| MP1b `emmc-cinema` (ref) | 2876 | 2852 | 0 | 0 | 0 | 0 | 0 | 23.9763 |
| **A `a-default`** | 2876 | **2852** | **0** | **0** | **0** | **0** | **0** | **23.9763** |
| **B `b-bt2020`** | 2876 | **2852** | **0** | **0** | **0** | **0** | **0** | **23.9763** |

Frame interval, in ms (ideal 41.708 at 23.976 fps):

| Run | mean | p50 | p95 | p99 | min | max |
| --- | --- | --- | --- | --- | --- | --- |
| MP1b ref | 41.708 | 41.707 | 41.766 | 41.789 | 41.560 | 41.850 |
| A | 41.708 | 41.707 | 41.773 | 41.795 | 41.584 | 41.870 |
| B | 41.708 | 41.707 | 41.774 | 41.800 | 41.565 | 41.845 |

B is not worse than A: p95 differs by 0.001 ms and p99 by 0.005 ms, both far
inside run-to-run noise, and B's max is actually lower. Both runs reproduce
MP1b's published figures exactly on the integer counters. `wall_span` is
118.909 s for all three. **Cadence: unchanged. Not a `RUN_INVALID`.**

## 15. dmesg comparison

Per-run deltas taken around each run and filtered on
`drm|vop|vop2|hdmi|dw-hdmi|phy|mpp|rkvdec|iommu|underflow|timeout|error|warning`.

| Run | delta lines | error / underflow / timeout / fail matches |
| --- | --- | --- |
| A | 36 | **0** |
| B | 36 | **0** |

With timestamps stripped the two deltas are **structurally identical** — the
same 36 lines in the same order. Both are the ordinary modeset sequence: vop
disable → `Update mode to 3840x2160p24` → hdmiphy PLL lock → `final tmdsclk =
296703000` → tmds mode → vop enable, then the mirror-image return to
1920x1080p60 on reset. No fatal, atomic or decode errors in either.

## 16. User visual A/B result

The operator watched both runs on the panel, told which was which:

* A = default plane encoding (first run)
* B = BT.2020 plane encoding (second run)

Reported: they could not tell the two apart — and, when asked to separate
"they looked the same" from "I could not judge reliably", chose the second:
*"güvenilir değerlendiremedim"*.

```text
Visual A/B classification: INCONCLUSIVE
```

This is deliberately **not** recorded as `NO_VISIBLE_DIFFERENCE`. The two are
different findings and they lead to different next steps, so the distinction
was put to the operator explicitly rather than inferred from a shrug.

The likely reason is protocol, not perception. The two runs were **serial and
~4 minutes apart**, with a full modeset, an SDR reset and a TV HDR
re-entry between them. Human colour memory across that gap is poor, and the
error under test — BT.601 coefficients applied to BT.2020-primaries content —
is a matrix error whose magnitude on naturalistic, muted, largely low-saturation
material is small. The 20:00 scene of *Past Lives* is exactly that kind of
material. A serial A/B was the wrong instrument for this size of effect, and
that is a limitation of this gate's design, not of the operator.

## 17. CSC hypothesis verdict

```text
CSC hypothesis: UNRESOLVED
```

Split by axis, because the two halves genuinely resolved differently:

| Axis | Verdict | Evidence |
| --- | --- | --- |
| Is the DRM property applied by VOP2? | **CONFIRMED — yes** | readback `BT.2020 (2)`; debugfs `color-encoding[BT.2020]`; `csc mode[0]`→`[3]` |
| Was MP1b's window CSC really on BT.601? | **CONFIRMED — yes** | A run readback `BT.601 (0)`, debugfs `BT.601`, `csc mode[0]` |
| Does correcting it change the picture? | **UNRESOLVED** | operator `INCONCLUSIVE` |

Against MP1b §23's decision table this is **none of Durum 1–5**. It is not
Durum 3 (property applied, so no property/VOP2 mismatch), not Durum 4 (no
`TEST_ONLY` failure), not Durum 5 (cadence, HDR state and bus format all
identical), and not Durum 2 — Durum 2 requires the operator to report *no
visible difference*, and they reported that they could not judge. The gate's
own rule is that a fidelity result is not interpreted without a usable visual
verdict, so no fidelity conclusion is drawn.

What the gate did buy, and it is not nothing: the mechanism is now known to
work end to end, and the one-property fix is implemented, tested and merged.
Any future attempt at this question can now run B without re-deriving it.

## 18. MP1b final classification

```text
MP1b = PARTIAL   (classification: PARTIAL_FIDELITY)   — unchanged
```

No `PASS` is claimed. Per this gate's rule, `MP1b PASS` requires an operator
visual verdict and there is none. MP2 / Kodi remains not recommended.

## 19. Audio status

```text
Audio status: NOT IMPLEMENTED / NOT TESTED
```

Audio was not brought up, implemented, configured or tested in any form. No
ALSA pipeline, no HDMI PCM, no AC3/EAC3/DTS/TrueHD/DTS-HD MA, no Atmos
passthrough, no IEC61937, no ELD read, no AV-sync work, no Kodi audio, no AVR
capability probing. The probe demuxes the container but selects only the video
stream and outputs no audio whatsoever. Nothing in this report's fidelity
assessment involves sound.

A separate gate is proposed for it — **`MA0 — HDMI Audio Bring-up`** — and,
per this task's scope freeze, **it was not started.**

## 20. Remaining uncertainties

1. **The window is still tagged `SDR[0]` while VP0 runs `HDR10[2]`.** This did
   not move, because `COLOR_ENCODING` does not address it. VOP2 tracks an
   HDR/SDR flag per window, and if the window is treated as SDR the port may
   be applying an SDR→HDR conversion the content does not want. There is no
   standard DRM property for this; it is now the strongest surviving candidate
   and it is a *different* mechanism from the one this gate tested.
2. **Whether the BT.601→BT.2020 matrix error is visible at all on this
   content.** Unmeasured. The 20:00 scene is muted and naturalistic, which is
   the worst case for detecting a matrix error by eye.
3. **Serial A/B is too weak an instrument for effects of this size.** Proven by
   this gate, at the cost of this gate.
4. **`color_format` reports `ycbcr444` while the wire runs 4:2:2.** Carried
   over from MP1b, still a request-vs-status trap, still unexplained.
5. **`csc mode[3]` semantics are not documented here.** It is known to be a
   different coefficient set from `[0]`; which exact matrix RK3588 loads for
   mode 3 was not verified against the vendor driver source, and this gate did
   not read kernel source to find out.
6. **The TV's own processing is uncontrolled.** `hdrMode=auto` and
   `colorSpace=auto` were held constant but not pinned, and BRAVIA's internal
   handling could mask a source-side matrix difference.

## 21. Recommended next single experiment

**Run the same A/B again, but as an interleaved, blind, short-cycle
comparison on a chroma-stressing scene.**

Concretely: keep everything this gate fixed, and change only the *protocol*:

* alternate A and B every ~20 seconds for several cycles instead of two
  120-second blocks four minutes apart, so the comparison is against
  short-term memory rather than long-term;
* do not tell the operator which leg is running (the probe logs it; the
  operator does not see it), and ask them to call "1" or "2" per cycle, then
  score the calls against the log; and
* pick a scene with saturated colour — strong reds, skin against a saturated
  background, or neon — because a BT.601-vs-BT.2020 matrix error grows with
  chroma magnitude and is near-zero on the muted 20:00 scene this gate used.

This is one experiment, it reuses the `--plane-color-encoding` flag exactly as
built, and it is the cheapest way to convert `INCONCLUSIVE` into a real
`CONFIRMED` or `REJECTED`. It needs a small runner change to alternate legs
within one process; it needs no new probe capability.

If that comes back `REJECTED`, the next fault domain is uncertainty 1 above —
VOP2's per-window `SDR[0]` tag under an `HDR10[2]` video port — which is a
different mechanism and would be its own gate.

Explicitly **not** recommended next: Kodi, mpv, Mesa/`libmali`/GBM, Stremio,
audio (see §19), CEC, HDR10+, Dolby Vision, `COLOR_RANGE` experiments, and any
kernel, device-tree, PHY or boot configuration change.

## 22. Evidence paths

Raw evidence: `logs/orangepi5-ultra-vendor/mp1b-plane-color-encoding-ab-2026-09-09/`

| File | Contents |
| --- | --- |
| `00-asset-sha256.txt` | eMMC asset checksum and size, re-verified this gate |
| `00-cli-parser.txt` | `--plane-color-encoding` parser, exercised on the target binary |
| `smoke-b-*` | 30 s technical pre-check of the B leg, run before the operator was involved |
| `a-default-*` | run A: ffprobe, TV state before/during/after, dmesg, debugfs, playback log, reset |
| `b-bt2020-*` | run B: the same set |
| `*-11-vp0-during.txt` | Video Port 0 extract used for the A/B diff |
| `40-tv-state-ab.txt` | TV state comparison table |
| `41-debugfs-ab-diff.txt` | the complete debugfs A/B diff |
| `42-dmesg-ab.txt` | dmesg delta comparison |
| `43-cadence-ab.txt` | cadence lines for A, B and the MP1b reference |
| `run-summary.txt` | one line per run: label, result, exit code |
| `SHA256SUMS` | checksums for every file above |

### 22.1 Why a `smoke-b` run exists

Before asking the operator to watch four minutes of film, a 30-second B run
established that the property is accepted and that debugfs actually changes.
Had it not, the gate would have been `BLOCKED_DISPLAY` or Durum 3 and the
operator's time would have been spent for nothing. It is included as evidence
rather than deleted, and it is not part of the A/B: only `a-default` and
`b-bt2020` are.

## 23. Checksums

`SHA256SUMS` in the evidence directory covers every raw output file.

| Source file | SHA-256 |
| --- | --- |
| `src/drm/display.h` | `6e381240fbf1e1970eeec01419cebe9392ed755e073855098c63aea44513e52e` |
| `src/drm/display.cpp` | `2b556c959b1b6c5a2d67d78c32608f17c0d6edd0f44acd3bb418dddfe00c6f95` |
| `tools/hdr-playback-probe.cpp` | `8ab129aa3d028deef55de7c53d275d70671a3ec813ce47a241550a3408dbc654` |
| `tests/run-host-tests.sh` | `a61395a3bebc59e2aa822adbd5d1bc8f7ece8824cb1a35d8befea319847e9985` |
| `scripts/run-mp1b.sh` | `0a99cc80a7074de208f18adf837dea3cfc9ddf20c1eb4e9e70e996c0f61a9256` |

The evidence asset:

| File | SHA-256 |
| --- | --- |
| `/var/tmp/mp1b/past-lives.mkv` | `f4e32b8d3feb7efdebaaefaf7338a25bcc86ee144bf82bd4b93da6b80906f96a` |
