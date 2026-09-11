# MediaBox Web UI (M2)

The web interface is the MediaBox appliance itself as far as anyone using it is
concerned. It runs in a desktop browser, on a phone or tablet, and on a TV, and
it is driven either by pointer/touch or by a remote control.

It is two things in one document:

- **the media experience** — upstream [stremio-web][web], built from a pinned
  revision and used unmodified;
- **the appliance shell** — the MediaBox layer over it, which is what lives in
  `webui/`.

The shell is deliberately small. Browsing, searching, a title's page, its
streams and the preview player are all the media app's; the shell owns handing
a stream to the television, showing what the television is playing, and device
settings. There is no MediaBox home screen, because the product's home screen is
the media experience.

[web]: https://github.com/Stremio/stremio-web

## How the two fit together

```
mediaboxd :8787                         one origin, no CORS, no iframe
  /ui/        index.html    upstream Stremio's page + two appliance tags
              mediabox/     the appliance shell bundle
              <rev>/        upstream's own assets, byte-for-byte
  /api/v1/    the control API the shell speaks
  /server/    the Stremio streaming server, reverse-proxied
```

The composition is `scripts/build-media-ui.sh`. It builds upstream at the
revision pinned in `packaging/upstream.env`, builds the shell, copies both into
one root, and adds exactly two lines to upstream's page: a stylesheet and a
script. No upstream byte is patched and no fork is maintained.

The shell then talks to the media app only through `window.core`, the transport
stremio-web publishes itself. That is the entire coupling surface: reading
which stream is selected, setting the streaming-server URL, and casting. All
three are documented core actions the upstream UI dispatches too.

### Why not an iframe

An iframe would put the two on separate documents, which costs the things that
make this work: one focus tree for the remote, one origin without CORS, and
access to `window.core` for the position handoff. Sharing the document gives all
three, and costs only discipline about what the shell is allowed to style.

That discipline is real and enforced: the shell's design tokens are scoped to
its own root element rather than `:root` and are prefixed `--mb-`, the shell
styles no bare element outside itself, and its root is `position: fixed` with
`pointer-events: none` so it claims no layout and swallows no clicks.

### Pointing Stremio at this box

stremio-core defaults its streaming server to `http://127.0.0.1:11470`, which is
right for the desktop app and wrong for every device that reaches the appliance
over the network. On load the shell reads the current setting and, **only if it
is still one of the upstream defaults**, dispatches `Ctx/UpdateSettings` to
point it at `/server/` on this origin. A server the user chose in Stremio's own
settings is left alone.

## Preview and the television

Clicking a stream in the media app opens the media app's own player. That is the
preview: it is there so you can see that the stream is the right thing, at the
right quality, before committing the television to it. It never starts Kodi.

While a preview is on screen the shell shows one control, **"Kodi'de Oynat"**.
It reads the live playhead from the media element, pauses the preview, and
dispatches Stremio's own `StreamingServer/PlayOnDevice` action at the MediaBox
cast device. From there it is `mediaboxd`'s business (see `docs/mediaboxd.md`).

The preview is paused before the handoff on purpose: one torrent engine feeding
two readers at two different seek positions makes both of them buffer.

### Position

Upstream's own cast menu calls the same action with the position hard-coded to
zero — `OptionsMenu` renders `onClick={playOnDevice}`, and the `Option`
component calls it with the device id alone, so `usePlayOnDevice` falls back to
`time: 0`. The shell's control is the same action with the real position filled
in, which is why it exists as a separate control rather than being left to the
upstream menu.

Source lives in `webui/`; the shell bundle is emitted to `webui/dist/mediabox/`.

## Quick start

```sh
cd webui
npm install

npm run dev        # dev server, proxies /api and /server to the appliance
npm run dev:mock   # dev server against the in-memory backend
npm run test       # vitest suite
npm run build      # typecheck + shell bundle into webui/dist/mediabox/
```

To build and deploy the whole media surface rather than only the shell:

```sh
scripts/build-media-ui.sh      # upstream Stremio + shell, composed
scripts/deploy-mediabox.sh     # installs it, the streaming server and the units
```

`webui/dist/` is a build artifact and is not committed.

## Backend contract

The UI speaks the mediaboxd v1 API. The production bundle is served below
`/ui/`, while API requests are always resolved from the same origin at
`/api/v1/…`.

| Method | Path | Used by |
| --- | --- | --- |
| GET | `/api/v1/health` | connection state, service version, action capabilities |
| GET | `/api/v1/system` | dashboard telemetry, Device page |
| GET | `/api/v1/network` | dashboard, Network page |
| GET | `/api/v1/kodi` | Kodi state and player position |
| GET | `/api/v1/display` | HDMI mode, colour, HDR state |
| POST | `/api/v1/kodi/playpause` | Player, dashboard quick controls |
| POST | `/api/v1/kodi/stop` | stop **playback** |
| POST | `/api/v1/kodi/seek` | absolute seek, body `{ "seconds": n }` |
| POST | `/api/v1/kodi/open` | open a URL, body `{ "url": "file:///…" }` |
| POST | `/api/v1/kodi/start` | start the Kodi **service** |
| POST | `/api/v1/kodi/stop-service` | stop the Kodi **service** |
| POST | `/api/v1/kodi/restart` | restart the Kodi service |
| GET | `/api/v1/stremio` | streaming-server reachability, version, cast device |
| GET | `/api/v1/cast` | the handoff the backend currently owns |
| POST | `/api/v1/cast/kodi` | hand a stream to Kodi with a position |

Event stream: the UI first tries `GET /api/v1/ws` (WebSocket), then falls back to
`GET /api/v1/events` (SSE). M1 serves SSE; each `data:` value is a JSON object of the shape
`{ "type": "kodi" | "system" | "network" | "display", "payload": { … } }` and
carry the same body as the matching GET endpoint. If neither stream connects,
polling alone keeps the UI correct.

### Assumptions beyond the frozen contract

Two things the UI needs are not in the M1A endpoint list. Both are optional, and
the UI degrades safely when they are missing:

1. **`health.actions`** — a capability map
   (`kodiStart`, `kodiStop`, `kodiRestart`, `reboot`, `shutdown`, plus an
   optional `reason` string). Policy:
   - explicit `true` → control enabled;
   - explicit `false` → control disabled, `reason` shown as its tooltip;
   - **absent** → Kodi service controls stay enabled (they are part of the
     frozen contract), power controls stay disabled (they are not).
2. **`POST /api/v1/system/reboot`** and **`POST /api/v1/system/shutdown`** back
   the Settings power buttons. Until `health.actions` advertises them, those
   buttons render disabled and are never sent.

The backend adapters and frontend fixtures share this camelCase wire schema;
hardware-dependent values remain optional and render as `—` when Linux does not
expose them.

## Architecture

```
webui/src/
  api/         typed client + endpoint map (the only place paths are written)
  store/       DeviceStore: the single polling/streaming data layer
  mocks/       in-memory backend and fixtures
  nav/         viewport classifier, remote/keyboard navigation
  components/  Button, Card, Status, ConfirmDialog
  shell/       Shell, CastBar, NowPlaying, SettingsPanel, Diagnostics, Panel
  stremio/     the window.core bridge — the only coupling to the media app
  styles/      design tokens (scoped to the shell) and its base stylesheet
```

### Data layer

`DeviceStore` is the only thing in the app that runs a timer. Components
subscribe to its snapshot through `useDeviceState()`; none of them fetch. Poll
cadences:

| Resource | Cadence |
| --- | --- |
| `kodi` | 1.5 s (slows 4× while the event stream is connected) |
| `system` | 5 s |
| `health`, `network`, `display`, `stremio` | 10 s |

Each resource carries `updatedAt`. A sample older than 4× its cadence is tagged
**"Eski veri"** in the UI, so a frozen backend never reads as current state. When
every resource fails with an unreachable error, the shell shows
*"MediaBox servisine ulaşılamıyor"* and keeps the last good values, labelled.

Commands go through `store.runAction()`, which re-reads Kodi state afterwards
(no optimistic UI), locks controls while in flight, and surfaces failures in a
banner instead of failing silently.

### Remote-first navigation

`useRemoteNavigation()` installs one document-level key handler:

| Key | Behaviour |
| --- | --- |
| Arrow keys | geometric focus move across every focusable element on the page |
| Enter | activates the focused control (native button behaviour) |
| Escape / Backspace | back: closes an open panel, else left to the media app |

Candidates are not limited to the shell's own controls. The media app marks its
controls the ordinary way — `tabindex="0"` on a div, or a real button — and from
the sofa there is one UI, so both kinds are reachable and both show a focus ring
once an arrow key has been pressed. Elements with no box are excluded: a media
app renders far more than it shows.

Two exceptions keep the media app whole. On its player the arrow keys are left
alone, because that is where they seek. And the back key is only claimed if the
shell actually consumed it, so otherwise the media app gets its own back
behaviour.

The geometry resolver (`nav/spatial.ts`) is DOM-free and unit-tested: it picks
the nearest candidate that makes real progress along the travel axis and
penalises off-axis movement, so "down" never jumps sideways across the screen.

Focus is always visible. Pointer users get `:focus-visible`; after the first
arrow press the root element is marked `data-input="remote"` and every focused
control shows a ring (thicker on TV).

While a confirmation dialog is open, navigation candidates are scoped to the
dialog (`data-nav-scope="dialog"`) so a remote cannot reach a control hidden
behind it. The back key always dismisses the dialog, so this is not a focus
trap. Cancel takes initial focus: a stray Enter must not reboot the box.

DOM order is navigation order — the sidebar precedes the page content, matching
how the layout reads on screen.

### Layout classes

`classifyViewport()` maps the viewport to one of four classes, exposed as
`data-layout` on `<html>`:

| Class | Condition |
| --- | --- |
| `tv` | width ≥ 1600 px **and** aspect ≥ 1.6 |
| `desktop` | width ≥ 1024 px |
| `tablet` | width ≥ 640 px |
| `mobile` | below that |

Only the type scale and spacing change; the structure is shared. TV raises the
root font size and lifts the smallest step of the type scale to body size, so no
label can become fine print across a room. Below 1024 px the sidebar becomes a
horizontal rail and the dashboard collapses to one column.

`?layout=tv` forces a class for kiosk browsers that report an unhelpful
viewport.

### Mock mode

The frontend is developable with no device attached. `?mock=1` (or `VITE_MOCK=1`)
swaps `HttpTransport` for `MockTransport`, an in-memory backend with mutable
playback state — seeking really moves the position, play/pause really toggles.

`?scenario=` selects the fixture set:

| Scenario | Shows |
| --- | --- |
| `playing` (default) | Kodi playing 4K HDR content, network up, HDR display |
| `idle` | Kodi running with nothing loaded |
| `kodi-offline` | Kodi service down |
| `backend-down` | mediaboxd unreachable |

A **"Mock mod"** marker is shown in the top bar whenever mock mode is active.

## Design

Dark, low-chroma, high contrast. Colour is used to mean something — state,
focus, danger — and never for decoration. Tokens live in
`src/styles/tokens.css`; component styles are CSS Modules. The only runtime
dependencies are React and React DOM.

## Tests

`npm run test` runs the vitest suite (jsdom): API client and endpoint mapping,
dashboard render, Kodi-offline and idle states, player controls hitting the
correct endpoints, remote/keyboard navigation, the reboot confirmation gate and
capability handling, mobile/TV layout behaviour, and the mock backend.

## Settings and Diagnostics

Settings is a panel behind one control in the corner, not a place the product
opens into. Its sections are Playback, Network, Bluetooth, HDMI/CEC, Display,
Audio, System and Diagnostics.

Sections with no service behind them — Bluetooth, CEC, Audio — say so in plain
words. They do not render switches that would look like they work.

**Diagnostics** is where the telemetry went: CPU, memory, storage, temperatures,
kernel, network, display, HDR, Kodi, the streaming server and service health. It
is deliberately the deepest screen in the product. None of it belongs on a media
home screen.

## Tests

`npm run test` runs the vitest suite (jsdom): the API client and endpoint map,
the shell's absence of telemetry and admin navigation, settings as a secondary
surface, diagnostics as the place telemetry lives, the preview/Kodi action
split, position forwarding, streaming-server URL adoption, remote navigation
across both apps' controls, layout classes, and the mock backend.

## Not in this milestone

Wi-Fi configuration, CEC and Bluetooth control, audio routing, and
kiosk/compositor setup.
