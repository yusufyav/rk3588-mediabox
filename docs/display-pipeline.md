# The display pipeline: what must not be broken again

This file exists because each rule below was learned by breaking it on the
appliance, in front of the person using it. None of them is obvious from the
code, and every one of them looks like a harmless improvement right up until
the television shows it.

Each rule names the one command that tells you whether it currently holds.

---

## 1. The browser's scale factor must be a whole number

**Never** pass a fractional `--force-device-scale-factor` to the kiosk browser.

A fractional scale costs this board direct scanout. Chromium cannot present a
buffer that matches the output exactly at, say, 1.33, so wlroots refuses the
direct flip and composites instead — and compositing puts this board's damage
bookkeeping back in the path, which is visible as **tearing**.

Measured on a 2560x1440 panel, changing nothing but this number:

| scale | `Cluster0-win0 format` | what it means |
|-------|------------------------|---------------|
| 1.33  | `XR24`                 | wlroots composited the frame |
| 2     | `AB24`                 | the browser's own buffer went straight out |

The user reported tearing within minutes of the 1.33 build reaching the
television, and it went away when the scale went back to a whole number.

`packaging/mediabox-display-scale` measures the panel and then rounds **up** to
a whole number, floor 1, ceiling 4.

**Check:**

```sh
awk '/Cluster0-win0/{f=1} f&&/format:/{print $2; exit}' /sys/kernel/debug/dri/0/summary
```

`AB24` is correct. `XR24` means direct scanout has been lost and the picture is
tearing — look at the scale factor first.

---

## 2. No resolution is written down anywhere

This appliance is plugged into whatever panel the house has: a 4K television,
a 1440p monitor, an older 1080p set. Every place that named a resolution was a
place that failed on somebody else's panel.

Removed, and must not come back:

| file | what it used to say |
|------|---------------------|
| `config/sway-kiosk.conf` (file since removed with the shell) | `output HDMI-A-1 mode 3840x2160@60Hz` |
| `config/sway-browser.conf` | `output HDMI-A-1 mode 1920x1080@60Hz` |
| `packaging/mediabox-kiosk-browser` (file since removed with the shell) | `--force-device-scale-factor=2` |
| `packaging/mediabox-kiosk-smoke` | `MODE=1920x1080` |

A pinned mode the panel cannot do produces, on every start:

```
[sway/config/output.c:887] Requested backend configuration failed, searching for valid fallbacks
[sway/config/output.c:896] Search for valid config failed
```

— a failed mode set followed by a fallback, which a person sees as the
television going dark twice on its way to the home screen.

wlroots takes the panel's preferred mode when nothing is written down — that is
the browser application's compositor. The television's own interface asks
`mediabox-platform` for the selected output and takes that connector's preferred
mode. The layout survives the change because the scale is measured from the mode
that was actually taken (rule 1), and `mediabox-display-changed`, triggered by
udev on a DRM hotplug, restarts the interface if the panel is swapped while the
box is running.

**Check:**

```sh
grep -rn '^output .* mode ' /etc/mediabox/          # must print nothing
journalctl -u mediabox-tv-ui -b | grep -c 'Requested backend configuration failed'
```

---

## 3. `mediabox-tv-ui.service` has an empty cgroup

`PAMName=login` — which the compositor needs, because libseat will not hand it
DRM master without a real login session — puts sway and every Chromium process
into a **PAM session scope**, not into the service's own cgroup.

```
$ systemd-cgls -u mediabox-tv-ui.service
Unit mediabox-tv-ui.service (/system.slice/mediabox-tv-ui.service):
                                                    <- empty
$ cat /proc/$(pgrep -f -- --app=http)/cgroup
/user.slice/user-0.slice/session-312.scope
```

So `KillMode=` reaches nobody. Stopping the unit kills the main PID and nothing
else; the browser survives with no output to draw on, spinning, and the next
thing to take the display — Kodi — starts on top of it. Orphans accumulated one
per restart.

Everything the unit leaves behind is killed by `ExecStopPost=`, and it must
**wait**: `mediabox-ui-reap` sends SIGTERM, waits up to 5 s, sends SIGKILL, and
only then returns. A bare `pkill` signals and returns, which is the same race
with a shorter fuse.

**Check:** after a stop, `pgrep -cf -- '--user-data-dir=/var/lib/mediabox-ui/chromium'`
must print 0.

---

## 4. Kodi takes about 5 seconds to let go

Measured: `Stopping kodi.service` at 02:54:00.300, `Deactivated successfully` at
02:54:05.200.

`mediabox-display-guard` used to wait a flat `sleep 2` before giving the display
back, so the compositor was started while Kodi still had the connector and came
up to the failed mode set from rule 2. Waiting for `kodi-gbm` to be gone and
then half a second more replaced it, and that in turn is now done by systemd:
the guard queues `mediabox-display-recover` as a transient unit with
`After=kodi.service`, so the start job cannot run until kodi.service's stop job
has finished and its processes have been reaped -- which is when the kernel
releases the master handle.

Do not replace that with a number.

**Check:** in the journal for a handover, `Starting mediabox-tv-ui.service` must
come **after** `kodi.service: Deactivated successfully`.

---

## 5. The panel can come back from Kodi showing green

Twice the monitor returned from a Kodi session showing nothing but green while
everything measurable on this side was correct — `RGB888_1X24`, `SDR[0]
BT.709`, full range, the mode right, the plane active and the interface drawing
into it. The fault is on the far end of the cable.

Turning the monitor off and on cleared it. So does blanking and unblanking the
output from the compositor, which renegotiates the link without anybody reaching
behind the television:

```sh
SWAYSOCK=$(ls /run/mediabox-ui/sway-ipc.*.sock | head -1) \
  swaymsg 'output HDMI-A-1 dpms off' && sleep 2 && \
SWAYSOCK=$(ls /run/mediabox-ui/sway-ipc.*.sock | head -1) \
  swaymsg 'output HDMI-A-1 dpms on'
```

This was automated once, by a `mediabox-display-settle` script that acted on a
timestamp `mediabox-display-guard` left at `/run/mediabox/display-handback` when
it was less than 60 s old. Both are gone, and the `swaymsg` above is why: the
script could only ever speak to a compositor, so it went when the compositor
did, and the timestamp outlived it by two releases with a writer and no reader
at all. Removed rather than kept, because a marker nothing reads is a contract
the next person will believe.

The symptom has not been seen on the native shell. If the green cast comes back
there, this is the shape the fix took and it needs rebuilding against KMS —
blank and unblank the connector directly — but it must not run on a cold boot,
where that is a second of black screen that buys nothing.

Note also that `EDID` reading 0 on this kernel is normal and is **not**
evidence of a fault. Do not chase it.

---

## 6. A screenshot is not evidence

`grim` captures what the compositor rendered, not what the panel received. A
green screen photographs perfectly in `grim`. Claiming a display fault is fixed
on the strength of a screenshot has been wrong here every time it was tried.

Evidence about the display comes from `/sys/kernel/debug/dri/0/summary`,
`/sys/kernel/debug/dri/0/state`, `modetest -M rockchip -c`, and from the person
looking at the television.

Likewise, driving the interface with `wtype` or CDP is not a substitute for the
physical remote. Sending keys blind once landed focus on "Kodi'ye Aktar", handed
the display to Kodi and left the monitor stuck.

---

## The smoke test

`packaging/mediabox-kiosk-smoke` checks rules 1 and 2 on every deploy, and
fails the deploy rather than reporting success over a blank screen:

```
PASS  output mode   2560x1440 (panel's own)      # DRM agrees with the compositor
PASS  ui scale      2 for 2560x1440              # whole number, measured
PASS  input devices 8 keyboard(s)
PASS  browser url   http://127.0.0.1:8787/
PASS  ui loaded     'MediaBox' at 2560x1440      # window title is the page's own
```

Nothing in it names a resolution, and nothing in it should.

## No Wayland client gets a GPU on this board

Every accelerated thing on this appliance is a *compositor* or a DRM client:
sway renders through GBM, Kodi runs GBM/DRM standalone. Nothing had ever asked
for GL as an ordinary Wayland client, and it turns out nothing can.

`scripts/install-mali-runtime.sh` pins
`libmali-valhall-g610-g24p0-gbm_1.9-1_arm64.deb`. That build advertises exactly
two EGL platform extensions, which is the whole story for a client:

```sh
strings /opt/rk3588-mediabox/mali-g24p0-runtime/lib/libmali.so.1 \
  | grep -oE 'EGL_[A-Z]+_platform_[a-z_]+' | sort -u
```

```text
EGL_EXT_platform_base
EGL_KHR_platform_gbm
```

What that costs is not obvious from outside, because nothing fails loudly. A
native Wayland client asked for a window, got as far as `wl_compositor`
`create_surface`, and stopped: no `xdg_surface`, no `xdg_toplevel`, no buffer
ever attached. The process stayed up, answered the control plane, logged its
metrics and drew nothing, and the television showed the compositor's background
colour.

### The Wayland build of the same driver does not fix it

The same pinned upstream release carries
`libmali-valhall-g610-g24p0-wayland-gbm_1.9-1_arm64.deb` — the same driver
version with the Wayland platform alongside GBM. It was installed, measured and
rolled back. It does advertise what it should:

```text
EGL_EXT_platform_base
EGL_EXT_platform_wayland
EGL_KHR_platform_gbm
EGL_KHR_platform_wayland
```

and it genuinely initialises: the client opened `/dev/mali0`, got its
`dma_heap` fds and its `mali-cpu-command` / `mali-event-handler` threads, and
made its own Wayland connection. The window was still never mapped.

It is not the toolkit. `mpv`, built with Wayland and EGL and entirely unrelated
to the interface, fails the same way on the same library:

```sh
LD_LIBRARY_PATH=/opt/rk3588-mediabox/mali-g24p0-runtime/lib \
  mpv --vo=gpu --gpu-context=wayland --msg-level=vo/gpu=debug <file>
```

```text
[vo/gpu/opengl] Initializing GPU context 'wayland'
[vo/gpu] Failed initializing any suitable GPU context!
```

No `EGL_VERSION` line: the display never initialises. The same command with the
Mali path removed reaches `EGL_VERSION=1.5`, `EGL_VENDOR=Mesa Project` — and
`libEGL warning: egl: failed to create dri2 screen`, because the vendor kernel
offers Mesa no DRI driver, so that path is llvmpipe.

So a Wayland client here has two options and both are wrong: no picture at all,
or a software-rasterised one. Measured, the software one costs the property
this whole document exists to protect — the plane's format was `XR24` rather
than the client's own buffer, which is direct scanout gone, and one first paint
took 38% of a core.

**Rolled back.** The GBM build is authoritative, and the check above is how to
tell which one is installed:

| | |
| --- | --- |
| package | `libmali-valhall-g610-g24p0-gbm_1.9-1_arm64.deb` |
| deb sha256 | `32ffe853e8d56295284637252f1da15dd868a8f7c6b8da6b9f77616ba285eb1a` |
| `libmali.so.1.9.0` sha256 | `fc17c1c2b4a2dea84df0811ae574e288bf10ae97dc4ef8911d0c75d335339a57` |

Before and after the swap, Kodi's chain was identical and is the thing any
future attempt has to leave alone — captured in `results/mali-wayland-gbm/`:

```text
bus_format[200d]: YUYV10_1X20
overlay_mode[0] output_mode[9] HDR10[2] color-encoding[BT.2020] color-range[Limited]
Display mode: 1920x1080p24
Esmart0-win0: format NV15, color HDR10[2] BT.2020 Limited, src 3840x2160 -> dst 1920x1080
```

That capture was taken while the compositor was driving 1920x1080, which is
what it had chosen — not what the panel offers. The attached monitor lists 59
modes including 3840x2160, and prefers 3840x2560:

```sh
sort -u "$(mediabox-platform connector-path)/modes" | sort -t x -k1 -rn | head -3
```

So the colour chain above is the accepted one and the scanout size is not, but
the reason is the compositor's mode choice rather than the panel's capability.
Why wlroots settled on 1080p with no mode written down anywhere is a separate
question from this one and has not been chased.

### Where exactly the Wayland path fails

`tools/egl-wayland-probe.c` answers it without a toolkit in the way: Wayland,
EGL, and nothing else. Built for the appliance and run there against the
wayland-gbm build, it gets the same driver to answer twice in one process:

```text
== control: the same driver on the GBM platform
  display: 0xaaaafd525ae0  error: EGL_SUCCESS
  arm_release_ver: g24p0-00eac0, rk_so_ver: 10
  eglInitialize returned 1  error: EGL_SUCCESS
  EGL_VERSION = 1.5 Valhall-"g24p0-00eac0"
  EGL_VENDOR  = ARM
  configs: 27

== eglGetPlatformDisplayEXT(EGL_PLATFORM_WAYLAND_KHR)
  display: 0xaaaafd6899b0  error: EGL_SUCCESS

== eglInitialize
  arm_release_ver: g24p0-00eac0, rk_so_ver: 10
  returned 0  error: EGL_NOT_INITIALIZED
```

So the driver is usable from an ordinary process — it is not a compositor-only
library, and the GBM platform initialises and offers 27 configs. The Wayland
platform is advertised, hands back a valid display, and then refuses to
initialise it.

mpv, pointed at the same runtime, fails at the same place with the same
signature — the driver banner prints, and no `EGL_VERSION` line ever follows:

```text
[vo/gpu/opengl] Initializing GPU context 'wayland'
arm_release_ver: g24p0-00eac0, rk_so_ver: 10
[vo/gpu] Failed initializing any suitable GPU context!
```

Three things were ruled out along the way, so they do not need ruling out
again. `libEGL.so.1` resolves to `libmali.so.1` and not to Mesa. The client
extension string really does carry `EGL_EXT_platform_wayland` and
`EGL_KHR_platform_wayland` at run time. And the `libwayland-egl.so.1` the
package ships under `mali/` — which `install-mali-runtime.sh` does not link
into the runtime directory — makes no difference: adding it changes nothing,
the failure is identical either way.

The conclusion is the driver's own Wayland backend, and there is nothing left
in this repository's configuration to change about it.
