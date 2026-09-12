# Audio to AC-3

> **The video is copied.** Advanced audio is never a reason to re-encode video.

## Why the appliance encodes AC-3 at all

Kodi can transcode to AC-3 itself, and on this hardware it produces silence.

Gate MA1 measured it directly: with the identical sink configuration — same
device, same 48 kHz `S16_LE` carrier, same `AES0=0x06` non-audio bits — a real
AC-3 file's frames play, and Kodi's own encoder output does not. The fault is
above every layer that gate proved, and `audiooutput.ac3transcode` is left
`false` because of it.

So the conversion has to happen **upstream of Kodi**, where the AC-3 arrives as
a file's frames. That is what this pipeline is.

## The command

```
ffmpeg -hide_banner -nostdin -loglevel error
       [-ss <seconds>]                        # before -i: seeks, does not decode
       [-user_agent … -reconnect …]           # http(s) inputs only
       -i <source>
       -map 0:<video index>
       -map 0:<audio index>
       -c:v copy
       -c:a ac3 -ac <channels> -b:a <bitrate> -ar <rate>
       -threads 2
       -map_metadata -1 -map_chapters -1
       -f matroska pipe:1
```

Measured on the appliance, DTS 5.1 beside 4K HEVC HDR10:

```
/usr/bin/ffmpeg -hide_banner -nostdin -loglevel error
  -i file:///var/tmp/ma1/ma1-dts.mkv
  -map 0:0 -map 0:1 -c:v copy -c:a ac3 -ac 6 -b:a 640000 -ar 48000
  -threads 2 -map_metadata -1 -map_chapters -1 -f matroska pipe:1
```

| | Source | Output |
| --- | --- | --- |
| Video | hevc Main 10, 3840×2080, yuv420p10le | **identical** |
| Colour | bt2020 / smpte2084 / bt2020nc | **identical** |
| Audio | dts 5.1(side), 1411 kbit/s | ac3, 6ch, 640 kbit/s, 48 kHz |

## The invariant, and how it is enforced

`assert_video_copy` inspects the finished argument vector and refuses to return
anything that could start a video encoder:

* `-c:v` / `-codec:v` / `-vcodec` with any value but `copy`
* a blanket `-c` / `-codec`, which would set every stream's codec
* `-vf`, `-filter:v`, `-filter_complex`, `-lavfi` — a filter chain is a
  decode/encode pipeline by definition
* a command that never states that video is copied at all

Every construction path goes through it, so there is no way to build a session
command that encodes video. Tests assert each refusal
(`media/tests/test_session.py`).

## Choosing the parameters

Channels are `min(source channels, 6)`; AC-3 carries at most 5.1 and the sink's
ELD declares 6 channels at 640 kbit/s.

| Channels | Bitrate |
| --- | --- |
| 1 | 192 kbit/s |
| 2 | 256 kbit/s |
| 3 | 384 kbit/s |
| 4–5 | 448 kbit/s |
| 6 | 640 kbit/s |

Sample rate is kept when AC-3 defines it (32 / 44.1 / 48 kHz) and resampled to
48 kHz otherwise.

## Track mapping

Mapping is by **absolute stream index from the decision**, not `a:0`. A release
with a subtitle at index 1 and a commentary track at index 2 maps `0:0` and
`0:3`, so the encoder works on the track the policy chose. Tested.

## It is an argument vector

The source URL comes from a third-party addon. It is passed to `execve` as one
argument and never assembled into a shell string:

```python
argv[argv.index("-i") + 1] == "https://host/a.mkv?x=`id`;rm -rf /&y=$(whoami)"
```

## Protocol options follow the protocol

ffmpeg rejects the whole invocation when handed an option the input protocol
does not define — `-user_agent` on a `file:` URL is an error, not a no-op — so
these are chosen from the scheme rather than always sent. This was found by
running the pipeline, not by reading about it.

## Cost on the appliance

One ffmpeg, one core briefly, measured over 40 MB of output while the session
was read:

```
ffmpeg processes  1 1 1 1 1 1 1 1 1 1 1 1
%cpu              77.0 37.7 25.1 18.9 15.1 12.6 10.8 9.4 8.4 7.5 6.8 6.2
load1             1.32 … 1.35
```

The opening burst is the muxer finding its way into a 4K stream; the steady
state is an AC-3 encoder and a byte copy. After `stop()`:

```
orphan ffmpeg: 0
orphan ffprobe: 0
```

There is no video decode and no video encode in that tree, which is the whole
claim: `-c:v copy`.

## Remux

The same module builds a remux — `-c:v copy -c:a copy` — for a container the
player cannot open directly. It is the same invariant with one more stream
copied, and it goes through the same assertion.
