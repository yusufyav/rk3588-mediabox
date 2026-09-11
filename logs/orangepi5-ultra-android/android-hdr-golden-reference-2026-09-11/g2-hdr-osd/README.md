# G2 — HDR10 playing, OSD visible

# host_utc: 2026-09-11T10:59:08Z
# device_uptime: 719.29 4895.91

Identical to G1 in every VOP2 register except the video buffer address. Raising the OSD changes the contents of the GUI buffer, never the plane set or any colour state.

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
