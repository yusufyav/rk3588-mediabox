# Playback lifecycle: stream availability and how a film ends

Status: implemented (worker `media/proxy/session.py`, control plane
`rust/crates/mediaboxd-rs/src/playback.rs`, TV UI `watch_the_film`). Written
after a film on the appliance's own player closed itself on a seek
(2026-09-27, session `a306b91d…`).

## What happened

| time (device) | layer | event |
|---|---|---|
| 20:02:42 | worker | last `GET /media/session/a306b91d… 206` |
| 20:04:38 | worker | `media session a306b91d… stopped (idle)` -- mpv was still playing from its cache |
| 20:14:25.500 | worker | seek → `GET /media/session/a306b91d… 404` |
| 20:14:25 | mpv | `Seek failed (…, size 68)`, then end of file; `--keep-open=no` exits |
| +88 ms | systemd | `mediabox-player.service: Deactivated successfully` |
| after | TV UI | plane dark → "film over" → account told it closed → 502 on screen |

Two faults, one per layer:

1. **Availability.** A DIRECT session's `last_seen` is set when it is created
   and never again: `relay()` streams the source without touching the session,
   and `reap_once` drops a DIRECT record once `now - last_seen > 4 × 45 s`.
   A film that plays longer than three minutes without a new request loses its
   URL while it is being watched.
2. **Termination.** Nothing on our side knows *why* mpv stopped. The TV UI
   decides a film is over when the video plane goes dark
   (`watch_the_film`), so a read failure, a user stop and the real end of
   the film all look the same, and all three are reported to the account as
   "closed".

## How proven players manage these layers (read from source)

### Availability

- **Stremio server** (`server.js`, flatpak build): every stream is counted by
  `Counter(EngineFS, "stream-open", "stream-close", …)`. The inactivity timer
  (`STREAM_TIMEOUT` 20 s, `ENGINE_TIMEOUT` 120 s in the app's settings) only
  starts when the open-stream count reaches zero. And the URL is
  content-addressed (`/:infoHash/:idx`): a request for an engine that was
  removed calls `createEngine` again. Expiry frees resources; it never makes a
  URL that a player holds invalid.
- **Our transforms** already do the first half: `attach`/`detach` count
  clients and the reaper waits for `clients == 0`. DIRECT relays do not.

### Transport errors

- **Kodi** `CCurlFile::CReadState::FillBuffer`: a transient curl error
  reconnects and seeks back to the current position (`SetResume()`), up to
  `m_curlretries` times, before the read fails.
- **ExoPlayer** `DefaultLoadErrorHandlingPolicy`: HTTP 404/5xx are retried
  with a 0–5 s backoff (minimum 3 attempts); only parser errors,
  `FileNotFoundException` and out-of-range reads are final.
- **mpv 0.41** `stream_lavf.c`: `reconnect=1`, `reconnect_delay_max=7` --
  a dropped connection is reopened; an HTTP 404 is not.

### How a film ends

- **Kodi** `CVideoPlayer` ends in exactly one of three callbacks:
  `OnPlayBackStopped` (close requested), `OnPlayBackError` (the error dialog,
  then stopped), `OnPlayBackEnded`.
- **ExoPlayer**: `STATE_ENDED` only when every renderer reaches end of stream;
  a load failure surfaces as `onPlayerError(PlaybackException)`, never as
  ended.
- **mpv**: `end-file` carries `reason` = `eof | stop | quit | error` and
  `file_error`. Its manual warns that `eof` "can include incomplete files or
  broken network connections". So the reason alone is not the verdict; the
  position against the duration is part of it.

## The model

### A. A session URL is valid for as long as someone can use it

- **Owner.** A session is created with an `owner` (`player`, `kodi`) and held
  until the control plane lets it go (`POST /media/session/{id}/release`, or
  a stop). A held session is never reaped: a film paused for an hour reads
  nothing and still needs its address.
- **Readers.** Every response being streamed -- a direct relay as much as a
  transform -- is counted while it is open (`read_direct`, `attach`).
- **Leak guard.** Only a session nobody holds and nobody reads expires, its
  TTL counted from the last of either -- the Stremio server's rule.
- **A stale address is an error.** A request for a session that has ended
  answers `410 SESSION_STOPPED`, never an empty 200 or a 404 a player could
  read as the end of a file.

### B. The control plane decides how a film ended, for every player

`playback::Supervisor` holds the one record of the film playing, here or in
Kodi, and decides once:

| player says | and | outcome |
|---|---|---|
| anything | we asked it to stop (viewer, new film, handover) | `stopped` |
| mpv `eof-reached` (`--keep-open=yes`) / `end-file eof` | position at the length | `ended` |
| the same | position short of the length | `failed` |
| mpv `end-file error` | | `failed`, with `file_error` |
| mpv `end-file quit/stop` | | `stopped` |
| Kodi `Player.OnStop` `data.end=true` | position at the length | `ended` (short of it: `failed`) |
| Kodi `Player.OnStop` `data.end=false`, Kodi closed | | `stopped` |
| mpv socket closes without a word | | `failed` |

"At the length" is the player's own reading of the file with a margin of
max(15 s, 0.5 %), or the catalogue's runtime with max(180 s, 3 %). It is not
Stremio's watched threshold, which the media core applies separately.

Kodi's `data.end` is read from its own schema at run time
(`JSONRPC.Introspect`); Kodi sends `end=false` after an error and can send
`end=true` after a read that failed half way (its VideoPlayer sets its error
flag only when a file fails to open), which is why the position check applies
to Kodi as well.

The same record is the only writer of the account while a film plays:
progress every 90 s and after a jump, and at the end:

| outcome | account |
|---|---|
| `stopped`, `ended` | progress with `closed: true` |
| `stopped` for a handover | progress, `closed: false`; the Kodi film carries on |
| `failed` | progress at the last position, `closed: false` |

The TV UI sends commands and draws `film` / `finished` from
`MediaStatusHere`. It decides only what the control plane cannot see: a
source that never puts a picture up (first-frame grace), and a control plane
that says nothing for 10 s after the picture went dark.

Automatic retry is deliberately not here. It is a product choice, to be made
once this is proven on the device, with a guard against loops.

## Proof required before calling it done

Host tests (in the tree):

- worker (`media/tests/test_api.py` `SessionLifetimeTests`): a held direct
  session seeks with a 206 after eleven minutes on a fake clock; a session
  being read is kept whoever opened it; a released one nobody reads is
  cleaned up after its TTL and then answers 410.
- control plane (`playback::tests`): early EOF and `end-file eof` short of
  the length → `failed`; at the length → `ended`; `error` → `failed`; our
  stop → `stopped` whatever the player says; Kodi `end=true/false`; a failure
  is written as progress and never as closed; a handover is progress and the
  film goes on in Kodi; a film is decided once.
- Kodi's schema (`kodi::tests`, ignored by default): a running Kodi's
  `JSONRPC.Introspect` declares `Player.OnStop.data.end`.
- TV UI (`ending_of`): only the ending of this film's number ends it.

On the device:

- The reported scenario: a direct film, more than ten minutes, seek back with
  Left, Enter. Expected: the seek lands.
- Stop the session under the player. Expected: `failed`, the message, the
  position on the account, the player closed; no silent exit.
- The real end of a short clip. Expected: `ended`, account closed.
- The same three through a handover to Kodi, and the account read back.
- Measure on the SK1 what Stremio shows when its source drops mid-film.
