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
| `config/sway-kiosk.conf` | `output HDMI-A-1 mode 3840x2160@60Hz` |
| `config/sway-browser.conf` | `output HDMI-A-1 mode 1920x1080@60Hz` |
| `packaging/mediabox-kiosk-browser` | `--force-device-scale-factor=2` |
| `packaging/mediabox-kiosk-smoke` | `MODE=1920x1080` |

A pinned mode the panel cannot do produces, on every start:

```
[sway/config/output.c:887] Requested backend configuration failed, searching for valid fallbacks
[sway/config/output.c:896] Search for valid config failed
```

— a failed mode set followed by a fallback, which a person sees as the
television going dark twice on its way to the home screen.

wlroots takes the panel's preferred mode when nothing is written down. The
layout survives the change because the scale is measured from the mode that was
actually taken (rule 1), and `mediabox-display-watch` restarts the interface if
the panel is swapped while the box is running.

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
up to the failed mode set from rule 2. It now waits for `kodi-gbm` to be gone,
with a timeout, and then half a second more for the kernel to close the last fd.

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

`mediabox-display-settle` does this automatically, and **only** on the way back
from Kodi: `mediabox-display-guard` leaves a timestamp at
`/run/mediabox/display-handback` and the settle script acts on it if it is less
than 60 s old. It must not run on a cold boot — a blank and unblank is a second
of black screen that buys nothing there.

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
