# Gate S1 — direct-KMS process-lifecycle handoff validation

## 1. Result

`NO_SAFE_HANDOFF_PROBE`

The lifecycle experiment was stopped before playback was opened or Kodi was
stopped.  The accepted repository contains GBM/EGL smoke-test source, but the
candidate does not acquire DRM master, set a mode, attach its rendered buffer
to a CRTC, or issue an atomic/page-flip commit.  The production target contains
no deployed copy of that probe (and no other finite GBM/KMS probe).  Therefore
running the candidate could not answer whether a transient direct-KMS owner can
acquire and release the display between Kodi lifecycles.

No handoff result is inferred from a render-only GBM/EGL test.  The PASS token
`DIRECT_KMS_LIFECYCLE_HANDOFF_PASS` is not awarded.

Gate observation window: `2026-09-11T19:01:58+03:00` through
`2026-09-11T19:05:35+03:00`.

## 2. Baseline

| Item | Observed | Required | Status |
| --- | --- | --- | --- |
| Main worktree | clean | clean | PASS |
| Main `HEAD` | `d02bd6f31b305daa10decb4eb4d2adf87879fdf6` | `d02bd6f` | PASS |
| `origin/main` | `d02bd6f31b305daa10decb4eb4d2adf87879fdf6` | `d02bd6f` | PASS |
| S1 branch | `agent/codex-s1-drm-handoff` | same | PASS |
| S1 worktree | `/tmp/rk3588-mediabox-codex-s1` | same | PASS |
| Reference repo `HEAD` | `582e1d30c6762c134118445bf660c0784aaffe58` | same | PASS |
| Target kernel | `6.1.115-vendor-rk35xx-screenbridge-hdmirx-audio` | same | PASS |
| Target uptime at start | `3:33` (`12808.71` seconds) | record | PASS |
| Kodi JSON-RPC | `{"id":"phase0","jsonrpc":"2.0","result":"pong"}` | reachable by Ping | PASS |
| Active players | `[]` | record | PASS |
| HDMI connector | connected, enabled | record | PASS |
| Unexpected display owner | none | none | PASS |

The two S0 reports were read with `git show` from commits `fc0cfa2` and
`c589d3bfd8248619865806e7d9b041e4186154f6`; neither branch was merged.

Canonical asset:

```text
path   /var/tmp/mp1b/past-lives.mkv
size   12081238172 bytes
mode   0644 root:root
mtime  2026-09-09 18:47:33.881648926 +0300
sha256 f4e32b8d3feb7efdebaaefaf7338a25bcc86ee144bf82bd4b93da6b80906f96a
```

The hash matches the required canonical asset.  It was not modified, moved,
replaced, or deleted.

Initial display-card clients:

```text
command   pid   dev master a uid magic
kodi-gbm  40781   0 y      y   0     0
kodi-gbm  40781 128 n      y   0     0
```

Kodi alone held `/dev/dri/card0` (fd 29) and `/dev/dri/renderD128` (fd 30).
`card1`/`renderD129` are the NPU device and had no clients.

## 3. Production Kodi lifecycle

The exact running process was:

```text
PID      40781 (PPID 1)
command  /opt/rk3588-mediabox/kodi/lib/kodi/kodi-gbm --standalone --debug
exe      /opt/rk3588-mediabox/kodi/lib/kodi/kodi-gbm
cwd      /var/tmp/kodi-home
cgroup   /user.slice/user-0.slice/session-238.scope
sha256   bfa5b1cb2ed73d139f0bcdb8652e73ea489f71edac51534fbeceddd73b1f71fb
build    Kodi 22.0-BETA2, Git 20260831-e513e0ff43
```

Environment relevant to the accepted launch:

```text
HOME=/var/tmp/kodi-home
MEDIABOX_GPU=mali
LD_LIBRARY_PATH=/opt/rk3588-mediabox/mali-g24p0-runtime/lib:/opt/rk3588-screenbridge/lib
AE_SINK=ALSA
XDG_RUNTIME_DIR=/run/user/0
DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/0/bus
```

There is no Kodi service or installed Kodi unit.  `systemctl status 40781`
resolved the already-orphaned process to abandoned transient SSH
`session-238.scope`.  The production mechanism is the operator-driven
`scripts/run-kodi-rk3588.sh` launcher, which starts the exact GBM executable
with `setsid --fork` and the environment above.  Its normal stop path begins
with JSON-RPC `Application.Quit` and then checks for stale processes.

This ownership was resolved safely, so `KODI_LIFECYCLE_UNRESOLVED` does not
apply.  The lifecycle commands were deliberately not invoked after the probe
safety prerequisite failed.

## 4. PRE_HANDOFF_GOLDEN

`NOT_RUN — NO_SAFE_HANDOFF_PROBE`.

The blocker was established through source and target inventory before any
display interruption.  Opening a 12 GB canonical asset, changing HDMI mode,
then stopping Kodi would add risk without making the unavailable ownership
proof possible.  No Kodi setting was changed and no playback request was sent.

## 5. Kodi DRM release evidence

`NOT_RUN — NO_SAFE_HANDOFF_PROBE`.

Kodi remained PID 40781 and DRM master for the entire observation window.
Consequently `KODI_DRM_RELEASE_FAIL` was neither tested nor asserted.

## 6. Transient direct-KMS client evidence

Repository candidate inspected:
`tools/gbm-egl-probe.c` at
`a2bac15d3201483ff6319510d0d169aff39d27f6fc72b9f02e806363d99a4471`.

Its implemented sequence is:

1. `open(..., O_RDWR | O_CLOEXEC)` on `/dev/dri/card0`;
2. `gbm_create_device`;
3. `eglGetPlatformDisplayEXT(EGL_PLATFORM_GBM_KHR, ...)` and `eglInitialize`;
4. create a GLES2 context and scanout-capable GBM surface;
5. render, `eglSwapBuffers`, and lock/release one front buffer;
6. destroy EGL/GBM objects and close the DRM fd.

Source inspection found no `drmSetMaster`, `drmModeSetCrtc`,
`drmModeAtomicCommit`, framebuffer registration, CRTC/connector discovery, or
page flip.  A GBM surface carrying `GBM_BO_USE_SCANOUT` is only an allocation
capability; it does not make the process DRM master or place that buffer on the
display.  The companion accepted source `tools/mali-gbm-probe.c` states its
contract explicitly: “It never sets a mode and never becomes DRM master.”

Target inventory searched `/tmp`, `/var/tmp`, `/opt`, `/usr/local`, and `/root`.
No `gbm-egl-probe`, `mali-gbm-probe`, `hdr-signaling-probe`,
`hdr-playback-probe`, `kmscube`, or other named GBM/KMS probe binary was
present.  `/usr/bin/modetest` was present, but it is a KMS test utility rather
than a GBM/EGL client and therefore cannot, by itself, validate the required
transient direct-KMS/GBM owner.  It was not run in modesetting mode.

The repository also contains direct-KMS HDR probe source, but no corresponding
binary exists on the cleaned production target.  Building, uploading, or
altering a display probe was not among the allowed target state changes, and
the Gate explicitly prohibited writing a new display application.

Accordingly the most specific mandated stop token is
`NO_SAFE_HANDOFF_PROBE`.  No transient probe was launched.

## 7. Kodi DRM reacquire evidence

`NOT_RUN — NO_SAFE_HANDOFF_PROBE`.

Kodi was never stopped, so reacquisition was not attempted and
`KODI_DRM_REACQUIRE_FAIL` was not asserted.

## 8. POST_HANDOFF_GOLDEN

`NOT_RUN — NO_SAFE_HANDOFF_PROBE`.

There was no handoff and therefore no post-handoff playback state to capture.

## 9. PRE vs POST invariant comparison

| Accepted invariant | PRE | POST | Comparison |
| --- | --- | --- | --- |
| RKMPP hardware decode | NOT_MEASURED | NOT_MEASURED | NOT_TESTED |
| DRM PRIME | NOT_MEASURED | NOT_MEASURED | NOT_TESTED |
| HEVC Main10 / NV15 | NOT_MEASURED | NOT_MEASURED | NOT_TESTED |
| 4K23.976 / HDR10 | NOT_MEASURED | NOT_MEASURED | NOT_TESTED |
| BT2020_YCC / 30-bit / YUYV10_1X20 | NOT_MEASURED | NOT_MEASURED | NOT_TESTED |
| Video EOTF 2 / GUI EOTF 0 | NOT_MEASURED | NOT_MEASURED | NOT_TESTED |
| SDR2HDR / `SDR2HDR_CTRL=0x0000000b` | NOT_MEASURED | NOT_MEASURED | NOT_TESTED |
| HDR2SDR OFF | NOT_MEASURED | NOT_MEASURED | NOT_TESTED |
| Horizontal pause/resume shift | NOT_MEASURED | NOT_MEASURED | NOT_TESTED |

No regression is claimed, but preservation across a handoff is unproved.

## 10. Three-cycle repeatability

`0/3 — NOT_RUN — NO_SAFE_HANDOFF_PROBE`.

Repeatability cannot be tested until one safe, sufficient lifecycle cycle is
possible.

## 11. Timing table

No lifecycle timestamp was generated.  Values are not inferred from the
launcher or previous gates.

| Measurement | Cycle 1 | Cycle 2 | Cycle 3 |
| --- | --- | --- | --- |
| Kodi stop → process gone | NOT_MEASURED | NOT_MEASURED | NOT_MEASURED |
| Kodi stop → DRM released | NOT_MEASURED | NOT_MEASURED | NOT_MEASURED |
| Probe launch → display acquired | NOT_MEASURED | NOT_MEASURED | NOT_MEASURED |
| Probe exit → DRM released | NOT_MEASURED | NOT_MEASURED | NOT_MEASURED |
| Kodi start → process created | NOT_MEASURED | NOT_MEASURED | NOT_MEASURED |
| Kodi start → DRM initialized/acquired | NOT_MEASURED | NOT_MEASURED | NOT_MEASURED |
| Kodi start → JSON-RPC ready | NOT_MEASURED | NOT_MEASURED | NOT_MEASURED |
| Total UI-to-Kodi-style handoff budget | NOT_MEASURED | NOT_MEASURED | NOT_MEASURED |

## 12. Gate-window error audit

Only entries at or after `2026-09-11 19:01:58 +03:00` were searched in the
complete journal and kernel journal for:

```text
drm, vop, vop2, atomic, commit fail, timeout, iommu, mpp, rkmpp,
segfault, fatal, failed to set mode, drm master, permission
```

There were zero matching journal entries and zero matching kernel-journal
entries.  Historical messages before the gate window were not classified as
new failures.

## 13. Final target state

At `2026-09-11T19:05:35.392047444+03:00`:

- Kodi was still the original PID 40781 with the original executable and was
  the sole master of display `card0`.
- Kodi still held only the observed display fds: `card0` fd 29 and
  `renderD128` fd 30.
- no probe, `kmscube`, or `modetest` process remained;
- JSON-RPC Ping returned `pong`, and active players remained `[]`;
- HDMI-A-1 remained connected/enabled at the unchanged idle
  `1920x1080p60`, RGB888, SDR state;
- the target Kodi settings hash was unchanged across the gate window:
  `fa075bee21609a606712999b70002104e129f005005240d761f0993259bc4328`;
- the target ALSA card-file hash was unchanged:
  `492b921e8b3bba6c472b981d8f40cc920e412e7a48237d371020a24303a48e`;
- no service was enabled/disabled, no configuration was written, and no
  kernel, DT, bootloader, networking, CEC, Bluetooth, or playback state was
  intentionally changed.

The original production-running state was preserved; `TARGET_RESTORE_FAIL`
does not apply.

## 14. Exact recommendation for S2

Do not begin S2 implementation or treat process-lifecycle handoff as proven.
First establish a separately reviewed, accepted probe baseline, then rerun S1.

The prerequisite probe must be one finite executable that demonstrably:

1. opens `card0` and successfully calls `drmSetMaster`;
2. creates the GBM device, EGL GBM-platform display, context, and
   scanout-capable surface;
3. renders and registers the front buffer as a DRM framebuffer;
4. performs one real atomic modeset/page flip on the already-connected output,
   emitting a machine-detectable “display acquired” point for T4;
5. captures and restores the original CRTC, plane, connector, and colour/HDR
   property state;
6. releases the framebuffer/EGL/GBM objects, calls `drmDropMaster`, closes all
   DRM fds, and exits by itself on success and handled failure;
7. is built and staged through an explicitly authorized workflow, with binary
   and source hashes recorded, without packages or persistent target config.

An update to `gbm-egl-probe` meeting that contract is preferable to combining
`modetest` with a separate render-only probe, because a two-process surrogate
does not prove that the proposed transient UI process owns both halves of the
direct-KMS/GBM path.  Once that tool is accepted and present, rerun this exact
S1 gate from Phase 0 and require all three lifecycle cycles plus PRE/POST HDR
evidence before S2.

## 15. Raw evidence paths/checksums where applicable

No large transient logs were added.  Evidence metadata is contained in this
report, consistent with the repository cleanup policy that removed one-off
probe binaries while retaining canonical source and reports.

| Evidence | Location / identity |
| --- | --- |
| S0-A report | commit `fc0cfa2`, `results/mediabox-platform/s0a-architecture-stremio-web-cec-2026-09-11.md` |
| S0-B report | commit `c589d3bfd8248619865806e7d9b041e4186154f6`, `results/mediabox-platform/s0b-target-runtime-display-cec-input-2026-09-11.md` |
| Candidate source | `tools/gbm-egl-probe.c`, SHA-256 `a2bac15d3201483ff6319510d0d169aff39d27f6fc72b9f02e806363d99a4471` |
| Companion source | `tools/mali-gbm-probe.c`, SHA-256 `de62ee6df7c83df7478bcedd4c67d73550ea80ba8e028c385002e3fd6645d7ff` |
| Canonical capture tool | `tools/vop2-sdr2hdr-capture/capture-state.sh` (inspected; not run) |
| Kodi lifecycle tool | `scripts/run-kodi-rk3588.sh` (inspected; not run) |
| Canonical asset | `/var/tmp/mp1b/past-lives.mkv`, SHA-256 `f4e32b8d3feb7efdebaaefaf7338a25bcc86ee144bf82bd4b93da6b80906f96a` |
| Production Kodi executable | `/opt/rk3588-mediabox/kodi/lib/kodi/kodi-gbm`, SHA-256 `bfa5b1cb2ed73d139f0bcdb8652e73ea489f71edac51534fbeceddd73b1f71fb` |
