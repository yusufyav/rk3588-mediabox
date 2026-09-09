# Gates

Work on this appliance proceeds in gates. A gate produces evidence and a
decision, not just a working feature, and each one stops for authorisation
before the next begins.

Success is never "a picture appeared". Every gate names what physical or
kernel-visible evidence counts.

## MP0 — Linux playback platform preflight (done, in the reference repository)

Read-only inventory of the board's display and media capabilities.
Result: `PARTIAL` — kernel side complete, playback user space entirely absent.
Report: `results/orangepi5-ultra-vendor/media-player-hdr-preflight-2026-09-09.md`
in [yusufyav/rk3588-screenbridge](https://github.com/yusufyav/rk3588-screenbridge).

Findings carried into this repository as inputs are listed in
[`architecture.md`](architecture.md).

## MP1a — HDR signalling isolation (done)

One question: **when the vendor Linux DRM stack is handed a correct 10-bit +
BT.2020 + HDR10 metadata atomic state, does the sink enter HDR10 mode?**

Kodi, Stremio and audio passthrough are explicitly out of scope. The point is
to find out whether the layers underneath them work, before anything is built
on top.

The A/B ladder changes one variable per rung, and stops at the first failure so
that the fault domain stays narrow:

| Step | Frame format | `Colorspace` | `color_depth` | `HDR_OUTPUT_METADATA` |
| --- | --- | --- | --- | --- |
| a0 | NV12 (8-bit, SDR asset) | `Default` | `Automatic` | unset |
| a1 | NV15 (10-bit, HDR10 asset) | `Default` | `Automatic` | unset |
| a2 | NV15 | `BT2020_YCC` | `Automatic` | unset |
| a3 | NV15 | `BT2020_YCC` | `30bit` | unset |
| a4 | NV15 | `BT2020_YCC` | `30bit` | from the asset |

`BT2020_YCC` rather than `BT2020_RGB` because the sink cannot take RGB at
4K with 10-bit: at 3840x2160p23.976 the 300 MHz TMDS ceiling permits 10-bit
only over YCbCr 4:2:2, so an RGB request could not be honoured.

Evidence captured per rung: the full atomic state, `requested` versus
`readback` for every property, `/sys/kernel/debug/dri/0/summary` before and
after, a filtered kernel-log delta, and the physically observed TV state.

The differential the gate is designed to produce:

| Scenario | `HDR_OUTPUT_METADATA` | debugfs HDR state | TV | Fault domain |
| --- | --- | --- | --- | --- |
| 1 | set | active | HDR | none — signalling path works |
| 2 | set | active | SDR | `dw-hdmi-qp`, DRM InfoFrame, PHY negotiation, sink |
| 3 | set | still SDR | — | DRM/VOP2 atomic property handling, vendor dependency |
| 4 | NV15 commit fails | — | — | plane / framebuffer / modifier / CRTC compatibility |

Result: `PASS`. All five rungs passed; the sink enters HDR10 at a4 and at no
earlier rung, and the link is byte-identical between a3 and a4, which isolates
the transition to the Dynamic Range and Mastering InfoFrame alone.
Report: [`../results/orangepi5-ultra-vendor/hdr-signaling-mp1a-2026-09-09.md`](../results/orangepi5-ultra-vendor/hdr-signaling-mp1a-2026-09-09.md).

## MP1b — real HDR10 content and picture fidelity (done)

MP1a proved the signalling path with a three-second synthetic test pattern.
That says nothing about how graded film looks. MP1b asks the one question left
over from it:

**does the same A4 output state show a real 4K23.976 HEVC Main 10 HDR10 film
with correct colour, tone, highlights and shadows?**

This is not a signalling gate. The A4 state is held fixed and is not a variable:

| Held fixed from MP1a | Value |
| --- | --- |
| frame format | `NV15`, DRM PRIME, RKMPP hardware decode |
| `Colorspace` | `BT2020_YCC` |
| `color_depth` | `30bit` |
| `HDR_OUTPUT_METADATA` | set, built from the asset |

The single new variable is the content: synthetic pattern to real HDR10 film.
Two things follow from that and are new instruments rather than new variables:

- the output mode is chosen from the **asset's own frame rate**, and a mismatch
  is a failure rather than a fallback — 23.976 content pulled to 60 Hz does not
  pass this gate; and
- frames are presented against their **PTS**, with cadence measured from the
  DRM vblank sequence counter, so `repeated` frames are counted rather than
  inferred from wall-clock jitter.

Forbidden throughout, and checked rather than assumed: software HEVC decode,
10-bit to 8-bit narrowing, `NV15` to `NV12`, BT.2020 to BT.709, PQ to SDR,
software tone mapping, `libswscale` and any CPU colour conversion. The probe
refuses to display a frame that is not DRM PRIME `NV15`.

Tool: [`../tools/hdr-playback-probe.cpp`](../tools/hdr-playback-probe.cpp),
built on the modules under [`../src`](../src). Gate MP1a's probe is left
untouched so that gate stays byte-for-byte reproducible.

Result: `PASS` (`PASS_WITHOUT_VISIBLE_DIFFERENCE`), reached in three stages.

The measurable path passed twice from eMMC at 23.976 fps with zero dropped,
repeated or late steady-state frames, but the physical picture was not
visually confirmed correct, so the gate first closed as `PARTIAL_FIDELITY`
with one hypothesis: the scanout plane's default BT.601 input encoding while
the content and output are BT.2020. See the
[`MP1b report`](../results/orangepi5-ultra-vendor/real-hdr10-playback-mp1b-2026-09-09.md).

`MP1b-CSC` then settled the mechanical half of that hypothesis. Requesting
`COLOR_ENCODING = ITU-R BT.2020 YCbCr` on the plane is accepted, reads back,
and makes VOP2 load a different matrix (`csc mode[0]` to `csc mode[3]`), with
HDMI state, cadence and TV HDR entry all unchanged. The perceptual half did
not resolve: two 120 s legs four minutes apart is not an instrument that can
detect a matrix error on muted material. See the
[`MP1b-CSC report`](../results/orangepi5-ultra-vendor/mp1b-plane-color-encoding-ab-2026-09-09.md).

`MP1b-FINAL` rebuilt the perceptual half as a double-blind, interleaved,
counterbalanced comparison on the highest-chroma sustained scene in the asset
(`SATAVG` 51 against the earlier scene's 17). The operator reported the two
legs identical in every completed pair, in both presentation orders. The CSC
visual hypothesis is therefore `NOT_MATERIALLY_VISIBLE`, and BT.2020 — the
semantically correct matrix — is adopted as the product behaviour while the
probe default stays "untouched" so earlier gates remain reproducible. See the
[`MP1b-FINAL report`](../results/orangepi5-ultra-vendor/mp1b-final-blind-fidelity-2026-09-09.md).

**Product decision carried forward: any player driving this pipeline sets the
scanout plane's `COLOR_ENCODING` to `ITU-R BT.2020 YCbCr` and leaves
`COLOR_RANGE` at limited.**

## MA0 — HDMI PCM audio bring-up (done)

The first audio gate. One question: **can this vendor BSP play PCM over HDMI,
and how does sink capability discovery actually work here?** Passthrough is
explicitly not the target; AC-3, E-AC-3, DTS, TrueHD, DTS-HD MA, Atmos and
IEC61937/HBR all belong to MA1.

Result: `PASS`. The HDMI playback endpoint is `hw:0,0`
(`rockchip-hdmi1 i2s-hifi-0`) on the same `fdea0000` HDMI controller the video
path drives. Stereo 48 kHz PCM plays continuously for 60 s with `xrun = 0`,
twice, with physical audio confirmed on the panel; every format except
`S24_3LE` and every rate from 32 kHz to 192 kHz is granted exactly as
requested; 4, 6 and 8 channel LPCM are accepted. Running PCM audio alongside
the HDR video probe costs the video pipeline nothing measurable — 1413 frames
presented, 0 dropped, 0 repeated, 0 late, at 23.9763 fps.

Two findings carry forward:

- **MP0's "no ELD" conclusion was a method error.** There is no ELD *file*, but
  the vendor driver publishes a populated 128-byte ELD *control* on the PCM
  interface. It advertises LPCM 6ch to 192 kHz, AC-3 640 kbps, DTS 1504 kbps
  and 8-channel E-AC-3, so Kodi's passthrough capability detection has what it
  needs and the MP0-era worry is retired.
- **The driver's channel order is FL, FR, LFE, FC, RL, RR**, not the order a
  channel list is usually written in. Anything hard-coding an interleave order
  for this board will swap centre with LFE.

The default ALSA buffer is 131072 frames — about 2.7 s at 48 kHz — so a player
must set its own buffer before lip sync is possible at all. See the
[`MA0 report`](../results/orangepi5-ultra-vendor/hdmi-audio-ma0-2026-09-09.md).

## Not yet authorised

Everything below waits for its own gate, and none of it is started as a
side effect of an MP1 gate:

- installing Kodi, or a GBM/Mesa/`libmali` user space
- Stremio integration and the control bridge
- compressed HDMI audio passthrough (MA1) and CEC
- frame-rate matching policy across 23.976/24/25/50/59.94/60
- HDR10+ and Dolby Vision
- any kernel, device-tree or boot configuration change
