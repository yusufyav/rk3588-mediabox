# Project and device cleanup

Orange Pi 5 Ultra, vendor kernel 6.1.115, Kodi 22.0b2-Piers. 2026-09-11.

Removal of research and experiment residue left in the repository and on the
appliance by gates MP1a through MP2-display, without touching the accepted
playback chain.

**Classification: `CLEANUP PASS`.** 3.50 GB reclaimed on the appliance, no
package removed, the active kernel and the production Kodi binary byte-identical
before and after, and the full HDR smoke re-passed after a reboot.

Evidence: `logs/orangepi5-ultra-vendor/cleanup-2026-09-11/`.

---

## 1. Method

Nothing was deleted before it was proved unnecessary. The order was: snapshot,
prove the live runtime, classify, plan, delete from an explicit path list,
reboot, re-measure.

The runtime proof is the part that constrains everything else. Kodi was started
with the accepted launcher and an HDR title played while `/proc/<pid>/maps`,
`/proc/<pid>/fd`, `/proc/<pid>/environ` and `ldd` were read
(`runtime-proc.txt`). Every path that appeared became
`runtime-keep-manifest.tsv`, and the delete list was mechanically checked
against it — no overlap, and no delete-list path was open by the live process.

A guard list in the delete script refused the active kernel, its modules, the
production binary, `kodi-src`, `kodi-home` and `/var/tmp/mp1b*` regardless of
what the list said. It fired once, on `/var/tmp/mp1b-ab.log`, whose name matched
the guard protecting the test asset; that file was then removed on its own.

## 2. Repository

The repository needed almost nothing. Rejected experiments were never committed
to `main` in the first place — the horizontal-shift report says so explicitly —
so the working tree held no rejected implementation to remove.

Removed:

| Path | Why |
| --- | --- |
| `scripts/deploy-kernel.sh` | sole purpose was installing the rejected RGB-force kernel (`LOCALVERSION=...-vop2rgb`). The accepted fix is userspace-only, and the vop2rgb kernel is now gone from the appliance, so the script's install, activate and rollback targets no longer exist. Documented in section 4 of the horizontal-shift report and preserved at `d9f7230`. |
| `results/orangepi5-ultra-vendor/.gitkeep` | placeholder; the directory holds nine reports |
| two stray `.claude/` directories inside evidence trees | agent telemetry, not evidence, not in any `SHA256SUMS` |

Rewritten, because they contradicted the accepted architecture:

* `README.md` — said *"Status: Gate MP1b. Kodi is not implemented yet and is not
  part of the current gate"* and listed only the two probes.
* `docs/architecture.md` — said *"None of this exists yet"* about the
  Kodi/RKMPP/VOP2/HDMI chain that is now the accepted baseline.
* `docs/gates.md` — stopped at MA0 and listed *"installing Kodi, or a
  GBM/Mesa/`libmali` user space"* under **Not yet authorised**.

`README.md` now carries the current chain, the accepted display baseline as a
measured table, and a short **Do not regress** section — do not software-PQ
encode the GUI on this path, never let a plane's `EOTF` tag disagree with its
pixels, the Android model is SDR GUI plus hardware SDR-to-HDR. The long history
stays in the result reports and is not repeated.

`.gitignore` gained `.claude/` and `*.a`; no tracked file is caught by it
(`git ls-files | git check-ignore --stdin` is empty).

**Retained deliberately**, against an initial reading that they were residue:

* `scripts/vop-geometry-watch.py` — unreferenced by any script, but it produced
  `kodi-pause-path.log`, cited in the horizontal-shift report's evidence table.
* `scripts/writeback-probe.sh`, `scripts/writeback-registration.py`,
  `tools/drm-writeback-probe.c` — the only instrument in the project that
  measures horizontal displacement objectively, and absence of that displacement
  is a PASS criterion of every later gate.
* every `logs/*` gate directory — each carries a `SHA256SUMS` covering every
  file in it, and the reports cite those manifests. Removing any member would
  invalidate a manifest that a retained report depends on.

Repository size: 33,283,483 → 33,422,209 bytes excluding `.git`. The source tree
lost 88,565 bytes; this gate's own evidence added 227,291. Net growth is
evidence, which is the intended trade.

## 3. Appliance

`3,755,548,672` bytes reclaimed — `df` used fell from 23,364,665,344 to
19,609,116,672, and free space from 34.3 GB to 37.8 GB.

| Category | Bytes | What |
| --- | --- | --- |
| rejected kernel modules | 1,977,126,057 | `/lib/modules/6.1.115-vendor-rk35xx-screenbridge-hdmirx-audio-vop2rgb` |
| apt download cache | 546,994,724 | 567 `.deb` files, via `apt-get clean` |
| experiment Kodi binaries | 802,169,000 | `kodi-gbm.pre0009`, `.with0009`, `.eotf1-candidate` under the Kodi prefix; `kodi-gbm.a-0009` and `kodi-gbm.b-parity` in `/var/tmp` |
| experiment kernel images | 138,702,336 | `...-vop2rgb`, `...bad-modules`, `...bad-release` |
| throwaway Kodi source copies | 214,785,942 | `/var/tmp/verify`, `/var/tmp/kodi-series-check-0008` |
| patch-authoring scratch | ~1,850,000 | `base`, `base2`, `base3`, `patchall`, `patchref`, `kodi-patches-0008`, `kodi-0008-gen`, the `fix_*.py` set, the `0008*`/`0009*`/`rgb10-ab` patch drafts |
| session logs and markers | ~690,000 | `kodi-build.log`, `kodi-log-mark.txt`, `mp2hdr-dmesg-*`, `mt.txt`, `mtp.txt`, `window.log`, `dmesgmark`, `logmark`, `shots` |
| GPU bring-up scratch | 1,349,546 | `/var/tmp/mali-stage` except the pinned `.deb` |

The two `kodi-gbm` copies in `/var/tmp` were confirmed redundant by hash rather
than by name: `kodi-gbm.b-parity` is `bfa5b1cb…`, identical to the production
binary, and `kodi-gbm.a-0009` is `25ad7a25…`, identical to the `with0009` arm
that was superseded.

### Kernel

`/boot` held five kernels. The boot chain was resolved before anything was
touched: `boot.cmd:45` loads `${prefix}Image`, `/boot/Image` is a symlink to
`vmlinuz-6.1.115-vendor-rk35xx-screenbridge-hdmirx-audio`, and that string is
`uname -r`. `lsmod`'s modules resolve into that release's own tree. No boot
configuration mentions `vop2rgb`, and `/boot/.previous-image` already named the
accepted kernel, so the rejected experiment had been rolled back and only its
files remained.

| Kernel | Decision |
| --- | --- |
| `...-screenbridge-hdmirx-audio` | **KEEP_BOOT** — active, target of `/boot/Image` |
| `6.1.115-vendor-rk35xx` | **KEEP_ROLLBACK** — stock Armbian, dpkg-owned |
| `...-vop2rgb` | removed — rejected RGB-force experiment |
| `...bad-modules`, `...bad-release` | removed — failed intermediate builds |

Active kernel before and after: `546d873bcaaae877c65245149a368cbb359c019bb93e34f687abbacc391a5447`.
Production Kodi binary before and after: `bfa5b1cb2ed73d139f0bcdb8652e73ea489f71edac51534fbeceddd73b1f71fb`.
The bootloader was not touched.

### Packages: none removed

`apt autoremove` was never run. Every candidate was simulated first
(`apt-simulation.txt`) and every one was blocked by a rule:

* **Build toolchain** — `apt-get -s remove build-essential cmake ninja-build
  nasm gperf swig` also takes `meson`. Gate MA1 is a Kodi-side compressed-audio
  change and rebuilds on the target through `scripts/build-kodi.sh`, so the
  toolchain stays.
* **Mesa** — removing `libgl1-mesa-dri mesa-utils-bin` cascades into 18
  packages including `libegl-dev`, `libgles-dev`, `libgl-dev` and `default-jre`,
  which are Kodi build dependencies, and Mesa is the documented
  `MEDIABOX_GPU=mesa` llvmpipe fallback in the launcher.
* **`libdrm-tests`** — removes cleanly on its own, but it provides `modetest`,
  which `tools/vop2-sdr2hdr-capture/capture-state.sh` uses. That is the
  instrument this gate's own smoke test runs on.

`dpkg-query` output is byte-identical before and after: **0 packages added or
removed.** The 547 MB reclaimed from `/var/cache/apt` is downloaded archives
only; no installed package was affected.

### Retained as ambiguous

`/var/tmp/kodi-src` (1.0 GB) — it is `KODI_SRC` in `scripts/build-kodi.sh` and
`build/kodi-gbm` hashes identical to the production binary; MA1 rebuilds here.
`/var/tmp/mp1b/past-lives.mkv` (12 GB) — the accepted test asset, protected by
instruction and by the guard list. The pinned
`libmali-valhall-g610-g24p0-gbm_1.9-1_arm64.deb` (19 MB) — offline reinstall
source for the accepted GPU runtime. `/var/log.hdd` (81 MB) — bulk log
truncation was out of scope. Operator tools (`vim`, `btop`, `fastfetch`, `tree`,
`pciutils`, `lm-sensors`, `cmatrix`) — not experiment artefacts.
`~/build/linux-rockchip-vop2` on the workstation — the rejected kernel's source
tree, outside both the repository and the appliance, so outside this gate's
scope; reported, not touched.

## 4. Post-clean smoke

Rebooted. Boot took 6.282 s (2.478 s kernel + 3.804 s userspace);
`graphical.target` at 3.673 s. `uname -r` unchanged.

| Check | Result |
| --- | --- |
| Kodi starts | yes, JSON-RPC answering in 3 s |
| GPU runtime | real Mali — `EGL_VENDOR = ARM`, `libmali.so.1.9.0`, `mali/libEGL.so.1`, `mali/libGLESv2.so.2` mapped |
| llvmpipe | absent |
| RKMPP / DRM PRIME | yes, `CRendererDRMPRIME` direct to plane |
| Frame format | `NV15` on `Esmart0-win0` (73) |
| Mode | 3840x2160p24 (23.976) |
| GUI plane | `Cluster0-win0` (57), `AR24`, **`SDR[0]`** |
| Video plane | **`HDR10[2]`**, BT.2020, limited range |
| `SDR2HDR_CTRL` | **`0x0000000b`** — `eotf_en=1 r2r_en=1 r2r_mode=0 oetf_en=1 bypass_en=0` |
| `r2r_mode` | `BT709_TO_BT2020` |
| `hdr2sdr_en` | 0 |
| `overlay_mode` | 0 |
| Output | `BT2020_YCC`, `color_depth=30bit`, `YUYV10_1X20` |
| Sink | HDR10 |
| HDMI PCM | `state: RUNNING` on `card0/pcm0p`, 0 xruns |
| Atomic errors | 0 |
| VOP underflow / `POST_BUF_EMPTY` | 0 |
| Kernel errors during the window | 0 |

Cycles: OSD open/close ×5, pause/resume ×5, stop/replay ×1. Continuous playback
reached 2 m 15 s, past the 120 s minimum, before the stop/replay cycle.

States captured under `smoke/`: `s1-hdr-video`, `s2-hdr-osd`, `s3-paused-osd`,
`s5-resumed`, `s7-replayed`. `s2`, `s3`, `s5` and `s7` are identical —
`0x0000000b`, GUI `SDR[0]`, video `HDR10[2]`, `overlay_mode[0]` — which is the
property the Android-parity gate established and the one that would break first
if the composition path had regressed.

One state reads differently from the accepted report and is not a fault. In
`s1-hdr-video`, captured after 30 s of untouched fullscreen playback, the GUI
plane is fully detached (`crtc=(null) fb=0`) and `SDR2HDR_CTRL` is `0x00000100`,
bypassed. With one layer on the port and that layer already PQ, there is no SDR
content to convert and nothing for the block to do. The fault this project
chased was never a single-layer state — it appeared when the OSD attached a
second plane — and in every captured two-layer state the hardware conversion is
active with the GUI tagged SDR.

The 82 new kernel lines during the smoke window are Mali firmware load, HDMI
TMDS clock selection and two ordinary VOP2 modeset sequences (1920x1080p60 to
3840x2160p24 and back). No commit failure, no underflow, no `Oops`, no panic.

`systemctl --failed` lists `ap6611s-bluetooth.service` and
`hdmirx-audio-loopback.service`. Both failed identically on the previous boot,
before any deletion: the first cannot find `/usr/sbin/rfkill`, which was never
installed; the second is the ScreenBridge HDMI-RX loopback and exits because
`arecord` gets `File descriptor in bad state` with no capture source attached.
Neither is caused by this cleanup and neither is in the accepted playback path.

### Not claimed

Horizontal shift and OSD colour are physical judgements, and this session did
not watch the screen. What is measured is that the composition state which
produced the displacement — an all-PQ multilayer port with the GUI plane tagged
`HDR10[2]` and `overlay_mode[1]` — did not occur in any captured state. The
operator's acceptance of the picture stands from the Android-parity gate; this
gate re-establishes the machine state underneath it, not the perception.

## 5. Verdict

`CLEANUP PASS`. The repository is consistent with the architecture it
describes, the appliance is free of the experiment residue, the active kernel
and its rollback are intact, no package changed, and the accepted HDR
composition re-measured byte-identical after a reboot.

MA1 HDMI compressed-audio passthrough is the next gate and is not started.
