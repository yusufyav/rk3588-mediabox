# Gate MA1: HDMI compressed audio passthrough on Orange Pi 5 Ultra

Date: 2026-09-11
Target: `root@10.27.27.25` (`orangepi5-ultra`, RK3588 OPi 5 Ultra)
Sink: Sony, ELD monitor name `SONY TV  *00`, HDMI 1
Kernel: `6.1.115-vendor-rk35xx-screenbridge-hdmirx-audio` (unchanged)
Kodi: `e513e0ff4331fc25fd2454659a9dd3e6b7670146` (unchanged, not patched)
Evidence: `logs/orangepi5-ultra-vendor/ma1-hdmi-passthrough-2026-09-11/`

## 1. Executive summary

**GATE MA1 RESULT: `PARTIAL`.**

Compressed passthrough works. Every codec this sink can actually reproduce is
passed through bit-exact, Kodi never falls back to PCM decode, and the 4K23.976
HDR10 display baseline is untouched and unregressed. The gate is `PARTIAL` and
not `PASS` for one reason: the ELD declares DTS, and this sink does not
reproduce DTS — so one ELD-declared codec with a valid test asset could not be
made to work at the sink.

| Pass criterion | Result |
| --- | --- |
| Low-level IEC61937 AC-3 | `PASS` |
| Kodi AC-3 passthrough | `PASS` |
| Other ELD-declared codecs with an asset | `PARTIAL` — E-AC-3 `PASS`, DTS not reproducible by the sink |
| Kodi never falls back to PCM decode | `PASS` |
| `xrun = 0` | `PASS` — zero, every run |
| 4K23.976 HDR10 / 10-bit preserved | `PASS` |
| GUI SDR→HDR composition preserved | `PASS` |
| 180 s cadence regression | `PASS` — none |
| atomic / VOP / kernel errors | `PASS` — zero |
| Unsupported codecs left off | `PASS` — TrueHD and DTS-HD absent from the ELD, both off |

**No kernel, DT, bootloader, Mali, RKMPP, DRM PRIME, NV15, HDR or VOP2 change
was made, and Kodi was not patched.** The one fix is a single ALSA
configuration file.

### The finding

Nothing was wrong below Kodi. The low-level proof played real AC-3 to the sink
with `ffmpeg -c:a copy -f spdif | aplay -D hw:0,0` before anything was changed —
the kernel, the dw-hdmi-qp controller and the I2S path all carry a compressed
burst correctly as shipped.

What was missing was a **name**. Kodi decides whether a device can do
passthrough in `CAESinkALSA::AEDeviceTypeFromName()`, purely from the ALSA PCM
name:

```cpp
if (name.substr(0, 4) == "hdmi")
  return AE_DEVTYPE_HDMI;
```

and only `AE_DEVTYPE_HDMI` ever gets `AE_FMT_RAW` and the AC3/DTS/E-AC-3 stream
types pushed onto it. This card advertised only `default`,
`sysdefault:CARD=rockchiphdmi1` and `hw:CARD=rockchiphdmi1` — no name beginning
with `hdmi` — so Kodi enumerated the HDMI sink as `AE_DEVTYPE_PCM` and logged
*"No passthrough capabilities"* for every device on the machine. Passthrough was
not merely off; it was unselectable.

`hdmi:CARD=…` exists on other machines because alsa-lib ships a per-card
definition for their driver (`HDA-Intel.conf`, `vc4-hdmi.conf`, …) and there was
none for `rockchip-hdmi1`. Adding that one file
(`config/alsa/rockchip-hdmi1.conf`, installed to
`/usr/share/alsa/cards/rockchip-hdmi1.conf`) makes the stock
`/usr/share/alsa/pcm/hdmi.conf` resolve for this card, and Kodi immediately
enumerates the sink correctly, reads its ELD, and offers passthrough.

## 2. Answers

| Question | Answer |
| --- | --- |
| `usedisplayasclock` at the start | **`false`** — already, at Kodi's own default; it was never pinned by the appliance profile |
| Was it set false for passthrough | **No, no change was needed.** It is now written out explicitly so the dependency is visible |
| Real ALSA HDMI endpoint | card 0 `rockchiphdmi1`, device 0 — `rockchip-hdmi1 i2s-hifi-0`. Reached as `hw:CARD=rockchiphdmi1,DEV=0` low-level, and as `hdmi:CARD=rockchiphdmi1,DEV=0` from Kodi |
| Compressed codecs the ELD declares | AC-3 6ch/640k, DTS 6ch/1504k, E-AC-3 8ch (plus LPCM 6ch). **No TrueHD, no DTS-HD** |
| Low-level IEC61937 AC-3 | **`PASS`** — machine-clean and audible as Dolby at the TV |
| Kodi passthrough device | `ALSA:hdmi:CARD=rockchiphdmi1,DEV=0\|rockchip-hdmi1` — the real endpoint, no plug/Pulse/PipeWire chain |
| AC-3 really RAW passthrough | **Yes** — `passthrough=1`, `devType=2` (`AE_DEVTYPE_HDMI`), `AES0=0x06`, parser not decoder |
| DTS result | **`DTS_SINK_LIMITED`** — Kodi's side correct, sink reproduces nothing. Disabled |
| E-AC-3 result | **`PASS`** — bit-exact; this is the canonical asset's native codec |
| Unsupported HD codecs off | **Yes** — TrueHD and DTS-HD are absent from the ELD and both are `false` |
| 4K23.976 HDR baseline intact | **Yes** — `3840x2160p24`, 23.98, HDR10, BT.2020, 10-bit, NV15, RKMPP |
| 180 s cadence | **Clean** — all sync activity in the first 43 s, silent thereafter |
| A/V correction / drop / late | **Zero** after settling; zero drops, skips or late frames at any point |
| Kernel / ALSA xrun / error | **Zero**, every run |

## 3. The sink over-declares its ELD

This is the single most useful thing the gate learned, and it is the reason the
result is `PARTIAL`.

| Format | ELD declares | Sink reproduces |
| --- | --- | --- |
| AC-3 6ch / 640 kbps | yes | **yes** |
| E-AC-3 8ch | yes | **yes** |
| DTS 6ch / 1504 kbps | yes | **no — silent** |
| LPCM 6ch | yes | **no — silent** |
| TrueHD | no | not tested, not enabled |
| DTS-HD | no | not tested, not enabled |

In both failing cases Kodi's output was correct at every machine-visible layer —
RAW sink opened on the right device, `AES0=0x06` non-audio set, `RUNNING`, zero
xruns — and the TV produced nothing. The DTS asset was ruled out as the cause:
it decodes to the same content and the same level as the AC-3 and E-AC-3 assets
built from the same source segment (mean −31.0 dB, max −1.0 dB for all three).

The negative results were controlled for sink state. After every failing test,
E-AC-3 passthrough was played again and was still clean at the TV, so the sink
had not latched into a mute state part-way through the session.

So an ELD entry was necessary to scope this gate but was **not** sufficient to
decide what to leave enabled. Two of the four declared formats are declared by a
sink that cannot play them. Kodi's own list is weaker still: it pushes TrueHD
and DTS-HD onto any HDMI device on purpose — *"we don't trust ELD information
and push back our supported formats explicitly"* — so the UI offering a codec
means nothing at all.

## 4. What was changed

Two files, both configuration.

**`config/alsa/rockchip-hdmi1.conf`** — new. Defines
`rockchip-hdmi1.pcm.hdmi.0`, which is what makes
`hdmi:CARD=rockchiphdmi1,DEV=0` exist. Modelled on the stock `HDA-Intel.conf`
rather than `vc4-hdmi.conf`: vc4 wraps the stream in `type iec958` because that
hardware needs IEC958 sub-frames built in software, whereas dw-hdmi-qp does the
framing itself and takes the burst as plain `S16_LE` stereo — exactly what the
low-level proof fed it by hand. A software subframe converter would have
corrupted it.

Its `ctl_elems` hook is what carries the low-level proof forward: it writes the
AES channel-status bytes for the life of the stream and restores them on close.
Measured:

```text
baseline           AES0=0x04                                 (PCM)
during passthrough AES0=0x06 AES1=0x82 AES2=0x00 AES3=0x02   (non-audio set)
after close        AES0=0x04                                 (restored)
```

`AES0` bit 1 is the IEC60958 non-audio flag. Without it the sink decodes the
identical bytes as PCM and plays white noise. Only `pcm.hdmi.0` is defined;
`default` and `sysdefault` are deliberately left alone.

> Aside worth recording: `amixer -c 0 cset numid=15 …` silently does not take on
> an IEC958-typed control, which initially looked like a read-only control and a
> driver limitation. `iecset -c 0 audio off` writes it. The control was writable
> all along.

**`config/kodi/guisettings-appliance.xml`** — the passthrough settings, plus
`videoplayer.usedisplayasclock` written out explicitly. It was already `false`,
but nothing else in that file hints that a *video clock* setting silences the
audio path in code, and `CVideoPlayerAudio` computes
`allowpassthrough = !usedisplayasclock`.

`scripts/run-kodi-rk3588.sh` now copies the ALSA file to the target on every
start, next to the guisettings copy it already did — a profile naming a PCM that
does not exist is the one failure that looks like a Kodi bug instead of a
missing config file.

### Final profile

```text
videoplayer.usedisplayasclock  false
audiooutput.audiodevice        ALSA:hdmi:CARD=rockchiphdmi1,DEV=0|rockchip-hdmi1
audiooutput.passthroughdevice  ALSA:hdmi:CARD=rockchiphdmi1,DEV=0|rockchip-hdmi1
audiooutput.passthrough        true
audiooutput.ac3passthrough     true     ELD-declared, verified audible
audiooutput.eac3passthrough    true     ELD-declared, verified audible
audiooutput.dtspassthrough     false    ELD-declared, sink silent
audiooutput.ac3transcode       false    see below
audiooutput.truehdpassthrough  false    not in the ELD
audiooutput.dtshdpassthrough   false    not in the ELD
audiooutput.channels           1        2.0; sink is silent on 6ch LPCM
```

Both devices now point at the `hdmi` PCM. It is the only PCM Kodi will consider
for passthrough, and it applies the correct AES bytes for *both* jobs — `0x04`
for PCM, `0x06` for a burst — so a GUI sound and a film no longer open different
endpoints.

The whole profile was re-verified by restarting Kodi from the repo file: every
value above is what a cold start produces, and Kodi rewrote none of them.

## 5. Transcoding: measured, not usable, left off

The operator asked for an AC-3 fallback so DTS titles would play. It does not
work on this sink, and the negative result is recorded so it is not re-derived.

Kodi picks the transcode target in `ActiveAE.cpp`: `STREAM_TYPE_EAC3`, falling
back to `STREAM_TYPE_AC3` only when `eac3passthrough` is false. Both were tried.

| Attempt | Encoder | Carrier | Machine state | At the TV |
| --- | --- | --- | --- | --- |
| DTS → E-AC-3 | `EAC3 encoder ready` | 192 kHz, `AES0=0x06` | `RUNNING`, 0 errors | noise |
| DTS → AC-3 | `AC3 encoder ready` | 48 kHz, `AES0=0x06` | `RUNNING`, 0 errors, sync settled | silence |
| DTS → 6ch LPCM | none (5.1 decode) | 48 kHz, 6ch, `AES0=0x04` | `RUNNING`, 0 errors | silence |

The second row's sink configuration is byte-for-byte the one that plays a real
AC-3 file — same device, same 48 kHz `S16_LE` stereo carrier, same `AES0=0x06`.
The only difference is that the payload is Kodi's own encoder output rather than
the file's frames, which points at Kodi's encoder / IEC61937 packing, above
every layer this gate proved. That is a real fault domain and a different one;
nothing here established it well enough to justify a patch, and the brief keeps
transcoding out of MA1 deliberately.

Left off, a DTS title is decoded and downmixed to 2.0 PCM, which this sink does
play.

## 6. Display regression

Captured during live passthrough playback of the canonical 4K HDR asset
(`hdr-passthrough-regression.txt`). Every element of the protected baseline is
intact:

```text
display mode    3840x2160p24   (#0 3840x2160 23.98, 296703 kHz; source 24000/1001)
bus_format      YUYV10_1X20
vp              HDR10[2] color-encoding[BT.2020] color-range[Limited]
GUI plane   57  AR24, eotf=0                          (SDR)
video plane 73  NV15, eotf=2, csc y2r[1] mode[3]      (PQ, SDR2HDR path)
connector       Colorspace=BT2020_YCC, color_depth=10, HDR_OUTPUT_METADATA set
decoder         Rockchip MPP HEVC via DRM PRIME
atomic failures 0     VOP underflows 0     kernel audio errors 0
```

The only `dmesg` line matching an error scan across the whole session is an OPP
regulator probe message at t=4.2 s — boot, not display, and long before this
gate ran.

## 7. Cadence

180 s uninterrupted, passthrough active, `usedisplayasclock=false`.

All A/V sync activity falls in the first 43 s and settles to *"average error
−9.75 below threshold of 30.0"*. From then to the end of the run the log
contains zero `RESYNC`, `SyncStream`, drop, skip, late-frame, xrun, underrun or
clock-jump lines — including across the ~95 s mark where a regression had been
seen before. Two sink opens total, both the normal PCM→passthrough switch at
start.

The ~95 s regression did not return. `usedisplayasclock=false` is accepted as
the passthrough-compatible profile state. This is **not** a
`PASSTHROUGH_WORKS_CADENCE_CONFLICT` outcome, and no raw compressed audio was
resampled to get there.

## 8. Test assets

The canonical asset was not modified:
`/var/tmp/mp1b/past-lives.mkv`, sha256
`f4e32b8d3feb7efdebaaefaf7338a25bcc86ee144bf82bd4b93da6b80906f96a`. It carries a
single **E-AC-3** 5.1 / 48 kHz / 640 kbps track, which is why E-AC-3 is the
codec that matters most on this box.

AC-3 and DTS had no local source, so two 200 s test assets were derived from it
under `/var/tmp/ma1/` — video stream copied, audio re-encoded — using the ffmpeg
already present at `/opt/rk3588-screenbridge`. No package was installed. The
DTS encode is core at 1411.2 kbps, inside the ELD's 1504 kbps ceiling.

## 9. Remaining work — not started

Per the brief, work stops here. Not attempted and not to be assumed:
Stremio integration, TrueHD / DTS-HD / HBR, any new kernel experiment.

Two things this gate deliberately left open:

- **Kodi's transcode / IEC61937 packing** (§5). Real, reproducible, and its own
  fault domain.
- **Why this sink declares DTS and 6ch LPCM it cannot play** (§3). May be an
  EDID quirk or an ARC/eARC path question; either way it is a sink property, not
  something this box can fix.
