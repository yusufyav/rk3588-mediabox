# G1 — HDR10 playing, OSD hidden

# host_utc: 2026-09-11T10:59:43Z
# device_uptime: 754.42 5058.44

Video moved to Cluster0-win0 as AFBC YU10 tagged HDR[2]; the Kodi GUI surface moved to Esmart0-win0 as linear RGBA tagged SDR[0]. Subtitles are drawn into that GUI surface, so the GUI plane is attached here too.

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
