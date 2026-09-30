# Media sessions

> A process the appliance started must not outlive the reason it was started.

## Why

An ffmpeg with no reader is a busy core forever, and the reader is a browser or
a television that can vanish without saying so. M2 shipped exactly that fault:
a preview's transcoder kept running after the preview closed, and on a board
where every hardware encode profile is rejected, an unowned session is a
software encode nobody is watching.

The fix there was a reaper on someone else's sessions. Here the media core owns
the process, so the lifecycle is a property of the object rather than a patch
on top of it.

## Modes

| Mode | Process | When |
| --- | --- | --- |
| `Direct` | **none** | the player opens the source itself |
| `DirectProxy` | relay | bytes need to reach a client that cannot reach the source |
| `Remux` | ffmpeg, all copy | container has to change |
| `AudioTranscode` | ffmpeg, video copy | audio has to become AC-3 |

A `Direct` session is recorded and starts nothing. Its bytes still come
through `GET /media/session/{id}`, relayed (`media/proxy/relay.py`): the
player MediaBox owns is built against the Rockchip ffmpeg, which has no TLS,
and every source worth playing is an HTTPS link. (It used to answer `409
SESSION_IS_DIRECT`; Kodi can open HTTPS itself, the appliance's player
cannot.)

## Lifecycle

```
create   one session per (source, mode) — asking twice returns the first
attach   starts the child on first read; counts a client
detach   in the generator's `finally`, whether the client finished or died
stop     idempotent; SIGTERM the process group → SIGKILL → wait()
expire   no reader for the TTL (45 s default) → stop("idle")
reap     a child that exited on its own → stop("child-exited")
shutdown every session stopped; nothing survives the daemon
```

### One encoder per source

Deduplication is the point, not an optimisation. Two encoders for one viewer is
the failure this module exists to prevent, so `create()` with the same source
and mode returns the live session.

### Process groups

Every child is started with `start_new_session=True`, so it is signalled as a
group and a helper it spawned cannot survive it. Tested by making a fake
encoder spawn a grandchild and asserting the grandchild is gone.

### Always waited on

`stop()` does not return until `wait()` has been called, so a stopped session
leaves no zombie. A child that ignores `SIGTERM` is killed; a test asserts it.

### Reading

Relaying uses `read1`, not `read`. `read` waits for a full 64 KiB buffer, which
turns a live stream into one that starts late and stutters after that. A stop
from another thread closing the pipe under a blocked read ends the stream
cleanly rather than surfacing as an error to the client.

## Relaying a remote source

Ranges pass both ways, one request per connection: a player seeks by
abandoning the reply it is reading and asking for a new range.

A seek is a new request through the addon's resolver (a 302 from Torrentio or
MediaFusion) and then the debrid host, and either can stumble once:

* **Opening.** A failure that is the host's -- no answer, a timeout, 408,
  425, 429 or 5xx -- is tried twice more, 0.5 s and 1.5 s later, before the
  player is told `502`. 404 and 416 are the source's own answer and are passed
  on at once; any other status is not retried. A `502` once ended a film at
  28:26 during a seek ("kaynak okunamadı") that the next request played on.
* **On the way.** When the upstream connection ends before the body it
  promised (it was dropped, or went quiet past the read timeout), the rest is
  asked for from the byte where it stopped (`Range: bytes=<start + sent>-`)
  and the same reply carries on, up to five times (`MAX_RESUMES`). A resumed
  answer that is not a `206` starting at that byte ends the reply instead: no
  byte is sent that is not the file's. Before this, mpv saw "Stream ends
  prematurely at 2522294964" of a 29.7 GB remux, reconnected, and did not
  start its sound again; the film froze at that point.
* **How long a `206` is** comes from its `Content-Range` (`bytes S-E/T` is
  `E - S + 1` bytes), never from its `Content-Length`: a CDN was measured
  answering a one-byte probe with `Content-Range: bytes 0-0/29159331995` and
  `Content-Length: 29159331995`. The length is only checked against the
  range; when they disagree the range wins, the player is given its length,
  and nothing past it is read or waited for. A `206` whose `Content-Range`
  cannot be read is passed on as it came and never resumed -- nothing is
  made up. A `200` is the whole file, as its `Content-Length` says.

Every failure is logged with the host, the range and the cause -- never the
URL, which carries an account token.

## State

```json
{
  "sessionId": "…32 hex…",
  "source": "…", "mode": "AudioTranscode", "state": "running",
  "pid": 18456, "clients": 1,
  "createdAt": …, "startedAt": …, "lastSeen": …, "stoppedAt": null,
  "exitCode": null, "stopReason": null,
  "playbackUrl": "http://box:8787/media/session/…",
  "selectedTracks": { "video": {"action": "copy", …},
                      "audio": {"action": "TranscodeToAC3",
                                "target": {"codec": "ac3", "channels": 6, …}} },
  "videoCopied": true,
  "reasons": [ … ],
  "stderr": null
}
```

## Handoff

A created session also answers with everything a preview → television handoff
needs, and nothing more:

```json
{
  "sourceIdentity": "…", "resolvedInput": "…",
  "playbackUrl": "http://box:8787/media/session/…",
  "kodiPlaybackUrl": "http://127.0.0.1:8787/media/session/…",
  "selectedTracks": {…}, "previewMode": "Unsupported",
  "kodiMode": "DirectWithAudioTranscode", "resumeSeconds": 0.0,
  "reasons": […]
}
```

The media core does not drive Kodi. It states what the thing that does will
need. When a loopback base URL is configured the television's copy of the URL
points at loopback, so production playback never leaves the appliance.

## Security

| | |
| --- | --- |
| Schemes | `http`, `https` only; `file:` under an explicit directory allowlist, off by default |
| Magnet / torrent | refused as a source — the streaming server resolves them, Kodi never sees one |
| Loopback | refused except for the configured streaming server |
| Link-local, metadata address | refused |
| Control characters in a URL | refused |
| Path traversal in a `file:` URL | refused |
| Session ids | `^[0-9a-f]{32}$`, validated before any lookup |
| Shell | never used; argument vectors only |
| Resolver failure | allowed through — the probe reports the real reason; a DNS blip is not a policy violation |

`media/tests/test_session.py` covers each row.
