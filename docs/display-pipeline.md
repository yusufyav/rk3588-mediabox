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

A mode a person chooses in the display settings is kept, but only for the
display it was chosen on (rule 9); nothing below may come back:

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

The television's own interface takes the mode rule 9 gives it, and the
browser's compositor is told the same mode through
`/run/mediabox/sway-output.conf`. The layout survives the change because the scale is measured from the mode
that was actually taken (rule 1), and `mediabox-display-changed`, triggered by
udev on a DRM hotplug, compares against the state `mediabox-display-seed`
records at boot -- so the first plug after a headless boot is not lost -- and
when a panel is plugged in hands the interface or Kodi the display again (stop,
`mediabox-hdmi-prepare` as root, start). The browser is not restarted: sway
modesets in place and the same Chromium carries on; only its configuration is
refreshed, and sway is reloaded only when the mode it should take changed.

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

## 7. The plane's transfer curve is written on every film, including an SDR one

`EOTF` belongs to the **plane**, not to the frame, and it outlives the process
that set it. Kodi leaves it on ST 2084 after an HDR film; a player that never
writes it inherits that for the next film. Measured on the Plus after a Kodi
handover, with an SDR clip on the plane:

```
format: NV12 little-endian     color: HDR10[2]
```

and on the television, a magenta picture.

This was invisible for a second reason worth remembering: **the property only
exists for a client that has asked for the atomic capability.** The interface
had asked only for universal planes, so the lookup found nothing and reported
nothing — there was no error anywhere to find.

```
modetest -M rockchip -p    | grep -c EOTF     # 0
modetest -M rockchip -a -p | grep -c EOTF     # 8, one per plane
```

Check: play anything and read the plane's colour line; `SDR[0]` for an SDR
film, `HDR10[2]` for a PQ one, whatever played before it.

## 8. Either HDMI socket must reach the video port with the HDR block

On RK3588 the video ports are not equals: the VOP2's HDR conversion block is
behind VP0. The driver says so on each CRTC, and this is the whole basis for
choosing one:

```
CRTC 89   PORT_ID 0   FEATURE 7
CRTC 130  PORT_ID 1   FEATURE 1
CRTC 170  PORT_ID 2   FEATURE 1
```

The board's device tree wires both HDMI transmitters to all three ports and
then switches the crossings off, which the kernel publishes as
`possible_crtcs 0x1` and `0x2` — one socket per port, no choice in userspace.
`packaging/overlays/mediabox-hdmi-any-vp.dtbo` switches the two crossings back
on; the interface then ranks the CRTCs a connector can reach by that FEATURE
mask and moves the television to the best one.

Do not "simplify" this into a fixed connector-to-port table. The answer is the
driver's, per board, and the cable moves.

```
modetest -M rockchip -e                        # both TMDS encoders: 0x3
grep -E '^Video Port|Connector:' /sys/kernel/debug/dri/0/summary
```

## 9. The mode and colour are the HDMI rules' answer, and the person's choice

What the display can be sent is worked out once, in
`mediabox_platform::output`, from the mode list the kernel gives the connector
and the display's EDID, by mainline Linux's own rules
(`drm_hdmi_compute_mode_clock`, `sink_supports_format_bpc`,
`hdmi_clock_valid`, the Y420VDB/Y420CMDB parsing of `drm_edid.c`). Every mode
gets every colour cell -- RGB, 4:4:4, 4:2:2, 4:2:0 at each depth the board has
-- with the rate it costs and, when it cannot be sent, the rule that says so.
The television's settings, the web page and `mediaboxctl display modes` all
draw that one answer.

`Auto` is the reference Android box's rule, measured on both inputs of the
Sony: the largest mode in the shape of the sink's preferred one, at the fastest
refresh the link carries **in any format**. On the 300 MHz input that is
2160p60 in 4:2:0 eight-bit, not the 1080p the set marks preferred; `Auto`
colour is RGB 8 bit where it fits and 4:2:0 8 bit otherwise, and for an HDR
film the first format that carries ten bits (RGB, 4:4:4, 4:2:2, 4:2:0).

A person can choose another mode and another colour at it. The choice is one
record, bound to the display it was made on by the EDID's block checksums --
the Android box's `hdmimode` / `<mode>_deepcolor` / `hdmichecksum`. A display
with another EDID is `Auto`, and the record moves to it: a mode chosen for one
display is never tried on another (rule 10 is what that costs).

A change is applied in place -- one atomic commit of mode, frame and colour,
asked first with `TEST_ONLY` -- and is on trial: unless it is kept within 15
seconds the daemon puts the earlier setting back, and it never reached the disk,
so a restart finds the earlier one too.

Kodi and the browser start from the same setting: the daemon writes
`/run/mediabox/output-plan` and `mediabox-hdmi-prepare` gives Kodi the chosen
mode and every refresh of that size as its whitelist -- a television box does
not drop to 1080p because a film is 1080p; it keeps the resolution and changes
cadence, and the one scale left is the display controller's:

```
Display mode: 3840x2160p24        # 1080p24 film, playing in Kodi
plane src 1920x1080 -> dst 3840x2160
```

Kodi's colour comes from the same place (`patches/kodi/0013`): the plan has a
line per mode, `colour 3840x2160@296703/5500x2250 rgb:8 ycbcr422:10` -- size,
clock and totals, because 2160p23.976 and 2160p29.97 share a clock and 1080i60
and 1080p30 share the totals too (an interlaced size carries an `i`) -- with
the SDR colour and the HDR one. Kodi names that format to the driver rather than
asking for RGB and letting it negotiate down, and tags it truthfully. The
interface reports the display again whenever it reconnects to the daemon,
because the daemon's runtime directory, and the plan in it, goes with it.

**Check:**

```sh
mediaboxctl display status        # what is on the wire, from the display controller
mediaboxctl display modes 3840x2160p30
cat /run/mediabox/output-plan
```

## 10. One mode across a handover — and never by switching fbdev emulation off

Between two applications the television belongs to nobody, and two things then
put a mode on it that nobody asked for:

* the kernel's fbdev emulation restores **its** mode, which is the sink's
  preferred one — 1920x1080 here;
* Kodi records the mode it found on startup and restores it on exit:
  `CDRMUtils::RestoreOriginalMode(): Set original crtc mode`.

Together they made one handover four mode changes: interface 4K, fbdev 1080p,
Kodi 4K, Kodi restores 1080p, interface 4K.

**Switching the emulation off does not fix it.** It was tried, it removes the
1080p, and it leaves Kodi with no mode to start from: measured, every video
port `DISABLED` and a black television. Kodi needs a mode already on the
connector — it just has to be the right one. The commit and its revert are in
the history for this reason.

This used to be answered with a mode on the kernel command line
(`video=HDMI-A-2:3840x2160@30`), written by `mediabox-hdmi-prepare`. It is no
longer written, and an existing one is removed. The argument belongs to the
display that was connected when it was written, and the kernel applies it to
whatever is connected at the next boot, before anything in this product runs.
A display that does not list that mode gets one invented by the GTF formula,
which the vendor HDMI driver does not reject. Measured on the Orange Pi 5 Plus,
2026-09-23: `video=HDMI-A-2:2560x1440@144` from a 1440p monitor, a Sony
television on the same socket, GTF 807.9 MHz, PHY PLL failure, SError in
`dw_hdmi_qp_setup`, kernel panic on every boot.

Without the argument the kernel takes its mode from the EDID of the display
actually connected. On a sink whose preferred mode is below the one Kodi uses,
a handover can show that preferred mode for a moment; that is the cost.

```
tr ' ' '\n' </proc/cmdline | grep ^video=               # expect nothing
/opt/rk3588-mediabox/bin/mediabox-hdmi-prepare        # says boot-video=not-asked
```

---

## 11. The television is told what it is being sent, and only when it fits

The plane's `EOTF` (rule 7) is how the display controller reads the film. It
says nothing to the television. Three connector properties do, and all three
are set together or none of them is:

```
HDR_OUTPUT_METADATA   the CTA-861 mastering infoframe, from the film
Colorspace            BT2020_RGB
color_depth           ten bits
```

In front of them is the decision this product already lives by: **can this
link carry it at the timing that is actually set?** Ten-bit RGB is 1.25x the
pixel clock; 4:2:2 twelve-bit is not, which is why this hardware ends up
sending HDR as 4:2:2 and that is not a fault. A link with no room is told SDR
and the plane is tone-mapped, rather than being asked for HDR and left to
subsample.

Both branches, measured on the same television through its two inputs, when
4:2:2 was still not counted as carrying HDR (it is now: ten bits in a
twelve-bit container, and the Android box sends HDR on the 300 MHz input as
exactly that -- whether this board's driver renders it correctly is to be
measured) -- and since measured, on 2026-09-23, on the 300 MHz input: 4:2:2 ten-bit
HDR is correct on the television from this appliance's player, and from Kodi
once it asked for `ycbcr422` by name (`YUYV10_1X20 HDR10[2] BT.2020` at
2160p23.976). The old "wrong colours" were a format left to the driver and
tagged as something else:

| input | mode | decision | connector |
|---|---|---|---|
| HDMI 3, 600 MHz | 3840x2160p60 | `the film asks for HDR (eotf 2), and it fits` | `YUYV10_1X20  hdr_type[HDR10] eotf[2] BT.2020` |
| HDMI 1, 300 MHz | 3840x2160p30 | `cannot carry it at 296703 kHz: sending SDR` | `RGB888_1X24  hdr_type[SDR]` with `hdr2sdr[1]` on the plane |

and after the film, in both cases, back to `RGB888_1X24 hdr_type[SDR]`. A
television must never be left being told it is receiving HDR by a player that
has gone.

```
journalctl -u mediabox-tv-ui | grep -E 'the film asks|output colour'
grep -E 'bus_format|hdr_type' /sys/kernel/debug/dri/0/summary
```

---

## 12. A television that is switched off is not a fault, and a film cannot outlive the interface

Both halves of this were measured on the Ultra on 2026-09-20, in one run, from
one cause. It is worth writing down as one rule because that is how it
happened.

A film was playing in the appliance's own player. The television was switched
away. At **12:48:29** the interface's flip failed, the error went up through
Slint, and the process exited; on the way out it released the display, and
`dmesg` shows `vop2_crtc_atomic_disable` with no enable after it. With no
signal the connector went to `disconnected`, and from then on **every** start
of `mediabox-tv-ui.service` died in the same line —

```
mediabox-tv.platform no connected output with a usable mode
Error: no connected display output
```

— `Restart=on-failure`, three seconds, again. **Two hundred and fifty times
over eighteen minutes.** The box could not recover by itself; the set had to be
woken by hand, at **13:06:21**, before the next restart happened to succeed.

Meanwhile mpv was still running. It is a transient unit of its own, nothing
bound it to the interface, and a video output that cannot place a frame does
not stop mpv's demuxer, decoder or sound. So when the interface finally came
back it drew the home screen over a film that was still playing: **home screen
on the panel, film on the speakers.** That is what the person watching saw, and
it is the fault as they reported it.

Three things follow, and all three are in the product now:

1. **The interface waits for a television; it never exits for the lack of
   one.** `SplitDisplay::wait_for_a_television` re-reads the connector once a
   second, forever, and says so once and then once a minute. The run continues
   into the same code as if the set had been there all along. Because
   `main` blocks SIGTERM before the thread that consumes it exists, the wait
   checks `sigpending` itself — otherwise `systemctl stop` waits fifteen
   seconds for a SIGKILL, on every deploy.
2. **A display error with no display behind it is not an error.**
   `SplitDisplay::present` asks the connector, and if the television is gone it
   keeps the interface up, logs once, and forgets what it knew about the
   controller so the frame after the set returns sets the mode again instead of
   flipping onto the console's configuration.
3. **The film dies with the interface, twice over.** `vo_mediabox` sends
   `MP_KEY_CLOSE_WIN` the moment the socket to the interface fails — the window
   this player had *was* the interface — and the transient unit carries
   `BindsTo=mediabox-tv-ui.service`, which does not depend on mpv being well
   enough to notice.

```
# with no television: waits, and still answers a stop
mkdir -p /var/tmp/noscreen/sys/class/drm /var/tmp/noscreen/dev/dri
MEDIABOX_PLATFORM_ROOT=/var/tmp/noscreen mediabox-tv &
# mediabox-tv.platform no television is connected; waiting for one
kill -TERM %1   # mediabox-tv.exit asked to stop while waiting for a television

# nothing of the player survives the interface
systemctl restart mediabox-tv-ui && sleep 2 && systemctl is-active mediabox-player
```

### And the journal has to survive the fault it is meant to explain

None of the above could be read from the box at first, because Rockchip MPP
logs one line per skipped NAL unit **through syslog**, under an identifier of
its own, where neither mpv's `--msg-level` nor Kodi's log settings reach it. On
the run above: 145 447 of 146 108 journal entries, **99.5%**, which left
thirteen minutes of history on a box that had been up for two and three quarter
hours. The fault was twenty minutes old and every record of it had rotated
away.

`mpp_log_level=3` — MPP's own `MPP_LOG_WARN` — is now set by
`packaging/mediabox-player` and in `kodi.service`, and the player's transient
unit carries `LogRateLimitIntervalSec=30s` / `LogRateLimitBurst=500` so the
next library to do this cannot take the journal with it.

```
journalctl --no-pager | wc -l; journalctl --no-pager | grep -c 'mpp\['
journalctl --no-pager -o short-iso | head -1   # how far back the box remembers
```

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
