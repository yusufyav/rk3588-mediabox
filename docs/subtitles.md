# Subtitles

Embedded, stream-bound and addon subtitles in one menu, fetched through the
worker, checked against the film's own timeline and, when they belong to it,
timed automatically by one constant offset.

```text
  mediabox-tv            mediaboxd-rs                 media worker                  mpv
  ───────────            ────────────                 ────────────                  ───
  subtitle panel ──────▶ subtitles.rs ──prepare────▶ service.py ──addons (HTTPS)
  (one list,             (unified list,  ──load─────▶  fetch, decode, keep
   by language)           preferences,                  files/<key>.srt ◀─ GET ── sub-add
  settings panel:         selection,     ──sync─────▶  mkv.py (index: rate, embedded
   "Otomatik eşitleme"    AutoSync on/off)               tracks' events; bounded reads)
                                                        eligibility.py (pre-filter)
                                                          │ ACCEPT: its offset
                                                          ▼  (no embedded track:
                                                        audio.py + sync.py)
  caption line ◀──text── sub-text ◀─────────────────── sub-delay ──────────────────▶
  (drawn by the UI)
```

## Why the interface draws the line

`packaging/mpv/vo_mediabox.c` hands the decoder's DMA-BUFs to the interface and
draws nothing, OSD and subtitles included (`control` answers `VO_NOTIMPL`).
Selecting a subtitle track before this change put nothing on the television.
The interface now asks the control plane for mpv's `sub-text` ten times a
second while a subtitle is on (`MediaSubtitleTextHere`) and draws it on its own
plane over the film. Timing therefore stays mpv's: `sub-delay` is applied
before `sub-text` is read.

What this path does not do: bitmap subtitles (PGS, VobSub) have no text and are
not drawn; ASS styling (fonts, positions, karaoke) is not drawn — the ASS file
is kept intact and mpv reads it, but the line on screen is plain. Drawing
libass bitmaps would need a new message on the video socket and an overlay
plane; see "Limitations".

## Sources

| Source | Where it comes from | Id in the menu |
| --- | --- | --- |
| `embedded` | mpv `track-list`, `external: false` | `emb:<mpv id>` |
| `stream_external` | Stremio `stream.subtitles` — kept by `parse_stream()` and carried back on the descriptor | `ext:<hash>` |
| `addon_external` | `/subtitles/{type}/{id}/videoHash=…&videoSize=…&filename=….json` | `ext:<hash>` |
| `local` | an external file mpv already had | `emb:<mpv id>` |

The addon request carries what is known of the file and nothing invented:
`videoHash`/`videoSize` from `behaviorHints`, from the streaming server's
`/opensubHash` for a torrent, or computed (OpenSubtitles hash, two 64 KiB range
reads) for an HTTP or local file; `filename` from `behaviorHints` or the URL.

Ranking: the stream's own first; then the viewer's languages in order; within a
language an addon-declared hash match, then a label sharing words with the
file name, then the addon's order. Three per language are offered.

## Delivery

The worker fetches (bounded: 8 MiB, source policy applied), undoes gzip,
decodes (BOM, UTF-8, else the language's legacy code page — cp1254 for
Turkish), checks it parses as SRT/WebVTT/ASS/SSA, and keeps it as UTF-8 under
`/var/lib/mediabox-media-worker/subtitles/files/<sha256>.<ext>` (64 MiB,
oldest first). mpv is given `http://127.0.0.1:8790/media/subtitles/file/<key>.<ext>`
with `sub-add … auto <title> <lang>`; it appears in `track-list` and is selected
with `sid`, without restarting the film.

## Eligibility: the pre-filter (`media/subtitles/eligibility.py`)

AutoSync is not a system that tries to rescue every subtitle. It corrects one
thing -- a constant offset between a subtitle made for this video's timeline
and the video -- and everything else is refused before anything is timed:

```text
external subtitle selected (automatically, or by the viewer)
    ↓  REJECT_PARTIAL           CD1/Part 1, a trailer's lines, half a film
    ↓  REJECT_WRONG_RELEASE     a step in the offset, lines outside the video,
    ↓                           broken timings -- whatever ratio also fits
    ↓  REJECT_TIMEBASE_MISMATCH another canonical timebase, clearly and stably
    ↓  INCONCLUSIVE             nothing anchored to the video explains it
    ↓  ACCEPT_TIMELINE_COMPATIBLE
    ↓
the reference's own offset → mpv sub-delay
(no embedded track: the sound decides, offset only)
```

`REJECT_DUPLICATE` is the control plane's: a candidate that loads to the same
file (content hash) as one already tried in this film is not sent again.
The answers are one enum in the worker and one in the control plane
(`Eligibility`), with the same strings on the wire and in the log.

**The model** is always `t_video = ratio · t_subtitle + offset`, with
`ratio = F_sub_equivalent / F_video`. `F_video` is the video's measured rate
and is fixed; `F_sub_equivalent` is a *canonical timebase equivalent* --
24000/1001, 24, 25, 30000/1001, 30 -- within 10 % of it. A subtitle file has
no frame rate; "25/24" only says its timeline follows that conversion. For a
24.000 video the hypotheses are 1, 23.976/24 and 25/24; for 23.976, 1,
24/23.976 and 25/23.976. No pool of every rate pair is tried. A subtitle on a
timebase other than 1 is refused, never scaled back: a false correction is
worse than none.

**The reference** is an embedded subtitle track of the same file, in any
language, text or picture (PGS, VobSub): when its events are -- a text
track's lines, a picture track's show and clear packets -- is when somebody
speaks in *this* file's timeline. Nothing is matched cue to cue (another
language splits and merges lines, and moves edges by a few frames); the two
activity patterns are compared. No bitmap is decoded, no OCR. The language
of the reference does not matter and has nothing to do with the preferred
subtitle language: on *Drive* the right Turkish subtitle came out at
−18.2…−18.7 s against each of thirteen dialogue tracks in thirteen languages.

Only dialogue tracks are references: a track flagged forced, or named as a
commentary, forced, signs or songs (`mkv.is_dialogue`), is not -- on *Drive*
the busiest track was "Japanese (Commentary #2)", and against it no subtitle
matched at all. Of the rest, busiest first, the first that a second track
confirms is used (`confirmed`); failing that, the first nothing contradicts
(`single`); a track another one contradicts is passed over (one of Drive's,
German, sat 24 s from all the others). Up to three are tried
(`REFERENCE_TRIES`).

**How it decides**, all numbers in `Thresholds`:

1. *Partial*, never from one number: an early last line is normal where the
   credits roll. It takes the subtitle stopping without thinning out while at
   least a quarter of the reference's events are still to come and its
   density carries on; or a "CD1"/"Part 1"/"1of2" name with the coverage to
   match; or (the old rule) a last line before a quarter of the film.
2. For each hypothesis, the offset over ±150 s at which most events meet
   (±0.1 s, 20 ms grid), as a z-score above that search's own noise. Then
   twelve windows of the subtitle's own time, each matched on its own around
   that offset (±15 s).
3. *Structure*, for every hypothesis that matches strongly, before any ratio
   is believed: three or more lines outside the video once fitted; two or
   more reversed timings; the window offsets forming two plateaus 1.5 s or
   more apart that fit two constants at least twice as well as a line.
4. *Timebase*: the best hypothesis must reach z 8 and lead the next by 3; it
   must leave less than 1 s of drift across the windows and less than 0.6 s
   of scatter. Ratio 1 is ACCEPT, another canonical ratio is TIMEBASE
   MISMATCH, anything else is INCONCLUSIVE.

**The offset.** An ACCEPT against the reference carries its own offset, the
median of its windows, and that is what is applied: nothing is listened to.
Measured on *The Social Network*: the reference said −1.17 s in under a
second; the sound, asked afterwards, listened for 276 s, found −1.14 s and
then refused it for an "unexplained region", so nothing had been applied.

**Without a reference** (MP4, no embedded subtitle, an index that cannot be
read within budget) the sound answers the same question under the same
rules: the engine below tries only this video's canonical ratios; its offset
model at ratio 1 is ACCEPT, a linear model at a canonical ratio is TIMEBASE
MISMATCH, a piecewise model or an unexplained region is WRONG RELEASE, the
old trailer refusal is PARTIAL, and everything else INCONCLUSIVE.

**Reading a remote file.** `mkv.py` reads the Matroska head (EBML, SeekHead,
Info, Tracks: the video's `DefaultDuration`), then `Cues`, where mkvmerge
indexes every subtitle block and the video's keyframes: the rate is the
declared one only if the keyframes sit on its frame grid. Every read is a
range with a client-side cap -- the body is read up to what was asked and the
connection closed -- because one CDN answered `bytes=0-0` with `206`,
`Content-Range: bytes 0-0/29159331995` and `Content-Length: 29159331995`.
One video's index may cost at most 6 MiB (`INDEX_BUDGET`), one element at most
4 MiB; past either, or on a server that ignores ranges, the reference is
unavailable and the sound decides. Measured: a 29 GB remux, 1.3 MB; three UHD
remuxes of 11–87 GB, 1.4–1.8 MB each. No full demux, no PGS extraction, no
audio for this step.

**Kept, once per file.** The index reading is kept per video identity
(`reference/`, `REFERENCE_VERSION`), whichever subtitle asks; each answer per
(video identity, subtitle content hash, `SYNC_VERSION` = engine + pre-filter
+ reference version) in `sync/`. The same file offered twice under two addon ids has one
key: the second `load` says `duplicateOf`, a second sync in the same film is
the same job, and the next time the film is played the answer is read, not
worked out. The evaluation runs in the engine's child process
(`runner.py`), like alignment.

## The sound (`media/subtitles/sync.py`)

Run only for a video with no usable embedded track, as the pre-filter itself
(at this video's canonical ratios). Only its `offset` answer is ever applied:

| Model | Form | Applied as |
| --- | --- | --- |
| offset | `t + b` | mpv `sub-delay = b` |
| linear | `a·t + b` | nothing: a timebase mismatch (canonical `a`) or inconclusive |
| piecewise | `a·t + b_k` on subtitle ranges | nothing: a wrong release |
| rejected | — | nothing |

**Evidence.** Sampled 30 s windows of the film's audio, decoded by ffmpeg to
8 kHz mono in the speech band, and an energy detector with an adaptive floor
and hysteresis (`audio.py`). Language-independent; no ASR. A track whose probe
gives it a centre channel (5.1, 7.1, …) is heard from that channel alone
(`pan=mono|c0=FC`), where the dialogue is mixed; anything else is downmixed
(`-ac 1`). Stereo has no centre and taking one hears silence.

**Per window.** The Pearson correlation of the speech indicator and the shifted
cue indicator at every shift in ±150 s. Both are interval unions, so the
overlap as a function of the shift is a sum of trapezoids, accumulated exactly
on a 50 ms grid from their corners — O(pairs + grid), no numpy. Cue tails are
trimmed by 0.4 s (lines stay on screen after they are said; untrimmed this
pulls the answer 0.2 s early). The peak is refined on a 10 ms grid.

**Rate.** The ratios it is given -- 1 alone after an ACCEPT, this video's
canonical ratios otherwise -- are tried on an even sample of ≤16 windows; the
film's own rate is kept unless another scores ≥ 11 % better on all windows.

**Fitting.** Three explanations of the windows' best shifts: one constant
offset, one line (residual slope ≤ 5·10⁻⁴, and only if it explains two more
windows), or runs of constant offset found by dynamic programming (≤ 5
regions, each region needing significance ≥ 4 on its own).

**Confidence.** A window whose best shift is noise lands anywhere in the
±150 s search; `k` of `n` windows agreeing within ±0.35 s by chance has a
probability that is computed (`_significance`), with the model's freedom
(slope, rate choice, region boundaries) charged against it. Confidence is that
significance, scaled so about six agreeing windows read as certain, times the
share of strong windows the model explains. A subtitle for another film scores
≈0; its windows each have a best shift, but they do not agree.

**Refusals.** A subtitle whose last line comes before a quarter of the film
is refused before anything is listened to (`does-not-cover-film`):
OpenSubtitles files trailers' subtitles under the film's id. Beyond the
confidence threshold, an answer is not applied when:
two windows the model does not explain agree with each other with nothing it
explains between them (an unmodelled region); a strong window at either end
disagrees and nothing has been heard beyond it; or more than 20 % of the film
at either end, or between two agreeing windows with disagreeing ones in it,
has no agreeing window. Each of these asks for listening in that stretch
first; if the budget runs out it is a rejection.

**Staging** (`next_windows`): 16 windows across the film (8 if the addon said
the subtitle was hash-matched); then 8 more at a time while the answer is short
of confident; windows placed to settle contested stretches; bisection of a
piecewise boundary; never past 40. A torrent is only sampled behind the
playhead (data the engine already has), and the job returns `waiting` and is
asked again every minute as the film plays. An HTTP source is only listened to
if 12+ windows fit in a 2 GiB download budget (`--subtitle-sync-max-bytes`):
a 4K remux does not, and is left untimed rather than half-timed.

**Isolation.** Alignment runs in a child process (`runner.py`) at nice 10:
the worker also relays film bytes and must not wait on the interpreter lock.
ffmpeg runs at nice 10 with one thread. One analysis at a time.

**Cache.** Results by (video identity, subtitle hash, `SYNC_VERSION`); speech
windows by video identity and how they were heard (centre or downmix), so a
second subtitle for the same film costs no listening. Video identity is
the OpenSubtitles hash and size when known, else the torrent's info hash and
file index, else file name and size, else the URL without its query.

## Calibration

`python3 -m media.tests.subtitle_calibration 40` — forty synthetic 90-minute
films, each with five subtitles. The detector is worse than a clean recording:
15 % of lines missed, a false detection every 12 s (music, effects), ±0.2 s
edges; the subtitle has lead-ins, merged lines, 6 % untranslated lines and sign
captions. A correction counts as right when 90 % of lines land within 0.5 s.

Workstation run, thresholds as committed (`apply_threshold = 0.70`):

| Kind | Applied and right | Applied and wrong | Mean windows |
| --- | --- | --- | --- |
| offset (−7.3 … +41 s) | 37 / 40 | **0** | 20.5 |
| linear (25↔23.976, ±offset) | 35 / 40 | **0** | 20.7 |
| one cut (+45 s scene) | 24 / 40 | **0** | 26.0 |
| two cuts (−30 s, +20 s) | 8 / 40 | **0** | 24.7 |
| another film | 0 / 40 (all rejected) | **0** | 28.4 |

Everything not applied was left as it came. The apply threshold was swept:
0.6, 0.7 and 0.8 all gave zero wrong corrections; what keeps the wrong ones
out is the significance and the refusals above, not the threshold. With a
cleaner detector (5 % missed, one false detection a minute) two-cut films are
corrected 12 / 20 times, still with none wrong.

## Eligibility, measured (2026-09-30)

Four films, eight Turkish subtitles from OpenSubtitles, each compared with the
file's own embedded track read through its index (`t_video = ratio · t_sub +
offset`; drift is the regression slope of the window offsets at that ratio):

| Film (video rate, reference) | Subtitle | Result | Evidence |
| --- | --- | --- | --- |
| *The Social Network* UHD (23.976, English SRT) | 3935585 | ACCEPT | ratio 1 z 18.3 (next 5.8); offset −1.0…−1.4 s; drift −0.0006 ± 0.027 s/1000 s; window RMS 0.17 s |
| | 3921849 | WRONG RELEASE | best 24/23.976 z 11.2, but −81.7 s lead, 13 lines before the film, a +3.4 s step, three reversed timings |
| *In the Mood for Love* (24.000, French PGS) | 65664 | TIMEBASE MISMATCH | 23.976/24 z 12.9 (ratio 1: 7.4); residual drift +0.03 s/1000 s |
| | 67750 | INCONCLUSIVE | 25/24 z 8.7 but +0.96 s/1000 s left: best fit 1.0426, no F_sub/24 |
| *To Rome with Love* (24.000, Hungarian PGS) | 4801898 | TIMEBASE MISMATCH | 25/24 z 11.4 (ratio 1: 4.0); residual +0.06 ± 0.09 s/1000 s |
| | 4787827 | TIMEBASE MISMATCH | 23.976/24 z 11.2 (ratio 1: 4.4); residual +0.10 ± 0.08 s/1000 s |
| | 4789706 | PARTIAL | ends at 49 %; 1308 of 2776 reference packets after it; its density does not fall |
| *Pride & Prejudice* US cut (23.976, PGS SDH) | 3248655 | PARTIAL | ends at 49 % mid-conversation; ratio 1 at −7.7 s over its half |

Measured first with diagnostic scripts, then with this module as deployed,
on the board, against the same remote files: all eight came out as above
(3921849 on its reversed timings, the first structural check). The index
reads cost 257–704 kB and 2.4–3.2 s per film; one subtitle's evaluation 0.1–
0.4 s in the child process.

**Calibration needed.** Every threshold in `eligibility.Thresholds` (z 8,
margin 3, twelve windows, window z 5, 1 s drift, 0.6 s scatter, a 1.5 s step,
the partial shares) and `mkv.GRID_AGREEMENT` comes from these eight
subtitles and the synthetic films in `media/tests/test_subtitle_eligibility.py`.
They are starting values that lean towards INCONCLUSIVE, not measured
constants; a wider sample should set them.

## Measured on the board (Orange Pi 5 Plus, 2026-09-29)

| | |
| --- | --- |
| ffmpeg, 30 s of 5.1 AC-3 from a local MKV, nice 10 | 0.37–0.52 s per window, edges within 20 ms |
| one alignment round, 16 / 24 / 40 windows | 0.69 / 0.53 / 0.74 s (0.74 / 0.62 / 0.81 s in the child process) |
| a whole staged sync, synthetic, 6 films | 1.4–3.7 s CPU per subtitle |
| a real film: *The Shawshank Redemption*, 1080p x264 MP4 over Real-Debrid, OpenSubtitles Turkish | offset +13.84 s, confidence 0.92, 21 windows (12 agreeing), 117 s wall clock while the film played |
| the same, `mbsync-2` (AC-3 5.1, heard from the centre), 2026-09-30 | offset +13.76 s, confidence 0.85, 16 windows (11 agreeing), 57 s; ffmpeg 3.1–4.4 s wall and ~1.1 s CPU per window; a transcript puts the lines within −0.15…+0.5 s of the speech at 4, 70 and 125 min; the same film again: `cache=hit`, nothing heard |
| *Avengers: Endgame* (DTS 5.1) and *The Social Network* (DTS 5.1), OpenSubtitles Turkish | not applied: the right answer was the engine's best guess both times (scale 1.001 / −6.6 s; −2.0 s against a transcript's −2.1 s) but too few windows agreed (2 of 24; 5 of 31) — dense dialogue under music peaks within ±0.5 s but with little prominence |

## Behaviour

* **"Tercih edilen altyazı dili"** is the first row of the player's Ayarlar
  panel: "Kapalı" or a language (Ok or Right for the next, Left for the
  previous). It is kept in `/var/lib/mediabox/subtitles.json` and is the only
  thing that writes the preference; a subtitle picked from the panel during a
  film is that film's choice and changes nothing else. Changing it during a
  film puts it into effect on that film at once.
* **"Otomatik eşitleme"** is the second row of the Ayarlar panel, "Açık" or
  "Kapalı" (Ok, Left or Right turns it over), independent of the language.
  It is kept as `auto_sync_enabled` in `/var/lib/mediabox/subtitles.json`; a
  file written before it existed has no such field and reads as on, which is
  what those films had. Off: no external subtitle is checked or timed, and
  nothing is ever applied on its own. Turned on during a film, the external
  subtitle that is on is checked now.
* When a film has a subtitle in the preferred language, that language is the
  first language on the subtitle panel (under "Etkisizleştirildi") and is on
  when the film starts: the file's own text track in that language (full
  before forced, default first), else the best-ranked fetched one, else a
  picture track (listed as "Resim (gösterilemez)": this path draws text only).
  When it has none, the language is not listed and subtitles start off. A
  viewer who has never set it gets mpv's default and nothing is loaded.
* Long panels scroll: nine rows are drawn, the focus stays on screen, and a
  mark at the column's edge says there is more above or below.
* The viewer's choice during a film is final: nothing automatic replaces it.
  An external subtitle the viewer picks is checked (with AutoSync on) even
  when the film has an embedded track in the preferred language -- that
  track is then only the timing reference, and the viewer's pick stays on
  whatever the answer. An automatic choice that is refused is replaced by the
  next candidate of its language (at most three; a candidate that is the same
  file as one tried is skipped as REJECT_DUPLICATE), and if all are refused
  the best-ranked of those that are not partial stays on untimed.
* A delay moved by hand is kept; a timing that arrives afterwards waits
  ("Otomatik eşitleme hazır"), and Ok on the delay puts it on. Ok on the delay
  is the viewer asking in so many words: it works with "Otomatik eşitleme"
  off too, and a timing found then is put on.
* The panel shows, under the delay, `Otomatik eşitleme · +1,82 sn · %97`, and
  the same once in the pill when it is put on; for a refused subtitle,
  `Otomatik eşitleme kullanılamıyor`. Why it was refused, and every number of
  the pre-filter, stays in the control plane's list (`sync`: `eligibility`,
  `reference`, `ratio`, `estimated_offset`, `cached`, `reason`) and the log.

## Observability

One line per finished sync, from `media.subtitles` in the worker's journal:

```text
subtitle sync video=osh:…:… subtitle=<key> source=http eligibility=ACCEPT_TIMELINE_COMPATIBLE
reference=embedded-picture ratio=1 model=offset offset=+13.760 scale=1.000000 windows=16
inliers=11 confidence=0.85 decision=apply seconds=57.0 cache=miss reason=confident
```

Per-window evidence is logged at DEBUG only. The control plane logs how many
external subtitles a film had and when an automatic choice moves on.

## Limitations

* No ASS styling or bitmap subtitles on screen (see the first section). The
  engine and delivery handle ASS; only the drawing is plain.
* The caption is polled at 10 Hz: up to 0.1 s late on and off.
* Energy VAD: a film scored wall to wall with music gives little evidence and
  is left untimed rather than guessed.
* A subtitle for another cut or another timebase is not corrected at all: it
  is refused and left as it came. The engine can still model both (see
  Calibration); nothing it finds other than one offset is applied.
* The embedded reference is read from Matroska indexes only; an MP4's
  sample tables are not read, and such a film is decided by the sound.
* Without an embedded track the sound decides, and a source too heavy for
  the download budget cannot be listened to: such a subtitle is left untimed.
* When every candidate of the language is refused, the best-ranked one that
  is not partial stays on untimed -- a subtitle known to be on another
  timebase included. Whether it should rather stay off is not decided yet.
* High-bitrate HTTP sources (4K remux) exceed the download budget and are not
  analysed.
* Subtitles are not handed to Kodi on handover.
* An optional ASR/NPU fallback is not implemented. The place for it is a
  second evidence source for windows whose energy evidence is weak
  (`next_windows` stage after "needs-more-evidence"), limited to those windows.
