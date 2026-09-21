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
* Tarayıcıda AV1 donanımda çözülmüyor (sürücüde yok): VA-API karolara başlıksız
  veri veriyor, MPP tam OBU istiyor. YouTube bu kutuda VP9 seçtiği için pratikte
  ısırmadı; ısırırsa çare hesap tarafında codec tercihi.
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
