# diff

`android-vs-linux-hdr-composition.tsv` — one row per composition field, with
the three Android playback states beside the four Linux arms already on record.

Linux columns are read from committed evidence, not re-measured here:

| column | source |
| --- | --- |
| `linux_video_only` | `logs/orangepi5-ultra-vendor/kodi-pause-horizontal-shift-2026-09-10/state-playing.txt` + `vop-regs-playing.txt` |
| `linux_0009_osd` | same directory, `state-paused.txt` + `vop-regs-paused.txt` |
| `linux_gui_nonpq` | same directory, `kodi-eotf1-candidate.txt` (rejected fix 1) |
| `linux_sentinel` | same directory, `sentinel-workaround.txt` (rejected workaround) |

Values in the last two columns that were never captured as registers are marked
`not measured (inferred ...)`. They are inferences from the kernel source path,
not measurements, and are labelled as such in the `interpretation` column.
