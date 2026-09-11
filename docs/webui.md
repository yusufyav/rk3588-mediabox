# MediaBox Web UI (M1)

The web interface is the first user-facing surface of the MediaBox appliance. It
runs in a desktop browser, on a phone or tablet, and on a TV-local kiosk
browser, and it is driven either by pointer/touch or by a remote control.

Source lives in `webui/`; the production bundle is emitted to `webui/dist/` and
is meant to be served as static files by `mediaboxd`.

## Quick start

```sh
cd webui
npm install

npm run dev        # dev server, proxies /api to http://localhost:8080
npm run dev:mock   # dev server against the in-memory backend
npm run test       # vitest suite
npm run build      # typecheck + production bundle into webui/dist
```

`webui/dist/` is a build artifact and is not committed; run `npm run build`
before packaging an image.

## Backend contract

The UI speaks the mediaboxd v1 API. Paths are resolved relative to the
document's base URL (`<base>/api/v1/…`), so the bundle works from any mount
point without rebuilding.

| Method | Path | Used by |
| --- | --- | --- |
| GET | `/api/v1/health` | connection state, service version, action capabilities |
| GET | `/api/v1/system` | dashboard telemetry, Device page |
| GET | `/api/v1/network` | dashboard, Network page |
| GET | `/api/v1/kodi` | Kodi state and player position |
| GET | `/api/v1/display` | HDMI mode, colour, HDR state |
| POST | `/api/v1/kodi/playpause` | Player, dashboard quick controls |
| POST | `/api/v1/kodi/stop` | stop **playback** |
| POST | `/api/v1/kodi/seek` | relative seek, body `{ "offsetSeconds": ±n }` |
| POST | `/api/v1/kodi/open` | open a path, body `{ "path": "…" }` |
| POST | `/api/v1/kodi/start` | start the Kodi **service** |
| POST | `/api/v1/kodi/stop-service` | stop the Kodi **service** |
| POST | `/api/v1/kodi/restart` | restart the Kodi service |

Event stream: the UI first tries `GET /api/v1/ws` (WebSocket), then falls back to
`GET /api/v1/events` (SSE). Events are JSON objects of the shape
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

Responses are read defensively: every field the UI consumes is optional, and a
missing value renders as `—` rather than breaking a page.

## Architecture

```
webui/src/
  api/         typed client + endpoint map (the only place paths are written)
  store/       DeviceStore: the single polling/streaming data layer
  mocks/       in-memory backend and fixtures
  nav/         hash router, viewport classifier, remote/keyboard navigation
  components/  Shell, Button, Card, Status, PlayerControls, ConfirmDialog
  pages/       Home, Player, Device, Network, Settings, Stremio
  styles/      design tokens and base stylesheet
```

### Data layer

`DeviceStore` is the only thing in the app that runs a timer. Components
subscribe to its snapshot through `useDeviceState()`; none of them fetch. Poll
cadences:

| Resource | Cadence |
| --- | --- |
| `kodi` | 1.5 s (slows 4× while the event stream is connected) |
| `system` | 5 s |
| `health`, `network`, `display` | 10 s |

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
| Arrow keys | geometric focus move between `[data-focusable]` elements |
| Enter | activates the focused control (native button behaviour) |
| Escape / Backspace | back: closes an open dialog, else returns to the dashboard |

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

## Not in this milestone

Stremio integration (the tab is a deliberate placeholder), Wi-Fi configuration,
CEC and Bluetooth control, and kiosk/compositor setup.
