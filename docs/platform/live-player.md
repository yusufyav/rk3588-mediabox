# Canlı TV oynatıcısı: mediabox-live ve diğer TV'ler nasıl yapıyor

Durum: uygulandı; Canlı TV'nin tek oynatıcısı
(`rust/crates/mediabox-live`, başlatıcı `packaging/mediabox-player`, derleme
`scripts/build-mediabox-live.sh`). 2026-10-25'te mpv'nin canlı yayın yolu,
motor seçimi ve geri dönüşü kaldırıldı; mpv yalnız film ve dizi oynatır.
Bölüm 4'teki mpv sütunu o tarihten önceki karşılaştırma ölçümüdür.
2026-10-24'te kaynak okuma ve Plus üzerinde ölçümle yazıldı (10.27.27.36,
Kanky Link-4K, SONY TV HDMI-A-2'de).

Bu belgedeki her iddia üç türden birindedir ve öyle işaretlenmiştir:
**ölçüldü** (cihazda, tarihli), **kaynak** (bağlantılı belge ya da kaynak
kod), **ölçülmedi**. Kapalı kaynak üreticilerin (Sony, TCL, LG, Samsung)
iç uygulamaları hakkında belgede yazandan fazlası söylenmez.

## 1. Bir uydu alıcısının yapması gereken iş

Bir DVB yayını, kendi saatini taşıyan bir akıştır:

1. **Demux.** PAT → PMT → servisin video, ses ve PCR PID'leri.
2. **Saat kurtarma.** Yayıncının 27 MHz saati, PCR alanlarıyla yaklaşık her
   40 ms'de bir gelir. Alıcı bundan kendi **STC**'sini (System Time Clock)
   kurar.
3. **Sunum.** Her video karesi ve ses örneği PTS'si STC'ye geldiğinde ekrana
   veya hoparlöre çıkar. Ses ile görüntü aynı saate bağlı olduğu için
   senkrondur. Saat yayıncıya bağlı olduğu için gecikme birikmez.
4. **Kanal değişimi.** Tuner oturumu korunur, son istek kazanır. İlk
   I-frame'den hemen görüntü verilir.

Farklı ürünlerin farkı, bu dört işin **nerede** yapıldığıdır: SoC
donanımında mı, yazılımda mı.

## 2. Başkaları nasıl yapıyor

### Android TV / Google TV (Sony BRAVIA, TCL ve diğer Android TV'ler)

**Kaynak:**
[Tuner framework](https://source.android.com/docs/devices/tv/tuner-framework),
[Multimedia tunneling](https://source.android.com/docs/devices/tv/multimedia-tunneling).

- Android 11'den beri canlı yayın **Tuner HAL** üzerinden gider. HAL'in
  arayüzleri `IFrontend`, `IDemux`, `IFilter`, `IDescrambler`, `IDvr` ve
  `ILnb`'dir. HAL, "üreticinin donanımına" açılan arayüzdür. Frontend ve
  demux uygulamaya kapalı iç bileşenlerdir. Filtre türleri arasında `TS.PCR`
  vardır.
- **Saat:** Canlı yayında ses saati değil, "tuner tarafından sürülen PCR ya da
  STC" kullanılır. Uygulama Tuner'dan bir `avSyncHwId` alır ve `MediaCodec`
  ile `AudioTrack`'e verir. Belgede bu, "tunneled oynatmada donanım A/V
  senkronu" olarak geçer.
- **Ekrana çıkış:** Çözücü bir *sideband* tutamacı döndürür. Kareler
  uygulamadan ve normal grafik yolundan geçmez. Hardware Composer (HWC)
  onları "ses çıkışına ya da tuner PCR'ına senkron olarak" alır.
- **Sony ve TCL:** Android/Google TV modelleri bu çerçeveden geçmek zorundadır.
  HAL'in, demux'un ve AV senkron firmware'inin altında ne olduğu çip
  üreticisinindir ve kapalıdır. Hangi modelde hangi SoC olduğu burada
  **doğrulanmadı**.

### Samsung (Tizen)

**Kaynak:**
[Broadcast API](https://developer.samsung.com/smarttv/develop/guides/multimedia/broadcast.html?device=htv).

- Uygulamaya yalnız kanala ayarlama (`tuneDirect`), iz seçimi ve altyazı
  açılır. Örneklerde yayın, `tizen.tvwindow.show(..., "MAIN", "BEHIND")` ile
  platformun penceresinde gösterilir.
- Uygulama kareyi görmez. Çözme ve senkronun nerede yapıldığını belge
  söylemez; bu yüzden burada iddia edilmez.

### RDK-V (operatör set üstü kutuları)

**Kaynak:**
[RDK GStreamer alt sistemi](https://developer.rdkcentral.com/documentation/documentation/rdk_video_documentation/sub-systems/g-streamer_subsystem/).

- Oynatma GStreamer'dır. Kaynak elemanı `qamtunersrc` cihaza özeldir.
  `playersinkbin` "demux, çözücü ve sink içeren bir bin"dir ve "Generic + SoC"
  olarak işaretlidir. Donanım hızlandırmalı oynatma için SoC üreticisi kendi
  eklentilerini yazar.
- Senkronun nerede olduğunu belge söylemez. Demux'un SoC'a bağlı olduğu
  açıktır, donanımda olup olmadığı yazmaz.

### Enigma2 (Linux uydu alıcıları: Dreambox, VU+ ve benzerleri)

**Kaynak:**
[openatv/enigma2 `lib/dvb/decoder.cpp`](https://github.com/openatv/enigma2/blob/master/lib/dvb/decoder.cpp),
[Linux DVB `DMX_GET_STC`](https://docs.kernel.org/userspace-api/media/dvb/dmx-get-stc.html).

- `/dev/dvb/adapterN/video0`, `audio0` ve `demuxN` açılır. Video ve ses PES
  filtreleri ile PCR filtresi `DMX_OUT_DECODER` çıkışıyla doğrudan
  **donanım çözücüye** bağlanır (`eDVBPCR::startPid`, `DMX_PES_PCR0..3`).
- STC çekirdekte/donanımdadır. `DMX_GET_STC`, PCR türü bir PES filtresinin
  sürdüğü sistem zaman sayacını döndürür.
- Yani Enigma2 demux, saat kurtarma ve senkronu hiç yapmaz. Bunları alıcının
  SoC'una (Broadcom, HiSilicon vb.) kernel DVB API'si üzerinden devreder.

### VDR

**Kaynak:**
[vdr-plugin-softhddevice README](https://github.com/ua0lnj/vdr-plugin-softhddevice).

- VDR'ın kendisi tuner, demux ve kayıttır. Görüntüyü bir *çıkış eklentisi*
  verir. Donanım çıkışlı kartlarda bu işi kart yapar.
- PC'de `softhddevice` görüntüyü CPU ya da VDPAU ile çözer, sesi FFmpeg/ALSA
  ile verir, çıkışı X11'dir. Senkron ayarları kullanıcıya açıktır
  (`AudioDelay`, `SoftStartSync`, `AudioBufferTime` varsayılan 336 ms,
  `ClearOnSwitch`). İç saat modelini README anlatmaz.

### tvheadend ve Kodi PVR

**Kaynak:** [tvheadend README](https://github.com/tvheadend/tvheadend),
[Kodi `VideoPlayerAudio.cpp`](https://github.com/xbmc/xbmc/blob/master/xbmc/cores/VideoPlayer/VideoPlayerAudio.cpp).

- tvheadend "Linux için TV akış sunucusu ve dijital video kaydedicisidir".
  HTSP ve HTTP ile istemcilere akış verir, kendisi oynatmaz.
- Kodi canlı yayını bir PVR eklentisinden (ör. `pvr.hts`) VideoPlayer'a alır.
  VideoPlayer gerçek zamanlı akışta (`IsRealtimeStream`) sesi
  **`SYNC_RESAMPLE`** ile kendi saatine yeniden örnekler ve **passthrough'u
  kapatır**. Passthrough açık kalırsa `SYNC_DISCON` kullanır: hata eşiği
  aşılınca saati düzeltir ve sink'e ofset verir.
- Bu bir medya oynatıcısının canlı akış modelidir, alıcınınki değildir. Bu
  yüzden ürün hedefi olarak reddedildi.

### Özet tablo

| | Demux | PCR → saat | A/V senkron | Ekrana çıkış |
|---|---|---|---|---|
| Android TV (Sony, TCL…) | SoC, Tuner HAL | donanım STC (`avSyncHwId`) | çözücü + ses donanımı STC'ye bağlı | sideband → HWC |
| Samsung Tizen | platform | belgede yok | belgede yok | platform penceresi |
| RDK-V | SoC eklentisi | belgede yok | belgede yok | SoC sink |
| Enigma2 | kernel DVB, donanım | donanım STC (`DMX_PES_PCR`) | donanım çözücü | donanım |
| VDR + softhddevice | VDR (yazılım) | README'de yok | kullanıcı ayarlı gecikmeler | VDPAU / X11 |
| Kodi PVR | VideoPlayer (yazılım) | yok: kendi saati | ses yeniden örnekleme | Kodi |
| mpv (bizde, eski) | lavf (yazılım) | yok: ses aygıtının saati | ses saati | MBV1 → VOP2 |
| **mediabox-live** | tsdemux (yazılım) | **yazılımla PCR kestirimi → CLOCK_MONOTONIC** | iki sink tek saatte | MBV1 → VOP2, compositor yok |

## 3. Bizim yaptığımız: mediabox-live

### Neden ayrı bir oynatıcı

mpv yayını bir dosya gibi oynatıyordu: saati ses aygıtıdır, her kanal bir
`loadfile`'dır ve gösterilen kare yayının PCR'ına bağlı değildir.
**Ölçüldü (2026-10-24):** TRT HABER HD'nin 40 saniyesi içinde 1,25 s donma
oldu ve görüntü PTS'ye göre 1,17 s kaydı (bkz. bölüm 4).

### Zincir

```text
Kanky ─USB─> mediabox-dvb ─FIFO (tek servis)─> mediabox-live
  appsrc (canlı; her okuma CLOCK_MONOTONIC çalışma zamanıyla damgalanır)
   └ tsdemux (PAT/PMT; PCR → bu saat: mpegtspacketizer skew kestirimi)
       ├ queue ─ h264parse/h265parse/mpegvideoparse ─ mppvideodec ─ input-selector ─ appsink
       │                                                └ MBV1 (dma-buf, SCM_RIGHTS) ─> mediabox-tv-ui ─ atomic commit ─ VOP2 Esmart0
       └ queue ─ mpegaudioparse/aacparse/ac3parse ─ çözücü ya da IEC 61937 ─ input-selector ─ alsasink (ses planının PCM'i)
```

- **Tek saat.** Pipeline, arayüzün PageFlip zaman damgalarıyla aynı saati
  kullanır: GstSystemClock, CLOCK_MONOTONIC. Kaynak canlıdır ve geliş
  zamanını damgalar. tsdemux, canlı TIME akışında yayıncı saatini bu geliş
  zamanlarına karşı PCR'lardan kestirir ve her PTS'yi bu saate koyar. İki sink
  de aynı çalışma zamanını artı pipeline gecikmesini bekler
  ([GStreamer senkron modeli](https://gstreamer.freedesktop.org/documentation/additional/design/synchronisation.html):
  sink, saat `sync_time`'a gelene kadar bekler). alsasink saate skew ile
  bağlıdır. Bu, Android'deki donanım STC'nin yazılım karşılığıdır.
- **Görüntü.** `appsink sync=true` kareyi zamanı gelince verir, arayüz onu
  bir sonraki vblank'te commit eder. Arayüz RELEASE gönderene kadar kare,
  dolayısıyla MPP tamponu, tutulur. En fazla 4 kare dışarıdadır (vo_mediabox
  ile aynı). Kopya yok, CPU'da dönüşüm yok, DRM master arayüzde kalır,
  compositor yok.
- **Deinterlace.** MPP IEP2 I5O2 kullanılır: her geçmeli kareden, kendi
  PTS'leriyle iki kare çıkar (kaynak: MPP `mpp_dec_vproc.c`). MPP bunu yalnız
  ayrıştırıcı hızlı kipte değilken başlatır. ffmpeg-rockchip geçmeli
  videoda hızlı kipi kapatır (`rkmppdec.c`), gstreamer-rockchip açık bırakır
  (`gstmppdec.c`). Bu yüzden burada `fast-mode=false` ayarlanır.
- **Her akış kendi iş parçacığında** (tsdemux'tan sonra `queue`). Bu
  olmadan ses, PTS'sinden 1,35 s sonra sink'e ulaştı; gecikme payı ise 0,83 s
  idi ve aygıt sessizlik çaldı (**ölçüldü**, bölüm 4).
- **Kanal değişimi.** Her kanal, iki input-selector'ın önüne kurulan bir
  daldır (kaynak, demux, ayrıştırıcılar, çözücüler). Eski dal seçicilerden
  hemen koparılır: panelde son kare kalır, ses aygıtı sessizlik çalar. Sonra
  kendi iş parçacığında kapatılır. Sink'ler, HDMI ses aygıtı ve arayüz
  bağlantısı hiç kapanmaz.
- **Denetim.** Denetim soketi, mediaboxd-rs'in oynatıcılarla konuştuğu JSON
  IPC'nin (mpv'ninki) canlı yayına gereken kısmını konuşur: `loadfile`,
  `get_property`, `observe_property`, `aid`, `volume`, `mute`, `audio-delay`
  ve `quit`. Yalnız yapılan iş `success` döner. Oynatıcıda olmayan özellik
  "property unavailable" döner. Açılamayan kanalın `loadfile` yanıtı sebebi
  taşır.
- **Durum anlamları.**
  - `core-idle=false` (daemon durumunda `picture=true`): son açılan kanalın
    ilk karesi arayüzün düzlemine verilmiştir ve oynatıcı duraklatılmamıştır.
    Görüntüsü olmayan serviste aynı şey, sesinin ses aygıtına ulaşmasıdır.
  - Kanal değişimi `end-file` sayılmaz.
  - FIFO'nun sonu yayının sonu değildir. Alıcı başka FIFO'ya geçerken eski
    okuyucuyu bu yolla bırakır. Yayının bittiğini alıcının kendi durumu
    söyler (`follow_live`).
  - Oynatıcı yalnız şu iki durumda biter: `quit` gelir, ya da kanal hiç
    oynatılamaz (görüntü çözülemez, aynı hata üç yeniden açılışta sürer).
    İkincisinde `end-file reason=error file_error=<sebep>` gönderir; arayüz
    bu sebebi gösterir.
- **Okuyucu.** Her FIFO'nun tek okuyucusu vardır ve akışı o anki dala verir.
  Aynı FIFO'da yeniden açılan kanal (ses izi, devam, hata sonrası) veri
  kaybetmez.

### Profesyonel alıcıyla aynı olan ve olmayan

**Aynı fikirler:** PCR'dan saat kurtarma, PTS'nin bu saate göre
zamanlanması, akış başına tampon, ekran bırakana kadar kare tutma, kopyasız
yol, kanal değişiminde sessiz donma.

**Fark:** Bunlar SoC donanımında değil, yazılımda yapılıyor. RK3588
Linux'unda kullanılabilir bir donanım STC'si ya da tunneled yol yok
(Rockchip'in Android BSP'si için **doğrulanmadı**). Ekran ve HDMI ses saatleri
PCR'a kilitli değil; aradaki fark kare tekrarı/atlaması ya da yeniden
örnekleme ile değil, zaman damgası eşlemesi ve skew ile kapatılıyor.

## 4. Ölçümler (Plus, 2026-10-24)

TRT HABER HD (10801, 12226 V 27500, H.264 1080i tff, MP2 stereo).

İki oynatıcı için aynı betik kullanıldı: 40 s kararlı oynatma, ardından
dört kanal isteği; her oynatıcı için tek çalıştırma. Kare zamanları
MEDIABOX_VO_TRACE'ten (CLOCK_MONOTONIC) alındı: karenin arayüze verildiği an.

| | mediabox-live | mpv |
|---|---|---|
| kare/s (kararlı 40 s) | 49,67 | 48,56 |
| kareler arası en uzun boşluk | 40 ms | 1248 ms (donma) |
| PTS aralıkları | 1974 × 20 ms, 14 × 40 ms | 1944 × 20 ms |
| PTS'ye göre gönderim kayması, 36 s | +150 µs | 1,17 s |
| CPU / RSS / fd / dma-buf | %11 / 22 MB / 59 / 38 | %10 / 42 MB / 74 / 45 |
| kanal değişimi, aynı multipleks (2 istek) | 2042, 1901 ms | 209, 302 ms |
| kanal değişimi, başka multipleks (2 istek) | 3506, 3695 ms | 1615, 2876 ms |

Değişim süresi: istekten, eski kanalın kareleri durduktan sonra yeni
kanalın arayüze verilen ilk karesine kadar geçen süre.

**Ses** (GST_DEBUG=audiobasesink ve oynatıcının kendi sayaçlarıyla):

| | queue'dan önce | sonra |
|---|---|---|
| ses tamponunun sink'e varışı (PTS'ye göre) | −1,35 s (geç) | −0,63 s (0,1 s pay) |
| alsasink "Unexpected discontinuity, resyncing" | 12 s'de 64 | 30 s'de 0 |
| televizyonda duyuldu | hayır | evet (sahip, 2026-10-24) |

alsasink iç saati ile pipeline saati arasındaki fark +0,25…+0,38 ms
(`skew_slaving`). ALSA aygıt gecikmesi 91–99 ms (100 ms tampon, S32_LE
48 kHz stereo, AES0 0x04).

**Aynı günün ara adımları:** MPP hızlı kip açıkken çıkış 25 kare/s idi ve
IEP yoktu. Kapatınca IEP seçildi ("device /dev/mpp_service select in vproc")
ve çıkış 50 kare/s oldu. Cihazdaki tek kayıt (`recording.ts`, tüm
multipleks) bozuk: ffmpeg 17,5 s içinde 10800'den yalnız 247 kare çözüyor.
Bu yüzden kadans için kullanılmadı.

## 5. Bilinen kusurlar ve ölçülmeyenler

- **Kanal değişimi mpv'den yavaş** (bölüm 4). Pipeline sabit 0,83 s ekliyor
  (tsdemux `latency` 700 ms + ses tamponu). Yeni kanal ayrıca PAT/PMT ve
  I-frame bekliyor. Henüz ayarlanmadı. Profesyonel alıcı ilk I-frame'i hemen
  gösterir.
- Kanal başında, tsdemux'un saat kestirimi oturana kadar yaklaşık 10 kare
  gösterilmiyor (iz olayı `M`; host'ta ölçüldü).
- Ekran ve HDMI ses PCR'a kilitli değil (bölüm 3).
- **Ölçülmedi:**
  - panel tarafı kadans (PageFlip/vblank ile arayüz izinin birleştirilmesi)
  - televizyondaki A/V ofseti
  - AC-3 passthrough
  - HEVC ve MPEG-2 kanalları
  - radyo servisleri
  - oynatma sırasında sink değişimi ve mod değişimi
  - uzun süreli oynatma
- "Oynatımı görüntüye eşitle" bu oynatıcıda yok; açıkken ses PCM gider.
- GStreamer eklentileri dlopen ile yüklendiği için release capture'ın ELF
  kapanışı onları bulmuyor. Capture betiği (`create-mediabox-release.sh`)
  bunların paketlerini ve `mediabox-live`/`mppvideodec` kapılarını
  2026-10-25'ten beri adlandırıyor; golden kart üzerinde gerçek bir capture
  ile henüz doğrulanmadı.

## 6. Kanal değişiminde "görüntü geldi" kararı (2026-10-24 olayı)

**Belirti (sahip):** TRT ULTRA (101) açılmadı. Başka kanala geçince önce eski
görüntü, sonra yeni kanal geldi, ardından "Bu kanaldan görüntü gelmedi"
uyarısı çıktı.

**Ölçülen nedenler** (arayüz, daemon, oynatıcı ve alıcı kayıtlarından):

1. **Motor eski kareyi yeni kanalınki sayıyordu.** Sink'te saatini bekleyen
   kare, kanal değiştikten sonra gönderiliyordu. Motorun günlüğünde başka
   transpondere geçişte "ilk kare 0–20 ms" yazıyordu.
2. **Motor kanal arasında paneli karartmıyordu.** Arayüzün kuralı ise "ilk
   sayılan kare, panel karardıktan sonraki karedir"
   (`previous_on_plane`). Arayüzde "first frame 56 ms after the key"
   görülüyordu.
3. **Arayüz ilk görüntüyü bir kez karar veriyordu.** Aynı oynatıcıdaki kanal
   değişiminde bu, daemon'un istek yanıtındaki `picture` alanıdır (en fazla
   8 s yoklama; "sinyal yok" sırasında iptal). Karar bir daha
   gözden geçirilmiyordu; 30 s sonra oynatıcı durduruluyordu. Eski motorun
   sahte "oynuyor"u bu kusuru gizliyordu.

**Profesyonel model:** Android TIF'te TV girişi görüntünün durumunu olay olarak
bildirir (`TvInputService.Session.notifyVideoAvailable()` /
`notifyVideoUnavailable(VIDEO_UNAVAILABLE_REASON_TUNING | WEAK_SIGNAL)`).
Uygulama bir defalık yoklamayla karar vermez.

**Yapılan:**

- **mediabox-live:** Kanal değişiminde sink flush edilir ve panel karartılır
  (MBV1 STOP). Eski dalın çözücüsünden gelen kareler kuşak numarasıyla atılır.
- **mediaboxd-rs:** Kanal, oynatıcı onu yüklediği anda durumda görünür.
  Durum yanıtında `picture` alanı vardır (`core-idle`).
- **mediabox-tv:** İlk görüntü her durum yanıtından okunur. Ayrıca kendisinin
  başlatmadığı canlı yayını film değil, Canlı TV ekranı olarak sahiplenir
  (`adopt_live`).

**Doğrulama:** Kumandayla 100 → 101 (sinyal yok) → 102 → 101 → 100 sırası
denendi. Arayüz 102'nin görüntüsünü 5105 ms'de, 100'ünkünü 4044 ms'de tanıdı.
40 s içinde "gave up" yok; ekranda uyarı yok.

**TRT ULTRA (101, 16300, TKGS: 11767 V SR 15000):** Sinyal kilitlenme
sınırında; ölçüm aynı gün referansla tekrarlandı. Sony ve Kanky (Android
kutuda) 8,06–8,43 dB SNR ile kilitlendi; Android kutu da kanalı ancak 3–4
denemede açtı. Plus'ta SNR USB portuna bağlı: arka USB3 4,0–4,8 dB, ön USB2
(Bus 003) 5,4–5,7 dB, öbür ön USB2 (Bus 006) 7,0–7,8 dB. Yukarıdaki ilk
denemedeki `NO_RF` kararı bu port ölçümlerinden önceydi.

**Kalan:** Kanal değişimi 4–5 s, profesyonel alıcıdan yavaş (bölüm 5).

## 7. Sony BRAVIA ile ölçülü karşılaştırma (2026-10-24)

**Cihaz:** Sony BRAVIA 4K GB, Android 9, MediaTek MT5891. Aynı çanağa bir
ayırıcıyla bağlı; adb ile yalnız okundu, kanalları sahip değiştirdi.

**Yapı (cihazdan):**

- TIF girişleri: `com.sony.dtv.tvinput.tuner`
  (`DvbGenSatInputService`, `DvbPrefSatInputService`, …).
- Uygulama: `com.sony.dtv.tvx` (`Tvx-TvPlayer` → `TvView.tune()`).
- Yerel taraf: `dtv_svc`, `MtkRmServer`, `mtktvapi.agent`,
  `android.hardware.tv.input@1.0-service-mediatek`.
- Kütüphaneler: `libmtal.so`, `libcmpb.so`.
- `libmtal.so` dışa açık fonksiyonları:
  - Donanım demux: `MTDMX_SetVideoPid` / `SetAudioPid` / `SetPcrPid`.
  - Donanım STC: `MTSTC_AllocStc`, `StartStc`, `SetSyncMode`,
    `MTDMX_GetCurSTC`.
  - Çözücülerin STC'ye bağlanması: `MTAUD_SetStcId`, `MTVDEC_GetStc`.
  - Görüntü dondurma: `MTVDEC_SetFreeze`, `MTVDO_SetFreeze`.
  - Panelin akışa kilitlenmesi: `MTPMX_PANEL_SetFrameTrackOnOff`.

**Davranış (Sony'nin kendi kaydından):**

- Olay sırası: `Tune to` → `onTuned` → `onVideoUnavailable(reason=1,
  TUNING)` → `onVideoAvailable` → `onSignalStatusChanged video:true
  audio:true scrambled:false`.
- Kanal değişiminde **önce kanal bilgisi** çok hızlı gelir, ekran **siyah**
  kalır, görüntü oturunca oynatım başlar (sahibin gözlemi). Bizde de şerit
  tuşla birlikte açılır (`Live::tuning`, ayar isteğinden önce; tuştan 0,5 s
  sonraki ekran görüntüsünde vardı, ms düzeyinde ölçülmedi).
- Ekrandaki her uyarı ara katmanın gerçek durumundan gelir. "Sinyal yok"
  diyorsa sinyal yoktur.

| Geçiş | Sony: istek → görüntü | MediaBox |
|---|---|---|
| TRT HABER HD (açılış) | 2457 ms | — |
| TRT1 HD (aynı transponder) | 2368 ms | 1,9–2,0 s (motorun ilk karesi) |
| TRT 4K (11767 V, başka transponder) | 2142 ms, ilk denemede | sınırda kilit; 3,5 s ve üzeri |
| ATV (12207, başka transponder) | 2303 ms | 3,5–3,7 s |

**Bir kanal geçişinin zaman çizelgesi (Sony kaydı, 4 geçiş, ayar isteğine
göre):**

| Adım | Aralık |
|---|---|
| `onTuning`: kanal seçildi, bilgi şeridi açık | −19…−9 ms |
| Önceki kanalın bilgisi temizlenir | +7…+210 ms |
| MediaTek `channelSelect` | +205…+227 ms |
| `onVideoUnavailable(TUNING)`: siyah; `notifyHideOSDMessage`: ekran mesajları gizlenir | +317…+691 ms |
| Şeridin zaman aşımı iptal edilir: ayar sürerken şerit kalır | +454…+795 ms |
| Ses açılır (`Set unmute`) | +2004…+2172 ms |
| Görüntü var (`onVideoAvailable`) | +2142…+2457 ms |
| Sinyal durumu doğrulanır (video/audio/scrambled), çözünürlük bildirilir | +2680…+3241 ms |

**Kumandanın kanal tuşuyla 17 geçiş** (ayar başı/siyah → görüntü;
MediaTek ara katmanı geçişi kendisi yapar, TIF'e `notifyChannelRetuned` ile
bildirir):

| Ölçü | Sony |
|---|---|
| Medyan | 2,45 s |
| En hızlı | 0,47 / 0,57 s: aynı transponderde komşu servis (52205 ↔ 52206, SR 30000) |
| Tipik | 1,5–3,0 s (farklı transponderler, SR 7500–30000) |
| En yavaş | 8,7 / 8,8 s (52205, 11005) |
| TRT ULTRA (16300) | 1,54 s |

**Bizim için kurallar:**

1. Canlı TV'de uyarı metni yalnız alıcının durumundan (`no_signal`,
   şifreli) ve oynatıcının görüntü durumundan (`picture`) üretilir. Süre
   sayacından ("görüntü gelmedi", 30 s) ya da oynatıcının bitişinden
   ("Yayın durdu") üretilmez. Bugünkü yanlış uyarıların hepsi bu iki
   yoldan geldi.
2. Kanal değişiminde önce kanal bilgisi, siyah ekran, mesaj yok.
3. Başka transpondere geçiş hedefi: Sony düzeyi (~2,2 s). Bizdeki fazlalık
   alıcının yeniden ayarı ile oynatıcının sabit 0,83 s gecikmesinin üst
   üste binmesi; ikisi ayrı ayrı ölçülüp küçültülecek.

## 8. Kullanım

```sh
scripts/build-mediabox-live.sh --install     # MEDIABOX_HOST ayarlı; Debian GStreamer,
                                             # medya çalışma zamanına karşı mppvideodec, ikili
mediaboxctl media live-here 10801 --frequency-khz 12226000 --symbol-rate 27500
```

Oynatıcının kendi durumu, `/run/mediabox/player.sock` üzerinde
`get_property mediabox-live` ile okunur: kareler, atlananlar, gecikme,
görüntü gecikmesi, ses önceliği, codec'ler ve ses biçimi. Ortam ayarları
`rust/crates/mediabox-live/src/main.rs` başında, iz biçimi `trace.rs`'te.
