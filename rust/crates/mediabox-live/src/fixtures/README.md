# Sound fixtures for the GStreamer parser tests

Each file holds a few frames of one broadcast sound codec. The tests
(`src/sound_tests.rs`) send these frames as TS/PES, through the reader
(`es::Reader`), into the real parsers: mpegaudioparse, aacparse and
ac3parse.

The frames were made with FFmpeg n9.0.2 on 2026-11-02, from a 0.2 s
440 Hz sine at 48 kHz in stereo (`-bitexact`).

Shared input options:

```
SRC="-f lavfi -i sine=frequency=440:sample_rate=48000:duration=0.2 -ac 2 -bitexact"
```

| File | Command | Bytes | Frames | sha256 (first 16) |
|---|---|---|---|---|
| sound.mp2 | `ffmpeg $SRC -c:a mp2 -b:a 192k -f mp2 sound.mp2` | 5184 | 9 × 576 | d561f86e6ece1515 |
| sound.adts | `ffmpeg $SRC -c:a aac -b:a 128k -f adts sound.adts` | 3557 | 11 | 4707501797a7d704 |
| sound.latm | `ffmpeg $SRC -c:a aac -b:a 128k -f latm -smc-interval 1 sound.latm` | 3600 | 11, StreamMuxConfig in each | 2beb001881c4e265 |
| sound-smc4.latm | `ffmpeg $SRC -c:a aac -b:a 128k -f latm -smc-interval 4 sound-smc4.latm` | 3560 | 11, StreamMuxConfig in 0, 4, 8 | 8db49c49e352b27e |
| sound.ac3 | `ffmpeg $SRC -c:a ac3 -b:a 192k -f ac3 sound.ac3` | 5376 | 7 × 768 | 89deb67a4ae51d78 |
| sound.eac3 | `ffmpeg $SRC -c:a eac3 -b:a 192k -f eac3 sound.eac3` | 5376 | 7 × 768 | 3c1c87a4fe9caa70 |

DVB does not require a StreamMuxConfig in every LATM frame, only periodically
(ETSI TS 101 154 V2.10.1 6.5.1.1). `sound-smc4.latm` is such a stream:
aacparse 1.26.2 lost three of its frames, 1.26.8 and later give all 11.

For the engine's PMT tests (`src/engine_tests.rs`):

| File | Command | Bytes | Frames | sha256 (first 16) |
|---|---|---|---|---|
| video.h264 | `ffmpeg -f lavfi -i testsrc2=size=64x64:rate=25 -t 4 -c:v libx264 -preset veryfast -bf 0 -g 10 -keyint_min 10 -sc_threshold 0 -x264-params aud=1:scenecut=0:threads=1 -bitexact -f h264 video.h264` | 39678 | 100, key every 10 | 290f86bfd8682d97 |
| sound-4s.mp2 | `ffmpeg -f lavfi -i sine=frequency=440:sample_rate=48000:duration=4 -ac 2 -bitexact -c:a mp2 -b:a 192k -f mp2 sound-4s.mp2` | 96192 | 167 × 576 | 48557053328bf0ce |
| sound-4s.ac3 | `ffmpeg -f lavfi -i sine=frequency=880:sample_rate=48000:duration=4 -ac 2 -bitexact -c:a ac3 -b:a 192k -f ac3 sound-4s.ac3` | 96000 | 125 × 768 | b1ef1a3a27209c19 |

The frame counts come from `ffprobe -count_frames`. The frame positions come
from `ffprobe -show_packets`. The tests find the same frames from the codec
headers themselves.
