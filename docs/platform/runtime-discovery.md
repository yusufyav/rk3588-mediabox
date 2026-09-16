# What this board is, asked of the board

This product runs on more than one RK3588 machine. One of them has a single HDMI
transmitter; another has two, plus DisplayPort. Their sound cards are numbered
differently, their CEC adapters are numbered differently, and which DRM device
does the modesetting and which one is the NPU is not the same on both.

None of that is exceptional and none of it is a reason to ask which board this
is. It is topology, and topology is readable. So there is one resolver —
`rust/crates/mediabox-platform` — and the television interface, the control
plane and the shell scripts all ask it, rather than each carrying its own idea
of where the display is.

```text
              capabilities / topology
                        |
                platform discovery
                /       |       \
             DRM      ALSA      CEC
                \       |       /
                 selected output
                  |     |     |
            audio    CEC    usable VOP2 planes
```

The order in that picture is the whole design. The display stage chooses an
output; the audio endpoint and the CEC adapter are **derived from that choice**.
Choosing them separately is how a box ends up drawing on one television and
talking to another.

---

## One command

```sh
mediabox-platform inspect          # everything, and why
mediabox-platform inspect --json   # the same, for a program
mediabox-platform kms-node         # the DRM device to set modes on
mediabox-platform render-node      # the DRM device to render on
mediabox-platform output           # the selected connector's name
mediabox-platform outputs          # one line per output: name, state, mode
mediabox-platform mode             # the selected output's preferred mode
mediabox-platform connector-path   # its sysfs directory
mediabox-platform alsa-card        # its sound card's id
mediabox-platform alsa-driver      # that card's ALSA driver name
mediabox-platform alsa-index       # that card's number this boot
mediabox-platform cec-device       # its CEC adapter
```

The one-word forms print nothing and exit non-zero when there is no answer, so
`card="$(mediabox-platform alsa-card)" || exit` is a caller's whole error
handling.

On the Ultra:

```text
KMS           /dev/dri/card0 (card0, driver rockchip-drm)
Render        /dev/dri/renderD128 (renderD128)

Transmitters
  fdea0000.hdmi      HdmiA (hdmi1) cable=yes

Outputs
  * HDMI-A-1     connected     2560x1440
      transmitter fdea0000.hdmi (measured)
      audio       rockchiphdmi1 (hw:CARD=rockchiphdmi1,DEV=0, eld absent)
      cec         /dev/cec0
      sink        HPN3725 serial 01010101 "HP X27q"

Selected      HDMI-A-1 (the only one connected)
```

On the Plus, read-only, with nothing plugged into it:

```text
Transmitters
  fde80000.hdmi      HdmiA (hdmi0) cable=no
  fdea0000.hdmi      HdmiA (hdmi1) cable=no
  fde50000.dp        DisplayPort (dp0) cable=no

Outputs
    HDMI-A-1     disconnected  -
      transmitter fde80000.hdmi (by order)
      audio       rockchiphdmi0 (hw:CARD=rockchiphdmi0,DEV=0, eld absent)
      cec         /dev/cec0
    HDMI-A-2     disconnected  -
      transmitter fdea0000.hdmi (by order)
      audio       rockchiphdmi1 (hw:CARD=rockchiphdmi1,DEV=0, eld absent)
      cec         /dev/cec1
    DP-1         disconnected  -
      transmitter fde50000.dp (by order)
      audio       rockchipdp0 (hw:CARD=rockchipdp0,DEV=0, eld absent)
      cec         unavailable

Selected      none
warning       no connected output with a usable mode
```

Note what that second listing says. `HDMI-A-1` on the Plus is the transmitter
the device tree calls `hdmi0`, so its sound card is `rockchiphdmi0` — while on
the Ultra the only connector there is has `rockchiphdmi1`. Same connector name,
different card. Anything that writes `amixer -c rockchiphdmi1` is addressing the
wrong television on one of the two boards, and this product used to.

---

## How each thing is found

### The display device

Every DRM device under `/sys/class/drm` is enumerated, and the one that **owns
connectors** is the one that can set a mode. That is asked of sysfs without
opening anything.

It is not `card0`. On both boards there are two DRM devices — the display
subsystem and the NPU — and on a board whose NPU probes first, `card0` is the
NPU: same `/dev/dri` directory, same driver family, no connectors, no display.

### The render device

The render node whose **parent device is the same** as the display device's.
Rendering and scanning out are two DRM devices here and that split is the shape
of the whole display path, but they are still one piece of hardware.

It is not `renderD128`: that is a number the kernel hands out in probe order,
and the other render node on this silicon is the NPU's.

"Same parent" is a topology fact; "the vendor driver is actually behind it" is
not, so the consumer checks again at the moment it matters. `mediabox-tv`
refuses to start unless the GBM device it creates reports the `armsoc` backend —
which is the vendor libmali and not Mesa.

### The transmitter behind a connector

This is the link everything else hangs off: the CEC adapter is a child device of
the transmitter, and the sound card names it as its codec.

Transmitters are found by what the device tree calls them — a platform device on
an `hdmi@`, `dp@` or `edp@` node — rather than by driver name. A platform device
only exists for a node the board brought to `okay`, so this is already the set
of transmitters the board actually wired up: one on the Ultra, three on the Plus,
and neither number written down anywhere.

Tying a connector to one of them is done two ways, and the two are combined
rather than ranked:

* **Measured.** Each transmitter publishes an extcon device with its own hotplug
  state. When exactly one connector of a kind is `connected` and exactly one
  transmitter of that kind reports a cable, the pair is unambiguous and the
  binding is reported as `measured`.
* **Ordered.** Otherwise, the Nth connector of a type is the Nth transmitter of
  that type, transmitters sorted by the address in their device-tree node name.
  The kernel numbers connectors in registration order and the address is the one
  property of a transmitter that does not move between boots, kernels or boards.
  Reported as `by order`.

When both readings are available and **disagree**, that is reported as
`ambiguous` and a warning is raised rather than resolved. A wrong answer here
sends the sound to a different television than the picture.

### The sound card

A device-tree link, and an exact one: an HDMI sound card's node names its codec
by phandle, and that phandle is the transmitter. Resolving one pointer answers
"which card carries this output's sound" on any board, with nothing written
down. A board without that property falls back to the naming convention —
`hdmi1-sound` is the card for whatever `/aliases/hdmi1` points at.

Cards are addressed by **id** (`rockchiphdmi1`) everywhere and by number
nowhere. The id comes from the device tree; the number comes from probe order,
and probe order is not a promise. The one place a number is unavoidable is
`/proc/asound`, which is indexed by number and offers nothing else — so
`mediabox-platform alsa-index` resolves the stable id to this boot's number, which
is the opposite of assuming it.

The card's **ALSA driver name** (`rockchip-hdmi1`) matters too, because
`alsa-lib` finds a card configuration at `/usr/share/alsa/cards/<driver>.conf`.
That file is what makes `hdmi:CARD=<id>,DEV=0` exist at all, and Kodi decides
whether a device can do compressed passthrough purely from the PCM *name*: only
a name starting with `hdmi` becomes `AE_DEVTYPE_HDMI`. So the file is rendered
from `config/alsa/mediabox-hdmi.conf.in` for whichever card the selected output
has, and Kodi's profile is rewritten to name it — both by
`packaging/mediabox-hdmi-prepare`, before anything draws.

### The CEC adapter

A directory listing rather than an inference: the adapter is registered as a
child device of the transmitter, so `/sys/devices/platform/fdea0000.hdmi/cec0`
*is* that output's adapter. `/dev/cec0` is never written down; on a board with
two transmitters, which of the two is the television's depends on which socket
the television is in.

**DisplayPort has no CEC.** That is reported as an output with no adapter, not
as a failure: a box on DisplayPort still plays films, it just cannot turn the
screen on. `mediaboxd-rs` therefore no longer runs with `--require-cec`; it
reports CEC as unavailable, which `mediaboxctl status` shows and the interface
draws honestly.

---

## Which output, when there is more than one

```text
display.output = auto | <selector>
```

`auto` is the product's answer and the only one a person has to have. The
selection, in order:

1. `MEDIABOX_OUTPUT` in the environment — a diagnostic override, reported as
   one.
2. A remembered choice in `/var/lib/mediabox/display-output`, one line. That is
   the control plane's own state directory, beside the indicator-light mode;
   this is not a new store.
3. Otherwise the policy: **a television first** — HDMI over DisplayPort, because
   that is what this appliance is for — then the lowest-numbered connector of
   that kind, so two identically cabled boards come up the same way round.

Only connected outputs with at least one mode are candidates. A remembered or
overridden choice that is not plugged in is **reported and then ignored** — the
policy chooses instead and the result says `ConfiguredUnavailable`, because the
case where what a person asked for is not what they got is the case worth
seeing.

A selector may be a connector name (`HDMI-A-2`), a sink's own name from its EDID
(`BRAVIA`), or a manufacturer-and-product code (`SNY0101`, `SNY0101-00002A2A`).
The last two are what make "the television" keep meaning the same television
after it has been moved to the other socket.

**Nothing transient is ever persisted as hardware identity.** DRM object ids are
allocated per boot; a different kernel hands out different ones. What is stored
is a connector name or an EDID identity.

---

## Overrides

| Variable | What it forces |
| --- | --- |
| `MEDIABOX_KMS_NODE` | the display device |
| `MEDIABOX_RENDER_NODE` | the render device |
| `MEDIABOX_OUTPUT` | the selected output |
| `MEDIABOX_PLATFORM_ROOT` | read a captured `sys/` and `dev/` tree instead of this machine's |

All four are for debugging. Nothing in the product sets them: an appliance that
needs `MEDIABOX_KMS_NODE` to come up is an appliance whose discovery is broken,
and putting that in a unit file is how it stays broken.

An override naming a device that is not present falls back to discovery. One
that names a device which *is* present but owns no connectors — the NPU, say —
is honoured, because that is what an override is for, and the resulting failure
names itself: `MEDIABOX_KMS_NODE named card1, which owns no connectors and
cannot set a mode`. Without that line the symptom is "no connected output",
which sends whoever reads it to look at the cable.

`MEDIABOX_PLATFORM_ROOT` is what makes the tests possible. The crate's fixtures
build boards out of directories — a single-HDMI board, a three-output board, a
board whose NPU took `card0` and `renderD128`, a board whose ALSA cards came up
in a different order, a board with nothing plugged in — and the same discovery
code answers all of them. Every assertion is written so that it would fail if
the answer were hard-coded.

---

## Living beside rk3588-screenbridge

The two products are both for RK3588 and can be installed on the same board.
They cannot both hold the hardware.

**Prefixes.** MediaBox owns `/opt/rk3588-mediabox` and builds its own Rockchip
MPP, RGA and FFmpeg under `media-runtime/` there. It neither reads nor writes
`/opt/rk3588-screenbridge`. Both players carry an RPATH naming MediaBox's
prefix, so no `LD_LIBRARY_PATH` decides which decoder they get.
`scripts/deploy-mediabox-v3.sh` hashes every file in the other prefix before and
after a deploy and fails if one moves.

**The display.** DRM master cannot be held twice. Every MediaBox unit that takes
it — `mediabox-tv-ui.service`, `kodi.service`, `mediabox-browser.service` —
declares:

```ini
Conflicts=screenbridge-daemon.service
After=screenbridge-daemon.service
```

`Conflicts=` alone would let systemd start one while the other was still letting
go; the `After=` beside it is what makes the stop happen first, which is the
documented way to get a transition rather than a race. Neither line does
anything on a board where that product is not installed.

`mediaboxd-rs`, `mediabox-media-worker` and `stremio-server` touch no display
hardware and declare nothing: they may run beside a capturing ScreenBridge, and
`tests/run-host-tests.sh` checks that they still do not declare an interlock
they do not need.

This is deliberately not a role manager. Which application owns the television
stays `mediaboxd-rs`'s decision at run time; this is only the interlock that
makes that decision land safely when the other product is on the board too.

**Which product the board comes up as.** One file:

```
/var/lib/mediabox/display-owner      mediabox | screenbridge
```

`mediaboxd-rs` reads it once, when it has finished starting, and that is the
only thing that decides whether MediaBox claims the panel. Anything that is not
one of those two words — no file, an empty one, half a word left by a power cut,
a value from a newer version — reads as `screenbridge`.

That default is the point of the default. The claim used to be unconditional:
this daemon put the interface on the display at every start, which is right for
a board that only has MediaBox on it and wrong for a board that does not.
Measured on the Plus before this existed: `screenbridge-daemon` started at 6.997
s into the boot and was being stopped 0.56 s later, every boot, because MediaBox
had been installed on it — a capture appliance quietly became a media appliance
and nobody had chosen that. Guessing the other way round is the worse failure:
it would put a second display owner beside a running one. Refusing to start
MediaBox's shell is recoverable with one command.

So a board becomes a MediaBox board when somebody says so:

```
mediaboxctl display-owner status
mediaboxctl display-owner set mediabox
mediaboxctl display-owner set screenbridge
```

`set` writes the preference first and moves the display second, so a box that
loses power halfway comes back as the thing that was asked for. The write is a
temporary file in the same directory renamed over the top, so a reader never
sees half a word. The file is `0644 root:root`: it decides which product owns
the hardware, so it is root's to write.

Both directions go through the same interlock as everything else. Handing the
display to MediaBox is an ordinary surface switch, and the units above stop
ScreenBridge first. Handing it back stops every MediaBox surface and only then
starts `screenbridge-daemon`. Measured on the Plus: `mediabox-tv-ui` deactivated
at monotonic 1016.68 and `screenbridge-daemon` started at 1017.32, and the other
way at 1029.63 and 1029.64 — ordered, in both directions, with `/dev/dri/card0`
open in exactly one process throughout.

Taking the display also records it. A `surface switch` to the interface or to
Kodi, or launching one of the display-owning applications, writes `mediabox`:
choosing to be on the panel is choosing to be the board's display owner, and a
person should not have to say it twice. `idle` does not, because putting nothing
on the screen is not a choice about which product the board is.

Installing MediaBox does not make that choice either. `deploy-mediabox-v3.sh`
needs the display for the twelve seconds its smoke takes, so it borrows it and
puts the board's role back exactly as it found it — including the absence of the
file, which is itself the answer "nobody has chosen yet".

---

## Validation status

| | Ultra | Plus |
| --- | --- | --- |
| Discovery run | yes, installed | yes, read-only probe from `/tmp`, removed afterwards |
| Display device found | `card0` (rockchip-drm) | `card0` (rockchip-drm) |
| Render device found | `renderD128`, same parent | `renderD128`, same parent |
| Connectors enumerated | 1 (`HDMI-A-1`) | 3 (`HDMI-A-1`, `HDMI-A-2`, `DP-1`) |
| Connected | `HDMI-A-1`, binding `measured` | none — nothing is plugged into the board |
| Audio resolved per output | `rockchiphdmi1` | `rockchiphdmi0`, `rockchiphdmi1`, `rockchipdp0` |
| CEC resolved per output | `/dev/cec0` | `/dev/cec0`, `/dev/cec1`, none for DP |
| MediaBox installed | yes | **no, deliberately** |

The Plus probe could not confirm "the connector that is connected" for the plain
reason that no display is attached to that board. Everything that does not
depend on a cable resolved, and matched `rk3588-screenbridge`'s own independently
measured platform matrix for that machine.
