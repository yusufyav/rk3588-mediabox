# identity

| file | contents |
| --- | --- |
| `build-props.txt` | build/vendor fingerprint, release, security patch, board, platform |
| `kernel.txt` | `uname -a`, `/proc/version`, `/proc/cmdline` (cmdline needed root) |
| `display-props.txt` | every property matching hwc/hdr/vivid/display/color/resolution/hdmi/composer |
| `vendor-hwc-props.txt` | vendor HWC, gralloc and `ro.rk.*` properties |
| `services.txt` | running composer processes, `service list`, `lshal` |
| `packages.txt` | installed packages, Kodi present |
| `kodi-package.txt` | Kodi version, code path, APK size and SHA256 |
| `test-asset-hash.txt` | SHA256 of the HDR10 reference asset on the device |

The test asset hash matches the Linux-side reference exactly:
`f4e32b8d3feb7efdebaaefaf7338a25bcc86ee144bf82bd4b93da6b80906f96a`.
Both sides therefore measured the same file.
