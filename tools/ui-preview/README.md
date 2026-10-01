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
be resized, maximized or snapped freely. The window's size is the panel's size —
`app.slint` copies `width`/`height` into `Metrics.vw`/`vh` — so the interface is
laid out again at every size, with the floors and ceilings the theme gives it.

### Reload

Saving any `.slint` file under `ui/`, or the fixture, reloads the open window in
well under a second. slint-viewer 1.17.1 misses a save made by renaming a
temporary file over the original (Kate, JetBrains' safe write, Vim for most
files); `preview.sh` touches such a file so it is seen as well.

### Screenshots

`capture.sh` renders headless with the viewer's software renderer at 1920×1080.
It is for looking at, not for comparing against the panel: gradients and emoji
are drawn differently from the GL window and from the appliance. `out/` is not
tracked.

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

Images (`image` properties) are left empty; the screens draw their fallbacks.
