# ui-preview

The television's interface on this machine, without a build or a board.

Slint's own `slint-viewer` opens the real `rust/crates/mediabox-tv/ui/app.slint`
— the same Theme, Metrics, components and screens the appliance compiles — and a
JSON fixture stands in for the properties Rust would have set. Nothing here is
linked into `mediabox-tv`; its Cargo graph and its KMS platform are untouched.

## Setup

```sh
cargo install slint-viewer --version 1.17.1 --locked   # the crate's own Slint
sudo pacman -S inter-font                               # the appliance's typeface
```

Without Inter the viewer draws in a fallback face, and every width that elides
or wraps is measured in the wrong font; the scripts warn about it.

## Use

```sh
tools/ui-preview/preview.sh home              # a desktop window, live
tools/ui-preview/preview.sh media-settings
tools/ui-preview/preview.sh detail
tools/ui-preview/preview.sh path/to/any.json  # any MediaBoxWindow properties

tools/ui-preview/capture.sh detail            # -> tools/ui-preview/out/detail.png
tools/ui-preview/capture.sh detail /tmp/d.png
```

`SLINT_VIEWER=/path/to/slint-viewer` picks another viewer binary. Arguments after
the fixture go to `slint-viewer` itself.

### The window

An ordinary window: it opens at `MediaBoxWindow`'s preferred 1920×1080 and can
be resized, maximized or snapped freely. `app.slint` copies the window's logical
`width`/`height` into `Metrics.vw`/`vh`, so the interface is laid out again at
every size, with the floors and ceilings the theme gives it.

The window's size is **not** a panel's size. The appliance lays out at a whole
scale of its own choosing (`platform.rs`, `scale_factor`):

    scale   = ceil(min(panel_w / 1920, panel_h / 1080))   # 1..4, rounded up
    logical = panel / scale

| Panel     | Scale | Logical (what Metrics sees) |
|-----------|-------|-----------------------------|
| 1920×1080 | 1     | 1920×1080                   |
| 2560×1440 | 2     | 1280×720                    |
| 3840×2160 | 2     | 1920×1080                   |

At 1280×720 the theme's floors bite (`root-font = max(15px, …)` and others), so
a 2560×1440 panel is not a 1920×1080 one made larger. To look at a panel, run
with its scale and give the window the panel's size in physical pixels:

```sh
SLINT_SCALE_FACTOR=2 tools/ui-preview/preview.sh home   # then size the window
                                                        # to 2560×1440 physical
```

Checked against the appliance on a 2560×1440 panel: home lands within 1 px of
the panel's own screenshot that way, and 39 px off at the bottom of the left
column when run at 2560×1440 logical instead.

### Reload

Saving any `.slint` file under `ui/`, or the fixture, reloads the open window in
well under a second. slint-viewer 1.17.1 misses a save made by renaming a
temporary file over the original (Kate, JetBrains' safe write, Vim for most
files); `preview.sh` touches such a file so it is seen as well.

### Screenshots

`capture.sh` renders headless with the viewer's software renderer at 1920×1080
logical; `SLINT_SCALE_FACTOR=2 tools/ui-preview/capture.sh home` is therefore a
3840×2160 panel. Other panel sizes need the live window. `out/` is not tracked.

The headless picture is the first frame. Anything animated in is not there yet
— a focused filter's white fill, the detail page's backdrop — and colour emoji
are drawn in outline or not at all. Layout is right; looks are not. Use the live
window for those.

## Fixtures

Each is a JSON object of `MediaBoxWindow` properties, with `screen` choosing the
screen. Shapes and strings follow what the Rust side produces (`vitals.rs`,
`state.rs`, `screens/media_settings.rs`, `detail.rs`).

- `home` — the launcher with five tiles (one not installed), the clock, CPU and
  memory, storage, Ethernet and Wi-Fi.
- `media-settings` — Filmler ve Diziler > Ayarlar on its Ses category, remote on
  the controls; the path's summaries carry the player, resume, refresh matching,
  subtitle and account states.
- `detail` — a film's page with the source column focused, and source rows as
  long as real addons write them.

The values are made up, and made-up values hide bugs: `home`'s short SSID fits
the AĞ card, while the appliance's real one (`DENKER-WiLAN-5`, `192.168.2.74`)
is elided on 2560×1440 and on 4K alike. Before judging a layout, put the
panel's real strings in.

Images (`image` properties) are left empty and the screens draw their
fallbacks; an `image` property takes a path to a PNG or JPEG file.
