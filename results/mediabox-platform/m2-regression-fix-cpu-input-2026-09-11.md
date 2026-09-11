# M2 regresyon düzeltmesi — CPU/ffmpeg ve Kodi fiziksel input (2026-09-11)

Başlangıç: `main` = `origin/main` = `fbe462c6c7d0fb9065d823fc20e79e489429ea3a`, tree temiz.
Doğrudan `main` üzerinde çalışıldı; yeni branch/worktree yok.

---

# REGRESYON 1 — gereksiz ffmpeg / CPU

## Kök neden

M2'de `stremio-server.service`'e `FFMPEG_BIN`/`FFPROBE_BIN` eklendi. Bu doğru bir
düzeltmeydi (onsuz sunucu probe edemiyor ve oynatılabilir medyada "video is not
supported" veriyordu), ama beraberinde ikinci bir problem getirdi:

**Hiçbir şey transcoding session'ını sonlandırmıyordu.**

Kapatılan, başka sayfaya geçilen veya TV'ye devredilen bir ön izlemenin transcoder'ı
çalışmaya devam ediyordu. Bu kartta bunun bedeli ağırdır: sunucu açılışta üç donanım
hızlandırma profilini de eliyor —

```
hls-converter - Some tests failed for hw accel profile: qsv-linux
hls-converter - Some tests failed for hw accel profile: nvenc-linux
hls-converter - Some tests failed for hw accel profile: vaapi-renderD128
```

— dolayısıyla sahipsiz kalan her session bir **software** encode'dur.

## Neden iki ffmpeg

Sunucu video ve ses için ayrı ffmpeg süreçleri kurar. Ölçülen video komut satırı
(uyumlu kaynak):

```
/usr/bin/ffmpeg -fflags +genpts -noaccurate_seek -seek_timestamp 1 -copyts
  -i http://127.0.0.1:8787/ui/mediabox/media/testcard.mp4
  -threads 6 -max_muxing_queue_size 2048 -ignore_unknown
  -map_metadata -1 -map_chapters -1 -map -0:d? -map -0:t?
  -map v:0 -c:v copy -force_key_frames:v source
  -map -0:a? -map -0:s?
  -movflags frag_keyframe+empty_moov+default_base_moof+delay_moov+dash
  -use_editlist 1 -f mp4 pipe:1
```

`-c:v copy` — yani **video encode edilmiyor**, stream-copy/remux yapılıyor; ölçülen
CPU %1,1. Kullanıcının gördüğü tek çekirdek %100 durumu, tarayıcının kodek desteği
olmayan bir kaynakta `-c:v`'nin gerçek bir encoder'a dönüştüğü durumdur. İki süreç +
sahipsiz kalan session birleşince yük kalıcı hale geliyordu.

Yani iki ayrı olgu vardı: (a) uyumsuz kaynakta software encode, (b) session'ın hiç
sonlanmaması. (b) düzeltildi; (a) için politika aşağıda.

## Before / after process tree

**Before** (ön izleme kapatıldıktan sonra bile):

```
systemd(1)─┬─node(975, stremio)─┬─ffmpeg(video)   ← sahipsiz, çalışmaya devam
           │                    └─ffmpeg(audio)   ← sahipsiz, çalışmaya devam
           └─python3(980, mediaboxd)───kodi-gbm   ← yanlış context (Regresyon 2)
```

**After**:

```
systemd(1)─┬─node(975, stremio)          ← ön izleme yokken child yok
           ├─python3(980, mediaboxd)
           └─kodi-gbm(kodi.service)      ← kendi unit'i
```

## Before / after CPU-load

| Aşama | ffmpeg | node CPU | load (1/5/15) |
| --- | --- | --- | --- |
| Before — genel durum | 2 | — | 1.30 / **2.46** / **2.43** |
| After — idle | **0** | 0.4% | 1.58 / 2.08 / 2.28 |
| After — ön izleme aktif | 1 | — | — |
| After — ön izleme durduruldu | **0** | — | — |
| After — devir sonrası | **0** | — | — |
| After — Kodi oynarken, 60 s ölçüm | **0** (6/6 örnek) | 0.3–0.4% | 1.92 / 2.03 / 2.24 |

60 saniyelik ölçüm sonunda en yüksek CPU: `python3 %7.5`, `kodi-gbm %7.0`.
**M2 kaynaklı tek çekirdek %100 durumu yok.**

## Preview stratejisi ve transcode/remux/direct-play kararı

Kararı stremio-core/stremio-video verir: tarayıcının bildirdiği `videoCodecs`/
`audioCodecs` listesine göre sunucu ya stream-copy (remux) yapar ya da encode eder.
MediaBox bu kararı değiştirmez; **session'ın ömrünü** yönetir.

Ölçülen yol uyumlu kaynakta remux'tur (`-c:v copy`), software video encode yoktur.

## Cleanup lifecycle

```
hlsv2/{id}/… okunuyor      → session kaydedilir (proxy görür)
ikinci session başlarsa    → birincisi upstream'de destroy edilir
ön izlemeden çıkılırsa     → shell POST /api/v1/preview/stop
sekme kapanırsa            → pagehide + fetch keepalive
hiçbiri gelmezse           → 45 s okunmayan session reaper tarafından destroy edilir
Kodi'ye devir              → Player.Open'dan ÖNCE preview destroy edilir
daemon kapanışı            → aktif session destroy edilir
```

`/hlsv2/probe` session sayılmaz: ffprobe çalışır ve çıkar.

## Politika (software transcode yasağı)

Bu kartta donanım hızlandırma yok; dolayısıyla "her kaynağı zorla tarayıcıda oynat"
politikası CPU yakmak demektir. Uygulanan sıra: **direct play → remux → yok**.
Uyumsuz bir kaynakta encode'a düşülmesi hâlinde bile session artık sahipsiz kalamaz:
ön izleme kapanır kapanmaz, devirde ve 45 s sonra kesin olarak sonlanır.

---

# REGRESYON 2 — Kodi fiziksel klavye

## Kök neden

Kodi, mediaboxd'nin **child process'i** olarak spawn ediliyordu ve mediaboxd'nin
sandbox'ını miras alıyordu:

```
$ systemctl show mediaboxd -p RestrictAddressFamilies --value
AF_INET AF_INET6 AF_UNIX
```

**AF_NETLINK listede yok.** Kodi klavyeleri udev netlink monitor ile keşfeder;
bu soket hiçbir zaman açılamadı.

Kanıt (before):

```
cgroup:     0::/system.slice/mediaboxd.service
PPID:       980  (python3 -m mediaboxd)
input fds:  0
kodi.log:   "WaitForUpdate - get udev monitor"  (kesintisiz tekrar)
```

USB klavye baştan sona mevcut ve okunabilirdi — Kodi onu hiç görmedi.
İzinler suçlu değildi: `/dev/input/event*` `root:input 0660`, Kodi root çalışıyor.

Klavye envanteri: `HP OMEN SPACER Wireless TKL Keyboard Dongle`
→ `event6` (kbd), ayrıca `event7` (Keypad), `event8` (Consumer Control), `event9` (Mouse);
`/dev/input/by-id/usb-HP_OMEN_SPACER_..._CNN0330A43-event-kbd -> ../event6`.

## M2 öncesi vs sonrası launch differential

| | M2 öncesi (kabul edilmiş) | M2 sonrası (bozuk) | Şimdi |
| --- | --- | --- | --- |
| Başlatan | `scripts/run-kodi-rk3588.sh` (`setsid`, ssh) | mediaboxd `Popen` | `kodi.service` |
| PPID | 1 (setsid) | 980 (mediaboxd) | 1 |
| cgroup | ssh session scope | `mediaboxd.service` | `kodi.service` |
| AF_NETLINK | serbest | **engelli** | serbest |
| udev monitor | açılıyor | **açılamıyor** | açılıyor |
| input fds | var | **0** | **9** |

Executable, argv, user, HOME, `AE_SINK`, `MEDIABOX_GPU`, `LD_LIBRARY_PATH` üçünde de
aynıdır; fark yalnızca **başlatma context'idir**.

## Exact fix

1. `packaging/systemd/kodi.service` — Kodi kendi unit'inde çalışır. Unit bilinçli
   olarak mediaboxd gibi sandbox'lanmamıştır; address family filtreleyen bir unit
   Kodi'yi input'suz bırakır.
2. `mediaboxd/lifecycle.py` — daemon artık Kodi'yi **spawn etmez**. `start`/`stop`/
   `restart` sabit argv ile `/usr/bin/systemctl <verb> <unit>` çağırır. `Popen` ile
   Kodi başlatan kod yolu tamamen kaldırıldı (test ile zorlanıyor).
3. Unit adı serbest metin değildir: `[A-Za-z0-9@._-]{1,64}\.service` deseniyle
   doğrulanır, argv kurulmadan önce reddedilir.

Kernel/DT/input driver'a dokunulmadı. `chmod 777 /dev/input/*` gibi bir şey yapılmadı.
Yeni paket kurulmadı.

## Doğrulama (after)

```
POST /api/v1/kodi/start   → PID 17016, cgroup=/system.slice/kodi.service, PPID=1
                            input fds = 9
                            /dev/input/event0 event3 event5 event6 event7 event8
                                       event9 event11 event12
                            "get udev monitor" hata sayısı = 0
                            CLibInputHandler::DeviceAdded - keyboard type device added

POST /api/v1/kodi/restart → PID 17502, cgroup=/system.slice/kodi.service,
                            input fds = 9
```

`kodi.service` **enabled** — boot'ta da aynı unit ile gelir.

---

# Regresyon sekansı (cihazda)

| Adım | Sonuç |
| --- | --- |
| idle | ffmpeg 0 |
| ön izleme başlat | ffmpeg 1, session `regrX` kaydedildi |
| ön izleme durdur | `{"stopped":true}` → ffmpeg 0, orphan yok |
| ön izleme başlat → Kodi'ye devret | devir öncesi 1 → devir sonrası **0** |
| Kodi playback | `state: playing`, `position: 25.4`, `duration: 120` (resume 20 s'den) |
| Kodi restart | input fds 9, doğru unit |
| Kodi playback sırasında 60 s | ffmpeg 0 (6/6 örnek) |

---

# Testler

Backend **70/70 PASS**, frontend **68/68 PASS**, TypeScript ve production build PASS.

Eklenen regresyon testleri:

- session kaydı; `/hlsv2/probe` session sayılmaz
- stop → upstream `destroy` çağrılır
- iki kez stop zararsız
- aynı anda tek ön izleme (ikincisi birincisini yıkar)
- Kodi devri ön izlemeyi sonlandırır
- istemci kendi session'ını bırakabilir (`POST /api/v1/preview/stop`)
- terk edilmiş session reaper tarafından toplanır
- `/api/v1/stremio` canlı ön izlemeyi raporlar
- Kodi canonical unit ile başlatılır
- unit adı doğrulanır; enjeksiyon denemeleri reddedilir
- **mediaboxd Kodi'yi spawn edemez** (kaynak kodda `Popen` yok, `systemctl` var)

---

# Final durum

| | |
| --- | --- |
| `mediaboxd.service` | active / enabled |
| `stremio-server.service` | active / enabled |
| `kodi.service` | active / **enabled** (yeni) |
| orphan ffmpeg | 0 |
| orphan node child | 0 |
| repo | clean |

## Fiziksel klavye kabulü

Otomatik olarak doğrulanabilecek her şey doğrulandı: Kodi doğru unit'te, doğru
cgroup'ta, PPID 1 ile, klavyenin event node'u dahil 9 input descriptor açık,
udev monitor hatası yok, libinput klavye cihazlarını ekliyor — hem ilk başlatmada
hem restart sonrasında.

Geriye yalnız insan eliyle yapılabilecek tek adım kalıyor: tuşlara basmak.
Kullanıcıdan tek seferlik doğrulama istendi.

## Final HEAD / origin/main

Bkz. aşağıdaki commit listesi; `git push origin main` sonrası `main == origin/main`.

```
fix: end Stremio preview transcoding sessions instead of leaking them
fix: restore Kodi physical input by giving it its own unit
```

## Bilinen sınır

Uyumsuz kodekli bir kaynağın tarayıcı ön izlemesi hâlâ software encode gerektirir
(bu kartta donanım encode profili yok). Artık sahipsiz kalamaz ve devirde kesin
olarak sonlanır, ama ön izleme açıkken CPU maliyeti sürer. Kalıcı çözüm, böyle
kaynaklarda ön izlemeyi hiç başlatmayıp "bu kaynak tarayıcıda doğrudan ön izlenemiyor"
demektir; bu ayrı ve küçük bir üründür.
