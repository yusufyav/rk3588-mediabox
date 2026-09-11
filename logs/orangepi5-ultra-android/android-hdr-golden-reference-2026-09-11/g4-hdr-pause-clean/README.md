# G4 — HDR10 paused, OSD auto-hidden

# host_utc: 2026-09-11T11:00:17Z
# device_uptime: 789.19 5226.42

Identical to G3. The OSD timing out changes nothing in the composition.

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
