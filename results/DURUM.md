# MediaBox — durum

Bu dosya **tek yaşayan belge**. Oturum başına yeni bir tarihli rapor yazılmaz;
burası güncellenir.

Geliştirme cihazı: Orange Pi 5 Ultra (RK3588),
`6.1.115-vendor-rk35xx-screenbridge-hdmirx-audio`. İkinci hedef: Orange Pi 5
Plus — hazırlandı, **kurulmadı**. Ne panel ne adres repoda sabit:
`MEDIABOX_HOST` / `scripts/env.sh`, çıkış `mediabox-platform` ile keşfediliyor.

> **Eski tarihli raporlar.** 27 oturum raporu ve bütün ham kanıt `710181b`
> commit'ine kadar `results/` ve `logs/` altındaydı. Silinmediler, geçmişte
> duruyorlar:
> `git show 710181b:results/orangepi5-ultra-vendor/hdr-signaling-mp1a-2026-09-09.md`
> Ham cihaz çıktıları ayrıca repo dışında: `../rk3588-mediabox-arsiv/`.

---

## 1. Ürün nedir

RK3588 için bir televizyon kutusu işletim deneyimi — oynatıcı değil, OS.
Salondan kumandayla, ağdan tarayıcıyla kullanılır.

```
Rust + Slint → FemtoVG/GLES → Mali-G610 (keşfedilen render aygıtı)
     → GBM armsoc → DMA-BUF/PRIME → keşfedilen Rockchip ekran aygıtı
     → DRM/KMS/VOP2 → seçilen çıkış
```

Aygıt numaraları yazılı değil; `mediabox-platform` topolojiden çözüyor
(bkz. `docs/platform/runtime-discovery.md`).

## 2. Cihazda çalışan

| Servis | Kaynak | Ne |
|---|---|---|
| `mediabox-tv-ui.service` | `rust/crates/mediabox-tv` | TV arayüzü (Slint, compositor yok, DRM master) |
| `mediaboxd-rs.service` | `rust/crates/mediaboxd-rs` | Denetim düzlemi; Web UI'yı `/opt/rk3588-mediabox/ui`'den sunar (8787/8788) |
| `mediabox-media-worker.service` | `media/` | Medya çekirdeği, Stremio köprüsü, ffprobe politikası |
| `stremio-server.service` | repo dışı (node) | Torrent ve akış çözümü |
| `mediabox-player.service` | `packaging/mediabox-player` | Geçici birim; film oynarken var, bitince yok |
| `kodi.service` | `packaging/` | Devredilince |

Üretim Web UI **`rust/crates/mediabox-ui`** (Rust/wasm). Başka web arayüzü yok.

## 3. Kurulmuş ve kabul edilmiş temeller

Bunlar ölçülüp kapatıldı. **Yeniden kampanya olarak koşturulmaz.**

* **HDR10 / Kodi devri.** Kodi ekranı alıyor, HDR10 + BT.2020 + `YUYV10_1X20`
  3840x2160p24 ile sürüyor, geri verirken arayüz panelin kendi modunda SDR
  dönüyor. `git show 710181b:results/orangepi5-ultra-vendor/`
* **HDMI ses.** PCM bring-up ve sıkıştırılmış passthrough (MA0, MA1).
* **Renk sadakati.** Düzlem `COLOR_ENCODING` A/B'si ve kör karşılaştırma
  (MP1b, MP1b-CSC). C++ probu `src/` + `tools/`, bekçisi
  `tests/run-host-tests.sh` — **bu üçü silinmez**, kabul edilmiş temeli onlar
  koruyor.
* **Android oracle.** RK3588 Android HWC davranışı referans olarak kaydedildi.

## 4. Native kabuk (V1)

`results/` içindeki eski `native-shell-v1` raporunun özü:

* Split render/display: Mali GBM render aygıtında, KMS konektör sahibi aygıtta,
  dma-buf export → PRIME import → ADDFB2 → page flip. Compositor yok, Wayland
  yok, llvmpipe yok.
* Kumandanın kutuyu yeniden başlatması kapatıldı — dört bağımsız kilit
  (udev `TAG=""`, logind drop-in, `ctrl-alt-del` mask, `actions.rs` yönlendirme
  tablosu ve testleri).
* Girişte çift tuş kapatıldı: rc-core cihazları atlanıyor + simetrik dedup.
* Panelde konsol metni yok: fbcon unbind **ve** `/dev/fb0` sıfırlama.
* Film sonrası pastel renk düzeltildi: bağlayıcıda `color_format:0`.
* Soğuk açılışta boş katalog düzeltildi: yeniden deneme + `network-online.target`.
* Kartın gösterge ışıkları ayar oldu: **Ayarlar > Işıklar**, üç mod
  (Kapalı/Açık/Nabız), seçim `/var/lib/mediabox/leds`'te saklanıp her boot'ta
  yeniden uygulanıyor. Kırmızı ışık beslemeye bağlı, yazılımdan kapatılamaz.
  Satır basış anında güncelleniyor (ölçülen tuş-çizim 3–7 ms); on saniyelik
  makine yoklaması beklenmiyor.
* `mediabox-kiosk-smoke` 20/20.

## 5. Gömülü oynatıcı (15 Eylül 2026)

Film kendi VOP2 düzleminde; arayüz DRM master'da kalıyor.

```
Video Port0  PLANE_MASK  Cluster0=0x1 … Esmart0=0x4 …  value: 5
Cluster0-win0  AR24  zpos 11   ← arayüz, üstte, alfalı
Esmart0-win0   NV15  zpos 0    ← film, altta, donanımda ölçekli
```

* Çözücü: mpv 0.41, cihazda MediaBox'ın kendi Rockchip ffmpeg'ine karşı derli,
  üç yama, RPATH `/opt/rk3588-mediabox/media-runtime/lib`.
  `packaging/mpv/vo_mediabox.c` kareleri `SCM_RIGHTS` ile arayüze veriyor;
  `rust/crates/mediabox-tv/src/video.rs` import edip düzleme koyuyor.
* İki tuzak: `DRM_CLIENT_CAP_UNIVERSAL_PLANES` istenmedikçe Esmart0 görünmüyor
  (sürücü onu `type: Cursor` bildiriyor), ve hangi düzlemin kabul edileceği
  userspace'ten okunamıyor — adaylar `SetPlane` ile deneyerek seçiliyor.
* Ad ve süre **katalogdan** geliyor, oynatıcıdan değil. Vekillenen oturumda
  konteyner süresi yok; mpv'ye sorulursa 99 dakikalık film 3:45 görünüyor.
* Sarma: çubuk odaklanabilir, Sol/Sağ hızlanıyor (10 sn → 10 dk), Ok mutlak
  arama yapıyor, sona 15 sn pay bırakıyor.

Ölçüm (katalogdan, tam yol):

```
85 dk film → katalogdan 5100 s · NV12 1920x1080 → 2560x1440
14 × Sağ  0:15 → 15:13 ; Ok → 915 s        60 × Sağ + Ok → 5088 s, oynuyor
yerel 4K NV15  mpv %6-9 CPU · arayüz ~%0 · 1845 kare · 0 düşük · 23.96 fps
```

## 6. Tarayıcı — donanım video çözme (21 Eylül 2026)

Plus üzerinde ölçüldü. Chromium sayfayı Mali'de çiziyordu ama videoyu CPU'da
çözüyordu; bu kapandı.

**Neden kapalıydı.** Vendor çekirdek (`6.1.115-vendor-rk35xx`) VPU'yu yalnız
`/dev/mpp_service` ardında veriyor. Kutuda **hiç `/dev/video*` yok**,
`CONFIG_VIDEO_HANTRO` kapalı, `/sys/class/video4linux` boş — yani Chromium'un
taşıdığı V4L2 yolunun bağlanacağı bir aygıt yok. Debian'ın Chromium'unda
hızlandırılmış çözmenin tek arka ucu VA-API ve kutuda hiçbir
`*_drv_video.so` yoktu:

```
ERROR:media/gpu/vaapi/vaapi_wrapper.cc:1801] vaInitialize failed: unknown libva error
chrome://gpu → Video Acceleration Information → Decoding: (boş)
```

`chrome://gpu`'daki **"Video Decode: Hardware accelerated" satırı yanıltıcı**:
özelliğin açık olduğunu söylüyor, çözücü bulunduğunu değil. `mediaCapabilities`
o haldeyken 4K H.264/VP9/AV1 için `powerEfficient=false` cevaplıyordu.

**Ne yapıldı.** libva'yı **ürünün kendi MPP'sine** köprüleyen bir VA-API
sürücüsü, sabit revizyonla kutuda derleniyor:
`scripts/build-vaapi-driver.sh` → `rockchip-vaapi` `v2.2.0`
(`8e41d785…`, LGPL-2.1) → `media-runtime/lib/dri/rockchip_drv_video.so`.
RPATH `/opt/rk3588-mediabox/media-runtime/lib`; kutuda **ikinci bir MPP yok**,
sistemin `dri` dizinine dokunulmuyor, ScreenBridge öneki anılmıyor.

Birim `DevicePolicy=closed` kalıyor; açılan tek şey iki aygıt
(`/dev/mpp_service`, `/dev/rga`) ve iki değişken
(`LIBVA_DRIVER_NAME`, `LIBVA_DRIVERS_PATH`). Mali yolu değişmedi.
Başlatıcıya eklenen üç feature adı, **bu ikilide gerçekten var olduğu
doğrulanarak** eklendi; `--ignore-gpu-blocklist`, `--enable-gpu-rasterization`
ve `--enable-zero-copy` **eklenmedi** çünkü ölçümde tarayıcının zaten kendi
cevabıydılar.

**Ölçüm** (aynı panel, aynı klip, 330 s sürekli):

| | 4K H.264 High | 4K VP9 P0 | YouTube 2160p60 | *öncesi (yazılım)* |
|---|---|---|---|---|
| çözülen kare | 9902 | 9901 | 3620 | 1801 |
| düşen kare | 0 (%0) | 5 (%0,05) | 5 (%0,138) | 0 |
| çıkış fps | 30,0 | 30,0 | 60,3 | 30,0 |
| Chromium CPU | %44,4 | %44,7 | %144,5 | **%161,9** |
| Chromium (8 çek.) | %5,6 | %5,6 | %18,1 | %20,2 |
| VPU sahipliği | var | var | var | **yok** |

Yazılım/donanım farkı tek değişkenle ölçüldü (`LIBVA_DRIVERS_PATH` geçersiz
yapılarak): 4K H.264'te tek çekirdeğin **%161,9 → %44,4**'ü, 3,6 kat.

Donanım kanıtı ad değil, aygıt: Chromium'un **gpu-process**'i
`/dev/mpp_service` üzerinde açık tanıtıcı tutuyor ve
`/proc/mpp_service/sessions-summary` oynatma boyunca
`device: fdc38100.rkvdec-core` gösteriyor.

YouTube gerçekte **VP9** verdi (`vp09.00.51.08…`, itag 315, 3840x2160@60) —
AV1 değil. AV1 bu sürücüde yok ve `powerEfficient=false` olarak doğru
bildiriliyor.

Sayfa çizim yolu (`tools/ui-perf.py`, panelin kendi 2560x1440@144'ünde):
kare süresi p50 6,9 ms / p95 7,0 ms, boşta CPU %1,2, düşen kare %0–0,14.

Doğrulayıcı: `mediabox-browser-verify` (kurulum, sürücü, aygıt izni, feature
adları). `mediabox-product-verify` PASS.

**Kadans — kapanmadı, bu işin dışında.** Takılı panel 2560x1440@**143,999 Hz**
ve 60 fps içerik tam bölünmüyor (144/60 = 2,4): her kare 2 veya 3 refresh
tutuluyor, 13,9 ms / 20,8 ms dönüşümlü. Panning görüntüde gözle *tearing gibi*
okunuyor. Bu **decode'dan gelmiyor ve yeni değil** — aynı A/B'de yazılım
çözmede daha kötü (ortalama 21,56 ms, tepe 444,5 ms, 18 kare atlama; donanımda
16,83 ms / 48,5 ms / 4). Gerçek tearing yolu da yok: oynatma sırasında DRM'de
**tek düzlem** aktif (`Cluster1-win0`, XR24, 2560x1440 tam ekran), ayrı video
overlay düzlemi yok. Çözümü ekran modu kararında (120 Hz veya 60 Hz mod),
bu görevin kapsamı değil.

### Tarayıcının kapatılması gerçekti değildi — oldu (21 Eylül 2026)

Birimin `DevicePolicy=closed` ve `DeviceAllow=` satırları **tarayıcıya hiç
uygulanmıyordu**. Sebep `PAMName=login`: `pam_systemd` süreçleri birimin
cgroup'undan alıp login session scope'una taşıyor, cgroup üzerinden zorlanan ne
varsa arkada kalıyor.

    sway ve chromium   /user.slice/user-0.slice/session-437.scope
    birim              /system.slice/mediabox-browser.service

Ölçüm, hiçbir satırın izin vermediği `/dev/loop-control` ile — beklenen / gerçekleşen:

| | beklenen | gerçekleşen |
|---|---|---|
| aynı listeyi taşıyan temiz kapsam | RED | **RED (EPERM)** |
| çalışan tarayıcının içinde | RED | **AÇILDI** |

Çözüm cgroup'u zorlamak değil, kısıtı **mount namespace'ine** taşımak:
namespace exec anında kuruluyor ve süreç sonradan hangi cgroup'a taşınırsa
taşınsın miras kalıyor. `PrivateDevices=yes` + adı adına `BindPaths=`.

| | önce | sonra |
|---|---|---|
| tarayıcının gördüğü `/dev` düğümü | 196 | **26** |
| `/dev/mem`, `/dev/mmcblk0`, `/dev/loop-control` | açılabiliyor | **namespace'te yok** |
| NPU render düğümü | açılabiliyor | **Permission denied** |
| donanım çözme | aktif | **aktif** (`fdc38100.rkvdec-core`) |

Ölçerken çıkan iki tuzak, ikisi de dosyada yazılı: `libseat` oturumdaki **her**
DRM cihazını `stat` ediyor, NPU'nun *kartı* gizlenirse sway açılmıyor (kapatılan
yalnız render düğümü); ve Mali'nin GBM'i tamponlarını `/dev/dma_heap/system`'den
alıyor, o bağlanmazsa `Failed to create GBM device`.

**NPU tuzağı.** Chromium'un Ozone katmanı videoyu çalıştıracak render düğümünü
`/dev/dri` taramasıyla seçiyor ve `Preferred drm_render_node not found` diyor —
yani doğru düğümü sıralama şansına buluyor. NPU erişilebilirken `picking rknpu`
deyip VA-API'yi sinir hızlandırıcısına kurdu; `chrome://gpu` → Decoding boş,
4K VP9 CPU'ya düştü. Aynı klip, aynı saniye: **%571 → %131** tek çekirdek
cinsinden. sway altında doğru düğümü alıyor — yani tehlike **gizli, aktif
değil**. Düğümü unit dosyasında adıyla kapatmak denendi ve **reddedildi**:
`renderD129` bir tahtada bir boot'ta verilmiş numara, `run-host-tests.sh` bunu
üretim dosyasında haklı olarak kabul etmiyor. Bunun yerine doğrulayıcı çalışan
tarayıcıya hangi düğümü tuttuğunu soruyor ve sürücüsünü ekranınkiyle
karşılaştırıyor — farklı sıralamalı bir tahtada da geçerli kalan soru bu.
Kalıcı kapatma platform keşfinin işi; açık iş olarak aşağıda.

Doğrulayıcı `mediabox-browser-verify` bunların hepsini sorguluyor; çalışan
tarayıcının `/dev`'ini host'unkiyle karşılaştıran canlı kontrol de içinde.

### Elenen yollar (21 Eylül 2026)

Ayrıntılı araştırma raporu: [tarayici-titreme-arastirmasi.md](tarayici-titreme-arastirmasi.md).

**Kapandı.** Sebep, sway yapılandırmasındaki `output * bg` satırıydı: bir renk
ayarı değil, çıktı boyutunda bir sahne düğümü. wlroots direct scanout'u yalnız
sahnede tek düğüm varken yapıyor ve Chromium'un `AB24` yüzeyi alfa kanallı
olduğu için o dikdörtgen elenemiyordu — scanout hiç denenmiyordu, bu yüzden
wlroots bir red mesajı da yazmıyordu. Satır kaldırıldı,
`WLR_SCENE_DISABLE_DIRECT_SCANOUT=1` kaldırıldı. VOP2 artık Chromium'un
tamponunu tarıyor (`AB24`, wlroots'unki `XR24`); 4K VP9'da CPU %131,5 → **%91,0**,
GPU %46-57 @1000 MHz → **%18 @300 MHz**, 861 karede **0 düşük**, fan 100 → 50.


Titreme/yırtılma için üç mimari aday ölçülüp kapatıldı. Tekrar denenmesin:

* **Weston** (14.0.2, kiosk-shell, aynı mod ve aynı Chromium bayrakları). Video
  yine donanım düzlemine çıkmadı — Chromium videoyu ayrı yüzey olarak teslim
  etmiyor, atanacak bir şey yok; üstelik Chromium'un VA-API'si kırılıyor
  (`PreSandboxInitialization() ... failed to find a suitable render node`,
  `vainfo` aynı düğümde çalışırken). YouTube 4K VP9 tam ekran:

  | | sway | Weston |
  |---|---|---|
  | Chromium CPU | **%131,5** | **%571,4** |
  | compositor CPU | %8,2 | %12,9 |
  | GPU yük (ort/maks) | %46 / %57 | %30 / %36 |
  | donanım çözme | aktif | **yok** |
  | donanım düzlemi | 1 | 1 |

  Weston'da GPU'nun düşük görünmesi iyi haber değil: işi CPU yapıyordu.

* **Compositor'süz Chromium** (Kodi ve native kabuk gibi doğrudan DRM'de).
  İmkânsız: Debian ikilisinde Ozone DRM/GBM platformu derlenmemiş
  (`ozone_platform_drm`, `OzonePlatformDrm`, `DrmThread` — hiçbiri yok).

* **`tearing-control` protokolü / async page flip.** sway sunuyor,
  **Chromium bağlanmıyor** — `wp_tearing_control_v1` ikilide geçmiyor bile.

## 6b. Tarayıcı — AV1 donanım çözme ve cadence (22 Eylül 2026)

Plus üzerinde ölçüldü. Bölüm 6'daki VA-API yolu H.264/HEVC/VP9 taşıyordu ama
AV1 taşımıyordu; artık tarayıcının tek donanım çözücü arka ucu **V4L2/RKMPP** ve
AV1 dahil hepsi onun üzerinden gidiyor.

**Silikon zaten vardı.** RK3588'in AV1 çözücüsü rkvdec çekirdeklerinden ayrı bir
blok: `av1d@fdc70000`, DT'de `status: okay`, çekirdek
`DEVICE[ 4]:AV1DEC HW_ID:0x80019000` diye ilan ediyor. Kontrollü 4K60 AV1 Main
klibi ürünün kendi ffmpeg'iyle (`av1_rkmpp`) çözüldü: 8-bit 1800/1800 kare
67 fps, 10-bit 1800/1800 kare 60 fps (çıkış `nv15`), `fdc70000.av1d` %64–82,
rkvdec %0. Yani eksik olan donanım değil, ona giden yoldu.

**Neden VA-API değil.** VA-API'nin AV1 giriş noktası sürücüye tarayıcının zaten
ayrıştırdığı tile verisini veriyor; MPP'nin genel AV1 çözücüsü ise akışın
kendisini istiyor. İkisini birleştiren bir şey yok. Chromium'un diğer arka ucu,
V4L2 stateful çözücü, tam da akış gönderir — MPP'nin şekli bu.

**Chromium derlenmedi.** Debian'ın Chromium 153'ü her iki arka ucu da içinde
taşıyor ve hangisinin çalışacağını `media/base/media_switches.h` yazıyor:
*"When both VA-API and V4L2 are compiled in, selects the active backend:
disabled (default) => VA-API, enabled => V4L2. Toggle via
`--enable-features=PreferV4L2VideoAcceleration`."* V4L2 codec tablosunda `AV01`
var ve `AV1PROFILE_PROFILE_MAIN`'e bağlı. Yol zaten oradaydı; eksik olan yolun
ucundaki cihazdı.

`scripts/build-browser-runtime.sh` o cihazı kuruyor —
`/opt/rk3588-mediabox/browser-runtime`, hepsi pinli, ürünün kendi MPP'sine
karşı derli, ikinci bir MPP yok. Cihaz düğümü bir *dosya*: libv4l-rkmpp
yeteneklerini açıldığı düğümün içeriğinden okuyor, ve birim onu kendi özel
`/dev`'inde `/dev/video0`'a bağlıyor. Host'un `/dev`'ine hiçbir şey yazılmıyor.

### Dört sessiz arıza

Hiçbiri hata mesajı vermiyordu; hepsi "çözücü kötü kare üretti" gibi görünüyordu.

1. **Fortified `open()`.** `v4l2convert.so` yalnız `open`/`open64` sarıyor.
   `_FORTIFY_SOURCE` ile derli Chromium sabit bayraklı `open()` çağrılarını
   `__open_2`/`__open64_2`'ye çeviriyor, yani sarmalayıcı **hiç** devreye
   girmiyordu. strace tarayıcının `/dev/video0`'ı açtığını gösterirken plugin
   tek satır yazmıyordu. (`packaging/v4l-utils-patches/0001`)

2. **Plugin stdout'a yazıyordu.** Chromium alt sürecinde fd 1 bir mojo IPC
   soketi. Plugin'in bütün günlüğü oraya gidip kayboluyordu; `2` ise
   yakalanabilir durumdaydı. Saatler, konuşan ama kimsenin okumadığı bir
   çözücüye harcanabilir. (`packaging/libv4l-rkmpp-patches/0002`)

3. **POLLPRI yok.** Stateful çözücüye çözünürlüğün bilindiğini söyleyen tek
   şey POLLPRI. Plugin cihaz fd'sini bir epoll fd'siyle değiştiriyor ve epoll
   fd'si POLLPRI üretemez — olay hiç sorulmuyordu. Shim artık olayı erken
   alıp saklıyor, POLLPRI'yi *yalnız gerçek olay varken* veriyor ve istemcinin
   kendi `VIDIOC_DQEVENT`'ini o saklanan olayla yanıtlıyor.
   (`packaging/v4l-utils-patches/0002`)

4. **POLLIN yanlış şeyi anlatıyordu.** Plugin'in tek "okunabilir" sinyali hem
   olay, hem geri dönen OUTPUT tamponu, hem hazır CAPTURE karesi içindi.
   V4L2'de POLLIN yalnız sonuncusu demek. Chromium bunu harfiyen alıp
   `TryAndDequeueCAPTUREQueueBuffers()` çağırıyor — ki yalnız `DCHECK` ile
   korunuyor, release'de derlenmiyor — ve henüz null olan CAPTURE kuyruğunu
   dereference ediyor: `GPU process exited unexpectedly: exit_code=11`, her
   codec'te. OUTPUT terimi kaldırıldı; olay anında POLLIN temizleniyor.
   (`packaging/libv4l-rkmpp-patches/0003`)

Ayrıca 10-bit: plugin'in tek capture formatı NV12 ve 10-bit akışta
`assert(mpp_format == MPP_FMT_YUV420SP)` GPU sürecini düşürüyordu. Çözücüye
`MPP_DEC_SET_OUTPUT_FORMAT` ile sekiz bit çıkış söyleniyor — donanım yine on
bitte kurguluyor, kendi çıkış katı NV12 yazıyor, CPU'da dönüşüm yok.
(`packaging/libv4l-rkmpp-patches/0001`)

### 144 Hz cadence

Panel 2560x1440'ta 143.999, 119.998, 74.97 ve 59.95 Hz sunuyor. Tarayıcı
Kodi gibi filme göre mod değiştiremez: bir web sayfası tek yüzey, video ne
hızda gelirse gelsin geri kalanı panelin hızında. 143.999/60 = 2.4 — 60 fps
kare tam sayıda refresh boyunca duramaz. Hiçbir kare düşmeden titrer.

`mediabox-hdmi-prepare` artık tarayıcı için en hızlı modu değil, refresh'i
60'ın veya 59.94'ün tam katı olan **en hızlı** modu seçiyor; yoksa eskisi gibi
en hızlıya düşüyor. Mod listesi EDID'den okunuyor, hiçbir timing yazılı değil.
Kodi'nin beyaz listesi ve native arayüzün politikası değişmedi.

Aynı klip, aynı yapı, yalnız mod farklı (4K60 AV1, tarayıcıda):

| kare kaç refresh durdu | 143.999 Hz | 119.998 Hz |
|---|---|---|
| 2 refresh | %58,0 | **%91,8** |
| 3 refresh | %39,8 | %0,5 |

### Ölçülen (22 Eylül 2026, Plus, 2560x1440p120)

Hepsi tarayıcının içinde, `tools/browser-video-probe.py` ile. "platform" sütunu
Chromium'un kendi `kIsPlatformVideoDecoder` cevabı; donanım sütunu
`/proc/mpp_service/load`'dan hangi bloğun **canlı** olduğu.

| test | decoder | düşük kare | donanım | cadence 2x |
|---|---|---|---|---|
| YouTube 2160p60 AV1 (`av01.0.13M.08`) | V4L2, platform | **%0,097** (7/7192) | `av1d` %59–90 | %99,6 |
| YouTube 2160p60 VP9 (`vp09.00.51.08`) | V4L2, platform | %0,132 (5/3780) | `rkvdec-core0/1` ~%14,5 | %99,7 |
| H.264 4K60, kontrollü | V4L2, platform | %0,115 (4/3469) | `rkvdec-core0/1` ~%19 | %99,9 |
| AV1 4K60 8-bit, kontrollü, **10 dakika** | V4L2, platform | %4,97 (1745/35137) | `av1d` %31–85 | %92,2 |

Codec yönlendirmesi doğru: AV1 ayrı `av1d` bloğuna, VP9 ve H.264 rkvdec
çekirdeklerine gidiyor.

**Kontrollü klip neden daha kötü.** `testsrc2` 25 Mbps'te gürültüye yakın —
YouTube'un 2160p60 AV1'inden belirgin şekilde ağır, sesi yok ve yerel diskten
geliyor. Sentetik klip **çözücü kapasitesini**, YouTube **ürünün gerçek hâlini**
ölçüyor. Düşük karelerin sunum yolundan değil çözücü kapasitesinden geldiğini
ayıran ölçüm de bu: gerçek içerikte oran %0,1'in altında.

**Direct scanout — ayrı bir regresyon, bu çalışmadan önce.** Tarama düzlemi
`XR24`, yani wlroots derliyor. Sebep bu bölümdeki hiçbir değişiklik değil:
`a72a2f9` kiosk modunu kaldırdığında Chromium pencere modunda kendi çerçevesini
çizmeye başladı ve yüzey geometrisi `2548x1418` kaldı — 2560x1440 çıkışı tam
kaplamadığı için wlroots scanout'u **denemiyor** bile. `MEDIABOX_BROWSER_KIOSK=1`
ile geometri çıkışa oturuyor ve düzlem `AB24` dönüyor. Ölçülen fark, aynı klip:

| | sekmeli (XR24) | kiosk (AB24) |
|---|---|---|
| GPU | %33 ort, 1000 MHz'e çıkıyor | **%22 ort, 300–400 MHz** |
| düşük kare | %4,97 | %4,37 |
| CPU | %121,6 | %122,8 |

Yani scanout GPU'yu ve ısıyı kurtarıyor, düşük kareyi kurtarmıyor. Bu
Chromium'da dekorasyonu kapatan bir bayrakla çözülebilirdi; bu derlemede öyle
bir feature adı yok (`WaylandWindowDecorations` ikilide geçmiyor). Sekme şeridi
ile direct scanout şu an gerçek bir takas — ürün kararı olduğu için burada
değiştirilmedi, açık iş olarak aşağıda.

### Ölçüm aracı

`tools/browser-video-probe.py` dört soruyu aynı anda soruyor: Chromium'un
çözücüye verdiği ad (DevTools Media alanı), `/proc/mpp_service/load`'dan hangi
bloğun uyandığı, CPU/GPU maliyeti, ve karelerin kaç refresh durduğu.

> **Tuzak.** `/proc/mpp_service/load` boştayken sıfırlanmıyor, **son hesapladığı
> değeri saklıyor**. Bu oturumda tam olarak bu, yazılımda çözen bir tarayıcıyı
> donanımda çözüyor gibi gösterdi. Araç değişmeyen bir okumayı "stale" diye
> işaretliyor ve yanına `/dev/mpp_service`'i kaç sürecin açtığını yazıyor.

## 6c. Tarayıcıda hover ile açılanların yanıp sönmesi (22 Eylül 2026)

Üç şikâyet tek nedenden: adres çubuğunda son karakterin bir kare görünüp
kaybolması, YouTube seek çubuğundaki önizlemenin sabit imleç altında gidip
gelmesi, hover ile açılan açılır kutunun aynı şekilde sönmesi.

**Hipotez yanlış çıktı.** Şüphe Chromium'un partial swap / buffer-age
defterindeydi. `--ui-disable-partial-swap` ile A/B yapıldı; GPU sürecinin kendi
izi (CDP `Tracing`, kategori `viz,gpu,gpu.service,cc`), 8 saniye:

| | `SkiaOutputSurfaceImplOnGpu::SwapBuffers` | `PostSubBuffer` | present/s |
|---|---|---|---|
| A (flag yok) | 479 / 480 / 412 | **0** | 51-60 |
| B (flag var) | 507 / 427 | **0** | 53-63 |

A'da da B'de de sub-buffer present yok: bu yığında viz zaten hep tam
`SwapBuffers` çağırıyor, yani kapatılacak bir partial present yoktu. Üç belirti
de B'de aynen sürdü. Flag launcher'a **eklenmedi**; ölçülen maliyeti var ve
karşılığı yok (tek küçük kare animasyonunda GPU %45,8 → %54,1, tarayıcı CPU
%83,3 → %96,8, kare süresi p50/p95 her ikisinde de 8,3/8,4 ms).

Eski/yeni tampon değişimi de doğrudan arandı ve yok: bir kez değişip sonra
sabit kalan bir kutu, 40 deneme / 640 örnek (dönen kare) ve 20 deneme / 320
örnek (4K klip oynarken) — stale 0, ping-pong 0, deneme başına tek görüntü.

**Gerçek neden compositor'da.** sway imleci gizlerken seat'in pointer focus'unu
da bırakıyor, yani sayfaya pointer leave gidiyor; bu dosyadaki `hide_cursor 1`
bunu fare son raporundan bir milisaniye sonra yapıyordu. Gerçek bir fareyle
ölçüldü — uinput üzerinden 10 Hz'de bir piksel gidip gelen bir aygıt, yani elin
altındaki optik farenin yaptığı (`swaymsg ... cursor set` ile ışınlanan imleç
libinput'tan geçmediği için bunu hiç sınamıyor; ilk ölçümüm bu yüzden yanlış
çıkmıştı):

| ayar | sayfaya giden olaylar | sonuç |
|---|---|---|
| `hide_cursor 1` | enter/leave **5,4/s** | YouTube önizlemesi yanıp sönüyor |
| `hide_cursor 8000` | tek enter, leave yok | önizleme sabit, kontroller açık |
| `hide_cursor` yok | tek enter, leave yok | aynı |
| `hide_cursor when-typing` | 12 tuşta 1 leave | açık menü kapanıyor |

`config/sway-browser.conf` artık `seat * hide_cursor 8000` yazıyor ve
`when-typing` satırı yok. Sekiz saniye, çünkü elde duran fare imleci canlı
tutuyor, dokunulmayan televizyon ise imleci yine de kaldırıyor. Bırakılan tek
şey ölçüldü: fareye hiç dokunulmazsa imleç sekiz saniye sonra bir kez geri
alınıyor, yani sekiz saniyedir dokunulmamış bir menü kapanıyor — yanıp sönme
değil. `mediabox-browser-verify` kısa bir `hide_cursor` değerini ve
`when-typing`'i FAIL veriyor.

**Düzeltme sonrası, gerçek fare elin altındayken:** hover açılır kutusu 10 s
boyunca 244 örnekte **tek görüntü**; YouTube seek önizlemesi 10 s boyunca tek
`pointerenter`, kontroller açık, önizleme ayakta.

**Adres çubuğu kapanmadı.** Altı ayrı ölçümde — yazarken ve yazdıktan sonra,
imleç üstünde dururken ve titrerken, flag'li ve flag'siz — mürekkep genişliği
hiç geri gitmedi (0 regresyon) ve bölgedeki tek değişiklik **2x34 pikselik bir
sütun**, yani metin imlecinin yanıp sönmesi. Düzeltmeden sonra kullanıcı
diğer iki belirtinin gittiğini, bunun **sürdüğünü** bildirdi. Yani nedeni
`hide_cursor` değil ve ölçüm aleti onu henüz yakalayamıyor; açık iş.

Ölçüm aletleri: `tools/ui-frame-integrity.py` + `.html` (ekranı compositor'dan
`grim` ile örnekler; koordinatları sayfadan alır), `tools/uinput-tremble.py`
(gerçek fare).

## 7. Açık işler

### Önce

1. **Oynatıcı katmanının görünümü — referans bekliyor.** İki deneme reddedildi:
   ilki herkesin kullandığı hap düğmeler ve VCR simgeleri, ikincisi onların
   çıkarılmışı — bir dil değil, bir eksiklik. Kullanıcının vereceği referansa
   bakılarak yapılacak. **Üçüncü bir zevk denemesi yapılmayacak.**
2. **CPU.** mpv %6-9, Kodi %2. İlk bakılacak yer: `vo_mediabox` kare başına
   `PRIME_FD_TO_HANDLE` + `ADDFB2` yapıyor; MPP havuzu 3-4 tampon döndürüyor,
   tampon başına bir kez yapılıp önbelleklenebilir.
3. **Detay ekranının düğmeleri** — 1 geldikten sonra, önce değil.
4. **`main.rs` 2.367 satır / 88 KB.** Tek başına ~25.000 token ve
   değişikliklerin çoğu oraya düşüyor. Ekran başına ayrılmalı.

### Sonra

5. Ses çıkışı seçimi (çekirdek USB kulaklığı görüyor, üründe seçecek yer yok)
6. Wi-Fi (NetworkManager yok)
7. Bluetooth (`bluetooth.service` inactive)
8. Web UI'da donanım hızlandırmalı önizleme/oynatma
9. Home yoğunluğu — sağda ve altta boş alan
10. Süreç içi DRM yeniden modeset (hot plug şu an birim yeniden başlatmayla telafi)
11. HDR regresyonu — takılı ekran SDR monitör, `edid` 0 bayt; HDR TV takılınca

### Bilinen açık kusurlar

* Sarmada çubuk yalnız işaretin yerini gösteriyor; filmin gerçek konumu çubuk
  üzerinde işaretlenmiyor.
* Web UI'dan başlatılan film sahipleniliyor ama adı ve süresi gelmiyor —
  `MediaPlayHere`'ın `title`/`duration_seconds` alanlarını web arayüzü henüz
  göndermiyor.
* Kodi'nin "Şimdi Oynatılan" ekranı hâlâ eski simge setini kullanıyor.
* ~~Tarayıcıda AV1 donanımda çözülmüyor~~ — **kapandı, 22 Eylül 2026**, bölüm 6b.
  Çözüm VA-API'yi genişletmek değil, tarayıcının zaten taşıdığı V4L2 arka ucunu
  seçmek oldu.
* **Direct scanout kapalı** (`XR24`). Sebebi `a72a2f9`'daki kiosk kaldırma:
  pencere modunda Chromium kendi çerçevesini çiziyor ve yüzey geometrisi
  `2548x1418` kalıyor, çıkışı tam kaplamıyor. Ölçülen maliyet GPU'da %22 → %33
  ve saatin 400 MHz → 1000 MHz'e çıkması. `MEDIABOX_BROWSER_KIOSK=1` scanout'u
  geri getiriyor ama sekme şeridini alıyor. Bu derlemede dekorasyonu kapatan bir
  Chromium bayrağı yok. Karar ürün tarafında: sekme mi, scanout mu.
* **Kontrollü 4K60 AV1'de düşük kare %4,4-5,0.** Gerçek içerikte (YouTube
  2160p60 AV1) %0,097 ölçüldü, yani ürün durumunda sorun değil; ama 25 Mbps
  sentetik akışta çözücü yetişemiyor ve sebep `av1d` doygunluğu değil (%85'te
  tavan yapmıyor). Bakılacak yer CAPTURE kuyruğu derinliği ve tampon dönüş
  gecikmesi.
* ~~Tarayıcıda hover ile açılanlar yanıp sönüyor~~ — **kapandı, 22 Eylül 2026**,
  bölüm 6c. Neden Chromium değil, `hide_cursor`'ın pointer focus'u bırakması.
  Kullanıcı doğruladı: hover açılır kutusu ve YouTube seek önizlemesi sabit.
* **Adres çubuğunda son karakterin yanıp sönmesi sürüyor.** Bölüm 6c'deki
  düzeltme diğer iki belirtiyi kaldırdı, bunu kaldırmadı; altı ölçümde
  üretilemedi de. Ayrı bir neden var ve henüz ölçülmedi.
* **Yırtılma (tearing) sürüyor** — video izlerken ve sayfalarda gezinirken,
  kullanıcı tarafından bildirildi. `hide_cursor` düzeltmesinden önce de vardı,
  sonra da var; yani bu çalışmanın getirdiği bir şey değil. Ölçülmedi. Bakılacak
  ilk yer düzlemin `XR24` olması, yani wlroots'un derliyor olması — direct
  scanout açıkken aynı şey oluyor mu, bilinmiyor.
* Tarayıcıda video kodlama (encode) yok — yalnız çözme.
* Chromium'un VA-API render düğümü seçimi sıralama şansına bağlı
  (`Preferred drm_render_node not found`). sway altında doğrusunu alıyor,
  ölçüldü; ama NPU'nun render düğümü erişilebilir kaldığı sürece bu garanti
  değil. Kalıcı çözüm `mediabox-platform` keşfinin düğümü çözüp birime drop-in
  yazması. `mediabox-browser-verify` şu an sapmayı yakalıyor, engellemiyor.

## 8. Taşınabilirlik ve ikinci cihaz (Plus)

**Durum: hazır, kurulmadı.** Bu bölümün önceki hâli dört sabit ve ayrı bir
önyükleme zinciri sayıyordu; hepsi kapandı ya da yanlış çıktı.

### Kendi medya çalışma zamanı

MediaBox artık `/opt/rk3588-screenbridge` prefix'ini **ne okuyor ne yazıyor**.
MPP, librga ve ffmpeg-rockchip sabitlenmiş revizyonlardan
`/opt/rk3588-mediabox/media-runtime` altına derleniyor
(`scripts/build-media-runtime.sh`); iki oynatıcı da o prefix'i adlandıran bir
RPATH taşıyor, yani hiçbir ortam değişkeni hangi kod çözücüyü aldıklarını
belirlemiyor. Ultra'da ölçüldü — hiçbir kütüphane yolu ayarlanmadan:

```
mpv  RUNPATH /opt/rk3588-mediabox/media-runtime/lib
     librga.so, librockchip_mpp.so.1 → media-runtime/lib
     /opt/rk3588-screenbridge eşlemesi: 0   (önce: 8)
```

Pinler ve nereden okundukları: `docs/platform/custom-runtime.md`.

### Donanım sahipliği sözleşmesi

DRM master iki kez tutulamaz. Ekranı alan her MediaBox birimi
(`mediabox-tv-ui`, `kodi`, `mediabox-browser`)
`Conflicts=screenbridge-daemon.service` + `After=` aynı birim diyor — yarış
değil, belirlenmiş geçiş. Denetim düzlemi, medya işçisi ve akış sunucusu ekran
donanımına dokunmuyor ve hiçbir kilit ilan etmiyor.

### Önyükleme: iki kart artık aynı

Eski kayıt Plus'ı EDK II → EFI GRUB → `/dev/nvme0n1p2` diye tarif ediyordu.
**Canlı ölçüm (2026-09-16) bunun geçmişte kaldığını gösteriyor:**

| | Ultra | Plus |
|---|---|---|
| Önyükleme | Rockchip DDR init → BL31 → Armbian U-Boot | **aynı** |
| Önyükleme yöneticisi | `boot.scr` + `armbianEnv.txt` | **aynı** |
| EFI/GRUB | yok (`/boot/efi` yok, `efibootmgr` yok) | **yok** |
| Kök | eMMC `/dev/mmcblk0p1` | NVMe `/dev/nvme0n1p1` |
| DTB | `rk3588-orangepi-5-ultra.dtb` | **stok** `rk3588-orangepi-5-plus.dtb` |
| Overlay | yok | `user_overlays=orangepi5-plus-screenbridge-hdmirx` |
| CMA | 256M | 512M |

### Plus'ta salt-okunur platform probu

Yeni keşif ikilisi `/tmp` altına geçici kopyalandı, çalıştırıldı ve silindi.
MediaBox **kurulmadı**, hiçbir paket kurulmadı, hiçbir servis değişmedi,
ScreenBridge'e dokunulmadı.

| | Plus'ta bulunan |
|---|---|
| KMS aygıtı | `card0` (rockchip-drm), konektör sahibi olduğu için |
| Render aygıtı | `renderD128`, aynı ana aygıt |
| Konektörler | `HDMI-A-1`, `HDMI-A-2`, `DP-1` |
| Verici eşlemesi | `fde80000.hdmi`, `fdea0000.hdmi`, `fde50000.dp` |
| Ses ucu | `rockchiphdmi0`, `rockchiphdmi1`, `rockchipdp0` |
| CEC | `/dev/cec0`, `/dev/cec1`, DP'de yok |
| Bağlı çıkış | **hiçbiri** — o karta ekran takılı değil |

Ses eşlemesi ScreenBridge'in bağımsız ölçtüğü platform matrisiyle birebir
tuttu. Dikkat: Plus'ta `HDMI-A-1` → `rockchiphdmi0`, Ultra'da tek konektör →
`rockchiphdmi1`. Aynı konektör adı, farklı kart — `amixer -c rockchiphdmi1`
yazan her şey kartlardan birinde yanlış televizyona sesleniyordu.

### Kapanan sabitler

| Eski | Şimdi |
|---|---|
| `amixer -c rockchiphdmi1` | Seçili çıkışın kartı, DT codec phandle'ı üzerinden |
| Birimlerde `/dev/cec0` | `DeviceAllow=char-cec`; adaptör verici aygıtın çocuğu |
| `MEDIABOX_KMS_NODE=/dev/dri/card0` (birim dosyasında) | Keşif; değişken yalnız teşhis için kaldı |
| `card0-HDMI-A-1` (betikler) | `mediabox-platform connector-path` |
| `--require-cec` | Kalktı — DP'de CEC yok, bu yetenek eksikliği |
| `MEDIABOX_HOST=10.27.27.25` varsayılanı | Varsayılan yok; verilmezse betik durur |

### Sonraki kapı için kalanlar

1. Plus'a bir ekran tak ve `mediabox-platform inspect` ile bağlı konektörün
   `measured` bağlandığını doğrula.
2. Plus'ta ScreenBridge daemon'ı çalışırken MediaBox ekran sahibi bir birimi
   başlat; geçişin belirlenmiş olduğunu (yarış değil) gözle.
3. Medya çalışma zamanını Plus'ta derle — Ultra'daki aynı pinlerle.
4. Stok çekirdekle MediaBox'ı açmayı dene: HDMI RX yaması TX'i ilgilendirmiyor,
   ama bu hâlâ çıkarım.

## 9. Nerede ne var

```
rust/crates/mediabox-tv      TV arayüzü (Slint + kendi DRM/GBM platformu)
rust/crates/mediaboxd-rs     denetim düzlemi, HTTP + unix soket
rust/crates/mediabox-ui      ÜRETİM Web UI (wasm)
rust/crates/mediabox-core    iki tarafın paylaştığı kapalı istek/yanıt tipleri
rust/crates/mediabox-platform  bu kartın ne olduğu: DRM/ALSA/CEC keşfi + ikili
media/                       medya çekirdeği (Python), cihazda çalışır
packaging/                   systemd birimleri, udev kuralları, kabuk betikleri,
                             packaging/mpv/vo_mediabox.c
scripts/                     çapraz derleme ve dağıtım (deploy-mediabox-v3.sh)
src/ + tools/ + tests/       C++ HDR probları ve bekçileri — KABUL EDİLMİŞ TEMEL
config/                      cihaza kurulan JSON'lar
docs/                        mimari notları
docs/platform/               pinlenmiş özel yığın, çalışma zamanı keşfi
```
