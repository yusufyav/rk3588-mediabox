# V2 medya çekirdeği — headless Stremio, medya politikası, HDR/DV ve AC-3 hattı (2026-09-12)

Başlangıç HEAD: `f3dd8f317438b41e49183a7ea497eb905be412cd`
Çalışma yeri: `/tmp/mediabox-claude` (detached HEAD). Yeni branch yok, yeni
worktree yok, push yok.

---

## 1. Düzeltilen temel hata

M2, medya deneyimini upstream Stremio web uygulamasının üzerine kurdu ve
MediaBox shell'ini **aynı dokümana** ekledi. Çalışıyor, ama bedeli şu: cihazın
davranışı başkasının sayfasının bir özelliği hâline geliyor. Playhead okumak
`window.core` okumak demek; transcode kararını televizyonun değil tarayıcının
kodek listesi veriyor.

V2 medya çekirdeği Stremio'nun yalnız **veri düzlemini** alır:

```
hesap API'si (api.strem.io)   → oturum, kurulu addon koleksiyonu, kütüphane
addon protokolü               → katalog, metadata, stream, altyazı
streaming server (11470)      → torrent → oynatılabilir HTTP
```

Sayfa yok, upstream JavaScript yok, DOM yok. `media/` paketinde
`querySelector`, `innerHTML`, `window.core`, `stremio-web`, `/hlsv2/`, `/ui/`
gibi tek bir iz bulunmaz ve bu **testle zorlanır**
(`media/tests/test_architecture.py`).

Mevcut legacy UI yolu **sökülmedi**. `mediaboxd`'nin Stremio proxy'si, cast
hedefi ve Kodi devri olduğu gibi duruyor; V2 çekirdeği onlardan bağımsız,
`/media/` altında ayrı bir yüzey. Codex'in alanına (rust/, CEC, input,
mediaboxctl, Kodi lifecycle) dokunulmadı.

---

## 2. Yeni headless mimari

```
                 ┌──────────────────────────────────────────┐
  gelecekteki UI │  GET /media/status  /home  /search  …     │
  ───────────────▶  POST /media/plan  /rank  /session        │
                 └───────────────┬──────────────────────────┘
                                 │  media/api.py
        ┌────────────────────────┼────────────────────────┐
        ▼                        ▼                        ▼
  media/stremio            media/inspector           media/policy
  headless adapter         ffprobe → MediaInfo       capability profile
  API + addon + server     codec/renk/HDR/DV         Direct / AC-3 / Risky
        │                        │                        │
        └────────────────────────┴───────────┬────────────┘
                                             ▼
                                      media/proxy
                                      session + ffmpeg
                                      VIDEO COPY · AUDIO AC-3
```

Dizin:

```
media/
  stremio/    api.py  addons.py  server.py  adapter.py  models.py
  inspector/  ffprobe.py  parse.py  model.py
  policy/     capabilities.py  video.py  audio.py  decide.py  ranking.py
              preview.py  reasons.py
  proxy/      ffmpeg.py  session.py  security.py
  tools/      cli.py                      → bin/media-core
  tests/      206 test
  api.py  server.py  http.py  errors.py
```

`media/` paketi **hiçbir üçüncü taraf bağımlılığı** kullanmaz ve
`mediaboxd`/`webui`/`rust`'tan hiçbir şey import etmez; ikisi de testle
zorlanır.

---

## 3. Stremio revizyonları

Revizyon **değiştirilmedi**. `packaging/upstream.env` içindeki M2 pinleri
aynen geçerlidir:

| Bileşen | Pin |
| --- | --- |
| `Stremio/stremio-web` | `509023270583077538caed04ffde0686cfa19bd9` (5.0.0-beta.39) |
| `Stremio/stremio-linux-shell` | `c6e7cd22e23ed6401e573fe7fe1a023fc07399a2` (v1.2.0) |
| `server.js` | sha256 `82175d79…`, 6.676.491 bayt |
| Node (arm64) | v22.23.1 |

Yeni medya çekirdeği bu artefaktlardan **yalnız streaming server'ı** kullanır
(`serverVersion 4.21.0`, hedeften okundu) ve onu da sadece torrent→HTTP
çözümü için. stremio-web hiç kullanılmaz.

Veri düzlemi için pin gerekmez: addon protokolü ve hesap API'si sürümlü HTTP
sözleşmeleridir, derlenen bir artefakt değil.

---

## 4. Adapter API

```python
session_status()                        catalog(type, id, addon_id=, extra=)
addons()                                search(query, types=)
home(types=)                            meta(type, id)
streams(type, id, video_id=None)        resolve(stream)
subtitles(type, id, video_id=, extra=)  library()
```

Dizi bölümü `{seriesId}:{season}:{episode}` video id'si ile istenir
(`video_id_for("tt10466872", 1, 1)`); bu şekli repoda bilen tek yer adapter'dır.

MediaBox seviyesinde HTTP yüzeyi:

```
GET    /media/status                  POST /media/resolve
GET    /media/capabilities            POST /media/inspect
GET    /media/home                    POST /media/plan
GET    /media/search?q=               POST /media/rank
GET    /media/catalog/{type}/{id}     POST /media/session
GET    /media/meta/{type}/{id}        GET  /media/session[/{id}][/status]
GET    /media/streams/{type}/{id}     DELETE /media/session/{id}
GET    /media/subtitles/{type}/{id}   POST /media/login | /media/logout
```

**Hiçbir route adı sağlayıcıyı anmaz** — test bunu zorlar.

Oturum açmak isteğe bağlıdır: `authKey: null` Stremio'nun varsayılan addon
koleksiyonunu döner, cihaz hesapsız gerçek kataloglarla çalışır. Login yalnız
auth key'i `0600` ile saklar, parolayı **asla**.

Addon başarısızlığı izole edilir: düşen bir addon yalnız kendi sonuçlarını
kaybettirir.

---

## 5. Media inspector

Her kaynak için normalize `MediaInfo`: container (format, süre, bitrate,
boyut), video (codec, profile, level, geometri, fps, pixel format, bit
derinliği, chroma, renk aralığı/matrisi/primaries/transfer, mastering display,
MaxCLL/MaxFALL, HDR tipi, DV profile/level/compat id, stream index), audio (her
track: codec/profile, kanal/layout, örnekleme, bitrate, dil, default/forced,
stream index, **yalnız güvenilir tespit edilirse** Atmos/JOC), subtitle (codec,
dil, forced/default).

ffprobe **ürün seviyesinde** çağrılır:

- argument vector — **shell string asla**
- duvar saati deadline + process group üzerinden SIGTERM→SIGKILL eskalasyonu
- stdout/stderr ayrı thread'lerde, tavanlı, eşzamanlı boşaltılır (pipe
  deadlock'u imkânsız)
- `-probesize 24M -analyzeduration 15s`, frame taraması `%+#2` — uzak kaynak
  **indirilmez**, range isteğiyle okunur
- her yolda `wait()` — zombie bırakılmaz
- bozuk/JSON olmayan/boş çıktı, sıfır olmayan exit, eksik stream: hepsi ayrı
  hata

Frame taraması **best effort**: mastering display, content light ve HDR10+
frame side data'sındadır; ikinci range isteğini reddeden kaynak yine tarif
edilir, eksiklik `warnings`'e yazılır.

12 GB'lık kanonik HDR varlığı hedefte **0,696 saniyede** tarandı.

### Renk metadata'sında tahmin yok

- `smpte2084` → HDR10 (HDR10+ frame metadata'sı varsa HDR10+)
- `arib-std-b67` → HLG
- transfer yok ama ST 2086 var → **`Unknown`** + gerekçe (ST 2086 kanıttır ama
  sinyallenmiş transfer değildir; hiçbir şey söylenmemiş bir ekran hiçbir şeye
  geçmez)
- BT.2020 primaries + BT.709 transfer → **`Unknown`**, "çelişkili"
- 8-bit üzerinde PQ, PQ'suz HDR10+ → çelişki notu

`Unknown` ile `SDR` bilinçli olarak farklıdır ve farklı kararlara gider.

Atmos yalnız codec profile gerçekten söylüyorsa iddia edilir
(`Dolby Digital Plus + Dolby Atmos`, `Dolby TrueHD + Dolby Atmos`,
`DTS-HD MA + DTS:X`). 7.1 kanal düzeni kanıt değildir; dosya adı hiç değildir.
Belirlenemeyen durum `None`'dır — `False` değil.

---

## 6. Capability profile

Tek profil: **`rk3588_orangepi5_production`**
(`media/policy/capabilities.py`). Kodun her yerine dağılmış `if codec == …`
blokları yoktur ve bu testle zorlanır.

| | |
| --- | --- |
| Video codec | hevc, h264, vp9, av1, mpeg2video, vc1, mpeg4 |
| HEVC profile | Main, Main 10 (range extension **yok**) |
| H.264 profile | Baseline/Main/High — 8-bit 4:2:0; **High 10 yok** |
| Bit derinliği | 8, 10 (**12 yok**) |
| Chroma | **yalnız 4:2:0** |
| Geometri | ≤ 3840×2160, ≤ 60 fps |
| HDR | SDR, HDR10, HDR10+, HLG |
| **Dolby Vision** | **doğrulanmış pipeline YOK** |
| Audio passthrough | ac3, eac3 |
| Audio decode → PCM | aac, mp3, opus, vorbis, flac, alac, pcm… |
| **Maks PCM kanal** | **2** — sink 6 kanal LPCM'de sessiz |
| Transcode hedefi | ac3, ≤ 6 kanal |

Her iddia kanıt dosyasını adıyla taşır ve "kanıtsız profil" testle reddedilir:

- `results/orangepi5-ultra-vendor/real-hdr10-playback-mp1b-2026-09-09.md`
- `results/orangepi5-ultra-vendor/ma1-hdmi-passthrough-2026-09-11.md`
- `results/mediabox-platform/m2-regression-fix-cpu-input-2026-09-11.md`

---

## 7. HDR / DV / renk politikası ve yeşil-mor problemi

### Analiz

Yeşil/mor açılma için en güçlü aday Dolby Vision Profile 5'tir ve **varsayımla
değil metadata ile** karar verilir. P5'in base layer'ı IPTPQc2'dir; cross-talk
matrisini yalnız bir DV decoder geri alır. RPU'suz düz bir HEVC Main 10 decoder
kanalları yanlış yerlere koyar — bildirilen semptomun tam karşılığı.

### Uygulanan politika

| Durum | Karar | Reason |
| --- | --- | --- |
| DV **P5** | **asla Direct değil** | `DV_PROFILE5_NO_BASE_LAYER`, `SAFER_ALTERNATIVE_EXPECTED` |
| DV **P7** | Direct, HDR10 olarak | `DV_PROFILE7_BASE_LAYER_HDR10`, `DV_DYNAMIC_METADATA_LOST` |
| DV **P8** compat=1 | Direct, HDR10 | `DV_BASE_LAYER_COMPATIBLE` |
| DV **P8** compat=2 | Direct, SDR | `DV_BASE_LAYER_COMPATIBLE` |
| DV **P8** compat=4 | Direct, HLG | `DV_BASE_LAYER_COMPATIBLE` |
| DV **P8** compat=0 | Risky | uyumlu base layer sinyali yok |
| DV compat=bilinmeyen | Risky | `DV_PROFILE_UNKNOWN` — **uydurulmaz** |
| DV profile okunamıyor (`dvhe` tag, record yok) | Risky | `DV_PROFILE_UNKNOWN` |

Belirleyici alan **profile numarası değil `dv_bl_signal_compatibility_id`**'dir;
P7 istisnadır çünkü base layer'ı profilin tanımı gereği HDR10'dur.

Hedefte doğrulandı (metadata fixture, §12'de gerekçesi):

```
DV P5            Risky        hdr=DolbyVision  mode=FallbackSourcePreferred
DV P7            Direct       hdr=HDR10        mode=DirectWithAudioTranscode
DV P8 compat=1   Direct       hdr=HDR10        mode=Direct
DV P8 compat=0   Risky        hdr=DolbyVision  mode=FallbackSourcePreferred
HDR10            Direct       hdr=HDR10        mode=Direct
```

DV P5 kaynağı **media session'a dönüşemez**: `POST /media/session` →
`409 SOURCE_NOT_PREFERRED`.

Bir gün bu donanımda DV pipeline doğrulanırsa, profildeki
`dolby_vision_pipeline = True` tek satırda hepsini değiştirir.

### Diğer sınıflandırmalar

H264 SDR, HEVC Main10, 8/10/12 bit, 4:2:0 / 4:2:2 / 4:4:4, BT.709, BT.2020,
limited/full range, HDR10, HDR10+, HLG, bilinmeyen/çelişkili metadata — hepsi
açıkça sınıflandırılır ve testlidir. 12-bit, 4:2:2 ve 4:4:4 `Unsupported`;
full range Direct + `VIDEO_COLOR_RANGE_FULL` uyarısı.

---

## 8. Source ranking

Çözünürlük **tek kriter değildir**. Tier sırası:

| Tier | |
| --- | --- |
| 1 `SAFE_DIRECT` | güvenli video + encoder gerektirmeyen ses |
| 2 `SAFE_AUDIO_TRANSCODE` | güvenli video + AC-3'e çevrilen ses |
| 3 `SAFE_SDR_ALTERNATIVE` | güvenli ama aynı başlığın HDR sürümü var |
| 4 `SAFE_REMUX` | güvenli ama container değişmeli |
| 5 `RISKY` | kullanılabilir base layer'ı olmayan DV, güvenilmez renk |
| 6 `UNSUPPORTED` | |

Tier içinde: piksel → HDR değeri → kanal → bitrate → kaynağın kendi kimliği.
Son terim sıralamayı **tam** yapar; aynı girdiyle iki koşu aynı listeyi verir
(test: ileri ve ters sırayla aynı sonuç).

Kanıt (hedef, gerçek varlıklar):

```
1. [SAFE_DIRECT]           Direct                    3840x2080 HDR10+ / Passthrough   ma1-eac3.mkv
2. [SAFE_DIRECT]           Direct                    3840x2080 HDR10+ / Passthrough   ma1-ac3.mkv
3. [SAFE_AUDIO_TRANSCODE]  DirectWithAudioTranscode  3840x2080 HDR10+ / TranscodeToAC3 ma1-dts.mkv
?. unprobed: …/testcard.mp4 (file url is outside the allowed directories)
```

Son satır ayrıca güvenlik politikasının çalıştığını gösterir: izinli dizin
dışındaki kaynak sessizce düşürülmez, **raporlanır**.

Ve kritik kural:

```
ranked (DV P5 4K vs HDR10 4K):
   SAFE_DIRECT   hdr10-4k
   RISKY         dv-p5-4k
```

SDR düşürmesi **kümenin** özelliğidir, tek başınayken SDR birinci sınıf
seçimdir.

---

## 9. Audio politikası ve AC-3 hattı

### Neden cihaz kendisi AC-3 üretiyor

Kodi'nin kendi AC-3 transcode'u bu sink'te **sessizlik** üretiyor (MA1 §5):
aynı cihaz, aynı 48 kHz `S16_LE` taşıyıcı, aynı `AES0=0x06` — gerçek bir AC-3
**dosyasının** frame'leri çalıyor, Kodi'nin encoder çıktısı çalmıyor. Bu yüzden
dönüşüm **Kodi'nin üstünde**, dosya frame'i olarak yapılır.

| Kaynak | Karar | Neden |
| --- | --- | --- |
| AC-3 | passthrough | MA1'de duyulur doğrulandı |
| E-AC-3 | passthrough | MA1'de duyulur doğrulandı |
| AAC/MP3/Opus/Vorbis ≤ 2ch | decode → PCM | sink kanal sınırı içinde |
| AAC > 2ch | → AC-3 | 6 kanal PCM burada sessiz; miks AC-3 olarak korunur |
| DTS | → AC-3 | ELD söylüyor, sink sessiz |
| DTS-HD / DTS:X | → AC-3 | ELD'de yok |
| TrueHD / TrueHD Atmos | → AC-3 | ELD'de yok |
| çok kanallı FLAC/PCM | → AC-3 | aynı 2 kanal PCM sınırı |
| bilinmeyen codec | → AC-3 + `AUDIO_CODEC_UNKNOWN` | **açık karar**, varsayım değil |

Kayıplar ima edilmez, yazılır: `AUDIO_OBJECT_METADATA_LOST`,
`AUDIO_LOSSLESS_TO_LOSSY`, `AUDIO_CHANNELS_REDUCED` (iki kanal sayısıyla).

### Track seçimi

Ucuzdan pahalıya: passthrough → decode → transcode, sonra kanal sayısı, sonra
default bayrağı, sonra stream index. İstenen dil hepsinden önce filtreler.

Aynı dosyada hem TrueHD Atmos hem native AC-3 varsa **AC-3 seçilir** ve
gereksiz transcode yapılmaz (`AUDIO_NATIVE_TRACK_PREFERRED`). Test, track
sırası ters çevrildiğinde de aynı seçimi zorlar.

### Hat

```
ffmpeg -hide_banner -nostdin -loglevel error
       [-ss <s>]                       # -i'den önce: seek eder, decode etmez
       [-user_agent … -reconnect …]    # yalnız http(s) girdilerde
       -i <source>
       -map 0:<video index> -map 0:<audio index>
       -c:v copy
       -c:a ac3 -ac <ch> -b:a <bitrate> -ar <rate>
       -threads 2 -map_metadata -1 -map_chapters -1
       -f matroska pipe:1
```

Bitrate kanal sayısından gelir (1→192k, 2→256k, 3→384k, 4-5→448k, **6→640k**);
örnekleme AC-3'ün tanımladığı bir değerse korunur, değilse 48 kHz.

Mapping **karardan gelen mutlak stream index** iledir, `a:0` değil. İndeks
1'de altyazı, 2'de yorum track'i olan bir sürümde `0:0` ve `0:3` map'lenir —
testli.

`-user_agent`/`-reconnect` yalnız `http(s)` girdiye eklenir: ffmpeg, protokolün
tanımlamadığı bir seçenek verildiğinde **tüm çağrıyı** reddeder. Bu, hattı
çalıştırarak bulundu.

---

## 10. Video-copy invariant

`assert_video_copy` bitmiş argument vector'ü denetler ve video encoder
başlatabilecek hiçbir şeyi geri vermez:

- `-c:v` / `-codec:v` / `-vcodec`, değeri `copy` dışında
- toplu `-c` / `-codec` (video dahil her stream'i etkiler)
- `-vf`, `-filter:v`, `-filter_complex`, `-lavfi` — filtre zinciri tanımı
  gereği decode/encode hattıdır
- videonun kopyalandığını **hiç söylemeyen** komut

Tüm inşa yolları buradan geçer; encode eden bir session komutu üretmenin yolu
yoktur. Her reddediş testlidir.

---

## 11. Preview politikası

Kodi oynatmasından **ayrı** hesaplanır:

| | |
| --- | --- |
| `BrowserDirect` | tarayıcı kaynağı doğrudan açar |
| `BrowserRemux` | stream'ler tarayıcının açtığı container'a kopyalanır — encoder yok |
| `Unsupported` | `PREVIEW_SOFTWARE_TRANSCODE_REFUSED` |

`allow_software_video_transcode = False` ve bu testle zorlanır.
`PREVIEW_UNSUPPORTED` normal bir cevaptır: "bunu televizyonda izle".

Kanonik HDR varlığında (hedef):

```
Preview: Unsupported
  x hevc is not decoded by the browser, and re-encoding it would be a software
    video transcode on a board with no hardware encoder
  x 10-bit video would have to be converted to 8-bit in software
  x HDR10+ would have to be tone mapped in software for a browser
    PREVIEW_UNSUPPORTED: still playable on the television
```

Aynı kaynak için Kodi kararı `Direct`'tir — iki karar gerçekten ayrıdır.

### Preview → Kodi devri için üretilen bilgi

Session yanıtı `handoff` taşır: `sourceIdentity`, `resolvedInput`,
`playbackUrl`, `kodiPlaybackUrl` (loopback'e yeniden yazılmış),
`selectedTracks`, `previewMode`, `kodiMode`, `resumeSeconds`, `reasons`.
**Kodi sürülmez** — Codex'in alanı; medya çekirdeği yalnız gerekeni söyler.

---

## 12. Media session lifecycle

Mod: `Direct` (process yok) · `DirectProxy` · `Remux` · `AudioTranscode`.

```
create   kaynak+mod başına tek session — ikinci istek birincisini döner
attach   ilk okumada child başlar; client sayılır
detach   generator'ın finally'sinde — client bitirse de ölse de
stop     idempotent; process group'a SIGTERM → SIGKILL → wait()
expire   TTL (45 s) boyunca okuyan yoksa → stop("idle")
reap     kendi kendine çıkan child → stop("child-exited")
shutdown her session durdurulur; daemon'dan sonra hiçbir şey kalmaz
```

- **Duplicate encoder açılamaz** — tek izleyici için iki encoder bu modülün
  var olma sebebidir.
- Her child `start_new_session=True` ile başlar; torun process testle
  öldürüldüğü doğrulanır.
- `stop()` `wait()` çağrılmadan dönmez → zombie yok. `SIGTERM`'i yok sayan
  child öldürülür (testli).
- Relay `read1` kullanır; `read` 64 KiB dolana kadar bekleyip canlı akışı
  geciktirirdi.

---

## 13. ffmpeg / ffprobe temizliği ve CPU ölçümleri

DTS 5.1 + 4K HEVC HDR10 kaynağında, 40 MB çıktı okunurken (hedef):

```
ffmpeg+ffprobe sayısı  1 1 1 1 1 1 1 1 1 1 1 1
%cpu                   77.0 37.7 25.1 18.9 15.1 12.6 10.8 9.4 8.4 7.5 6.8 6.2
load1                  1.32 1.32 1.32 1.32 1.30 1.30 1.30 1.30 1.30 1.35 1.35 1.35
-c:v değeri            copy
```

Açılıştaki sıçrama muxer'ın 4K stream'e girmesi; kararlı hâl bir AC-3 encoder
ve bir bayt kopyası. Process ağacında **video decoder/encoder yok**.

`stop()` sonrası:

```
after stop: state = stopped  exit = -9
orphan ffmpeg: 0
orphan ffprobe: 0
```

Tüm medya çekirdeği test koşusu ve tüm hedef kabul adımlarından sonra son
durum:

```
pgrep -a ffmpeg   → (boş)
pgrep -a ffprobe  → (boş)
/proc/loadavg     → 1.49 1.42 1.38
mediaboxd / stremio-server / kodi → active active active
```

M2'de görülen tek çekirdek %100 durumu **oluşmadı**. Önizleme için pahalı
software video transcode yolu ürün tarafından reddediliyor (§11).

---

## 14. Host testleri

| Suite | Sonuç |
| --- | --- |
| `tests/run-media-core-tests.sh` (medya çekirdeği) | **206/206 PASS** |
| `tests/run-mediaboxd-tests.sh` (mevcut + yeni mount testleri) | **88/88 PASS** (M2'de 70 idi) |

Kapsanan başlıklar: ffprobe parsing, malformed ffprobe, timeout + child
cleanup, torun process kill, stderr flood deadlock'u, H264 SDR, HEVC Main10
HDR10, HDR10+, HLG, DV P5 riski, DV P7/P8 politikası (her compat id),
bilinmeyen/çelişkili renk metadata'sı, AC3 passthrough, AAC direct, EAC3→AC3,
DTS→AC3, DTS-HD→AC3, TrueHD→AC3, Atmos kayıp metadata'sı, native AC3 track
tercihi, video-copy invariant (5 ayrı ihlal biçimi), source ranking
(determinizm dahil), preview eligibility, duplicate session engeli, TTL
cleanup, child kill/reap, geçersiz URL (magnet/data/gopher/ftp/javascript,
loopback, link-local, metadata servisi, path traversal, kontrol karakteri),
Stremio adapter parsing (manifest scope, extra encoding, stream sınıflandırma,
addon izolasyonu), ve **Stremio Web DOM bağımsızlığının mimari kanıtı**.

Mimari testler bilinçli olarak kabadır — kaynağı okur ve herhangi bir DOM
işareti, tarayıcı sürücüsü, upstream web uygulaması yüzeyi, control-plane
import'u veya üçüncü taraf bağımlılığında düşer. İnce bir test yanlışlıkla
geçilebilirdi; amaç tam tersi.

---

## 15. Target testleri

Hedef: Orange Pi 5 Ultra / RK3588, `10.27.27.25`.
Staging: `/var/tmp/mediabox-v2-media` — **yeni paket kurulmadı**, production
servisleri değiştirilmedi (`mediaboxd`, `stremio-server`, `kodi` boyunca
`active`). Python 3.13.5, ffmpeg/ffprobe 7.1.5-0+deb13u1 (dağıtım paketi).

### Suite

`./tests/run-media-core-tests.sh` → **206/206 PASS** (hedefte).

### A — Headless Stremio (UI açılmadan)

| Adım | Beklenen | Gerçekleşen |
| --- | --- | --- |
| session/status | gerçek oturum durumu | `authenticated False`, `addons 7`, `api reachable True`, `streaming server True (4.21.0)` |
| kurulu addon'lar | gerçek koleksiyon | Cinemeta, YouTube, WatchHub, Public Domain Movies, OpenSubtitles v3, OpenSubtitles, Local Files |
| gerçek katalog | gerçek başlıklar | `movie/top` → tt28014327 Mayday (2026), tt11561116 The Whisper Man (2026), … |
| gerçek arama | gerçek sonuçlar | "Dune" → tt1160419 Dune: Part One (2021), tt15239678 Part Two (2024), tt31378509 Part Three (2026), … + 2 dizi |
| gerçek metadata | dolu kayıt | Dune: Part One (2021), rating 8.0, 155 min, Action/Adventure/Drama, Chalamet/Ferguson/Zendaya |
| gerçek stream listesi | addon'lardan | tt1160419 → 8 external; tt0063350 → 1 external + **1 torrent (playable)** |
| dizi bölümü | video id ile | `tt10466872:1:1` → HBO Max (external) |
| **stream resolution** | oynatılabilir URL | `torrent:11ea0258…:0` → `http://127.0.0.1:11470/11ea0258…/0`, `via streaming-server:torrent` |

**Hiçbir adımda tarayıcı, sayfa veya DOM kullanılmadı.**

### B — Kanonik HDR kaynağı

`/var/tmp/mp1b/past-lives.mkv` (12.081.238.172 bayt) — **silinmedi,
değiştirilmedi**.

Inspector (0,696 s):

```
Container  matroska,webm  1:45:43  15236 kbit/s
Video      hevc Main 10  3840x2080  23.976 fps
           yuv420p10le  10-bit  4:2:0
           bt2020 / smpte2084 / bt2020nc  (limited range)
           HDR10+
           mastering display 0.005–4000.0 nits
           MaxCLL 401  MaxFALL 77
Audio      [1] eac3 5.1(side) eng 640 kbit/s (default)
Subtitles  [2] ass eng (default, forced)  [3] ass eng  [4] ass fre
```

Kayda değer bulgu: varlık **HDR10 değil HDR10+**'tır — SMPTE 2094-40 dinamik
metadata'sı frame side data'sında mevcut. Bu, ancak frame taraması yapan bir
inspector'ın görebileceği bir şey.

Policy:

```
Video      hevc Main 10  3840x2080  bt2020/smpte2084  HDR10+   DIRECT
Audio      eac3 5.1(side)  Passthrough
Playback   Direct · VIDEO COPY · AUDIO PASSTHROUGH
Reasons    CONTAINER_SUPPORTED / VIDEO_CODEC_SUPPORTED
           ~ HDR10_PLUS_BASE_LAYER_ONLY / AUDIO_PASSTHROUGH
```

### C — Audio-only transcode (gerçek kaynak)

`/var/tmp/ma1/ma1-dts.mkv` — 4K HEVC Main10 HDR10+ + DTS 5.1 1411 kbit/s.

Çalıştırılan komut:

```
/usr/bin/ffmpeg -hide_banner -nostdin -loglevel error
  -i file:///var/tmp/ma1/ma1-dts.mkv
  -map 0:0 -map 0:1 -c:v copy -c:a ac3 -ac 6 -b:a 640000 -ar 48000
  -threads 2 -map_metadata -1 -map_chapters -1 -f matroska pipe:1
```

| | Beklenen | Gerçekleşen (çıktının ffprobe'u) |
| --- | --- | --- |
| Video codec | kaynakla **aynı** | `hevc`, profile `Main 10` ✔ |
| Geometri | aynı | `3840x2080` ✔ |
| Pixel format | aynı | `yuv420p10le` ✔ |
| Renk | aynı | `bt2020` / `smpte2084` / `bt2020nc` ✔ |
| Kaynak ses | dts 5.1 | — |
| Çıktı ses | **AC-3** | `ac3` ✔ |
| Kanal | 6 | `channels=6` ✔ |
| Bitrate / rate | 640k / 48k | `bit_rate=640000`, `sample_rate=48000` ✔ |

Aynı videoya sahip `ma1-ac3.mkv` ise `Direct · AUDIO PASSTHROUGH` verir —
gereksiz transcode yapılmaz.

### D — Process / CPU

§13'teki ölçüm. Video encoder/decoder hattı **oluşmadı**, session kapanınca
child **kalmadı**, orphan ffmpeg/ffprobe **0**.

### E — Problemli kaynak

Cihazda veya repoda **gerçek bir yeşil/mor kaynak bulunamadı** ve **kaynak
uydurulmadı**. Politika bunun yerine, ffprobe'un o kaynaklar için ürettiği
tam JSON şeklinde yazılmış metadata fixture'larıyla test edildi (DV P5, P7, P8
her compat id ile, HDR10+, çelişkili renk). Sonuçlar §7'de.

Ses tarafında gerçek problem kaynağı **vardır** ve kullanılmıştır: DTS
(`ma1-dts.mkv`) bu sink'te sessizdir (MA1) ve yeni hat onu AC-3'e çevirir.

---

## 16. Torrent / magnet — ağ sınırı (güncel kanıt)

Kodi'ye **hiçbir koşulda** magnet veya infoHash verilmez; akış
`Stremio streaming server → oynatılabilir HTTP → media policy → Kodi`'dir ve
güvenlik testleri magnet'i kaynak olarak reddeder.

M2 §21'de "giden BitTorrent bağlantısı hiç denenmiyor (`connectionTries: 0`)"
denmişti. **Bu artık doğru değil, ama akış hâlâ kurulmuyor.** Gerçek torrent
(`11ea0258…`, Night of the Living Dead 1968, public domain), 205 saniye:

| t | peers | connectionTries | swarmConnections | downloaded | downloadSpeed |
| --- | --- | --- | --- | --- | --- |
| +45 s | 13 | 228 | 13 | 0 | 0 |
| +125 s | 2 | 229 | 2 | 0 | 0 |
| +165 s | 13 | 240 | 13 | 0 | 0 |
| +205 s | 13 | 240 | 13 | **0** | **0** |

Metadata **geldi** (torrent adı ve dosya listesi çözüldü:
`Night.Of.The.Living.Dead.1968.1080p.BluRay.x264-[YTS.AM].mp4`, 1.625.672.967
bayt) ve HTTP uç noktası `206 Partial Content` ile doğru `Content-Range`
veriyor — ama 205 saniyede **tek bayt piece indirilmedi**.

Yani: peer bağlantısı ve metadata değişimi çalışıyor, piece aktarımı
çalışmıyor. Bu bir ürün kusuru değil **ağ koşuludur**; brief §18 uyarınca
saatler harcanmadı, kanıt kaydedildi. Torrent kaynağının uçtan uca oynatılması
bu ağda **doğrulanamaz**.

---

## 17. Güvenlik

| | |
| --- | --- |
| Şema | yalnız `http`/`https`; `file:` açık dizin allowlist'i ile, **varsayılan kapalı** |
| Magnet/torrent | kaynak olarak reddedilir |
| Loopback | yalnız yapılandırılmış streaming server |
| Link-local, metadata servisi (169.254.169.254) | reddedilir |
| URL'de kontrol karakteri | reddedilir |
| `file:` path traversal | reddedilir |
| Session id | `^[0-9a-f]{32}$`, her aramadan önce doğrulanır |
| Shell | **hiç kullanılmaz** — yalnız argument vector |
| CORS | hiçbir `Access-Control-*` başlığı üretilmez |
| Yanıt başlıkları | `nosniff`, `no-store` |
| Secret | parola saklanmaz; auth key `0600` |

Medya proxy'si open proxy değildir: hedef isteğin kendisinden değil karardan
gelir ve her session için yeniden doğrulanır.

---

## 18. mediaboxd entegrasyonu

`/media/` mediaboxd'de **rezerve edildi** (`RESERVED_PREFIXES`), çünkü kök
mount'taki Stremio proxy'si rezerve edilmemiş her yolu alır. Medya çekirdeği
streaming server değildir ve biri için gelen istek diğerine iletilemez.

Medya çekirdeği **import edilir, bağımlılık değildir**: onsuz deploy edilmiş
bir control plane yine başlar ve `/media/` için 404 döner. `[media] enabled =
false` bilinçli olarak kapatır.

`config/mediaboxd.example.toml` yeni `[media]` tablosunu belgeler.

Yeni entegrasyon testleri (`tests/test_media_core_mount.py`, 18 test): mount
sınırı, proxy'nin `/media/`'yı yutmaması, `/mediafoo`'nun medya çekirdeği
olmaması, streaming server adresinin devredilmesi, Kodi'ye loopback URL,
kapatılabilirlik, health raporu, config doğrulama.

---

## 19. Bilinen sınırlar

**HIGH — Torrent piece aktarımı bu ağda kurulmuyor.** §16. Peer bağlantısı ve
metadata çalışıyor, veri akışı yok. Giden BitTorrent'e izin veren bir ağda
tekrar denenmeli.

**MEDIUM — Gerçek bir yeşil/mor kaynak bulunamadı.** DV politikası metadata
fixture'larıyla doğrulandı (§15-E). Eline gerçek bir DV P5 dosyası geçen ilk
seferde `media-core inspect` + `media-core policy` ile doğrulanmalıdır.

**MEDIUM — Transcode edilen session'da seek yok.** Çıktı canlı bir muxer'dan
gelir; `Content-Length` yoktur ve `Accept-Ranges: none` gönderilir. Baştan
başlamayan oynatma için session `startSeconds` ile kurulur (ffmpeg girdide
seek eder), ama oynatma sırasında seek edilemez. AC-3 gerektiren kaynaklar için
geçerlidir; `Direct` kaynaklar (çoğunluk) etkilenmez.

**LOW — E-AC-3 passthrough profil değeri MA1'e dayanır.** Bu sink E-AC-3'ü
çalıyor, dolayısıyla profil onu passthrough sayar ve EAC3→AC3 yolu bu profilde
tetiklenmez. Yol mevcuttur ve E-AC-3 passthrough'u olmayan bir profille
testlidir; farklı bir TV takıldığında profil değeri güncellenmelidir.

**LOW — Kütüphane (library) yalnız oturum açıkken.** Hesapsız kullanımda
`datastoreGet` çağrılamaz; `library()` boş liste döner.

---

## 20. Commit'ler

`f3dd8f3`'ten bu yana, detached HEAD üzerinde, sırayla:

| # | SHA | Başlık |
| --- | --- | --- |
| 1 | `0a102fae0e4a96b3b2616c842ff7e5b73d498424` | feat: add a headless Stremio adapter |
| 2 | `c982d4d7dbd9e2ab6123c9606ef18592d058c9f2` | feat: add the media inspector |
| 3 | `4668740f1bbd8ddd7add6c3ec88cec7fa8b9dacd` | feat: add the media capability policy |
| 4 | `a0b3c43fc15cfd63e217290688d9c892aadd0f7f` | feat: add audio-to-AC3 media sessions |
| 5 | `c86a4dcca56d8e0983c5d6ac8e948dc0684e67ae` | feat: expose the media core over HTTP and from a terminal |
| 6 | `3a68457f7b2c34545cceed5a6c482e4820df3b4d` | feat(mediaboxd): mount the media core at /media/ |
| 7 | `843fe0b1675c671c7c3aecb71d83049ed437decc` | test: cover the media core |
| 8 | `de637310abad86f107ff9a7693c5b343e090a910` | docs: document the MediaBox V2 media core |

Bu rapor 9. commit'tir.

Push yapılmadı, branch açılmadı, worktree temiz.

---

## 21. Dokümanlar

- [`docs/stremio-headless.md`](../../docs/stremio-headless.md)
- [`docs/media-policy.md`](../../docs/media-policy.md)
- [`docs/audio-transcode.md`](../../docs/audio-transcode.md)
- [`docs/media-session-proxy.md`](../../docs/media-session-proxy.md)
