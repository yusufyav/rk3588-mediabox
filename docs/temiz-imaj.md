# Temiz imajdan çalışan cihaza

Boş bir karttan MediaBox'a giden tek yol. İki karta göre ayrılmış yerler
işaretli: **[Ultra]** / **[Plus]**.

İki repo gerekiyor ve sıraları önemli:

| Repo | Ne verir |
|---|---|
| `rk3588-screenbridge` | Önyükleme zinciri denetimi, RKMPP FFmpeg, (gerekirse) özel çekirdek |
| `rk3588-mediabox` | Mali kullanıcı alanı, Kodi, oynatıcı, denetim düzlemi, arayüzler, birimler |

> **Durum.** Ultra bu yoldan **çalışıyor** ve ölçüldü. Plus için önyükleme
> zinciri farkı ölçüldü ama **temiz imaj hiç koşulmadı** — ScreenBridge'teki
> `clean-vendor-reimage-bringup-2026-09-05.md` raporu
> `PARTIAL (not started: target SSH unreachable)` diye kapanıyor. Plus adımları
> bu yüzden "doğrulanmış" değil, "türetilmiş" sayılmalıdır.

---

## 0. Önce bilinmesi gerekenler

```
Ultra : DDR init → BL31 → Armbian U-Boot → boot.scr → armbianEnv.txt:fdtfile
        kök: eMMC /dev/mmcblk0p1
        DTB: rockchip/rk3588-orangepi-5-ultra.dtb
        HDMI: tek çıkış (card0-HDMI-A-1), tek /dev/cec0
        ALSA: 0 rockchiphdmi1 · 1 rockchiphdmiin · 2 rockchipes8388

Plus  : EDK II v2.70 → EFI GRUB → GRUB girdisinde açık `devicetree`
        kök: NVMe /dev/nvme0n1p2
        DTB: rk3588-orangepi-5-plus-screenbridge.dtb
        HDMI: iki çıkış → konektör ve CEC indeksleri farklı
        ALSA: HDMI-IN kartı 3 (Ultra'da 1)
```

SoC çevre birimleri (HDMI RX kayıt penceresi, IRQ'lar, saatler, resetler, güç
alanı) iki kartta **birebir aynı**. Farkın tamamı önyükleme zinciri, kök aygıtı
ve çevre birimi indekslerinde.

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

## 2. Derleme bağımlılıkları

Hedefte:

```
git g++ cmake meson ninja-build pkg-config nasm libdrm-dev
libsrt-openssl-dev          # SRT, build-media-stack.sh şart koşuyor
```

## 3. Donanım medya yığını — `rk3588-screenbridge`

```bash
# hedefte çalışır; /opt/rk3588-screenbridge altına kurar
SCREENBRIDGE_INSTALL_PREFIX=/opt/rk3588-screenbridge scripts/build-media-stack.sh
```

`rockchip-linux/mpp`, `airockchip/librga` ve `nyanmisaka/ffmpeg-rockchip`
klonlanır, derlenir, kurulur; betik sonunda `rkmpp` kodlayıcılarını, `rkrga`
filtrelerini ve `srt` protokolünü kendisi doğrular.

**Doğrulama:**
`/opt/rk3588-screenbridge/bin/ffmpeg -hide_banner -decoders | grep rkmpp`

Bu adım MediaBox'ın **donanım kod çözmesinin tamamı** buna bağlı: oynatıcı da,
Kodi de bu prefix'e linklenir.

## 4. Çekirdek — çoğu zaman **gerekmez**

Ultra'da çalışan `6.1.115-vendor-rk35xx-screenbridge-hdmirx-audio`, stok Armbian
çekirdeğine tek yama eklenerek üretildi:

```
kaynak : armbian/linux-rockchip  fd9f82366e235b8afbdf516765210e97d24dce93
yama   : patches/kernel/0001-hdmirx-enable-i2s-capture.patch
release: 6.1.115-vendor-rk35xx-screenbridge-hdmirx-audio
Image SHA-256: 474eb0a3d045b1ec031b657d7294c32beccef83e6e90a2734becb0635ecf5512
geri alma: /root/screenbridge-backup/rollback-stock-kernel.sh
```

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
./scripts/install-mali-runtime.sh     # özel Mali G610 kullanıcı alanı (sürüm sabitli .deb)
./scripts/build-kodi.sh               # kartta derler; saatler sürer
./scripts/build-mediabox-player.sh    # mpv 0.41 + 3 yama + vo_mediabox.c; ~10 dk
./scripts/deploy-mediabox-v3.sh       # denetim düzlemi, TV arayüzü, Web UI, node,
                                      # Stremio, birimler, udev, config — hepsi
```

`deploy-mediabox-v3.sh` gerektiğinde `fonts-inter`, `fonts-noto-color-emoji`,
tarayıcı uygulaması için `sway` ve `chromium` paketlerini kendisi kurar.
node, Stremio web ve akış sunucusu `packaging/upstream.env` içinde revizyon ve
SHA-256 ile sabitlidir.

**Doğrulama:**

```bash
ssh root@$MEDIABOX_HOST /opt/rk3588-mediabox/bin/mediabox-kiosk-smoke
```

16/16 PASS beklenir. Ayrıca `systemctl is-active mediabox-tv-ui mediaboxd-rs
mediabox-media-worker stremio-server` ve Web UI'nin `200` dönmesi.

## 6. **[Plus]** MediaBox tarafında değiştirilmesi gerekenler

Ultra'ya sabitlenmiş dört yer var. Plus'a geçmeden önce bunlar çözülmeli:

| Yer | Sabit | Yapılacak |
|---|---|---|
| `packaging/mediabox-hdmi-prepare:38` | `amixer -c rockchiphdmi1` | Kart adını `/proc/asound/cards` içinden çöz. ScreenBridge'te `scripts/hdmirx-audio-loopback.sh` bunu zaten yapıyor, aynı yaklaşım alınmalı |
| `packaging/systemd/*.service` | `/dev/cec0` | Plus'ta iki HDMI → doğru CEC aygıtı seçilmeli |
| `scripts/capture-*.sh`, `run-mp1*.sh` | `card0-HDMI-A-1` | Bağlı konektörü bul, isim sabitleme |
| Önyükleme | `armbianEnv.txt` / `fdtfile` | Plus'ta GRUB girdisi + kendi DTB'si |

**TV arayüzünün kendisi bu listede değildir.** `find_output` ilk bağlı `HDMI-A`
konektörünü seçer, video düzlemini isimle değil `SetPlane` deneyerek bulur — VOP2
düzlem haritası farklı olsa da kendi bulur.

## 7. Süre tahmini

| Adım | Tahmin |
|---|---|
| İmaj + önyükleme zinciri **[Plus]** | saatler, tek seferlik, belirsizliğin tamamı burada |
| İmaj + önyükleme zinciri **[Ultra]** | ~30 dk, ölçülmüş yol |
| `build-media-stack.sh` | ~1 saat |
| Özel çekirdek (yalnız HDMI RX gerekiyorsa) | ~2 saat |
| Mali | ~5 dk |
| Kodi | saatler |
| Oynatıcı (mpv) | ~10 dk |
| `deploy-mediabox-v3.sh` (node + Stremio dahil) | 20-40 dk |

Ultra için **yarım gün**, Plus için **bir gün** — Plus'taki farkın tamamı 1.
adımda.

## 8. Kabul

Cihaz şunları yapıyorsa temiz imaj kabul edilmiştir:

1. `mediabox-kiosk-smoke` 16/16 PASS
2. Dört servis `active`, Web UI `200`
3. Katalogdan bir film **gömülü oynatıcıda** açılıyor; VOP2 özetinde
   `Cluster0-win0 AR24 zpos 11` ve `Esmart0-win0 NV12/NV15 zpos 0` görünüyor
4. Kumandada Sol/Sağ sarma, Ok onay, iki Geri durdurma çalışıyor
5. Kodi'ye devir ve geri dönüş: renk SDR'a dönüyor, panelde konsol metni yok
