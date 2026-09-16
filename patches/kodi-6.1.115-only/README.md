# Colour patches that only belong on 6.1.115

These six patches drove the HDR output path by hand: they asked the connector
for deep colour, kept a YCbCr colorimetry tag for the direct-to-plane path,
chose the output pixel format, and tagged each plane's EOTF so the display
engine would compose an SDR GUI over HDR video itself.

They were written against the golden appliance's kernel,
`6.1.115-vendor-rk35xx-screenbridge-hdmirx-audio`, and they worked there.

**On the stock `6.1.172-vendor-rk35xx` they destroy the picture.** Measured on
2026-09-17, bisected patch by patch against a 4K HDR10 BT.2020/ST2084 test
clip on the appliance's own sink:

| what was built | what reached the panel |
| --- | --- |
| all eight patches | colours collapse to red and blue |
| `0001` alone | same |
| `0001` with its colorimetry half reverted | same |
| `0001` with `SetDeepColor()` removed | correct |
| `0003` + `0005` + `0007` only (this series) | correct, RGB 10-bit, BT.2020, ST2084 |

The single change that flips it is `SetDeepColor()` writing `color_depth=30bit`
on the connector. `dw_hdmi_qp-rockchip.c` in 6.1.172 carries a rule -- "We
prefer use YCbCr422 to send hdr 10bit" -- that fires when the colorimetry is
BT.2020, the depth is ten bits, and the previously committed bus format was
8 bit or already 4:2:2. Left alone, the driver picks RGB 10-bit for an HDR
output by itself, which is a better signal than anything these patches asked
for: full chroma and full depth.

Everything else in the chain was eliminated by measurement first, so this is
not a guess about where the fault lives:

* the clip is genuine HDR10 and its bars are the same in every frame;
* the Rockchip MPP decoder is bit-identical to the software decoder
  (`PSNR inf`, `MSE 0.00`);
* the DRM framebuffer the decoder hands over (NV15, pitch 4864,
  offset 10506240, modifier 0) scans out correctly from a standalone probe;
* so do 1400 consecutive page flips of it at 24 fps;
* the VOP2 composition, read back through the writeback connector, is correct;
* the HDMI transmitter reports the signal it was asked for.

They are kept, rather than deleted, because the golden board they were written
for still exists and the reasoning in them is sound for that kernel. Anything
that revives them has to name the kernel it is reviving them for.
