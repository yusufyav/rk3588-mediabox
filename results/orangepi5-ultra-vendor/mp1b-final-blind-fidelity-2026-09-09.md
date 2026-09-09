# Gate MP1b-FINAL: blind interleaved HDR colour fidelity A/B

Date: 2026-09-09
Target: `root@10.27.27.25` (`orangepi5-ultra`, RK3588 OPi 5 Ultra)
Sink: Sony BRAVIA `KD-65XE9005` at `10.27.27.51`, HDMI 1, `cinemaHome`
Asset: *Past Lives* (2023), 4K23.976 HEVC Main 10 HDR10, `f4e32b8d…f96a`

## 1. Executive summary

**MP1b FINAL: `PASS` (`PASS_WITHOUT_VISIBLE_DIFFERENCE`).**
**CSC visual hypothesis: `NOT_MATERIALLY_VISIBLE`.**
**Product decision: keep `COLOR_ENCODING = ITU-R BT.2020 YCbCr`.**

Gate MP1b-CSC proved the mechanical half of the colour-encoding question and
failed the perceptual half for protocol reasons. This gate rebuilt the
perceptual half properly — double-blind, interleaved, short-cycle,
counterbalanced, on a scene selected by measured chroma rather than by
convenience — and the operator reported **no perceivable difference in every
completed pair, in both presentation orders**.

Three of eight planned pairs were completed. The operator elected to stop
there and move to audio, which is their call; the sample is smaller than
planned and this report does not pretend otherwise (§8). What the three pairs
do establish is that the effect is not large: it was not merely
*unidentifiable*, as in MP1b-CSC, it was reported as *absent*, from a scene
carrying three times the chroma of the one MP1b-CSC used, with only ~10
seconds between the two legs instead of four minutes.

Since the BT.2020 matrix is the semantically correct one for BT.2020 primaries
and no visible degradation from it was demonstrated, it is adopted as the
product behaviour. MP1b closes as a pass on that basis: every measurable
dimension passed, and the one dimension that could only be judged by eye
produced no evidence of a defect under a protocol built to find one.

| Dimension | Result |
| --- | --- |
| Blind protocol executed as designed | `PASS` |
| Variant correctly applied in all 6 trials | `PASS` |
| HDMI state invariant across all trials | `PASS` |
| Cadence invariant across all trials | `PASS` |
| SDR reset verified between every trial | `PASS` — 6/6 |
| Kernel errors | `PASS` — 0 |
| Operator can distinguish the legs | **no — in 3/3 pairs** |
| Planned sample size reached | **no — 3 of 8 pairs** |

## 2. What this gate changed relative to MP1b-CSC

MP1b-CSC's own §16 named three protocol faults. All three are fixed here:

| Fault in MP1b-CSC | Fix in MP1b-FINAL |
| --- | --- |
| Operator knew which leg was which | Assignment written to a file the runner never prints; neither operator nor orchestrator saw it until `reveal` |
| Legs ~4 minutes apart, testing long-term colour memory | 25 s per trial, the two legs of a pair back to back, ~10 s apart |
| Muted 20:00 scene, minimum effect size | Scene chosen by measured chroma; `SATAVG` 51.3 vs 17.0, a 3.0× increase |
| Two 120 s blocks, one comparison | 8 pairs planned, counterbalanced AB/BA, randomised order |

## 3. Scene selection, and why this timestamp

MP1b-CSC recommended "a chroma-stressing scene" without naming one. This gate
measured rather than guessed: `ffmpeg signalstats` over the eMMC copy, 3 frames
per sample, on a 150 s grid across the whole 6343 s film, then a 10 s grid
around the maximum. `SATAVG` is mean distance of chroma from neutral on the
10-bit scale, which is the quantity a BT.601-vs-BT.2020 matrix error scales
with.

| Region | `YAVG` | `SATAVG` | `SATMAX` |
| --- | --- | --- | --- |
| t=1200 (20:00) — the scene MP1b and MP1b-CSC used | 278.8 | **17.0** | 69 |
| t=5400–5495 (90:00–91:35) — selected here | ~255 | **~51** | ~133 |
| next best candidate, t=1740 (29:00) | 226.8 | 30.5 | 88 |

The fine pass shows a stable plateau from 5400 to 5490 (`SATAVG` 49.9 → 51.3,
no cut) ending in a hard cut at ~5495 (`SATAVG` drops to 11.5). Trials
therefore run `--start 5410 --duration 25`, covering 5410–5435: entirely
inside the plateau, with margin at both ends, so every trial shows identical
content and no shot change.

The scene is a warm, amber-lit bar interior: tungsten practicals, specular
highlights on glassware, and skin tones against a saturated orange ground. That
is the combination where a wrong YUV→RGB matrix is most visible — it loads the
R and B coefficients hardest and puts skin, the reference every viewer carries,
directly against the error.

`02-scene-5415-thumbnail-sdr-debug.png` is an SDR tone-mapped render of one
frame, included to identify the scene. **It is not evidence of colour**: per
this gate's rules no fidelity judgement is made from a screenshot.

## 4. Blind protocol

Runner: [`scripts/blind-ab.py`](../../scripts/blind-ab.py).

```text
seed        = 683049930
pairs       = 8 planned, 3 completed
duration    = 25 s per trial
start       = 5410 s
plane       = 73 (forced, both legs)
A           = plane COLOR_ENCODING default      (VOP2 tags the window BT.601)
B           = plane COLOR_ENCODING ITU-R BT.2020 YCbCr
```

* **Counterbalanced**: four AB pairs and four BA pairs, then the pair order
  itself shuffled, so noticing "it alternates" reveals nothing.
* **Double-blind in practice**: `plan` wrote the assignment to
  `mapping.secret.json` and printed only the seed. `run` prints trial status
  and never the variant. Evidence filenames are numbered by position in the
  pair (`pairN-trialM-*`), never by variant. Neither the operator nor the
  orchestrator saw the assignment until `reveal`, after the last score was
  recorded.
* **Per trial**: play → sample debugfs and the TV mid-trial → capture the
  playback log and a dmesg delta → `--reset` → **verify the link actually
  returned to SDR** before the next trial starts.
* The operator was offered four responses: *1st clip*, *2nd clip*, *same*,
  *not sure*. "Same" and "not sure" are separate answers for the reason
  MP1b-CSC established: they are different findings.

### 4.1 One blinding breach, and what it does and does not affect

The run was interrupted during pair 4's second trial. Recovering the target
required listing processes, and the probe's command line — which carries
`--plane-color-encoding` — was visible in that listing. That disclosed pair 4's
assignment **to the orchestrator**. Pair 4 was therefore discarded and never
scored. The operator's blinding was never broken: they saw only the picture,
and pair 4's second trial was cut short before it could be judged. Pairs 1–3,
the only scored pairs, were unaffected — they had completed and been scored
before the interruption.

## 5. Blind result

```text
pair  order  operator  picked
   1     BA      same       -
   2     AB      same       -
   3     BA      same       -

decided pairs        : 0
  picked B (BT.2020) : 0
  picked A (default) : 0
reported 'same'      : 3
reported 'unsure'    : 0
two-sided exact binomial p = n/a (no decided pairs)
```

**Blind A/B score: 0 discriminations in 3 pairs; 3 × "same", 0 × "not sure".**

Two details make this more informative than the count alone:

* both presentation orders were covered (BA, AB, BA), so the answer is not an
  artefact of one ordering; and
* the operator chose *"same"* every time, not *"not sure"*. In MP1b-CSC the
  same operator explicitly declined "they looked the same" in favour of "I
  could not judge". Under a protocol they could actually judge, they judged,
  and judged them identical.

No binomial statistic is reported because there were no directional choices to
test. Reporting a p-value over zero decisions would be theatre.

## 6. Per-trial technical evidence

Every trial applied its variant correctly and held everything else constant.

| Trial | Variant | plane `COLOR_ENCODING` readback | `csc mode` | `bus_format` | VP0 | decoded | presented | drop/rep/late |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| pair1-trial1 | B | `ITU-R BT.2020 YCbCr` | **3** | `YUYV10_1X20` | `HDR10[2]`/BT.2020 | 598 | 574 | 0/0/0 |
| pair1-trial2 | A | `ITU-R BT.601 YCbCr` | **0** | `YUYV10_1X20` | `HDR10[2]`/BT.2020 | 598 | 574 | 0/0/0 |
| pair2-trial1 | A | `ITU-R BT.601 YCbCr` | **0** | `YUYV10_1X20` | `HDR10[2]`/BT.2020 | 598 | 574 | 0/0/0 |
| pair2-trial2 | B | `ITU-R BT.2020 YCbCr` | **3** | `YUYV10_1X20` | `HDR10[2]`/BT.2020 | 598 | 574 | 0/0/0 |
| pair3-trial1 | B | `ITU-R BT.2020 YCbCr` | **3** | `YUYV10_1X20` | `HDR10[2]`/BT.2020 | 598 | 574 | 0/0/0 |
| pair3-trial2 | A | `ITU-R BT.601 YCbCr` | **0** | `YUYV10_1X20` | `HDR10[2]`/BT.2020 | 598 | 574 | 0/0/0 |

The `csc mode` column is the load-bearing one: VOP2 really does swap
coefficient sets between the legs, exactly as MP1b-CSC established, so the
operator was genuinely being shown two different matrices.

`COLOR_RANGE` was never written in any trial and read back as
`YCbCr limited range (0)` throughout, as the gate requires.

### 6.1 TV state and kernel log

| Trial | Variant | `pictureMode` | `xtendedDynamicRange` | `brightness` | `lightSensor` | Result |
| --- | --- | --- | --- | --- | --- | --- |
| pair1-trial1 | B | cinemaHome | high | 50 | UNAVAILABLE | `PASS` |
| pair1-trial2 | A | cinemaHome | high | 50 | UNAVAILABLE | `PASS` |
| pair2-trial1 | A | cinemaHome | high | 50 | UNAVAILABLE | `PASS` |
| pair2-trial2 | B | cinemaHome | high | 50 | UNAVAILABLE | `PASS` |
| pair3-trial1 | B | cinemaHome | high | 50 | UNAVAILABLE | `PASS` |
| pair3-trial2 | A | cinemaHome | high | 50 | UNAVAILABLE | `PASS` |

The TV was in its HDR profile for every trial, identically. dmesg deltas were
36 lines for all six trials — the ordinary modeset sequence — with **0**
matches for error, underflow, timeout or fail. The SDR reset was verified
after **6 of 6** trials, so no colour state leaked from one trial into the
next.

## 7. Verdict

```text
CSC visual hypothesis : NOT_MATERIALLY_VISIBLE
MP1b final            : PASS  (PASS_WITHOUT_VISIBLE_DIFFERENCE)
Product decision      : plane COLOR_ENCODING = ITU-R BT.2020 YCbCr
```

The reasoning, kept explicit because this is a pass built partly on a negative:

1. Every machine-measurable dimension of MP1b passed, twice in MP1b and again
   in all six trials here.
2. The one hypothesis that could have explained a real colour defect — the
   window running a BT.601 matrix — is real, is correctable, and the correction
   is proven to reach the hardware.
3. Under a protocol designed to expose it, on the highest-chroma sustained
   scene in the asset, the correction produced **no perceivable change**.
4. Therefore there is no evidence of a colour defect attributable to this
   pipeline. MP1b's original `PARTIAL_FIDELITY` rested on the operator's
   impression that colour was "not right"; that impression has not survived a
   blind test, and an unblinded impression is not evidence a gate can fail on.
5. BT.2020 is nonetheless the correct matrix for BT.2020-primaries content, so
   it is what the product uses. Choosing the semantically right value costs
   nothing and removes a known-wrong state from the pipeline.

The probe's **default remains "leave the property untouched"**, deliberately:
changing it would silently alter the A leg of MP1a's and MP1b's published,
hash-pinned evidence. The product decision is a decision about what downstream
players and configuration must set, and it is recorded here and in the gate
ledger, not smuggled into a probe default.

## 8. Remaining uncertainties

1. **Sample size.** Three pairs, not the eight planned. Three unanimous
   "same" answers across both orders is a consistent signal, not a
   statistically powered one. A directional effect smaller than the operator's
   discrimination threshold is not excluded — only an effect large enough to
   matter is.
2. **One operator, one sink, one scene.** No second observer, no reference
   display, no second asset.
3. **The window is still tagged `SDR[0]`** while VP0 runs `HDR10[2]`. This gate
   did not address it and `COLOR_ENCODING` does not reach it. It remains the
   most plausible mechanism for any residual difference between this board and
   a commercial player, and it has no standard DRM property.
4. **`csc mode[3]` semantics** are still not verified against vendor driver
   source. It is known to differ from mode 0; which exact matrix it loads was
   not read out of the kernel.
5. **No objective colour measurement.** A colorimeter or a capture card would
   settle in minutes what a human observer cannot, and neither was available.

## 9. Evidence paths

`logs/orangepi5-ultra-vendor/mp1b-final-blind-fidelity-2026-09-09/`

| File | Contents |
| --- | --- |
| `01-scene-selection-scan.txt` | full `signalstats` scan, coarse and fine passes |
| `02-scene-5415-thumbnail-sdr-debug.png` | SDR thumbnail, scene identification only |
| `mapping.secret.json` | the assignment, written at plan time and not read until reveal |
| `pairN-trialM-10-playback.txt` | probe log: property discovery, `TEST_ONLY`, readback, cadence |
| `pairN-trialM-20-summary-during.txt` | VOP2 debugfs sampled mid-trial |
| `pairN-trialM-21-tv-state-during.json` | Sony getters sampled mid-trial |
| `pairN-trialM-30-dmesg-delta.txt` | kernel log delta for that trial |
| `pairN-trialM-40-reset.txt`, `-41-summary-after-reset.txt` | cleanup and its verification |
| `trials.jsonl` | per-trial status: result, SDR reset verified, dmesg line count |
| `scores.jsonl` | operator responses as recorded, in order |
| `50-blind-result.txt` | the reveal: assignment, scores, tally |
| `51-trial-evidence.txt` | per-trial technical table |
| `52-tv-dmesg-evidence.txt` | TV state and kernel log table |
| `SHA256SUMS` | checksums for every file above |

Pair 4's partial evidence is retained under `pair4-*` and is **excluded from
the result** for the reason in §4.1.

## 10. Next

Gate MA0 — HDMI audio bring-up — follows immediately, and is reported
separately in
[`hdmi-audio-ma0-2026-09-09.md`](hdmi-audio-ma0-2026-09-09.md).
