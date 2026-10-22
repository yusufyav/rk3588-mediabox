# Live television: the Kanky Link-4K DVB-S2 receiver

`mediabox-dvb` (rust/crates/mediabox-dvb) is the receiver's own process: the
USB device and the transport stream. It is the Rust form of the protocol
verified on the hardware by `yusufyav/kanky-link-linux` (Python);
`tests/replay.rs` holds it to that reference's real sessions frame for frame,
with their LNB frames taken out. There is no Python, PyUSB or python-libusb1
on this path.

**MediaBox does not power or steer the LNB.** The RF arrives prepared from
outside: supply (polarisation), 22 kHz tone (band) and any DiSEqC switching
are the RF source's. The demodulator's write policy has no scope that reaches
the LNB pins (0x120008/0x12000C) or the DiSEqC block (0x16C000..0x16C0FF);
they are only read, after the demodulator's reset and again at cleanup, and
must not have changed. Measured in the reference's 12 replayed sessions:
both pins read 2 after the reset (high-Z). What the board's LNB regulator
does with an undriven enable pin is not known here: before an external
supply is connected to the F connector, the voltage on it with the receiver
running must be measured.

```text
mediaboxctl / interface ── LivePlayHere{service_id, source} ──> mediaboxd-rs
mediaboxd-rs (no USB: DevicePolicy=closed)
  ├─ systemd-run mediabox-dvb.service   root, BindsTo=mediabox-tv-ui
  │    libusb-1.0 (dlopen) ─ IT9303 ─ AVL62X1 ─ RDA5815M   (no LNB control)
  │    EP 0x84 async bulk ─> Analyser ─> Selector(SID) ─> /run/mediabox-dvb/live.ts (FIFO)
  │    /run/mediabox-dvb/status.json  (state, end, integrity, USB, signal, cleanup)
  └─ systemd-run mediabox-player.service  mpv --hwdec=rkmpp --vo=mediabox
       MEDIABOX_PLAYER_LIVE=1: --demuxer-lavf-probe-info=nostreams
                               --vd-queue-enable=yes --vd-queue-max-samples=8 --vd-queue-max-secs=0.5
       dma-buf ─> mediabox-tv-ui ─> VOP2 Esmart0
```

A live film is typed apart from a film (`playback::Content::Live`,
`player::Playback::Live`): no worker session, no account record, no Kodi
handover. It is stopped with `MediaStopHere`, and the receiver's cleanup
(MPEG out off, EP4 off, GPIO2 restored) is waited for before anything else
starts. The display is matched before the first frame from the rate the
receiver reads in the video's SPS (1080i25: 50 fields, MPP deinterlaces to
50 frames), as a film's is from its container. How
it ended is the receiver's word (`End` in `service.rs`): only a recording read
to its end is natural; anything else live is a failure, never "the end of the
film".

## Live television in the interface

`mediabox-tv` has a "Canlı TV" application (`src/screens/live.rs`,
`ui/live.slint`) after AOSP's Live TV reference application
(platform/packages/apps/TV): the picture full screen; a channel banner
(8 s), number entry (2 s, exact number first), a menu of rows (7 s: the
guide, the channel list, favourites, recent channels; sound, subtitles, TV
settings) and a programme guide (60 s, half-hour time line, "Bilgi yok" for
stretches nobody described). Up/Down and the channel keys change channel;
Back closes what is up, and with nothing up leaves.

Channels and the guide are the broadcasts' own: the receiver reads every
service's SDT entry and EIT (present/following and schedule, PID 0x12) and
writes them to `guide.json` beside its status; the control plane keeps them,
per multiplex, in `/var/lib/mediabox/live-guide.json` (`LiveChannels`,
`LiveGuide`). Numbers are MediaBox's, in service-id order, until the TKGS
channel list is in. A service the receiver finds scrambled is said to be at
once (`selection.scrambled_packets`).

### Channel change

Measured on the Plus, TRT multiplex 12188 V, key to first frame:

| | |
|---|---|
| cold (the receiver's process, demodulator patch 3.5 s, tune, lock, player) | 7.4 - 8.6 s |
| warm, same multiplex | 0.61 - 0.82 s |

A satellite receiver is started with `--hold-reader-s 30`: it outlives its
player, and a channel of the same multiplex is a new service for its
selector (`select.json` beside the status, token-checked) and a new player
-- nothing is reset, reloaded or retuned (AOSP's tuner keeps its TunerHal
for the session the same way). The new channel starts from its video's last
random access point (`ts::RapCache`; DVB's random_access_indicator, set on
every TRT access point, GOP 1.0 s), held for the player until it opens the
FIFO. The display keeps its mode through the change
(`Output::content_handover`).

## Files on the appliance (the owner's, never in Git or a release)

| Path | What |
|---|---|
| `/var/lib/mediabox-dvb/firmware/libdvb_usb_phy.so` | Kanky Link 2.103/2.109/2.110 arm64 library: AVL62X1 patch + RDA5815M tables, read as data, SHA-256 checked |
| `/var/lib/mediabox-dvb/firmware/dvb-usb-it9303-01.fw` | bridge firmware (Linux), SHA-256 checked; loaded when the device comes up COLD (every plug-in) |
| `/var/lib/mediabox-dvb/recording.ts` | a captured multiplex, for `source: recording` |
| `/var/lib/mediabox/live-guide.json` | the multiplexes the receiver has been on: services and EIT events (the control plane's) |

`mediabox-dvb firmware-check` verifies the first two without touching USB.
`mediabox-dvb selftest [--recording FILE]` checks libusb's symbols (no
context, no device), the firmware, the TS pipeline and SPS reader, a FIFO
and the recording, and reports `usb_open_attempts` (must be 0). Only
`play --hardware` can open the receiver (`usb::HardwareAccess`).

## Commands

```sh
mediaboxctl media live-here 10801 --recording --title "TRT HABER HD"   # no USB access
mediaboxctl media live-here 10801 --frequency-khz 12188000 --symbol-rate 27500   # opens the receiver; RF prepared outside
mediaboxctl media status-here --json      # .result.live: lifecycle, receiver status
mediaboxctl media live-channels           # the channel list
mediaboxctl media live-guide --hours 6    # the programme guide from now
mediaboxctl media stop-here
mediabox-dvb selftest --recording /var/lib/mediabox-dvb/recording.ts    # no USB
mediabox-dvb select --input rec.ts --service 10801 --output one.ts       # offline
mediabox-dvb analyse --input rec.ts                                      # offline
```

## Physical test (needs explicit approval: it opens and tunes the receiver)

Before: the RF source is prepared outside (LNB supply for vertical, 22 kHz
for the high band at 12188 MHz) and the F-connector voltage with the receiver
running has been measured.

1. `lsusb | grep 23e2`, `systemctl is-active mediabox-player` inactive,
   `mediabox-dvb selftest` ok, journal cursor.
2. `live-here 10801 --frequency-khz 12188000 --symbol-rate 27500`.
3. For 5 minutes, every 5 s from `status-here`: `receiver.state=streaming`,
   `receiver.usb.overflow_bytes`, `out_of_order`, `errors`, `no_device`;
   `receiver.integrity.cc_errors/tei/sync_errors`; `receiver.signal` (fec, snr,
   strength); `receiver.tune.lnb_pins_untouched`; mpv `hwdec-current`,
   `frame-drop-count`, `avsync`; VOP2 Esmart0; mpv descriptor count.
4. Back (`stop-here`) three times: both units inactive, `receiver.tune.cleanup_ok`,
   `gpio_restored=true`, `lnb_pins_untouched=true`; a fourth start locks again.
5. Unplug during playback: `end=usb_disconnected` after the 20 s reconnect window,
   or a new session (`receiver.sessions=2`) when plugged back within it.

STOP at once (`stop-here`) on: no lock, `lnb_pins_untouched=false`,
`gpio_restored=false`, USB `overflow_bytes>0` or `out_of_order>0`,
`hwdec-current≠rkmpp`, no picture for >10 s, mpv descriptors climbing,
xHCI/EHCI resets or errors in dmesg.
