# binaries

Identity only. No Android binary, library or APK is committed to this
repository — paths, sizes and SHA256 are recorded instead.

| file | contents |
| --- | --- |
| `composer-process.txt` | resolved executable of PID 325 and every mapped library |
| `sha256-and-sizes.txt` | size and SHA256 of the composer binary and its vendor libraries |

The HAL service `/vendor/bin/hw/android.hardware.graphics.composer@2.1-service`
is a thin wrapper; the actual implementation is the legacy HAL module
`/vendor/lib64/hw/hwcomposer.rk30board.so` (Rockchip drmhwc2, reported as
`vendor.ghwc.version = HWC2-1.5.143`).

A copy of that library was pulled to a scratch directory for offline `strings`
and `readelf` analysis and was not retained. Its GNU build-id is
`81cd2e59581c5d6d07ece36237254b49`; its SHA256 is in `sha256-and-sizes.txt`.
