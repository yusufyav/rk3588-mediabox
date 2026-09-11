# Gate S0-B: target runtime, display, input, and HDMI-CEC audit

Date: 2026-09-11
Target: `root@10.27.27.25` (`orangepi5-ultra`, RK3588 Orange Pi 5 Ultra)
Audit window: 2026-09-11 18:07:15–18:17:34 +03:00
Repository baseline: `HEAD = origin/main = d02bd6f31b305daa10decb4eb4d2adf87879fdf6`, original tree clean
Audit branch/worktree: `agent/codex-s0b-runtime-audit`, `/tmp/rk3588-mediabox-codex-s0b`
Reference repository: `/home/yu/Projeler/rk3588-screenbridge` at `582e1d30c6762c134118445bf660c0784aaffe58`

Every target command below used this SSH prefix:

```text
ssh -o BatchMode=yes -F /dev/null -i /home/yu/.ssh/id_ed25519 -o IdentitiesOnly=yes root@10.27.27.25 '<command>'
```

The audit was read-only. Kodi was not restarted, DRM master and VTs were not
changed, input devices were not grabbed, CEC messages were not transmitted,
and no package, service, setting, pairing, display, audio, or network state was
changed. The only network requests to Kodi were `JSONRPC.Ping`, a TCP ping
attempt, a WebSocket handshake, and `Player.GetActivePlayers`. No
`Player.Open` call was made.

## 1. Executive summary

The target is a deliberately minimal, direct-to-DRM appliance rather than a
desktop system. Kodi 22 Beta 2 runs as root through the GBM executable, owns
DRM master on the Rockchip display card, renders an SDR 1920x1080p60 idle GUI,
and directly opens the available input event devices. There is no running or
installed X server, Wayland compositor, display manager, browser, or WebView.
The existing launcher starts Kodi from SSH with `setsid --fork`; there is no
Kodi systemd service or boot ordering to integrate with yet.

Kodi JSON-RPC is operational, but its HTTP service is exposed without
authentication on every IPv4 and IPv6 interface at port 8080. Raw
TCP/WebSocket JSON-RPC is loopback-only on port 9090. A future browser UI
should not speak directly to the current unauthenticated Kodi LAN endpoint;
`mediaboxd` should proxy a loopback-only Kodi endpoint and provide its own
authenticated, least-privilege LAN API.

Linux-native CEC is real and usable at the adapter level: `/dev/cec0` is backed
by `dwhdmi-rockchip`, reports physical address `1.0.0.0`, and supports logical
addresses, transmit, passthrough, and remote-control support. It currently has
zero logical addresses and no process has it open. Kodi loads its generic CEC
peripheral mapping, but neither maps libCEC nor opens `/dev/cec0`; there is no
current contention. Topology was deliberately not queried because the
available `cec-ctl --show-topology` path may transmit.

Bluetooth is **not** ready for a HID pairing test. The AP6611/SYN43711 SDIO
function and `hci0` sysfs node exist, but BlueZ executables and daemon are
absent, there is no `/var/lib/bluetooth`, both Bluetooth rfkill entries are
soft-blocked, and the enabled board bring-up unit failed because
`/usr/sbin/rfkill` is missing.

The evidence supports `DIRECT_KMS_UI_HANDOFF_LIKELY`: the system already uses
exclusive direct DRM with no compositor, and therefore an idle direct-KMS UI
could plausibly own `card0`, terminate completely, and allow Kodi to acquire
master. This is a feasibility classification, not a handoff proof. Process
supervision, release timing, failure recovery, and an actual UI runtime all
require a separate active gate.

| Area | Result |
| --- | --- |
| Kodi JSON-RPC | Ready; HTTP works, but unauthenticated and LAN-wide |
| HDMI-CEC | Native device present, unowned, no logical address configured |
| Bluetooth HID | Not ready; userspace absent and bring-up failed |
| USB HID | Present and already consumed directly by Kodi |
| Stremio/browser runtime | Absent |
| Display architecture | Direct GBM/DRM, no compositor |
| `mediabox.local` today | Not available; hostname differs and Avahi/mDNS is absent |

## 2. System baseline

At `2026-09-11T18:07:15,514711121+03:00`:

```console
$ date -Ins; hostname; uname -a; uname -m; uptime; cat /proc/cmdline; systemctl --version | head -n 1; cat /etc/os-release
2026-09-11T18:07:15,514711121+03:00
orangepi5-ultra
Linux orangepi5-ultra 6.1.115-vendor-rk35xx-screenbridge-hdmirx-audio #3 SMP Fri Sep  4 00:27:42 +03 2026 aarch64 GNU/Linux
aarch64
 18:07:15 up  2:38,  3 users,  load average: 1.08, 1.13, 1.15
root=UUID=2713b423-533b-4969-a9b7-69e93cbf2b98 rootwait rootfstype=ext4 splash=verbose console=ttyS2,1500000 console=tty1 consoleblank=0 loglevel=1 ubootpart=bdb6d978-3c90-46ee-9b86-cdc12bbc8a3e usb-storage.quirks=0x2537:0x1066:u,0x2537:0x1068:u cma=256M  cgroup_enable=cpuset cgroup_memory=1 cgroup_enable=memory androidboot.fwver=ddr-v1.20-b8ce94f14b,bl31-v1.48,uboot-rmbian-201-08/20/2026
systemd 257 (257.13-1~deb13u1)
PRETTY_NAME="Armbian 26.11.0-trunk.35 trixie"
VERSION_ID="13"
VERSION="13 (trixie)"
```

The DT identifies `RK3588 OPi 5 Ultra`, compatible with
`rockchip,rk3588-orangepi-5-ultra` and `rockchip,rk3588`.

The default boot target is `graphical.target`, but there is no display-manager
unit. The only active terminal services are `getty@tty1`,
`serial-getty@ttyFIQ0`, and `systemd-logind`. `fgconsole` returned `1`,
`/sys/class/tty/tty0/active` returned `tty1`, and the bound framebuffer console
is `(M) frame buffer device` on `rockchipdrmfb`.

`loginctl list-sessions` showed root SSH/user-manager sessions only. Kodi lives
in abandoned remote SSH session 238:

```text
Id=238
User=0
Remote=yes
RemoteHost=10.27.27.11
Service=sshd
Scope=session-238.scope
Type=tty
Class=user
State=closing
```

No X11, Wayland, compositor, greeter, or display-manager process was present.

## 3. Kodi runtime

At `2026-09-11T18:07:43,832529415+03:00`:

```console
$ p=$(pgrep -xo kodi-gbm); ps -p "$p" -o pid,ppid,user,group,uid,gid,tty,stat,lstart,args; readlink -f /proc/$p/exe; readlink -f /proc/$p/cwd; cat /proc/$p/cgroup
    PID    PPID USER GROUP UID GID TT STAT STARTED                       COMMAND
  40781       1 root root   0   0 ?  Ssl  Fri Sep 11 16:25:08 2026     /opt/rk3588-mediabox/kodi/lib/kodi/kodi-gbm --standalone --debug
/opt/rk3588-mediabox/kodi/lib/kodi/kodi-gbm
/var/tmp/kodi-home
0::/user.slice/user-0.slice/session-238.scope
```

The executable is a 64-bit AArch64 PIE, build ID
`994f354a9662bf1b16f38ba7ff2fa05d260ad5a1`, SHA-256
`bfa5b1cb2ed73d139f0bcdb8652e73ea489f71edac51534fbeceddd73b1f71fb`.
The runtime log reports:

```text
Starting Kodi (22.0-BETA2 (21.90.802) Git:20260831-e513e0ff43). Platform: Linux ARM 64-bit
Kodi compiled 2026-09-09 by GCC 14.2.0 for Linux ARM 64-bit version 6.12.107 (396395)
Running on Armbian 26.11.0-trunk.35 trixie 13, kernel: Linux ARM 64-bit version 6.1.115-vendor-rk35xx-screenbridge-hdmirx-audio
```

Selected live environment at
`2026-09-11T18:12:58,483518888+03:00`:

```text
HOME=/var/tmp/kodi-home
MEDIABOX_GPU=mali
LD_LIBRARY_PATH=/opt/rk3588-mediabox/mali-g24p0-runtime/lib:/opt/rk3588-screenbridge/lib
AE_SINK=ALSA
LANG=en_US.UTF-8
XDG_RUNTIME_DIR=/run/user/0
DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/0/bus
```

`DISPLAY` and `WAYLAND_DISPLAY` were absent. Kodi is therefore GBM/direct DRM,
not an X11 or Wayland client. `/sys/kernel/debug/dri/0/clients` proves current
ownership:

```text
command   pid dev master a uid magic
kodi-gbm 40781   0   y    y   0     0
kodi-gbm 40781 128   n    y   0     0
```

Open display nodes are `/dev/dri/card0` (fd 29) and
`/dev/dri/renderD128` (fd 30). Kodi also has `/dev/mali0`, ALSA card 0,
and nine input event nodes open; the input list is in section 9.

There is no `kodi.service`, no Kodi unit file, and therefore no systemd startup
ordering. `systemctl status 40781` resolves only to abandoned
`session-238.scope`; PID 1 adopted the process after the SSH launcher exited.
Repository source `scripts/run-kodi-rk3588.sh` explains the exact model: its
`start` action enters `/var/tmp/kodi-home`, sets `HOME`, `LD_LIBRARY_PATH`,
`AE_SINK=ALSA`, and `MEDIABOX_GPU=mali`, then invokes:

```text
setsid --fork /opt/rk3588-mediabox/kodi/lib/kodi/kodi-gbm --standalone --debug \
  </dev/null >/var/tmp/kodi-home/kodi-stdout.log 2>&1
```

This launcher is operator-driven and includes stop/start behavior; it was only
read during this audit and was not invoked.

## 4. DRM/display topology

At `2026-09-11T18:08:09,255037092+03:00`:

```text
/dev/dri/card0       -> platform-display-subsystem-card, rockchip-drm
/dev/dri/renderD128  -> platform-display-subsystem-render, rockchip-drm
/dev/dri/card1       -> platform-fdab0000.npu-card, RKNPU
/dev/dri/renderD129  -> platform-fdab0000.npu-render, RKNPU
```

`card1` is the NPU DRM device, not a display card. Display connector state:

```text
card0-HDMI-A-1:  status=connected enabled=enabled dpms=On
card0-Writeback-1: status=unknown enabled=disabled dpms=On
```

`modetest -M rockchip -c` opened `RockChip Soc DRM` driver version 4.0.0
and reported HDMI-A-1 connector ID 201 with 41 modes. The preferred and active
idle mode is 1920x1080 at 60.00 Hz. 4K modes through 4096x2160p60 and
3840x2160p23.98 are advertised.

The live KMS state at `2026-09-11T18:12:32,083642438+03:00` was:

```text
Video Port0: ACTIVE
    Connector:HDMI-A-1 Encoder: TMDS-200
    bus_format[100a]: RGB888_1X24
    overlay_mode[0] output_mode[f] SDR[0] color-encoding[BT.709] color-range[Full]
    Display mode: 1920x1080p60
    Cluster0-win0: ACTIVE
    format: AR24 little-endian (0x34325241)
```

This is the idle Kodi GUI, not an HDR playback sample. The connector's current
`HDR_OUTPUT_METADATA` blob is empty and `Colorspace=Default`; this does not
contradict the accepted HDR playback baseline. Maximum sink output BPC is 12.

Framebuffer/VT state is `fb0 = rockchipdrmfb`, mode
`U:1920x1080p-0`, active VT `tty1`. Logind associates both display cards with
seat0 and labels `card0` as master. No VT was switched.

There is no active X socket (`/tmp/.X11-unix` exists but is empty), no Wayland
socket, no X/Wayland process, and no display manager. `Xorg`, `Xwayland`,
`weston`, `cage`, `sway`, `kmscube`, and `drm_info` are `ABSENT`.
`modetest` is present. Wayland client/server libraries are installed, but a
compositor executable is not.

## 5. Kodi JSON-RPC

Production settings file:
`/var/tmp/kodi-home/.kodi/userdata/guisettings.xml`, root-owned, mode 0644,
mtime `2026-09-11 17:55:10.489069664 +0300`.

At `2026-09-11T18:08:43,363471794+03:00` it contained:

```xml
<setting id="services.webserver">true</setting>
<setting id="services.webserverport" default="true">8080</setting>
<setting id="services.webserverauthentication">false</setting>
<setting id="services.webserverusername" default="true">kodi</setting>
<setting id="services.webserverssl" default="true">false</setting>
<setting id="services.esenabled" default="true">true</setting>
<setting id="services.esport" default="true">9777</setting>
<setting id="services.esallinterfaces" default="true">false</setting>
```

The web-server password length is zero. Socket evidence:

```text
0.0.0.0:8080  LISTEN kodi-gbm     [::]:8080  LISTEN kodi-gbm
127.0.0.1:9090 LISTEN kodi-gbm     [::1]:9090 LISTEN kodi-gbm
127.0.0.1:9777 UDP    kodi-gbm
```

Thus HTTP JSON-RPC and the web UI bind to all LAN interfaces, while the raw
TCP/WebSocket service and EventServer remain loopback-only. Kodi's log records
successful initialization of JSON-RPC v13.200.0, TCPServer, web server 8080,
and Avahi publication requests. Avahi is not actually installed/running.

Observed endpoints are `http://<target>:8080/` (Kodi web UI),
`http://<target>:8080/jsonrpc` (HTTP JSON-RPC),
`tcp://127.0.0.1:9090` and `tcp://[::1]:9090` (JSON-RPC stream), and
`ws://127.0.0.1:9090/jsonrpc` / `ws://[::1]:9090/jsonrpc` (WebSocket upgrade).
Only the port-8080 endpoints are LAN-accessible in the observed socket state.

Exact allowed HTTP request/response:

```http
POST http://127.0.0.1:8080/jsonrpc
Content-Type: application/json

{"jsonrpc":"2.0","method":"JSONRPC.Ping","id":1}

HTTP/1.1 200 OK
Content-Type: application/json
Content-Length: 40

{"id":1,"jsonrpc":"2.0","result":"pong"}
```

At `2026-09-11T18:09:02,761773655+03:00`, a standard WebSocket upgrade at
`http://127.0.0.1:9090/jsonrpc` returned:

```http
HTTP/1.1 101 Switching Protocols
Upgrade: websocket
Connection: Upgrade
Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=
```

An upgrade request at port 8080 did not upgrade and returned the HTTP service
description. A raw TCP ping attempt did not yield a line before its three-second
read timeout; raw TCP remains present by listener and Kodi log evidence, but
request framing should be verified by a future client integration test.

## 6. Stremio/Web prerequisites

At `2026-09-11T18:09:26,858634568+03:00`, no matching process, package, or
file was found for Stremio, `stremio-service`, or `server.js` under `/opt`,
`/usr/local`, or Flatpak roots.

Command inventory:

```text
stremio             ABSENT
stremio-service     ABSENT
node                ABSENT
npm                 ABSENT
pnpm                ABSENT
yarn                ABSENT
ffmpeg              ABSENT
ffprobe              ABSENT
flatpak              ABSENT
chromium             ABSENT
chromium-browser     ABSENT
google-chrome        ABSENT
firefox              ABSENT
cog                  ABSENT
wpewebkit-mini-browser ABSENT
epiphany             ABSENT
pipewire             ABSENT
pipewire-pulse       ABSENT
pulseaudio           ABSENT
```

No Chromium, Firefox, WebKitGTK, WPE, Cog, Electron, kiosk, XDG desktop portal,
PipeWire, or PulseAudio package/process was found. Kodi's build has its own
runtime dependencies; its log's attempted PipeWire config read is not evidence
of a PipeWire daemon.

A root user DBus session is available at `/run/user/0/bus`, and Kodi has both
`XDG_RUNTIME_DIR=/run/user/0` and
`DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/0/bus`. `busctl --user list`
successfully reached the user manager. This is the only desktop-like session
facility presently available.

## 7. HDMI-CEC

At `2026-09-11T18:10:17,669332619+03:00`, the read-only default adapter query
(`cec-ctl -d /dev/cec0`, with no address/configuration or message option)
reported:

```text
Driver Info:
        Driver Name                : dwhdmi-rockchip
        Adapter Name               : dw_hdmi_qp
        Capabilities               : 0x0000001e
                Logical Addresses
                Transmit
                Passthrough
                Remote Control Support
        Driver version             : 6.1.115
        Available Logical Addresses: 4
        Connector Info             : None
        Physical Address           : 1.0.0.0
        Logical Address Mask       : 0x0000
        CEC Version                : 2.0
        OSD Name                   : ''
        Logical Addresses          : 0
```

`cec-ctl --list-devices` maps `dwhdmi-rockchip (dw_hdmi_qp)` to
`/dev/cec0`. The node is `crw-rw---- root:video 247,0`; udev maps it to
`/sys/devices/platform/fdea0000.hdmi/cec0`. `/sys/class/cec` is `ABSENT`, but
the character device and platform sysfs device are present and usable.

The parent device is bound to
`/sys/bus/platform/drivers/dwhdmi-rockchip`, DT node
`/hdmi@fdea0000`, compatible `rockchip,rk3588-dw-hdmi`, status `okay`.
The DT contains the RK3588 HDMI CEC pin groups, including
`pinctrl/hdmi/hdmim0-tx1-cec`. Kernel config has `CONFIG_CEC_CORE=y`,
`CONFIG_CEC_NOTIFIER=y`, and `CONFIG_DRM_ROCKCHIP=y`; these are built in, so no
CEC module appears in `lsmod`. Dmesg contains `Registered IR keymap rc-cec` and
the expected `dwhdmi-rockchip`, HDPTX PHY, and VOP2 mode-setting records.

Tools/libraries:

```text
cec-ctl 1.30.1        PRESENT (/usr/bin/cec-ctl; v4l-utils 1.30.1-1)
cec-client            ABSENT
libcec.so.7           PRESENT (/lib/aarch64-linux-gnu/libcec.so.7; libcec7 7.0.0-1+b1)
libcec development    PRESENT (7.0.0-1+b1)
```

`fuser -v /dev/cec0` returned no owner, and an exhaustive `/proc/*/fd` symlink
scan found no open `/dev/cec*`. Kodi's process maps no libCEC library and has
no CEC fd. Its log only records loading the generic `CEC Adapter` and
`Pulse-Eight CEC Adapter` peripheral mapping nodes. No per-device CEC settings
were present in its userdata.

No `--playback`, logical-address assignment, poll, power, active-source,
monitor, or topology command was run. `cec-ctl --show-topology` can cause CEC
traffic and therefore belongs in a separately authorized active gate.

**Classification: `CEC_DEVICE_PRESENT`.** The kernel adapter is usable for
read-only capability inspection, currently unconfigured at the logical-address
layer, and not contended by Kodi/libCEC.

## 8. Bluetooth

At `2026-09-11T18:10:37,995313440+03:00` and
`18:10:49,739168000+03:00`:

- `/sys/class/bluetooth/hci0` exists and resolves to SDIO function
  `mmc2:0001:3`, device `06CB:AABF`, driver `btsdio`.
- `bluetoothctl`, `bluetoothd`, `btmgmt`, and `rfkill` are `ABSENT`.
- `bluetooth.service` is absent and no bluetoothd process runs.
- Only BlueZ libraries/development files are installed
  (`libbluetooth3`/`libbluetooth-dev` 5.82-1.1); the BlueZ daemon/tools package
  is absent.
- `/var/lib/bluetooth` is absent. Paired, trusted, connected, and exported HID
  devices therefore cannot be enumerated through BlueZ, and no userspace
  pairing database exists.
- No Bus 0005 (Bluetooth HID) input device appears in
  `/proc/bus/input/devices`. `bt-powerkey` is a platform power-key device, not
  a paired Bluetooth peripheral.

Direct rfkill sysfs evidence:

```text
/sys/class/rfkill/rfkill0 name=bt_default type=bluetooth state=0 soft=1 hard=0
/sys/class/rfkill/rfkill1 name=hci0       type=bluetooth state=0 soft=1 hard=0
```

The enabled `ap6611s-bluetooth.service` is failed:

```text
ExecStartPre=/usr/sbin/rfkill unblock all
ExecStart=/usr/bin/brcm_patchram_plus_rk3399 ... \
  --patchram /lib/firmware/brcm/SYN43711A0.hcd /dev/ttyS7

Unable to locate executable '/usr/sbin/rfkill': No such file or directory
Failed at step EXEC spawning /usr/sbin/rfkill
```

The referenced SYN43711 firmware is present, as are AP6611 Wi-Fi firmware files.
No unblock, service start, scan, pair, trust, connect, disconnect, or unpair was
attempted.

**Classification: `BLUETOOTH_HID_NOT_READY`.** A future pairing gate first
needs an explicitly authorized prerequisite/remediation gate for rfkill and
BlueZ, followed by verification that the controller initializes normally.

## 9. Linux input subsystem

At `2026-09-11T18:11:00,945124570+03:00`, `/dev/input/event0` through
`event12` existed, mode `0660 root:input`. `evtest`, `libinput`, and
`input-events` are absent, so capabilities were read from
`/proc/bus/input/devices`; no event node was opened by the audit.

| Event node | Kernel name / role | Kodi open? |
| --- | --- | --- |
| `event0` | `dw_hdmi_qp`, CEC/HDMI RC input, `EV_KEY/EV_REL/EV_MSC` | yes, fd 25 |
| `event1` | `rockchip-hdmi1`, ALSA jack/switch | no |
| `event2` | `rockchip,hdmiin`, ALSA jack/switch | no |
| `event3` | `headset-keys` | yes, fd 20 |
| `event4` | `rockchip,es8388 Headset`, ALSA switch | no |
| `event5` | `bt-powerkey`, platform key | yes, fd 27 |
| `event6` | HP OMEN Spacer wireless USB keyboard | yes, fd 21 |
| `event7` | same USB receiver, keypad/keyboard | yes, fd 22 |
| `event8` | same USB receiver, consumer control | yes, fd 23 |
| `event9` | same USB receiver, mouse | yes, fd 24 |
| `event10` | same USB receiver, absolute axis | no |
| `event11` | `adc-keys` | yes, fd 19 |
| `event12` | `rk805 pwrkey` | yes, fd 26 |

Stable `/dev/input/by-id` links exist for the HP receiver's keyboard,
consumer-control, and mouse functions; `/dev/input/by-path` also maps the
platform CEC device and board keys. The HP device is USB HID (Bus 0003,
vendor/product `03f0:5141`), providing direct evidence that USB HID can enter
the existing evdev path and be consumed by Kodi.

A future normalized input layer has four plausible adapters:

1. USB HID and, once BlueZ is functional, Bluetooth HID: consume evdev using
   stable udev identity and translate `EV_KEY` media/navigation codes.
2. HDMI-CEC: consume the kernel RC event node for navigation keys and use
   `/dev/cec0` separately for explicitly authorized CEC control operations.
3. Browser remote: accept authenticated WebSocket commands in unprivileged
   `mediaboxd`, normalize them into the same semantic actions, and proxy allowed
   Kodi JSON-RPC calls.
4. Kodi coexistence: do not `EVIOCGRAB`; either allow shared reads or make one
   supervisor the sole input owner and forward actions through JSON-RPC after an
   active compatibility test.

## 10. Wi-Fi/network

At `2026-09-11T18:11:18,934492639+03:00`, only loopback and wired Ethernet
were present:

```text
enP3p49s0 UP 10.27.27.25/24
default via 10.27.27.1 dev enP3p49s0 proto dhcp
```

No wireless network interface exists and `iw dev` returns no PHY/interface, so
there is no current Wi-Fi association. The AP6611/SYN43711 SDIO functions
`mmc2:0001:1` and `:2` exist but have no bound driver. AP6611 firmware files
exist under `/lib/firmware/ap6275p`, including
`fw_syn43711a0_sdio.bin`, `clm_syn43711a0.blob`, and
`nvram_ap6611s.txt`; no Wi-Fi driver module was loaded. This is hardware and
firmware inventory, not proof of a usable Wi-Fi interface.

Networking is managed by active `systemd-networkd` from generated netplan
configuration (`/run/systemd/network/10-netplan-all-eth-interfaces.network`;
source `/etc/netplan/10-dhcp-all-interfaces.yaml`). `wpa_supplicant.service` is
running in DBus/control-interface mode but has no Wi-Fi interface.
NetworkManager is absent. `systemd-resolved` is active.

Hostname is `orangepi5-ultra`. Avahi and `avahi-browse` are absent, nothing
listens on mDNS UDP 5353, and `getent hosts mediabox.local` returned nothing.
The only LAN-facing TCP listeners were:

```text
0.0.0.0:22 / [::]:22       sshd
0.0.0.0:8080 / [::]:8080   kodi-gbm HTTP/JSON-RPC
0.0.0.0:5355 / [::]:5355   systemd-resolved LLMNR
```

Port 9090 is loopback-only. No other HTTP server process was present.

`http://mediabox.local/` is reasonable as a future feature on the wired LAN,
but it is **not supported today**. It requires an HTTP service, an mDNS
publisher/responder, the `mediabox` name (or an explicit service name independent
of the current static hostname), collision handling, and firewall/exposure
review. None was configured here.

## 11. Telemetry sources

Read-only sources suitable for a future dashboard are mapped below. Values are
examples from `2026-09-11T18:12:02,738313662+03:00`, not constants.

| Metric | Source/interface | Observed value |
| --- | --- | --- |
| Board/SoC | `/sys/firmware/devicetree/base/{model,compatible}` | RK3588 OPi 5 Ultra / RK3588 |
| CPU topology | `/sys/devices/system/cpu`, `/proc/cpuinfo` | 8 AArch64 CPUs |
| CPU frequencies | `/sys/devices/system/cpu/cpufreq/policy*/{affected_cpus,scaling_cur_freq,scaling_min_freq,scaling_max_freq,scaling_governor}` | policies 0/4/6; 1.8/1.2/2.352 GHz current sample |
| Temperatures | `/sys/class/thermal/thermal_zone*/{type,temp}` | SoC/big/little 41.615 C; center/GPU/NPU 40.692 C |
| RAM/swap | `/proc/meminfo`, `free` | 7.7 GiB RAM; 7.2 GiB available; 3.9 GiB zram swap |
| Storage | `statvfs(2)`, `df`, `/sys/class/block`, `lsblk` | root ext4 on `mmcblk0p1`, 57 GiB, 35% used |
| Uptime/load | `/proc/uptime`, `/proc/loadavg` | 9812.90 s; 1.27/1.16/1.15 |
| GPU frequency | `/sys/class/devfreq/fb000000.gpu/{cur_freq,min_freq,max_freq,available_frequencies,governor}` | 300 MHz, range 300 MHz–1 GHz, `simple_ondemand` |
| GPU identity/memory | `/dev/mali0`, boot dmesg, `/sys/kernel/debug/mali0/gpu_memory` | Mali arch 10.8.6, DDK g25p0; Kodi 7972 pages |
| NPU frequency | `/sys/class/devfreq/fdab0000.npu/*` | 1 GHz sample, range 300 MHz–1 GHz, `rknpu_ondemand` |
| NPU utilization | `/sys/kernel/debug/rknpu/load` | Core0/1/2 each 0% |
| NPU identity | `/dev/dri/card1`, dmesg | RKNPU 0.9.8 (20240828) |
| DRM/HDMI connection | `/sys/class/drm/card0-HDMI-A-1/{status,enabled,dpms,modes}` | connected/enabled/On |
| Active mode/planes | `/sys/kernel/debug/dri/0/{summary,state}` | 1920x1080p60 SDR idle GUI |
| HDR/color state | `modetest -M rockchip -c`, DRM `summary`; connector properties `HDR_OUTPUT_METADATA`, `Colorspace`, `color_depth` | idle: metadata empty, Default colorspace, RGB888; playback must be sampled separately |
| Network | rtnetlink (`ip`), `/sys/class/net`, networkd DBus | wired 10.27.27.25/24 |
| Bluetooth | `/sys/class/bluetooth`, `/sys/class/rfkill`, future BlueZ DBus | hci0 present, soft-blocked, no BlueZ |
| Kodi health/playback | loopback JSON-RPC: `JSONRPC.Ping`, `Player.GetActivePlayers`, `Player.GetProperties`, `Application.GetProperties` | ping pong; active players `[]` |

Exact playback-state request/response:

```text
{"jsonrpc":"2.0","method":"Player.GetActivePlayers","id":3}
{"id":3,"jsonrpc":"2.0","result":[]}
```

Privileged debugfs paths should not be exposed directly to the browser. A
collector should read an allowlist, normalize units, cache slow queries, and
return bounded data through an unprivileged API.

## 12. Display ownership classification

**`DIRECT_KMS_UI_HANDOFF_LIKELY`.**

Evidence for this classification:

- Kodi is definitively `kodi-gbm --standalone`, with no `DISPLAY` or
  `WAYLAND_DISPLAY`, and owns DRM master on `card0`.
- No compositor, X server, display manager, or graphical session is active.
- The active KMS plane framebuffer is allocated by `kodi-gbm`.
- The existing launcher explicitly avoids two simultaneous DRM masters and
  uses a stop-then-start lifecycle.
- Rockchip DRM exposes a normal card/render split and `modetest` can enumerate
  KMS resources while Kodi is active.

Therefore option A—an idle direct-KMS UI temporarily owning the display and
fully releasing it before Kodi starts—is consistent with the real target.
Option B, a compositor, is not required by the current runtime and would add a
new display architecture. However, there is no UI/browser runtime and no
supervisor today, so actual master release/reacquisition, VT behavior, HDMI
mode continuity, crash recovery, and transition latency remain unproved.

If the chosen UI technology inherently requires Wayland/X11, then a compositor
would be required for that technology; that would be a product choice rather
than a limitation demonstrated by this target.

## 13. Security/API observations

The immediate issue is Kodi HTTP/JSON-RPC on `0.0.0.0:8080` and `[::]:8080`
with `services.webserverauthentication=false`, empty password, and no TLS. Any
LAN peer can currently issue methods allowed by Kodi's HTTP transport. The
future design should:

- bind Kodi JSON-RPC to loopback only if Kodi permits it, or enforce the same
  boundary with host firewall/service isolation without changing playback;
- have `mediaboxd` proxy a small allowlist of Kodi operations rather than
  exposing arbitrary JSON-RPC;
- authenticate browser and WebSocket clients on the LAN, protect against CSRF
  and cross-origin WebSocket abuse, rate-limit commands, and use explicit
  session expiry;
- run the web/API process as a dedicated unprivileged account, with no direct
  write access to Kodi configuration, DRM, input, CEC, Bluetooth storage, or
  systemd;
- split privileged actions into a minimal broker with narrowly scoped IPC and
  policy (for example, only start/stop named units and only approved CEC
  actions), never a general shell endpoint;
- expose telemetry through fixed schemas and allowlisted paths, not arbitrary
  file or command parameters;
- protect Stremio stream URLs/tokens and Kodi credentials from logs and browser
  storage; and
- define ownership/state transitions atomically so two DRM masters or two
  input dispatchers cannot race.

SSH is also LAN-wide on port 22, which is expected administration exposure but
should remain outside the browser API trust boundary.

## 14. Unknowns requiring active experiments

The following cannot be answered by this read-only gate and are recorded as
`NEEDS_SEPARATE_ACTIVE_GATE`:

- whether a candidate direct-KMS UI can release all DRM resources and Kodi can
  reacquire master reliably across normal exit, crash, and timeout paths;
- transition latency, blanking, HDMI resync, mode/HDR restoration, VT/fbcon
  interaction, and input routing during that handoff;
- browser/WebView rendering, video preview, DRM acceleration, codec support,
  and Stremio service behavior, because every candidate runtime is absent;
- raw TCP JSON-RPC client framing and notification behavior beyond the proven
  WebSocket upgrade and HTTP ping;
- CEC topology, TV logical address behavior, receive/transmit reliability, and
  key routing after a logical address is intentionally assigned; no topology
  or transmitted query was permitted here;
- whether the CEC `event0` RC path delivers the desired TV remote keys in the
  final logical-address state;
- Bluetooth controller initialization after rfkill/BlueZ prerequisites are
  installed, and real Android TV/Box remote discovery, pairing, reconnect,
  profile, battery, and evdev behavior;
- Wi-Fi driver binding, association, coexistence with Bluetooth, and RF
  reliability; there is no wireless interface now;
- mDNS name collision and resolution behavior on the intended LAN; and
- authenticated browser remote behavior under loss/reconnect, duplicate
  commands, privilege-boundary failure, and concurrent local input.

## 15. Exact recommended next Gate

**Gate S0-C — reversible runtime prerequisite and direct-KMS handoff proof.**

Run it only with explicit authorization for target changes and display
interruption. Keep the accepted playback stack, kernel, DT, bootloader, Kodi
binary/config, ALSA, networking, CEC logical address, and pairing state
unchanged. Scope it to:

1. Select one minimal direct-KMS UI runtime that does not require a compositor
   and stage it outside the production Kodi prefix; record hashes and an exact
   rollback path.
2. Add a temporary, explicitly bounded supervisor in `/run` or `/var/tmp` (not
   a persistent service yet) that serializes `UI -> fully stopped -> Kodi` and
   `Kodi -> fully stopped -> UI`.
3. Prove from `/sys/kernel/debug/dri/0/clients` that exactly one process owns
   DRM master at every stable state and no stale card/input fd remains.
4. Exercise normal exit, forced UI crash, Kodi launch failure, and timeout
   recovery; capture VT, connector, mode, plane, HDR/color, input, and kernel
   evidence before/after every transition.
5. Verify the accepted Kodi playback pipeline after reacquisition using the
   canonical SDR and 4K23.976 HDR10 samples, without changing any accepted
   playback setting.
6. Do **not** combine CEC address assignment/transmission, Bluetooth pairing,
   package remediation, Wi-Fi enablement, or mDNS configuration into S0-C.
   Those should be separately authorized gates so TV, pairing, and network
   side effects are isolated and attributable.

S0-C should pass only if ownership is exclusive, rollback is automatic, the
idle UI is usable without a compositor, all four failure paths recover, and
the accepted Kodi display/audio pipeline is byte-for-byte/config-for-config
unchanged. Otherwise classify the direct-KMS architecture as failed and use
the evidence to decide whether a compositor design gate is justified.

Final read-only state check at
`2026-09-11T18:17:34,774375117+03:00`: Kodi was still original PID 40781
(start time 16:25:08), still DRM master on card0, HDMI-A-1 remained connected,
the active VT remained tty1, CEC still reported physical address `1.0.0.0` and
zero logical addresses, and both Bluetooth rfkill soft flags remained `1`.

## Summary tokens

```text
KODI_JSONRPC_READY
CEC_DEVICE_PRESENT
BLUETOOTH_HID_NOT_READY
DIRECT_KMS_UI_HANDOFF_LIKELY
```
