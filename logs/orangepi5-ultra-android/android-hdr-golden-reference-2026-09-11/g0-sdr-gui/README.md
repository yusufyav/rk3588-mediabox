# G0 — Kodi GUI, SDR, no video

# host_utc: 2026-09-11T10:57:20Z
# device_uptime: 612.13 4382.61

Baseline before playback. Both Kodi surfaces sit on Cluster0 (win0 and win1) as AFBC RGBA, the port is RGB888 SDR, and the SDR2HDR block is bypassed.

File set is the standard capture described in ../README.md.

## Key lines

```
Video Port0: ACTIVE
    Connector: HDMI-A-1
	bus_format[100a]: RGB888_1X24
	overlay_mode[0] output_mode[f] color_space[0], eotf:0
    Cluster0-win0: ACTIVE
    Cluster0-win1: ACTIVE
```

SDR2HDR_CTRL (0xfdd92010): `00000100`
