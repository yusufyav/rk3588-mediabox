# Android-parity HDR composition — Orange Pi 5 Ultra, vendor Linux

Evidence for the change that makes Kodi stop PQ-encoding its GUI during HDR10
direct-to-plane playback and lets VOP2's hardware SDR-to-HDR block do the
conversion instead, as the vendor Android stack does.

Captured 2026-09-11 on kernel 6.1.115-vendor-rk35xx, Kodi 22.0b2-Piers
(`e513e0ff4331fc25fd2454659a9dd3e6b7670146`) with patches 0001-0010.

| file | what it is |
| --- | --- |
| `source-before.txt` | the four call sites as they stood on 0009 |
| `patch-diff.txt` | the change itself (patch 0010) |
| `a-0009/` | control arm: accepted 0009 build, OSD hidden and OSD visible |
| `b-android-parity/` | test arm, same two states |
| `b-*.txt` | the test arm's decisive state (OSD visible), split by evidence type |
| `lifecycle-s0.txt` … `lifecycle-s7.txt` | S0-S7 as specified in the brief |
| `physical-observation.txt` | the operator's own words, both judgements |
| `playback-stats.txt` | 120 s continuous playback |
| `dmesg-delta.txt` | kernel lines over the window, plus a fault scan |

## Headline

`SDR2HDR_CTRL` at `0xfdd92010`, the register the whole gate turns on:

| | A (0009) | B (Android-parity) | Android oracle |
| --- | --- | --- | --- |
| OSD visible | `0x00000100` bypassed | **`0x0000000b`** | **`0x0000000b`** |
| GUI plane | `HDR10[2]` | `SDR[0]` | `SDR[0]` |
| video plane | `HDR10[2]` | `HDR10[2]` | `HDR10[2]` |
| overlay_mode | `1` | `0` | `0` |

The Linux register value is byte-identical to the Android one, so the bit
semantics did not have to be reconciled: the ABI is the same in 5.10 and 6.1.

S1 through S5 are identical to each other at `0x0000000b` — the composition
state no longer changes when the OSD appears or when playback pauses, which is
the property the Android capture measured and Linux previously lacked.
