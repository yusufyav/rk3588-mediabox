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

## MP1a — HDR signalling isolation (this gate)

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

Report: [`../results/orangepi5-ultra-vendor/hdr-signaling-mp1a-2026-09-09.md`](../results/orangepi5-ultra-vendor/hdr-signaling-mp1a-2026-09-09.md).

## Not yet authorised

Everything below waits for its own gate, and none of it is started as a
side effect of MP1a:

- installing Kodi, or a GBM/Mesa/`libmali` user space
- Stremio integration and the control bridge
- audio passthrough and CEC
- frame-rate matching policy across 23.976/24/25/50/59.94/60
- HDR10+ and Dolby Vision
- any kernel, device-tree or boot configuration change
