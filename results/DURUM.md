# MediaBox — durum

Bu dosya **tek yaşayan belge**. Oturum başına yeni bir tarihli rapor yazılmaz;
burası güncellenir.

Cihaz: Orange Pi 5 Ultra (RK3588), `6.1.115-vendor-rk35xx-screenbridge-hdmirx-audio`.
Panel: HDMI-A-1. Adres repoda sabit değil — `MEDIABOX_HOST` / `scripts/env.sh`.

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
Rust + Slint → FemtoVG/GLES → Mali-G610 (/dev/dri/renderD128)
     → GBM armsoc → DMA-BUF/PRIME → Rockchip /dev/dri/card0
     → DRM/KMS/VOP2 → HDMI
```

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

* Split render/display: Mali GBM `renderD128`'de, KMS `card0`'da, dma-buf export
  → PRIME import → ADDFB2 → page flip. Compositor yok, Wayland yok, llvmpipe yok.
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
* `mediabox-kiosk-smoke` 16/16.

## 5. Gömülü oynatıcı (15 Eylül 2026)

Film kendi VOP2 düzleminde; arayüz DRM master'da kalıyor.

```
Video Port0  PLANE_MASK  Cluster0=0x1 … Esmart0=0x4 …  value: 5
Cluster0-win0  AR24  zpos 11   ← arayüz, üstte, alfalı
Esmart0-win0   NV15  zpos 0    ← film, altta, donanımda ölçekli
```

* Çözücü: mpv 0.41, cihazda Rockchip ffmpeg'e karşı derli, üç yama.
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

## 6. Açık işler

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

## 7. Temiz imaj ve ikinci cihaz (Plus)

### Bu repodan yeniden üretilebilenler

| Ne | Nasıl |
|---|---|
| Mali G610 kullanıcı alanı | `scripts/install-mali-runtime.sh` — sürüm sabitli .deb, URL ile |
| Kodi 22.0-BETA2 | `scripts/build-kodi.sh` — kartta derler |
| Oynatıcı (mpv 0.41) | `scripts/build-mediabox-player.sh` + `packaging/mpv-patches/` + `packaging/mpv/vo_mediabox.c` |
| node 22.23.1, Stremio web, akış sunucusu | `packaging/upstream.env` — revizyon ve SHA256 ile sabitli |
| Denetim düzlemi, TV arayüzü, Web UI | `scripts/deploy-mediabox-v3.sh` (çapraz derleme) |
| Birimler, udev, config, smoke | `packaging/` |

### İki temel: bu repoda yok, **yan repoda var**

`~/Projeler/rk3588-screenbridge` (main, `582e1d3`) — ikisi de orada, sabitlenmiş:

| Önkoşul | Tarifi |
|---|---|
| `/opt/rk3588-screenbridge` (RKMPP FFmpeg) | `scripts/build-media-stack.sh` — `rockchip-linux/mpp`, `airockchip/librga`, `nyanmisaka/ffmpeg-rockchip` klonlar, derler, prefix'e kurar ve `rkmpp` kodlayıcılarını doğrular |
| Çekirdek | `armbian/linux-rockchip` `fd9f82366e235b8afbdf516765210e97d24dce93` + `patches/kernel/0001-hdmirx-enable-i2s-capture.patch`; release adı ve `Image` SHA-256 `474eb0a3…` kayıtlı, cihazda `rollback-stock-kernel.sh` var |

**Ama MediaBox o çekirdeğe muhtemelen ihtiyaç duymuyor.** Yama okundu: yaptığı
tek şey HDMI **alıcısının** I2S capture DAI'sini açmak; verici (TX) tarafı
değişmeden çıkış-only kalıyor. MediaBox HDMI TX kullanıyor — yani stok Armbian
vendor çekirdeği yetmeli. **Bu bir çıkarım, ölçüm değil**: MediaBox stok
çekirdekle hiç açılmadı.

### Plus'a taşımak — yan repo bunu zaten ölçmüş

`results/orangepi5-plus-vendor/ultra-plus-hdmirx-4k60-differential-2026-09-05.md`
(Gate 2E) iki kartı yan yana koymuş:

| | Ultra | Plus |
|---|---|---|
| Önyükleme | DDR init → BL31 → Armbian U-Boot | **EDK II v2.70 → EFI GRUB** |
| DTB seçimi | `armbianEnv.txt` içindeki `fdtfile` | GRUB girdisinde açık `devicetree` |
| DTB | `rk3588-orangepi-5-ultra.dtb` | `rk3588-orangepi-5-plus-screenbridge.dtb` |
| Kök | eMMC `/dev/mmcblk0p1` | **NVMe `/dev/nvme0n1p2`** |
| Çekirdek | `…-screenbridge-hdmirx-audio` `#3` | **stok** `6.1.115-vendor-rk35xx` `#1` |
| HDMI RX denetleyici DT'si | kayıt penceresi, IRQ, saat, reset, güç alanı | **birebir aynı** |
| HDMI-IN ses kartı indeksi | 1 | 3 |

Yani önyükleme zinciri ve kök aygıtı tamamen farklı; SoC çevre birimleri aynı.

MediaBox tarafında değişmesi gereken, Ultra'ya sabitlenmiş dört yer:

```
packaging/mediabox-hdmi-prepare:38   amixer -c rockchiphdmi1     ← kart adı sabit
packaging/systemd/*.service          /dev/cec0                   ← Plus'ta iki HDMI
scripts/capture-*.sh, run-mp1*.sh    card0-HDMI-A-1              ← Plus'ta iki çıkış
/boot/armbianEnv.txt                 rk3588-orangepi-5-ultra.dtb ← Plus'ta GRUB + kendi DTB
```

Kart indeksini isimden çözme sorunu yan repoda zaten çözülmüş —
`scripts/hdmirx-audio-loopback.sh` bunu yapıyor; aynı yaklaşım buraya alınmalı.

**TV arayüzünün kendisi bu listede değil:** `find_output` ilk bağlı `HDMI-A`
konektörünü seçiyor, video düzlemini isimle değil `SetPlane` deneyerek buluyor.
VOP2 düzlem haritası farklı olsa bile kendi bulur.

### Temiz imajın masrafı

| Adım | Nerede | Tahmin |
|---|---|---|
| Armbian vendor imajı + önyükleme zinciri (Plus'ta EFI/GRUB + NVMe) | screenbridge | saatler, tek seferlik |
| `build-media-stack.sh` (MPP + RGA + ffmpeg, kartta) | screenbridge | ~1 saat |
| Özel çekirdek — **yalnızca HDMI RX sesi gerekiyorsa** | screenbridge | ~2 saat |
| Mali kullanıcı alanı | bu repo | ~5 dk |
| Kodi (kartta, 8 çekirdek) | bu repo | saatler |
| mpv oynatıcı | bu repo | ~10 dk |
| node + Stremio web/sunucu | bu repo | 15-30 dk |
| Rust çapraz derleme + `deploy-mediabox-v3.sh` | bu repo | ~5 dk |

Kabaca **bir iş günü**, ve belirsizliğin tamamı önyükleme zincirinde — Plus'un
EFI/GRUB + NVMe düzeni Ultra'nınkinden farklı, MediaBox'ın hiç görmediği bir yol.

### Kapatılması gereken işler

1. `scripts/bootstrap-appliance.sh` — boş Armbian'dan çalışan cihaza tek yol;
   mevcut betikleri sırayla çağırır, iki önkoşul yoksa **yüksek sesle** durur.
2. Çekirdek ve FFmpeg'i yol olarak değil, `rk3588-screenbridge` reposunun
   **sabit commit'i** olarak kaydet; `packaging/upstream.env` bunun için zaten
   doğru yer.
3. HDMI ses kartı adını sabit yazmak yerine keşfet
   (`/proc/asound/cards` içinden `rockchiphdmi*`).
4. Kabuk dışındaki betiklerdeki `card0-HDMI-A-1` sabitlerini bağlı konektörü
   bulacak şekilde değiştir.

## 7. Nerede ne var

```
rust/crates/mediabox-tv      TV arayüzü (Slint + kendi DRM/GBM platformu)
rust/crates/mediaboxd-rs     denetim düzlemi, HTTP + unix soket
rust/crates/mediabox-ui      ÜRETİM Web UI (wasm)
rust/crates/mediabox-core    iki tarafın paylaştığı kapalı istek/yanıt tipleri
media/                       medya çekirdeği (Python), cihazda çalışır
packaging/                   systemd birimleri, udev kuralları, kabuk betikleri,
                             packaging/mpv/vo_mediabox.c
scripts/                     çapraz derleme ve dağıtım (deploy-mediabox-v3.sh)
src/ + tools/ + tests/       C++ HDR probları ve bekçileri — KABUL EDİLMİŞ TEMEL
config/                      cihaza kurulan JSON'lar
docs/                        mimari notları
```
