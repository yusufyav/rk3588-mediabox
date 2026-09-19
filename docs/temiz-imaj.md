# Temiz imajdan çalışan cihaza

Boş bir karttan MediaBox'a giden tek yol. İki karta göre ayrılmış yerler
işaretli: **[Ultra]** / **[Plus]**.

Artık **tek repo yetiyor**. MediaBox kendi donanım medya çalışma zamanını
(`Rockchip MPP`, `librga`, `ffmpeg-rockchip`) kendisi derliyor ve kendi
prefix'ine kuruyor; `rk3588-screenbridge` yalnızca mühendislik referansı ve
(yalnızca HDMI **RX** gerekiyorsa) çekirdek kaynağı olarak devrede.

| Repo | Ne verir |
|---|---|
| `rk3588-mediabox` | Medya çalışma zamanı, Mali kullanıcı alanı, Kodi, oynatıcı, denetim düzlemi, arayüzler, birimler |
| `rk3588-screenbridge` | (isteğe bağlı) HDMI RX için özel çekirdek; ayrıca platform referans ölçümleri |

> **Durum (2026-09-19, ölçüldü).** Her iki kart da o gün sıfır Armbian
> vendor-6.1 imajından kuruldu ve artık **aşağıdaki derleme yolundan değil,
> tek komutluk kurucudan** geçiyor (bkz. bölüm 1.5):
>
> * **Ultra** — kurulum tamamlandı; kurulu ürün doğrulayıcısı kart üzerinde
>   yeniden koşuldu ve **tam PASS** verdi (atlanan kontrol yok). Televizyon
>   HDMI 1'de, 4K HDR içerikte konektör `RGB888_1X24` + `hdr_type[SDR]`,
>   film düzlemi NV15 / `hdr_type[HDR10]` + `hdr2sdr[1]`.
> * **Plus** — `install-mediabox.sh` → `INSTALLED, NOT YET PROVEN`: karta hiçbir
>   ekran takılı olmadığı için kurulum **headless** tamamlandı, ekran gerektiren
>   dört kontrol ve film kapısı `SKIP`/`NOT RUN` olarak işaretlendi. Kurulumun
>   ekran istemeyen her parçası ölçüldü: 170 çalışma zamanı paketi, 0 derleme,
>   `/opt/rk3588-mediabox` 1.1 GB, `mediaboxd-rs` + `mediabox-media-worker` +
>   `stremio-server` `active`.
>
> Plus'ta geriye yalnız ekranlı kapılar kaldı; kablo takıldığında bölüm 6'nın
> sonundaki üç komut onları kapatır.

> **Aşağıdaki bölümler (2-5) üretim yolu değildir.** Ürün, çalışan bir Ultra'dan
> yakalanmış prebuilt arşivdir ve kurulum hiçbir şey derlemez. Derleme zinciri
> yalnız yeni bir altın sürüm yakalanırken çalışır.

---

## 0. Önce bilinmesi gerekenler

Her iki kartın da **canlı ölçülmüş** hâli (2026-09-16):

```
Ultra : Rockchip DDR init → BL31 → Armbian U-Boot → boot.scr → armbianEnv.txt
        kök: eMMC /dev/mmcblk0p1 (ext4)
        DTB: rockchip/rk3588-orangepi-5-ultra.dtb, overlay yok
        extraargs: cma=256M
        HDMI TX: tek verici (hdmi1 @ fdea0000) → tek konektör, tek CEC
        ALSA: rockchiphdmi1 · rockchiphdmiin · rockchipes8388

Plus  : Rockchip DDR init → BL31 → Armbian U-Boot → boot.scr → armbianEnv.txt
        kök: NVMe /dev/nvme0n1p1 (ext4)
        DTB: rockchip/rk3588-orangepi-5-plus.dtb — **stok**
             + kullanıcı overlay'i: orangepi5-plus-screenbridge-hdmirx
        extraargs: cma=512M
        HDMI TX: iki verici (hdmi0 @ fde80000, hdmi1 @ fdea0000) + DP (dp0)
                 → üç konektör, iki CEC, DP'de CEC yok
        ALSA: rockchipdp0 · rockchiphdmi0 · rockchiphdmi1 · rockchiphdmiin ·
              rockchipes8388
```

> **EDK II / EFI GRUB yolu geçmişte kaldı.** Bu belgenin daha eski bir sürümü
> Plus'ı EDK II v2.70 → EFI GRUB üzerinden ve kökü `/dev/nvme0n1p2`'de tarif
> ediyordu. Bugün ölçülen hâli yukarıdaki: her iki kartta da `/boot/efi` yok ve
> `efibootmgr` kurulu değil; ikisi de Armbian U-Boot ile açılıyor. O yol
> ScreenBridge'in 2026-09-05 tarihli raporlarında **tarihsel kanıt** olarak
> duruyor ve güncel dağıtım yolu değildir.

SoC çevre birimleri (HDMI RX kayıt penceresi, IRQ'lar, saatler, resetler, güç
alanı) iki kartta **birebir aynı**. Farkın tamamı kök aygıtı, etkin HDMI verici
sayısı ve çevre birimi indekslerinde — ve **hiçbiri ürün mantığına girmiyor**:
çalışma zamanı keşfi bunları topolojiden çözüyor (bkz.
[`platform/runtime-discovery.md`](platform/runtime-discovery.md)).

## 1. İmaj ve önyükleme — `rk3588-screenbridge`

**Armbian vendor-6.1 imajı** gerekiyor; Ubuntu preinstalled imajları ve Android
SPI/NVMe arşivleri **seçilmez** (eşleşen U-Boot/BL31 zinciri kurmazlar).

1. Kartı doğrula: `cat /proc/device-tree/model`, `findmnt /`,
   `lsblk -o NAME,PATH,SIZE,MODEL,SERIAL,TRAN,FSTYPE,MOUNTPOINTS`
2. Mevcut kanıtı sakla: DTB, EDID, cmdline, DTB/SPI hash'leri, bölüm düzeni
3. Karta özgü Armbian vendor-6.1 imajı + eşleşen U-Boot / BL31 / (gerekirse)
   BL32 setini indir ve **checksum'la**
4. **[Plus]** Yalnız tespit edilen NVMe aygıtını yaz. İlk açılışta imajın **stok
   DTB'si** ile aç ve Rockchip U-Boot yolunu doğrula. Ultra imajını yeniden
   kullanma.
5. **[Ultra]** eMMC'ye yaz; `armbianEnv.txt` içindeki `fdtfile`'ın
   `rockchip/rk3588-orangepi-5-ultra.dtb` olduğunu doğrula.

**Doğrulama:** `uname -a`, `cat /etc/os-release`, `findmnt /` — hedeflenen
çekirdek, dağıtım ve kök aygıtı.

## 1.5. Üretim kurulumu — tek komut

İmaj açıldıktan sonra üretim yolu bu; 2-5. bölümler yalnız yeni sürüm
yakalarken gerekir.

```bash
git clone https://github.com/yusufyav/rk3588-mediabox
cd rk3588-mediabox && sudo ./scripts/install/install-mediabox.sh
```

Kurucu `releases/current.env` içindeki tag + sha256'yı indirir, arşivin kendi
manifestini doğrular, çalışma zamanı paketlerini kurar, `/opt`'u tek `rename`
ile yerine taşır ve iki kapıyla biter: ürün doğrulayıcısı ve **bir film**.

**Ekransız (headless) kurulum.** Takılı ekran yoksa kurulum durmaz: boş bir HDMI
soketi çekirdek yeteneği değildir. Kurucu ekran gerektiren kontrolleri atlar,
arayüzü `enable` eder ama başlatmaz ve sonucu `PASS` değil
**`INSTALLED, NOT YET PROVEN`** diye raporlar. Kablo takıldığında:

```bash
systemctl start mediabox-tv-ui.service
/opt/rk3588-mediabox/bin/mediabox-product-verify
/opt/rk3588-mediabox/bin/mediabox-playback-smoke
```

Muafiyet istenerek alınamaz: doğrulayıcı `/sys/class/drm` taramasını kendi
yapar, ekran takılı bir kartta `MEDIABOX_VERIFY_NO_DISPLAY` hiçbir şeyi
atlatmaz.

## 2. Derleme bağımlılıkları

Hedefte:

```
git g++ cmake meson ninja-build pkg-config nasm yasm libdrm-dev
libsrt-openssl-dev          # SRT protokolü; ffmpeg yapılandırmasında açık
```

Bunları `scripts/build-media-runtime.sh` eksikse kendisi kurar; liste, ne
gerektiğini görmek için burada.

## 3. Donanım medya çalışma zamanı — MediaBox'ın kendisi

```bash
export MEDIABOX_HOST=<cihazın adresi>
./scripts/build-media-runtime.sh        # kartta derler; ~30-45 dk
```

`rockchip-linux/mpp`, `airockchip/librga` ve `nyanmisaka/ffmpeg-rockchip`
**sabitlenmiş revizyonlardan** klonlanır, derlenir ve
`/opt/rk3588-mediabox/media-runtime` altına kurulur. Betik sonunda pinleri
artefaktın kendisinden okuyarak doğrular: MPP kendi commit'ini kütüphaneye
damgalar, FFmpeg sürüm dizesinde taşır.

Revizyonların nereden geldiği ve neden stok Debian'ın yetmediği:
[`platform/custom-runtime.md`](platform/custom-runtime.md).

**Doğrulama:** betiğin kendi `verify` adımı —

```bash
./scripts/build-media-runtime.sh verify
```

> **Bu prefix `/opt/rk3588-screenbridge` değildir ve olmamalıdır.** İki ürün de
> aynı karta kurulabilir; ortak prefix, en son derlenenin diğerinin kod
> çözücüsünü değiştirmesi demekti. `deploy-mediabox-v3.sh` her deploy'da o
> dizinin her dosyasını öncesi/sonrası hash'ler ve bir tanesi bile oynarsa
> başarısız olur.

## 4. Çekirdek — çoğu zaman **gerekmez**

Ultra'da çalışan `6.1.115-vendor-rk35xx-screenbridge-hdmirx-audio`, stok Armbian
çekirdeğine tek yama eklenerek üretildi:

```
kaynak : armbian/linux-rockchip  fd9f82366e235b8afbdf516765210e97d24dce93
yama   : patches/kernel/0001-hdmirx-enable-i2s-capture.patch
release: 6.1.115-vendor-rk35xx-screenbridge-hdmirx-audio
geri alma: /root/screenbridge-backup/rollback-stock-kernel.sh
```

> İki kart aynı **sürüm dizesini** taşıyor ama aynı çekirdeği çalıştırmıyor:
> farklı derleme makineleri, derleyiciler, boyutlar ve hash'ler. O dizeyi kimlik
> değil, aile adı say.

Yama yalnız HDMI **alıcısının** I2S capture DAI'sini açar; verici (TX) tarafı
değişmeden çıkış-only kalır. **MediaBox HDMI TX kullanır** — yani bu yamaya
ihtiyacı yoktur ve stok Armbian vendor çekirdeğiyle çalışmalıdır.

> Bu bir çıkarım, ölçüm değil: MediaBox stok çekirdekle hiç açılmadı. Temiz
> imajda **önce stokla deneyin**; HDMI TX sesi ve passthrough çalışıyorsa özel
> çekirdek hiç derlenmez. Yalnız HDMI RX (capture) gerekiyorsa derleyin.

## 5. MediaBox — `rk3588-mediabox`

Bağlantı ayarları `scripts/env.sh` üzerinden; adres repoda sabit değildir:

```bash
export MEDIABOX_HOST=<cihazın adresi>
```

Sırayla:

```bash
./scripts/build-media-runtime.sh      # MPP + RGA + ffmpeg-rockchip (bölüm 3); ~30-45 dk
./scripts/install-mali-runtime.sh     # özel Mali G610 kullanıcı alanı (sürüm sabitli .deb)
./scripts/build-mediabox-player.sh    # mpv 0.41 + 3 yama + vo_mediabox.c; ~10 dk
./scripts/install-stremio-server.sh   # node + akış sunucusu (upstream.env'de sabitli)
./scripts/deploy-mediabox-v3.sh       # denetim düzlemi, TV arayüzü, Web UI,
                                      # birimler, udev, config — hepsi
./scripts/build-kodi.sh               # devir hedefi; kartta derler, saatler sürer
```

Kodi son sırada, çünkü çalışan bir arayüze giden yolda değil: ilk dördü ürünün
kendisi, beşincisi HDMI devir yolunu ve HDR referansını getirir.

`deploy-mediabox-v3.sh` gerektiğinde `fonts-inter`, `fonts-noto-color-emoji`,
tarayıcı uygulaması için `sway` ve `chromium` paketlerini kendisi kurar.

**Doğrulama:**

```bash
ssh root@$MEDIABOX_HOST /opt/rk3588-mediabox/bin/mediabox-kiosk-smoke
ssh root@$MEDIABOX_HOST /opt/rk3588-mediabox/bin/mediabox-platform inspect
```

20/20 PASS beklenir. Ayrıca `systemctl is-active mediabox-tv-ui mediaboxd-rs
mediabox-media-worker stremio-server` ve Web UI'nin `200` dönmesi.

## 6. **[Plus]** MediaBox tarafında ne kaldı

Bu belgenin önceki sürümünde dört sabit listeleniyordu. **Dördü de kalktı**;
yerlerine ne geldiği:

| Eski sabit | Şimdi |
|---|---|
| `amixer -c rockchiphdmi1` | Seçili çıkışın ses kartı, cihaz ağacındaki codec phandle'ı üzerinden çözülüyor — Plus'ta `HDMI-A-1` → `rockchiphdmi0` |
| Birimlerde `/dev/cec0` | CEC adaptörü verici platform aygıtının çocuğu olarak bulunuyor; birim `DeviceAllow=char-cec` diyor, numara demiyor. `--require-cec` kalktı: DP'de CEC yok, bu bir yetenek eksikliği, ürün arızası değil |
| `card0-HDMI-A-1` | `mediabox-platform connector-path`; konektör, adıyla eşleşiyor, DRM nesne numarasıyla değil |
| Önyükleme farkı | Yok: her iki kart da Armbian U-Boot + `armbianEnv.txt`. Plus'ta tek fark `user_overlays` satırı |

**Kurulumdan ölçülen (2026-09-19).** Plus sıfır imajdan kuruldu; kurulu
`mediabox-platform inspect` üç konektörü, her birinin ALSA kartını ve CEC
adaptörünü doğru çözüyor:

```
  HDMI-A-1  disconnected  fde80000.hdmi  rockchiphdmi0  /dev/cec0
  HDMI-A-2  disconnected  fdea0000.hdmi  rockchiphdmi1  /dev/cec1
  DP-1      disconnected  fde50000.dp    rockchipdp0    cec unavailable
  Selected  none   warning: no connected output with a usable mode
```

Bu çıktı tabloyu doğruluyor. `Selected none` ve `exit 1` bir arıza değil, boş
soket: kart tarif edilmiş, seçilecek çıkış yok. Hem kurucu hem doğrulayıcı bunu
artık çıktıdan okuyor, çıkış kodundan değil.

**Kalan tek iş ekranlı kapılar.** Televizyon Plus'a takıldığında (zorunlu olarak
**HDMI-A-1**; HDMI-A-2 → VP1, VOP2'nin SDR→HDR bloğunu atlar) bölüm 1.5'teki üç
komut çalıştırılır; film kapısı geçene kadar Plus "kurulu ama kanıtlanmamış"
sayılır.

## 7. Süre tahmini

| Adım | Tahmin |
|---|---|
| İmaj + önyükleme zinciri **[Plus]** | saatler, tek seferlik, belirsizliğin tamamı burada |
| İmaj + önyükleme zinciri **[Ultra]** | ~30 dk, ölçülmüş yol |
| `build-media-runtime.sh` | ~30-45 dk (ölçüldü: Ultra, 8 çekirdek) |
| Özel çekirdek (yalnız HDMI RX gerekiyorsa) | ~2 saat |
| Mali | ~5 dk |
| Oynatıcı (mpv) | ~10 dk |
| `install-stremio-server.sh` | ~10 dk |
| `deploy-mediabox-v3.sh` | ~5 dk |
| Kodi | saatler |

Ultra için **yarım gün**, Plus için **bir gün** — Plus'taki farkın tamamı 1.
adımda.

## 8. Kabul

Cihaz şunları yapıyorsa temiz imaj kabul edilmiştir:

1. `mediabox-kiosk-smoke` 20/20 PASS
2. Dört servis `active`, Web UI `200`
3. Katalogdan bir film **gömülü oynatıcıda** açılıyor; VOP2 özetinde
   `Cluster0-win0 AR24 zpos 11` ve `Esmart0-win0 NV12/NV15 zpos 0` görünüyor
4. Kumandada Sol/Sağ sarma, Ok onay, iki Geri durdurma çalışıyor
5. Kodi'ye devir ve geri dönüş: renk SDR'a dönüyor, panelde konsol metni yok
6. `mediabox-platform inspect` seçili çıkışı, onun ALSA kartını ve CEC
   adaptörünü raporluyor; uyarı listesi boş
7. Oynatıcı ve Kodi ikilileri `/opt/rk3588-screenbridge` altından **hiçbir**
   kütüphane çözmüyor (`ldd`, `/proc/<pid>/maps`)
