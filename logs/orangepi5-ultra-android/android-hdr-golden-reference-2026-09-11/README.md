# Android HDR composition golden reference — Orange Pi 5 Ultra

Read-only capture of the working Android 13 + Kodi 21.3 HDR10 pipeline, taken
on 2026-09-11 to serve as the oracle for the Linux/Kodi HDR composition work.

Nothing on the device was flashed, written or restarted. The only mutation was
`vendor.hwc.log`, set to `255` for one logging pass and restored to its original
empty value afterwards (see `logcat/README.md`).

## Layout

| directory | contents |
| --- | --- |
| `identity/` | build fingerprint, kernel, display properties, Kodi package, test asset hash |
| `binaries/` | composer/HWC binary paths, sizes and SHA256 (no blobs committed) |
| `g0-sdr-gui/` | G0 — Kodi GUI, SDR, no video |
| `g1-hdr-video/` | G1 — HDR10 playing, OSD hidden |
| `g2-hdr-osd/` | G2 — HDR10 playing, OSD visible |
| `g3-hdr-pause-osd/` | G3 — HDR10 paused, OSD visible |
| `g4-hdr-pause-clean/` | G4 — HDR10 paused, OSD auto-hidden |
| `hwdumps/` | out-of-sequence live probe kept as corroboration |
| `logcat/` | HWC verbose log across G1→G2→G3→G4 |
| `diff/` | machine-readable Android vs Linux comparison |

Each state directory is produced by `tools/android-hdr-oracle/capture-state.sh`
and holds the same file set:

| file | source |
| --- | --- |
| `00-meta.txt` | state label, host UTC, device uptime |
| `10-drm-summary.txt` | `/sys/kernel/debug/dri/0/summary` (vendor VOP2 view) |
| `11-drm-state.txt` | `/sys/kernel/debug/dri/0/state` (generic atomic state) |
| `12-drm-framebuffer.txt` | `/sys/kernel/debug/dri/0/framebuffer` |
| `13-drm-clients.txt` | `/sys/kernel/debug/dri/0/clients` |
| `14-drm-mm-dump.txt` | `/sys/kernel/debug/dri/0/mm_dump` |
| `15-drm-active-regs.txt` | `/sys/kernel/debug/dri/0/active_regs` (VOP2 registers) |
| `16-connector-sysfs.txt` | `/sys/class/drm/card0-HDMI-A-1/*` except edid/modes |
| `20-sf-list.txt` | `dumpsys SurfaceFlinger --list` |
| `21-sf-dump.txt` | `dumpsys SurfaceFlinger` |
| `23-dumpsys-display.txt` | `dumpsys display` |
| `30-hwc-props.txt` | HWC/gralloc/HDR runtime properties |
| `40-activity.txt` | foreground activity |
| `90-screencap.png` | screenshot confirming the state (G1–G4 only) |

`90-screencap.png` is a SurfaceFlinger GPU composite. It proves which state was
captured; it cannot show the VOP2 hardware composition, so the absence of
horizontal displacement is an operator observation, recorded in the report.

## Headline result

All of G1, G2, G3 and G4 are byte-identical in `10-drm-summary.txt` and in the
VOP2 `HDR:` register block apart from the video buffer address. Android does not
change its composition state when the OSD appears or when playback pauses.

The full reading is in
`results/orangepi5-ultra-android/android-hdr-golden-reference-2026-09-11.md`.
