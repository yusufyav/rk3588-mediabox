# Headless Stremio

> Stremio is a provider. It is not MediaBox's user interface, and MediaBox's
> user interface must never need to know it exists.

## Why this exists

M2 built the media experience out of the upstream Stremio web application and
added a MediaBox shell to the same document. That works, and it costs the thing
the product actually needs: the appliance's behaviour becomes a property of
somebody else's page. Reading a playhead means reading `window.core`; changing
what is offered means changing what a third-party application renders; and the
transcoding decision belongs to a browser's codec list rather than to the
television the box is plugged into.

The V2 media core takes Stremio's **data plane** instead, and nothing else.

## What the data plane is

Three HTTP surfaces, all of them Stremio's own, all of them documented:

| Surface | What it answers | Where |
| --- | --- | --- |
| Account API | session, installed addons, library | `https://api.strem.io/api` |
| Addon protocol | catalogue, metadata, streams, subtitles | each addon's transport URL |
| Streaming server | torrent → playable HTTP | `http://127.0.0.1:11470` |

An addon is an HTTP service answering under its transport URL:

```
/manifest.json
/catalog/{type}/{id}.json          (+ /{extra}.json)
/meta/{type}/{id}.json
/stream/{type}/{id}.json
/subtitles/{type}/{id}.json        (+ /{extra}.json)
```

That is the whole of it. `media/stremio/addons.py` speaks exactly this.

## Signing in is optional

`addonCollectionGet` accepts `authKey: null` and returns Stremio's default
collection, so the appliance has real catalogues out of the box with no
account. Verified on the target:

```
authenticated       False
addons              7
api reachable       True
streaming server    True (4.21.0)
```

| Addon | Resources |
| --- | --- |
| `com.linvo.cinemeta` | catalog, meta, addon_catalog |
| `com.linvo.stremiochannels` | catalog, meta |
| `org.stremio.watchhub` | stream |
| `org.stremio.pubdomainmovies` | catalog, stream |
| `org.stremio.opensubtitlesv3` | subtitles |
| `org.stremio.opensubtitles` | subtitles |
| `org.stremio.local` | meta, stream |

A login stores the returned auth key at `0600` and never the password.
`state_path = ""` in `[media]` keeps nothing on disk at all.

## The contract

```python
session_status()                       # who we are, and what is reachable
addons()                               # what can answer questions
home(types=...)                        # catalogue rows worth showing first
catalog(type, id, addon_id=, extra=)   # one catalogue
search(query, types=)                  # every searchable catalogue at once
meta(type, id)                         # one item in full
streams(type, id, video_id=None)       # the ways to watch it
resolve(stream)                        # a URL a player or ffprobe can open
subtitles(type, id, video_id=, extra=) # subtitle tracks
library()                              # only when signed in
```

An episode is asked for by its video id, `{seriesId}:{season}:{episode}` —
`video_id_for("tt10466872", 1, 1)` — because that is what a stream addon keys
on. `media/stremio/adapter.py` is the only place in the repository that knows
this.

### Failure is per-addon

`streams()` asks every addon that declares support and merges what comes back,
deduplicating on stream identity. An addon that fails costs its own results and
nothing else. `home()` is the same: an appliance whose home screen is empty
because one third-party service is down is not an appliance.

### Scoping is honoured

A manifest may scope a resource by type and by id prefix, and may override the
manifest-level scope per resource. `Addon.supports(resource, type, id)` applies
both, so a Kitsu-only stream addon is never asked about an IMDb id.

## Stream resolution

| Descriptor | Resolves to | Needs the streaming server |
| --- | --- | --- |
| `url` | that URL | no |
| `infoHash` (+ `fileIdx`) | `{server}/{infoHash}/{fileIdx}` | yes |
| `ytId` | `{server}/yt/{id}` | yes |
| `externalUrl` only | refused — it is a link into another application | — |

**Kodi is never given a magnet link, an info hash, or a torrent object.** The
streaming server turns a torrent into HTTP and only the HTTP URL travels
onward. Resolution also reports whether the engine has made any peer contact,
because "the swarm has 55 members" and "we have reached one of them" are
different facts.

The streaming server's transcoding endpoints (`hlsv2`) are deliberately unused.
Transcoding decisions belong to [`docs/media-policy.md`](media-policy.md),
which knows what this board can do; the streaming server would decide from a
browser's codec list and, finding no usable hardware encode profile here, would
fall back to a software encode.

## The MediaBox API on top

Nothing above the media core talks to any of the above. It talks to `/media/`:

```
GET  /media/status                  GET  /media/streams/{type}/{id}
GET  /media/capabilities            GET  /media/subtitles/{type}/{id}
GET  /media/home                    POST /media/resolve
GET  /media/search?q=               POST /media/inspect
GET  /media/catalog/{type}/{id}     POST /media/plan
GET  /media/meta/{type}/{id}        POST /media/rank
```

No route names the provider. `media/tests/test_architecture.py` enforces that,
along with: no DOM marker anywhere in the package, no browser driver, no
reference to the upstream web application's surfaces, no import of the control
plane, and no third-party dependency.

## From a terminal

```
media-core stremio status
media-core stremio search "Dune"
media-core stremio meta movie tt1160419
media-core stremio streams movie tt0063350
media-core stremio resolve movie tt0063350
```

`--json` prints what the HTTP API returns.
