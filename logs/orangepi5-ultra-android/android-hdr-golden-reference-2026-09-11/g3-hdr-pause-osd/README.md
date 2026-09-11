# G3 — HDR10 paused, OSD visible

# host_utc: 2026-09-11T10:59:54Z
# device_uptime: 766.08 5112.32

Identical to G2. This is the exact condition that displaces the picture on Linux.

File set is the standard capture described in ../README.md.

## Key lines

```
Video Port0: ACTIVE
    Connector: HDMI-A-1
	bus_format[200d]: YUYV10_1X20
	overlay_mode[0] output_mode[9] color_space[10], eotf:2
    Cluster0-win0: ACTIVE
    Esmart0-win0: ACTIVE
```

SDR2HDR_CTRL (0xfdd92010): `0000000b`
