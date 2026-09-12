# Media policy

> One capability profile, one decision, and a reason code for every part of it.

## The shape of a decision

```
source URL
  │
  ├─ media.inspector ──> MediaInfo         what this actually is
  │
  └─ media.policy ─────> PlaybackDecision  what this appliance does about it
                          ├─ VideoDecision  Direct | Risky | Unsupported
                          ├─ AudioDecision  Passthrough | DecodeToPCM | TranscodeToAC3
                          └─ mode           Direct
                                            DirectWithAudioTranscode
                                            Remux
                                            FallbackSourcePreferred
                                            Unsupported
```

Every decision carries reasons, and every reason has a stable code
(`media/policy/reasons.py`). The code is the contract; only the wording may
change.

## The capability profile

`rk3588_orangepi5_production` in `media/policy/capabilities.py`. Every claim
names the gate that measured it, and the two that matter most are negative.

| | |
| --- | --- |
| Video codecs | hevc, h264, vp9, av1, mpeg2video, vc1, mpeg4 |
| HEVC profiles | Main, Main 10 (range extensions are a different decoder) |
| H.264 profiles | Baseline, Main, High — 8-bit 4:2:0; **not** High 10 |
| Bit depths | 8, 10 |
| Chroma | 4:2:0 only |
| Geometry | ≤ 3840×2160, ≤ 60 fps |
| HDR | SDR, HDR10, HDR10+, HLG |
| **Dolby Vision** | **no verified pipeline** |
| Audio passthrough | ac3, eac3 |
| Audio decode → PCM | aac, mp3, opus, vorbis, flac, alac, pcm… |
| **Max PCM channels** | **2** — the sink is silent on 6-channel LPCM |
| Transcode target | ac3, up to 6 channels |

Evidence: `results/orangepi5-ultra-vendor/real-hdr10-playback-mp1b-2026-09-09.md`,
`results/orangepi5-ultra-vendor/ma1-hdmi-passthrough-2026-09-11.md`,
`results/mediabox-platform/m2-regression-fix-cpu-input-2026-09-11.md`.

A profile with no evidence is a guess, and a test asserts that every profile
has some.

## Colour and HDR

The inspector classifies what the stream **signals**, from the transfer
function first — it is the only field that changes how a decoder must interpret
the samples — with two cases kept deliberately separate from SDR:

* **incomplete** — no transfer function on a wide-gamut or 10-bit stream, or
  ST 2086 mastering metadata with no signalled transfer. The samples decode;
  the display may not be driven in the intended colour volume.
* **conflicting** — BT.2020 primaries with a BT.709 transfer, PQ on an 8-bit
  stream, HDR10+ metadata without PQ.

Both are `Unknown`, both play, both carry a reason, and both lose to a
fully-signalled rendition during ranking. "We do not know" is not "it is SDR".

HDR10+ plays as its HDR10 base layer with `HDR10_PLUS_BASE_LAYER_ONLY`; the
dynamic metadata is not applied and the decision says so rather than implying
it was.

## Dolby Vision — the green and magenta guard

The reported fault is a picture that opens green and purple. The cause is not
assumed; it is decided from the DOVI configuration record.

**Profile 5** has no backward-compatible base layer. Its base layer is IPTPQc2
with a cross-talk matrix only a DV decoder undoes, so a plain HEVC Main 10
decoder puts the channels in the wrong places — exactly the reported symptom.
On a device with no DV pipeline a Profile 5 stream is **never Direct**:

```
Video      RISKY,  prefer_alternative = True
Playback   FallbackSourcePreferred
Reasons    DV_UNSUPPORTED_PIPELINE
           DV_PROFILE5_NO_BASE_LAYER
           SAFER_ALTERNATIVE_EXPECTED
```

It cannot become a media session at all: `POST /media/session` answers
`409 SOURCE_NOT_PREFERRED`.

**Profile 7** is dual-layer and its base layer is HDR10 by definition of the
profile. It plays as HDR10 with the enhancement layer and RPU dropped
(`DV_PROFILE7_BASE_LAYER_HDR10`, `DV_DYNAMIC_METADATA_LOST`).

**Profile 8** says what its base layer is compatible with, and the policy keys
on that field rather than on the profile number:

| `dv_bl_signal_compatibility_id` | Plays as | Verdict |
| --- | --- | --- |
| 1 | HDR10 | Direct |
| 2 | SDR | Direct |
| 4 | HLG | Direct |
| 6 | HDR10 | Direct |
| 0 | — | Risky |
| anything else | — | Risky, `DV_PROFILE_UNKNOWN` |

An unrecognised id stays unrecognised. A Dolby Vision stream whose profile
cannot be read at all — a `dvhe` codec tag with no configuration record — is
risky for the same reason.

If a DV pipeline is ever verified on this hardware, `dolby_vision_pipeline =
True` on the profile changes all of this in one place.

## Audio

The reliable target is AC-3, and the reason is measured rather than
architectural. Gate MA1 found that this sink plays a real AC-3 file's frames
perfectly and plays **Kodi's own AC-3 encoder output not at all**, and that it
is silent on DTS and on 6-channel LPCM despite its ELD declaring both. So the
conversion happens upstream of Kodi, as file frames.

| Source | Decision on this profile | Why |
| --- | --- | --- |
| AC-3 | passthrough | verified audible |
| E-AC-3 | passthrough | verified audible |
| AAC / MP3 / Opus / Vorbis ≤ 2ch | decode to PCM | within the sink's channel limit |
| AAC > 2ch | → AC-3 | 6-channel PCM is silent here; the mix survives as AC-3 |
| DTS, DTS-HD, DTS:X | → AC-3 | sink silent on DTS; HD not in the ELD |
| TrueHD, TrueHD Atmos | → AC-3 | not in the ELD |
| multichannel FLAC / PCM | → AC-3 | same 2-channel PCM limit |
| unknown codec | → AC-3, `AUDIO_CODEC_UNKNOWN` | an explicit decision, not an assumption |

Losses are stated, never implied: `AUDIO_OBJECT_METADATA_LOST` when an object
mix is flattened, `AUDIO_LOSSLESS_TO_LOSSY` when a lossless source is encoded,
`AUDIO_CHANNELS_REDUCED` with both counts.

### Track selection

Cheapest first: passthrough beats decode beats transcode, then more channels,
then the default flag, then stream index. A requested language filters before
any of that, because the wrong language played perfectly is still wrong.

This is what stops a TrueHD Atmos track being transcoded while a native AC-3
track sits unused in the same file — `AUDIO_NATIVE_TRACK_PREFERRED`.

## Source ranking

Resolution is not the first criterion. Tiers, best first:

| Tier | |
| --- | --- |
| 1 `SAFE_DIRECT` | safe video, audio needing no encoder |
| 2 `SAFE_AUDIO_TRANSCODE` | safe video, audio converted to AC-3 |
| 3 `SAFE_SDR_ALTERNATIVE` | safe, but an HDR rendition of the same title exists |
| 4 `SAFE_REMUX` | safe, container has to be changed |
| 5 `RISKY` | Dolby Vision with no usable base layer, untrustworthy colour |
| 6 `UNSUPPORTED` | |

Within a tier: pixels, then HDR value, then channels, then bitrate, then the
source's own identity — so the order is total and two runs over the same
inputs produce the same list. A 4K Dolby Vision Profile 5 file therefore loses
to a 1080p HDR10 one, which is the whole point.

Whether an SDR rendition is demoted is a property of the *set*, computed once:
alone, SDR is a first-class choice.

## Preview

Decided separately from television playback, against a browser's capabilities,
and permitted to refuse:

| | |
| --- | --- |
| `BrowserDirect` | the browser opens the source |
| `BrowserRemux` | streams copied into a container it opens — no encoder |
| `Unsupported` | `PREVIEW_SOFTWARE_TRANSCODE_REFUSED` |

`allow_software_video_transcode` is `False` and a test asserts it. A 4K HEVC
software decode into an H.264 software encode is the one thing this appliance
must never do, so it is refused before it starts rather than started and then
cleaned up. `PREVIEW_UNSUPPORTED` means "watch it on the television".

## From a terminal

```
media-core inspect URL          media-core rank URL [URL ...]
media-core policy URL           media-core capabilities
media-core preview URL
```
