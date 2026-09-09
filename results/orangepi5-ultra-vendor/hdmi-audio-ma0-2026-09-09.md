# Gate MA0: HDMI PCM audio bring-up on Orange Pi 5 Ultra

Date: 2026-09-09
Target: `root@10.27.27.25` (`orangepi5-ultra`, RK3588 OPi 5 Ultra)
Sink: Sony BRAVIA `KD-65XE9005` at `10.27.27.51`, HDMI 1
Kernel: `6.1.115-vendor-rk35xx-screenbridge-hdmirx-audio`

## 1. Executive summary

**GATE MA0 RESULT: `PASS`.**

HDMI PCM audio works on this board, end to end, and the sink's audio
capability discovery works too — including the one thing Gate MP0 concluded was
missing.

Every MA0 pass criterion is met:

| Criterion | Result |
| --- | --- |
| HDMI PCM endpoint identified | `PASS` — `hw:0,0`, `rockchip-hdmi1 i2s-hifi-0` |
| 48 kHz stereo PCM plays | `PASS` |
| Physical audio confirmed | `PASS` — operator, on the TV |
| Continuous ≥ 60 s | `PASS` — 60.038 s, 2 880 000 frames |
| `xrun = 0` | `PASS` — 0 across **every** run in this gate |
| Fatal kernel errors = 0 | `PASS` — dmesg delta empty for all audio-only runs |
| Cleanup works | `PASS` |
| Repeat run succeeds | `PASS` — identical second 60 s run |

### The finding that changes a previous conclusion

**Gate MP0 recorded that no ELD is present. That conclusion was wrong, and the
error was in the method, not the observation.** MP0 searched `/proc/asound` for
an ELD *file*, and there is none. On this vendor BSP the ELD is exposed as a
**128-byte ALSA control** on the HDMI card, and it is fully populated with the
Sony's real audio capabilities:

```text
monitor            : 'SONY TV  *00'
speaker allocation : FL/FR, LFE, FC, RL/RR          (5.1)
LPCM     6ch  32/44.1/48/88.2/96/176.4/192 kHz  16/20/24-bit
AC-3     6ch  32/44.1/48 kHz   max 640 kbps
DTS      6ch  32/44.1/48 kHz   max 1504 kbps
E-AC-3   8ch  44.1/48 kHz
```

This matters well beyond MA0. The open worry carried forward from MP0 was that
Kodi would be unable to detect passthrough capability without an ELD. That
worry is now retired: the ELD is there, it is standards-conformant, and it
advertises AC-3, DTS and E-AC-3. Gate MA1 starts from a much better position
than expected.

**Audio scope held**: this gate implemented PCM only. No AC-3, E-AC-3, DTS,
TrueHD, DTS-HD MA, Atmos, IEC61937 or HBR passthrough was implemented or
attempted. What is reported about those formats above is the *sink's* declared
capability read out of its ELD, not playback.

Nothing was installed on the target. No kernel, device-tree, bootloader or boot
configuration change. No reboot. No package upgrade. Only read-only getters
were used on the TV.

## 2. Audio device inventory

```text
card 0: rockchiphdmi1  [rockchip-hdmi1]  device 0: rockchip-hdmi1 i2s-hifi-0   <- HDMI TX, this gate
card 1: rockchiphdmiin [rockchip,hdmiin] device 0: capture only                <- HDMI-RX, ScreenBridge
card 2: rockchipes8388 [rockchip,es8388] device 0: playback + capture          <- onboard analog codec
```

The HDMI playback endpoint is **`hw:0,0`**. Card 1 is the HDMI *input* capture
device from the ScreenBridge work and is not an output. Card 2 is the board's
analog codec and is not on the HDMI path.

`/proc/asound/card0/pcm0p/info` confirms a single playback subdevice. The
`rockchip-hdmi1 Jack` control reads **`on`**, so the driver has sink presence
detection working.

## 3. Device tree and driver route

Read-only. Nothing in the device tree was changed.

| Node | `compatible` | Role |
| --- | --- | --- |
| `/hdmi1-sound` | `rockchip,hdmi` | the sound card behind `card 0`; vendor HDMI sound driver, not `simple-audio-card` |
| `/hdmi@fdea0000` | `rockchip,rk3588-dw-hdmi` | HDMI TX controller — **the same one the video path drives** |
| `/hdmi@fde80000` | `rockchip,rk3588-dw-hdmi` | the other HDMI TX (`hdmi0-sound`) |
| `/i2s@*` | `rockchip,rk3588-i2s-tdm` | I2S/TDM controllers |
| `/spdif-tx@*` | `rockchip,rk3588-spdif` | S/PDIF transmitters, unused by this path |

`hdmi1-sound` carries `rockchip,card-name = "rockchip-hdmi1"`, plus
`rockchip,cpu`, `rockchip,codec` and `rockchip,jack-det` — the vendor's own
HDMI sound binding rather than the generic `hdmi-codec` link. dmesg confirms
the video path runs on `dwhdmi-rockchip fdea0000.hdmi`, so **audio and video
share one HDMI controller and one cable**, which is why §7 matters.

Mixer controls on card 0 relevant to audio: `ELD` (PCM, 128 bytes, read-only),
`ELD Bypass Switch` (off), `IEC958 Playback Default`
(`AES0=0x04 AES1=0x00 AES2=0x00 AES3=0x01`) and `Mask`, `Playback Channel Map`,
and `rockchip-hdmi1 Jack`. The `IEC958` and `ELD Bypass` controls are the
levers Gate MA1 will need; they are recorded here and not touched.

## 4. ELD, decoded

Read programmatically by `tools/hdmi-audio-probe.py --probe` from the ALSA
control, not from a file, and cross-checked against `amixer cget`.

| Field | Value |
| --- | --- |
| `eld_ver` | 2 (CEA-861D or below) |
| baseline length | 40 bytes |
| `CEA_EDID_Ver` | 3 |
| `SAD_Count` | 4 |
| connection type | HDMI |
| `supports_ai` / `HDCP` | 1 / 0 |
| audio sync delay | 0 ms |
| speaker allocation | `0x0f` → FL/FR, LFE, FC, RL/RR (**5.1**) |
| monitor name | `SONY TV  *00` |

| SAD | Raw | Format | Channels | Rates | Extra |
| --- | --- | --- | --- | --- | --- |
| 1 | `0d 7f 07` | **LPCM** | 6 | 32/44.1/48/88.2/96/176.4/192 kHz | 16/20/24-bit |
| 2 | `15 07 50` | **AC-3** | 6 | 32/44.1/48 kHz | max 640 kbps |
| 3 | `3d 07 bc` | **DTS** | 6 | 32/44.1/48 kHz | max 1504 kbps |
| 4 | `57 06 00` | **E-AC-3** | 8 | 44.1/48 kHz | — |

No TrueHD (MLP) and no DTS-HD SAD, which is consistent with an HDMI 1.4 sink:
those need HBR and this panel does not advertise it. That is a fact about the
sink, not about the board.

## 5. Capability matrix vs. what the sink asks for

`hw_params` negotiation, one axis at a time, each on a freshly opened handle so
the axes do not narrow each other:

| Axis | Driver accepts | Sink's ELD asks for | Agreement |
| --- | --- | --- | --- |
| Formats | `S16_LE` ✓ `S24_LE` ✓ `S32_LE` ✓ `S24_3LE` ✗ | 16/20/24-bit LPCM | **yes** — 24-bit is available as `S24_LE` |
| Rates | 32k ✓ 44.1k ✓ 48k ✓ 88.2k ✓ 96k ✓ 176.4k ✓ 192k ✓ | all seven | **yes** — exact match |
| Channels | 1 ✗ 2 ✓ 4 ✓ 6 ✓ 8 ✓ | 6ch LPCM | **yes**, and the driver goes beyond it |

Two asymmetries, both recorded as capability facts rather than defects:

* **`S24_3LE` is refused** (`-EINVAL`). The packed 3-byte layout is simply not
  offered; 24-bit content uses `S24_LE` in a 32-bit container instead. Nothing
  is lost.
* **Mono is refused.** The HDMI audio sample packet has no 1-channel layout, so
  this is correct behaviour, not a limitation.
* **The driver accepts 8 channels although the sink declares 6ch LPCM.** ALSA
  does not clamp to the ELD here. An 8-channel stream is accepted and played;
  what the TV does with channels 7 and 8 is its own business. A player must
  therefore consult the ELD itself rather than trusting `hw_params` to refuse
  an over-wide stream.

## 6. Playback results

Every run below reported `xrun = 0` and produced an **empty dmesg delta**.

### 6.1 Stereo baseline, twice

| Run | Rate | Ch | Format | Period / buffer | Frames written | Expected | Wall | xruns |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| smoke | 48000 | 2 | `S16_LE` | 256 / 131072 | 720 000 | 720 000 | 15.041 s | **0** |
| baseline 1 | 48000 | 2 | `S16_LE` | 256 / 131072 | 2 880 000 | 2 880 000 | 60.038 s | **0** |
| baseline 2 | 48000 | 2 | `S16_LE` | 256 / 131072 | 2 880 000 | 2 880 000 | 60.038 s | **0** |

`/proc/asound/card0/pcm0p/sub0/hw_params` sampled 20 s into each 60 s run
confirms the kernel's own view: `RW_INTERLEAVED`, `S16_LE`, 2 channels,
48000 Hz, period 256, buffer 131072, `state: RUNNING`.

**Physical audio: `YES`.** The operator confirmed hearing the tone.

### 6.2 Format sweep, 48 kHz stereo

| Format | Granted | Period / buffer | Frames | xruns | Result |
| --- | --- | --- | --- | --- | --- |
| `S16_LE` | 2 bytes/sample | 256 / 131072 | 288 000 | 0 | `PASS` |
| `S24_LE` | 4 bytes/sample | 256 / 65536 | 288 000 | 0 | `PASS` |
| `S32_LE` | 4 bytes/sample | 256 / 65536 | 288 000 | 0 | `PASS` |
| `S24_3LE` | — | — | — | — | `UNSUPPORTED` (`-EINVAL`, capability result) |

The buffer halves for the 4-byte formats, which is the expected constant-bytes
allocation.

### 6.3 Rate sweep, stereo `S16_LE`

| Rate | Granted | Period | Frames written | Expected | xruns | Result |
| --- | --- | --- | --- | --- | --- | --- |
| 32000 | 32000 | 256 | 192 000 | 192 000 | 0 | `PASS` |
| 44100 | 44100 | 256 | 264 600 | 264 600 | 0 | `PASS` |
| 48000 | 48000 | 256 | 288 000 | 288 000 | 0 | `PASS` |
| 88200 | 88200 | 512 | 529 200 | 529 200 | 0 | `PASS` |
| 96000 | 96000 | 512 | 576 000 | 576 000 | 0 | `PASS` |
| 176400 | 176400 | 1024 | 1 058 400 | 1 058 400 | 0 | `PASS` |
| 192000 | 192000 | 1024 | 1 152 000 | 1 152 000 | 0 | `PASS` |

Every rate was granted exactly as requested — no resampling, no `set_rate_near`
substitution. The driver scales the period with the rate, holding the buffer at
131072 frames.

**Physical audio: `YES` for all of them.** The tone frequency is `rate/100`, so
each rate has a distinct pitch; the operator confirmed hearing every segment
and hearing the pitch rise across the sweep. That is stronger evidence than a
single "I heard something": it confirms the rates actually reached the sink as
different rates.

### 6.4 Multichannel

| Channels | Period / buffer | Frames | xruns | Result |
| --- | --- | --- | --- | --- |
| 4 | 256 / 65536 | 288 000 | 0 | `PASS` |
| 6 | 257 / 43690 | 576 000 | 0 | `PASS` |
| 8 | 256 / 32768 | 384 000 | 0 | `PASS` |

Note the 6-channel period of **257** frames — the allocator divides a
fixed byte budget by a frame size that is not a power of two. Harmless, but
worth knowing before someone reads a non-power-of-two period as a bug.

### 6.5 Channel mapping — and a trap worth recording

The driver's own `Playback Channel Map` control, read while the 6-channel
stream was open, reports:

```text
3, 4, 8, 7, 5, 6   ->   FL, FR, LFE, FC, RL, RR
```

**This is not the order a channel list is usually written in.** The obvious
guess — FL, FR, FC, LFE, SL, SR — is wrong in four of six positions: slots 2
and 3 are LFE and FC, not FC and LFE. The first version of the probe assumed
the written order and mislabelled the test; it now reads the chmap from the
hardware and labels channels from that. Any player or test that hard-codes an
interleave order for this board will silently swap centre with LFE.

Physical result on the current sink: the operator reported **FL from the left
speaker, FR from the right, and the remaining four channels downmixed to both**.
That is exactly correct behaviour for a television with two speakers, and it
confirms discrete channel separation is really present on the wire for at least
the front pair.

```text
Multichannel discrete verification beyond the front pair:
NOT_APPLICABLE_WITH_CURRENT_SINK
```

There is no AVR in this setup. The sink declares 5.1 in its ELD and accepts
6-channel LPCM, but has two speakers, so it downmixes. Per this gate's own
rule that is not a blocker and not a failure.

## 7. Concurrent HDR video + PCM audio

The load-bearing test, because audio and video share one HDMI controller and
one cable. The HDR video probe ran for 60 s on the 90:00 scene with the
product's `COLOR_ENCODING = ITU-R BT.2020 YCbCr`, and 40 s of PCM audio was
layered in from t=12 s, once the modeset had settled — so this measures
concurrent steady state rather than a modeset landing on an open PCM stream.

| Measurement | Value |
| --- | --- |
| **Audio** frames written / expected | 1 920 000 / 1 920 000 |
| **Audio** xruns | **0** |
| **Audio** wall time | 40.145 s |
| **Video** decoded / presented | 1437 / **1413** |
| **Video** dropped / repeated / late | **0 / 0 / 0** |
| **Video** flip errors / timeouts | **0 / 0** |
| **Video** effective rate | **23.9763 fps** (expected 23.9760) |
| **Video** frame interval mean / p95 / p99 | 41.708 / 41.780 / 41.808 ms |
| HDMI `bus_format` | `YUYV10_1X20` — unchanged |
| VP0 state | `HDR10[2]` / `BT.2020` / Limited — unchanged |
| Plane `COLOR_ENCODING` readback | `ITU-R BT.2020 YCbCr (2)`, `csc mode[3]` |
| TV | `cinemaHome`, `xtendedDynamicRange=high`, `lightSensor` unavailable → **HDR active** |
| dmesg delta | 36 lines, the ordinary modeset sequence, **0** errors |

**No regression in the video pipeline.** The cadence figures are
indistinguishable from the audio-free runs in Gates MP1b and MP1b-FINAL: same
mean interval to three decimals, same zero counters, p99 within 0.02 ms.
Audio ran clean alongside it. Adding audio cost the video path nothing
measurable, and adding video cost the audio path nothing measurable.

## 8. A/V sync

Per this gate's scope, **no lip-sync tuning was performed**. The probes are
independent processes on independent clocks and were not synchronised to each
other, so no sync figure is claimed. What can be reported is the absence of the
gross failures the gate asked to be watched for: **no audible dropout, no
audible drift and no second-scale interruption** during the 40 s concurrent
run, and no xrun or dropped frame on either side to cause one.

Two facts recorded now because MA2/Kodi will need them:

* the sink's ELD declares `audio_sync_delay = 0 ms`, so it advertises no
  intrinsic audio delay for the source to compensate; and
* ALSA reports `delay ≈ 130 920` frames at 48 kHz with the default 131 072-frame
  buffer — about **2.7 seconds** of buffering. A real player must set a much
  smaller buffer than the default, or lip sync is impossible regardless of how
  well the clocks are matched. This is the single most important number in this
  report for the player integration.

## 9. ELD questions from the gate brief, answered

1. **Does ALSA PCM work without an ELD file?** The premise is false — there is
   an ELD, exposed as a control. PCM works, and it works with the ELD present.
2. **Does the HDMI codec get sink capability another way?** No other way is
   needed. The vendor driver parses the sink's EDID CTA audio block into a
   standards-conformant 128-byte ELD and publishes it on the PCM interface.
3. **Is there another ELD expose point under `/sys`?** No. `find` over
   `/proc/asound`, `/sys/class/sound` and `/sys/devices` returns nothing for
   `*eld*`. The control interface is the only route, which is exactly why MP0's
   file-based search found nothing.
4. **Do the EDID audio block and the ALSA hw constraints agree?** On formats and
   rates, exactly (§5). On channels they do not: ALSA accepts 8 where the ELD
   declares 6ch LPCM, so ALSA is not clamping to the ELD.
5. **Would stock Kodi struggle with passthrough capability detection here?**
   **Unlikely.** Kodi reads exactly this ELD control to build its passthrough
   capability list, and it is present and correctly populated with AC-3, DTS and
   E-AC-3 SADs. The MP0-era concern does not survive contact with the hardware.
   The residual risks for MA1 are the `ELD Bypass Switch` and the `IEC958`
   status bits, neither of which was touched here.

## 10. Result

```text
MA0 = PASS
```

All pass criteria met, with physical audio confirmed by the operator at every
stage that produced sound, `xrun = 0` on every run in the gate, zero kernel
errors, a verified repeat, and no regression to the video pipeline when both
run together.

## 11. Remaining uncertainties

1. **The 2.7-second default buffer** (§8) is not a defect but is a hard blocker
   for lip sync until a player sets its own buffer and period.
2. **ALSA does not clamp channels to the ELD**, so a player that trusts
   `hw_params` alone can emit an 8-channel stream to a 6-channel sink.
3. **No discrete verification beyond the front pair** — no AVR available.
4. **`ELD Bypass Switch` and the `IEC958` status bits are unexplored.** Both are
   central to MA1 and neither was written in this gate.
5. **A/V sync is unmeasured**, by design. Nothing here says the two clocks stay
   aligned over an hour; it says they did not visibly break over 40 seconds.
6. **The second HDMI port (`hdmi0-sound`, `fde80000`) was not tested.** All
   results are for `fdea0000` / `card 0`.

## 12. Evidence paths

`logs/orangepi5-ultra-vendor/hdmi-audio-ma0-2026-09-09/`

| File | Contents |
| --- | --- |
| `00-alsa-inventory.txt` | `aplay -l/-L`, `/proc/asound/{cards,devices,pcm}` |
| `01-card0-detail.txt` | card 0 tree, PCM info, the ELD file search that finds nothing |
| `02-dt-and-mixer.txt` | device-tree audio nodes and card 0 mixer controls |
| `03-eld-and-route.txt` | raw ELD control, IEC958 bits, chmap container, DT node, dmesg |
| `04-eld-decoded.txt` | the ELD decoded field by field |
| `10-capability-probe.txt`, `11-capability-probe.json` | `--probe` output, text and JSON |
| `20-…`, `21-…` | 48 kHz stereo smoke test and its dmesg delta |
| `22-baseline-60s-x2.txt`, `23-…` | two 60 s runs with live `/proc` hw_params, dmesg delta |
| `30-format-rate-sweep.txt`, `31-…` | format and rate sweeps, dmesg delta |
| `32-s24_3le-unsupported.txt` | the refused format, reported as a capability result |
| `40-multichannel.txt`, `41-…` | 4/6/8-channel runs and dmesg delta |
| `42-chanid-6ch.txt` | channel identification with labels read from the driver chmap |
| `50-…` through `57-…` | the concurrent HDR video + PCM audio run and its evidence |
| `SHA256SUMS` | checksums for every file above |

Tool: [`../../tools/hdmi-audio-probe.py`](../../tools/hdmi-audio-probe.py).

## 13. Recommended next single gate

**MP2 — Kodi GBM/DRM + RKMPP HDR video bring-up.**

Both prerequisites are now evidenced rather than assumed: the video path passes
MP1b, and the audio path passes MA0 with a working ELD. MP2 is the gate that
turns two proven subsystems into something a person can use, and it is where
the 2.7-second buffer and the channel-order trap in §6.5 will first bite.

**MA1 — compressed HDMI audio / passthrough** should follow MP2, not precede
it. Its prerequisites are in better shape than MP0 suggested — the sink
advertises AC-3 640 kbps, DTS 1504 kbps and 8-channel E-AC-3, and Kodi reads
the same ELD control this gate just decoded.

Explicitly not recommended next: TrueHD/DTS-HD/Atmos (the sink advertises
neither and it is an HDMI 1.4 panel), CEC, HDR10+, Dolby Vision, and any
kernel, device-tree or boot configuration change.
