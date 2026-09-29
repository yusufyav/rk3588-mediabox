# Subtitles

Embedded, stream-bound and addon subtitles in one menu, fetched through the
worker, and timed automatically against the film's own sound.

```text
  mediabox-tv            mediaboxd-rs                 media worker                  mpv
  ───────────            ────────────                 ────────────                  ───
  subtitle panel ──────▶ subtitles.rs ──prepare────▶ service.py ──addons (HTTPS)
  (one list,             (unified list,  ──load─────▶  fetch, decode, keep
   by language)           preferences,                  files/<key>.srt ◀─ GET ── sub-add
                          selection)     ──sync─────▶  audio.py (ffmpeg, sparse)
  caption line ◀──text── sub-text ◀───────────────────  sync.py  (offset/linear/
  (drawn by the UI)       sub-delay/sub-speed ──────────────────  piecewise/reject)
```

## Why the interface draws the line

`packaging/mpv/vo_mediabox.c` hands the decoder's DMA-BUFs to the interface and
draws nothing, OSD and subtitles included (`control` answers `VO_NOTIMPL`).
Selecting a subtitle track before this change put nothing on the television.
The interface now asks the control plane for mpv's `sub-text` ten times a
second while a subtitle is on (`MediaSubtitleTextHere`) and draws it on its own
plane over the film. Timing therefore stays mpv's: `sub-delay`, `sub-speed` and
a corrected timeline are all applied before `sub-text` is read.

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

## The sync engine (`media/subtitles/sync.py`)

The question is `video = f(subtitle)`:

| Model | Form | Applied as |
| --- | --- | --- |
| offset | `t + b` | mpv `sub-delay = b` |
| linear | `a·t + b` (23.976/24/25 conversions and residual drift) | `sub-speed = a`, `sub-delay = b` (mpv: video = speed·sub + delay, measured on 0.41) |
| piecewise | `a·t + b_k` on subtitle ranges | a corrected file (original untouched), swapped in for the track |
| rejected | — | nothing |

**Evidence.** Sampled 30 s windows of the film's audio, decoded by ffmpeg to
8 kHz mono in the speech band, and an energy detector with an adaptive floor
and hysteresis (`audio.py`). Language-independent; no ASR.

**Per window.** The Pearson correlation of the speech indicator and the shifted
cue indicator at every shift in ±150 s. Both are interval unions, so the
overlap as a function of the shift is a sum of trapezoids, accumulated exactly
on a 50 ms grid from their corners — O(pairs + grid), no numpy. Cue tails are
trimmed by 0.4 s (lines stay on screen after they are said; untrimmed this
pulls the answer 0.2 s early). The peak is refined on a 10 ms grid.

**Rate.** Seven frame-rate ratios (1, 25/23.976, 24/23.976, 25/24 and their
inverses) are tried on an even sample of ≤16 windows; the film's own rate is
kept unless another scores ≥ 11 % better on all windows.

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

**Refusals.** Beyond the confidence threshold, an answer is not applied when:
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

**Cache.** Results by (video identity, subtitle hash, `ALGORITHM`); speech
windows by video identity, so a second subtitle for the same film costs no
listening; corrected files by (video, subtitle, algorithm). Video identity is
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

## Measured on the board (Orange Pi 5 Plus, 2026-09-29)

| | |
| --- | --- |
| ffmpeg, 30 s of 5.1 AC-3 from a local MKV, nice 10 | 0.37–0.52 s per window, edges within 20 ms |
| one alignment round, 16 / 24 / 40 windows | 0.69 / 0.53 / 0.74 s (0.74 / 0.62 / 0.81 s in the child process) |
| a whole staged sync, synthetic, 6 films | 1.4–3.7 s CPU per subtitle |
| a real film: *The Shawshank Redemption*, 1080p x264 MP4 over Real-Debrid, OpenSubtitles Turkish | offset +13.84 s, confidence 0.92, 21 windows (12 agreeing), 117 s wall clock while the film played |

## Behaviour

* **"Tercih edilen altyazı dili"** is the first row of the player's Ayarlar
  panel: "Kapalı" or a language (Ok or Right for the next, Left for the
  previous). It is kept in `/var/lib/mediabox/subtitles.json` and is the only
  thing that writes the preference; a subtitle picked from the panel during a
  film is that film's choice and changes nothing else. Changing it during a
  film puts it into effect on that film at once.
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
  An automatic choice whose timing is rejected is replaced by the next
  candidate of its language (at most three), and if all are rejected the
  best-ranked stays on untimed.
* A delay moved by hand is kept; a timing that arrives afterwards waits
  ("Otomatik eşitleme hazır"), and Ok on the delay puts it on.
* The panel shows, under the delay, `Otomatik eşitleme · +1,82 sn · %97` or
  `Otomatik eşitleme · sürüm zamanlaması düzeltildi`, and the same once in the
  pill when it is put on. Nothing else about the engine reaches the television.

## Observability

One line per finished sync, from `media.subtitles` in the worker's journal:

```text
subtitle sync video=osh:…:… subtitle=<key> source=torrent model=linear offset=+0.412
scale=1.042708 windows=22 inliers=14 confidence=0.93 decision=apply seconds=11.8
cache=miss reason=confident
```

Per-window evidence is logged at DEBUG only. The control plane logs how many
external subtitles a film had and when an automatic choice moves on.

## Limitations

* No ASS styling or bitmap subtitles on screen (see the first section). The
  engine and delivery handle ASS; only the drawing is plain.
* The caption is polled at 10 Hz: up to 0.1 s late on and off.
* Energy VAD: a film scored wall to wall with music gives little evidence and
  is left untimed rather than guessed.
* Two cuts or more are corrected only when the evidence is clean (see
  Calibration); otherwise the subtitle is left as it came.
* High-bitrate HTTP sources (4K remux) exceed the download budget and are not
  analysed.
* Subtitles are not handed to Kodi on handover.
* An optional ASR/NPU fallback is not implemented. The place for it is a
  second evidence source for windows whose energy evidence is weak
  (`next_windows` stage after "needs-more-evidence"), limited to those windows.
