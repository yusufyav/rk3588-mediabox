# To do

Open items that were measured but not yet fixed. Each says what was seen,
what was measured, and what was not established.

## A sink that comes back after a hot-plug can get no picture (HDMI 2.0 scrambling)

**Seen** on the Plus, 2026-09-29, monitor on HDMI-A-1 at 2560x1440p144
(TMDS 583.6 MHz, RGB 8-bit): the sink dropped HPD for about a second at
15:59:16 (`the sink changed (HDMI-A-1 … -> ekran yok)` and back);
`mediabox-display-changed` restarted the interface, which kept the lit display
(`took back the display device kept across the last stop`) and did not
modeset. The monitor said "no HDMI signal" while the box reported
`connected`, DPMS `On`, VP0 active at 2560x1440p144.

**Measured** over DDC (`/dev/i2c-9` = fde80000.hdmi, SCDC at 0x54):

| | expected at 583.6 MHz | read |
|---|---|---|
| `0x20` TMDS_Config | `0x03` (scrambling, clock ratio 1/40) | `0x00` |
| `0x21` Scrambler_Status | `0x01` | `0x00` |
| `0x40` Status_Flags_0 (clock, ch0–2 lock) | `0x0f` | `0x00` |

dmesg had no modeset after boot (last `Update mode to 2560x1440p144` at
14:30:08). Writing `0x20 = 0x03` by hand did not bring the clock back
(`0x40` stayed `0x00`), so the SCDC state is not the whole story: the
transmitter is probably not driving TMDS after the sink's HPD cycle either.
The picture came back at 16:22 after two further HPD cycles from the sink
(`0x21 = 0x01`, `0x40 = 0x0f`), again with no modeset in dmesg.

**Reproduced** at 16:23:24: switching the monitor's input source in its own
menu is an HPD cycle, and afterwards SCDC read `0x00 0x00 0x01` (clock seen,
no lock, no scrambling) with no modeset in dmesg. The drop at 15:59:16 was most
likely the same kind of event from the monitor's side. A normal thing to do
with a monitor must not cost the picture.

**Recovered without a permanent change** at 16:26 through the product's own
self-reverting trial (`output_try` 2560x1440p59.95, not kept): the trial's
modeset at 241.5 MHz locked (`0x00 0x00 0x0f`), and the modeset back to
2560x1440p144 fifteen seconds later programmed SCDC again (`0x03 0x01 0x0f`).
A full modeset of the same mode is enough; restarting the interface is not,
because it takes over the lit display without one.

**Not established:** whether the vendor dw-hdmi-qp HPD path is meant to
re-enable TMDS and SCDC by itself on a same-sink reconnect (a kernel fault) or
whether user space has to. Modes below 340 MHz need no scrambling and are
probably not affected; not measured.

**To do:** after a hot-plug of the same sink, when the interface keeps the lit
display without a modeset, check that the sink is locked (SCDC `0x40`, above
340 MHz also `0x21`) and force a full modeset of the same mode if it is not.
