# Remote control: infrared, Bluetooth, voice

Measured on the Orange Pi 5 Plus with a Ugoos UR-02, 2026-09-27.

## Infrared

The Plus has an IR receiver on GPIO4_B3, active low. The vendor device tree
gives the pin to `pwm15` under `rockchip,remotectl-pwm`, which is not rc-core
and drops every remote but the board vendor's (usercode `0x04fb`).
`packaging/overlays/mediabox-ir-opi5plus.dts` disables `pwm15` and puts the pin
under `gpio-ir-receiver`: rc-core, every kernel decoder, `ir-keytable`.
`packaging/mediabox-ir-setup` installs it on a Plus only.

* The overlay has no pinctrl group of its own: one added under `&pinctrl`
  oopsed the vendor pinctrl driver (`__pi_strcmp` in `pinmux_map_to_setting`).
* The UR-02 sends NEC, address `0x19`, command byte = Linux key code. The
  measured codes are in `packaging/rc/mediabox-remote.toml`, loaded for the
  receiver alone by `packaging/udev/82-mediabox-ir.rules`.
* Once connected to a Bluetooth host the UR-02 stops sending infrared.
* libinput reads a device's keys when it opens it. A keymap loaded after the
  interface started is not seen until the interface restarts; the udev rule
  loads it when the receiver appears, before the interface starts.

The interface tells the CEC adapters' rc devices (protocol `cec`, parent an
HDMI transmitter) from the receiver (`gpio_ir_recv`) by the rc device itself
(`mediabox-tv/src/input.rs`, `classify`); only the CEC ones are skipped, since
the daemon already publishes that remote.

## Bluetooth

BLE HID (HOGP), paired through the settings' Bluetooth screen:
`UR02 Keyboard` and `UR02 Mouse`, vendor `0508` product `1980`.

| Key | evdev |
| --- | --- |
| arrows | KEY_UP/DOWN/LEFT/RIGHT |
| OK | KEY_SELECT |
| Back | KEY_BACK |
| Home | KEY_HOMEPAGE |
| Volume, Mute, Play/Pause | KEY_VOLUMEUP/DOWN, KEY_MUTE, KEY_PLAYPAUSE |

The air mouse is report ID 3 (buttons, X, Y, wheel, 8-bit relative) and
arrives as `REL_X/REL_Y` on `UR02 Mouse`. Menu and the microphone key sent no
evdev event over Bluetooth.

## Voice

No ALSA, PipeWire or BlueZ audio source appears. The UR-02 carries the Android
TV "Voice over BLE" GATT service `ab5e0001-5a21-4f05-bc7d-af01f617b664`:

* control `ab5e0004`: `04 03 01 01` when the microphone key is pressed,
  `00` when it is let go;
* audio `ab5e0003`: 134-byte notifications, `seq(2) | id(1) | predictor(2) |
  step index(1) | 128 bytes of IMA ADPCM`, 256 samples each, 8 kHz mono.

The remote starts streaming on its own; the host sends nothing first.
`tools/atvv-voice/atvv_decode.py` decodes the frames from a `btmon` capture to
WAV. On an 18 s capture (560 frames, no sequence gaps) the last decoded sample
of every frame equals the next frame's predictor 559 times out of 559 with the
high nibble first (2 of 559 low nibble first), so the decode is exact.
The product has no speech recognition; nothing consumes the audio yet.
