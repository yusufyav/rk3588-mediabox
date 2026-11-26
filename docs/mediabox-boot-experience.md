# MediaBox açılış deneyimi — donanımsal fizibilite

Durum: araştırma ve mimari karar. Bu belgeyle birlikte üretim kodu, bootloader,
kernel, device tree veya boot bölümü **değiştirilmedi**.

Hedef akış: **güç → özgün marka görseli → sinematik tanıtım videosu → Ana Menü**,
açılışı uzatmadan, tek DRM master ve VOP2 donanım düzlemleriyle.

Ölçüm cihazı: Orange Pi 5 Plus, `192.168.2.51`, Armbian 26.8.2 `vendor`,
kernel `6.1.115-vendor-rk35xx`. Monitör HDMI-A-1, `2560x1440p144` (debugfs
`dri/0/summary`: `dclk[583600 kHz]`). Ölçülen kod: `main` `beedbbb`.

Ham kanıt: cihazda `/root/mediabox-verify/boot-experience/`, geliştirme
bilgisayarında `/var/tmp/rk3588-mediabox/logs/boot-experience/device-evidence.tgz`
(dmesg, `journalctl -o short-monotonic`, `systemd-analyze` blame ve critical-chain,
birim zaman damgaları, U-Boot DTB/FIT dökümleri).

Etiketler: **[ölçüldü]** bu cihazda komut çıktısıyla görüldü. **[kaynak]**
resmi kaynak kodundan okundu, bu cihazda çalıştırılmadı. **[çıkarım]** ikisinden
türetilen ama doğrudan ölçülmeyen değer.

---

## 1. Ölçülmüş açılış zaman çizelgesi

İki açılış var:

- **A0**: 08:30'daki açılış. Wi-Fi açılış süresince bağlanamadı ve
  `CTRL-EVENT-CONNECTED` sinyali monotonic 102,3 s'de geldi. Ağ yokken nasıl
  açıldığını gösteriyor.
- **A1**: 10:21'de yapılan tek kontrollü reboot. Wi-Fi monotonic 11,2 s'de
  bağlandı.

Reboot'tan önce cihazın boşta olduğu doğrulandı: player, Kodi, tarayıcı ve DVB
servisleri inactive'di.

### Saat ekseni

Tablodaki zamanlar **kernel printk saatidir** (`T_k`, dmesg). Bu saat bootloader
süresini de içerir: `Booting Linux` satırı 2,1 s'de basılıyor. systemd ve
uygulama zaman damgaları ise `CLOCK_MONOTONIC` saatini kullanır. İki saat
`/dev/kmsg`'ye yazılan bir prob satırıyla hizalandı **[ölçüldü]**:
`T_k = mono + 3,14 s` (A0) ve `T_k = mono + 3,05 s` (A1). A0'da bu fark, ayrıca
`mediabox-console-off` başlangıcıyla kernel'in `Console: switching to colour
dummy device` satırının çakışmasıyla da tutuyor.

Journal'ın kernel satırlarına verdiği monotonic zaman, journald başlamadan önceki
mesajlarda dmesg ile tutarsız. Bu yüzden kernel olayları için dmesg kullanıldı.

| Olay | A0 `T_k` (s) | A1 `T_k` (s) | Not |
|---|---:|---:|---|
| `Booting Linux` | 2,193 | 2,135 | Öncesi BootROM + SPL + TF-A + U-Boot + boot.scr + Image/initrd yükleme |
| `mpp_service: probe success` (VPU sürücüsü hazır) | 3,791 | 3,702 | built-in |
| `[drm] Initialized rockchip` + `failed to parse loader memory` | 3,842 | 3,757 | U-Boot logo devretmedi |
| **İlk HDMI modeset**, fbdev `2560x1440p60` | 3,885 | 3,800 | Kablodaki ilk sinyal |
| fbcon panele geçer | 3,923 | — | |
| Mali (bifrost kbase) probe | 4,277 | — | |
| initramfs `/init` | 4,448 | 4,350 | |
| rootfs (ext4, eMMC) bağlandı | 5,764 | 5,909 | |
| systemd başladı (`systemd-analyze` "kernel") | ≈ 6,05 | ≈ 6,11 | 2,908 / 3,064 s mono |
| `mediabox-console-off` (fbcon unbind + fb0 sıfırlama) | 7,92 | 7,94 | |
| İkinci `1440p60` modeset (aynı mod) | 8,209 | **yok** | Nedeni belirlenemedi |
| `mediabox-display-seed` | 9,62 → 14,04 | 10,30 → 14,53 | 4,4 / 4,2 s |
| HDMI link retrain (`final tmdsclk`, modeset yok) | 13,929 | 14,457 | seed içindeki `modetest -w` |
| `network-online.target` | 27,27 (**20 s timeout**) | 16,25 | |
| `mediabox-tv-ui` başlatılıyor | 27,98 | 16,79 | |
| `ExecStartPre` (`mediabox-hdmi-prepare`) bitti | 31,95 | 20,65 | 3,97 / 3,86 s |
| UI `first_frame_ms` | 33,34 (1144 ms) | 21,96 (1054 ms) | |
| **UI modeset `2560x1440p144`** | 33,385 | 22,038 | Ana Menü görünür |
| `systemd-analyze` toplam | 24,484 s mono | 13,401 s mono | |

UI'ın yeniden başlatılması da ayrıca ölçüldü. Önceki açılışta (boot −1) UI
servisi art arda birkaç kez yeniden başlatılmıştı **[ölçüldü]**. Bu yolda
`Starting` satırından `first_frame` satırına kadar 1,5–1,6 s geçti. Bunun 0,6–0,9
s'i `ExecStartPre` adımı; `first_frame_ms` 659–790 ms arasındaydı. Bu yolda plan
geçerliydi, dolayısıyla 3 s'lik bekleme yoktu.

### Kritik yol ve gereksiz beklemeler [ölçüldü]

A0'da `systemd-analyze critical-chain mediabox-tv-ui.service` şu zinciri veriyor:

```
mediabox-tv-ui ← mediaboxd-rs ← mediabox-media-worker ← stremio-server
              ← network-online.target @21.227s ← network.target @3.723s
```

1. **`network-online.target` en büyük bekleme.** Armbian'ın drop-in'i
   (`armbian-any-interface.conf`) servisi `systemd-networkd-wait-online --any
   --timeout=20` olarak çalıştırıyor. A0'da iki Ethernet portu da `no-carrier`
   durumundaydı ve Wi-Fi bağlanamadı, bu yüzden servis tam 20 s bekleyip
   `Timeout occurred` ile düştü. Ana Menü'nün yaklaşık 20 s'si buradan geldi.
   A1'de bekleme 9,1 s sürdü. Bu bağımlılığa üç ayrı yoldan bağlanılıyor:
   - `mediabox-tv-ui.service`: `After=` ve `Wants=network-online.target`.
   - `mediabox-media-worker.service`: `After=stremio-server.service`;
     `stremio-server.service` de `After=network-online.target`.
   - `stremio-server.service`: `Before=mediaboxd-rs.service`. Bu satır kontrol
     düzlemini de ağ beklemesine bağlıyor.

   UI arayüzü katalog isteklerini zaten yeniden deniyor (birimdeki yorum bunu
   söylüyor). Dolayısıyla ağ, ilk karenin önkoşulu değil.
2. **`hdmi-prepare` her soğuk açılışta 3 s bekliyor.** Script
   `mediabox-platform plan --wait 3` çağırıyor. Observer'ın açılış sırasındaki
   yanıtı şu: `DRM query deferred: no active master` / `a display transition is
   in progress`. Bu durumda geçerli bir plan oluşamıyor ve iki çağrı da
   `plan=stale` sonucuyla dönüyor: biri `display-seed` içinde, diğeri UI'ın
   `ExecStartPre` adımında. A0 ve A1'in ikisinde de görüldü. `display-seed`
   `Before=mediabox-tv-ui` olduğu için iki bekleme de, ağ beklemesi kalktığında
   kritik yola girer: yaklaşık 2 × 3 s.
3. **Kablo, Ana Menü'den önce 3–4 kez tazeleniyor.** Sırasıyla: fbdev 60 Hz
   modeset (3,8 s), A0'da açıklanamayan ikinci 60 Hz modeset (8,2 s), seed'in
   property yazımından gelen link retrain (13,9–14,5 s) ve UI'ın 144 Hz modeset'i.
   Script'teki ölçüm notuna göre bu retrain HP X27q'da siyah ekran üretiyor. Her
   biri monitörde sinyal kaybı ya da yeniden kilitlenme demek. Kaç tanesinin
   gözle görülebildiği SSH üzerinden ölçülemez.

### Ölçülemeyenler

- **Güç verildiği andan ilk fiziksel görüntüye kadar geçen süre ölçülmedi.**
  `T_k` saati SoC'nin generic timer'ının başlamasıyla sayılmaya başlar. Güç,
  PMIC rampası ve BootROM'un sayacı başlatması arasındaki süre bu saate girmez.
  Elimizdeki tek kesin bilgi şu: kernel'e kadar geçen süre ≥ 2,1 s. Monitörün
  sinyale kilitlenme süresi de görünmüyor. Bunları ölçmek için kamera ya da
  fotodiyot (veya seri konsol + kamera) gerekir.
- Hangi bootloader'ın çalıştığı (bkz. §2).
- Panelde 3,9–7,9 s arasında metin görünüp görünmediği. fbcon bağlı,
  `ShowStatus=yes` ve kernel komut satırında `console=tty1 splash=verbose` var;
  dolayısıyla systemd durum satırlarının bu aralıkta panele düşmesi beklenir
  **[çıkarım]**.

---

## 2. En erken açılış görseli

### Kurulu bootloader [ölçüldü]

| | SPI NOR (`mtd0`, 16 MiB) | eMMC (`mmcblk0`, sektör 64 `RKNS`) |
|---|---|---|
| U-Boot | `2017.09-orangepi (Nov 24 2024)` | `2017.09_armbian-2017.09-S39cd-P2e20-Hbe55-V5abd-B5da4-R448a (Aug 09 2026)` |
| FIT yapılandırması | **`rk3588-orangepi-5-ultra`** | `rk3588-orangepi-5-plus` |
| U-Boot DT `model` | **`RK3588 Orange Pi 5 Ultra`** | `Orange Pi 5 Plus` |
| Kaynak | Orange Pi vendor | `linux-u-boot-orangepi5-plus-vendor 26.8.2`, `radxa/u-boot` `next-dev-v2024.10` @ `39cd993e` |

Plus'ın SPI flash'ında **Ultra için derlenmiş** bir loader var. Bilinen RK3588
BootROM önceliğine göre SPI NOR, eMMC'den önce denenir [R1]. [R1] bu oturumda
yeniden okunmadı; bu sıra bu cihazda da geçerliyse çalışan U-Boot SPI'daki Ultra
loader'dır. Seri konsol
(`ttyS2`, 1,5 Mbaud) olmadan bu doğrulanmadı. Logo sorusunun cevabı iki
loader'da da aynı çıkıyor (aşağıda). Yine de bu durum, bootloader'a dokunacak her
çalışmadan önce çözülmesi gereken bir risk olarak kaydedildi.

Armbian U-Boot yapılandırması (`/usr/lib/linux-u-boot-*/u-boot-config-target-1`):
`CONFIG_DRM_ROCKCHIP=y`, `CONFIG_DRM_ROCKCHIP_DW_HDMI_QP=y`,
`CONFIG_PHY_ROCKCHIP_SAMSUNG_HDPTX_HDMI=y`, `CONFIG_ROCKCHIP_RESOURCE_IMAGE=y`,
`CONFIG_USING_KERNEL_DTB=y`, `CONFIG_BOOTDELAY=0`. İki imajda da `logo.bmp` ve
`logo_kernel.bmp` dizgileri bulunuyor. Yani **logo kodu derlenmiş durumda.**

### Bu kurulumda U-Boot neden logo göstermiyor

- **[kaynak]** Vendor U-Boot'ta logo, `load_bmp_logo()` →
  `rockchip_read_resource_file()` zinciriyle okunur. Bu fonksiyon yalnız ham
  bölümlerden okur: `boot`/`recovery` içindeki RSCE, `resource` bölümü veya
  `logo` bölümü. ext4 ya da FAT dosya sisteminden okumaz [R2][R3].
  `USING_KERNEL_DTB` de kernel DTB'yi aynı resource zincirinden (`rk-kernel.dtb`)
  arar [R4]. Ekran düğümleri (`display-subsystem/route`) kernel DTB'den gelir;
  U-Boot'un kendi DTB'si yalnız açılış için gerekli donanımı tanımlar.
- **[ölçüldü]** eMMC'de tek bölüm var: `mmcblk0p1`, ext4, kök dizin. Armbian,
  `boot.scr` ile kernel'i ext4'ten `booti` komutuyla yüklüyor. `resource`,
  `logo` veya `boot` bölümü yok. İki U-Boot DTB'sinde de VOP, HDMI veya route
  düğümü yok. Kernel DTB'de `route-hdmi0` için `status = "disabled"`
  (`logo,uboot = "logo.bmp"`, `logo,mode = "center"`), `drm-logo@0` için
  `reg` boyutu 0. Kernel iki açılışta da `failed to parse loader memory`
  yazdı. Bu satır `init_loader_memory()` tarafından basılıyor ve U-Boot'un logo
  belleği bırakmadığı anlamına geliyor [R5].
- `armbianEnv.txt` içinde `bootlogo=false` var. Bu ayar yalnız Plymouth'u açıp
  kapatır, U-Boot logosuyla ilgisi yoktur. `/boot/boot.bmp` (38 KiB) dosyası
  `boot.cmd` tarafından hiç okunmuyor.

### Sorulara kesin cevaplar

| Soru | Cevap |
|---|---|
| İlk özgün görsel hangi aşamada gösterilebilir? | Bu kurulumda, üretim tarafına dokunmadan: **userspace'te**. Erken seçenek `mediabox-console-off` anında fb0'a yazmak (`T_k` ≈ 7,9 s). Önerilen seçenek UI'ın ilk karesi (bugün 22 / 33 s, §4'teki düzeltmelerle tahminen ≈ 12–13 s **[çıkarım]**). |
| Mevcut U-Boot destekliyor mu? | Kodu **var**, bu disk düzeninde **çalışmıyor**. Açmak için en azından şunlar gerekir: ham bir `logo`/`resource` bölümü (boot bölüm düzeni değişikliği), kernel DTB'de `route-hdmi0` etkinleştirme ve `drm-logo` belleği ayırma (DT değişikliği). İkisi de bu görevin dışında. |
| Kernel açılırken aynı görüntü korunabilir mi? | **[kaynak]** Vendor kernel `rockchip_drm_show_logo()` ile U-Boot'un framebuffer'ını kopyalamadan devralır. Mod U-Boot'unkiyle eşleşirse `mode_changed` bayrağı kurulmaz [R5]. Bu kurulumda devralınacak bir logo olmadığı için konu boşa düşüyor. |
| HDMI/DP yeniden senkronizasyonu veya siyah ekran oluşuyor mu? | **[ölçüldü]** Evet. Kernel sink'in tercih ettiği modu kurar (60 Hz), UI ise 144 Hz'e geçer. Arada A0'da ikinci bir 60 Hz modeset ve seed'in link retrain'i de var. U-Boot logosu açılsaydı bile U-Boot 60 Hz kurardı ve UI'ın 144 Hz geçişi bir kez daha sinyali tazelerdi **[çıkarım]**. |
| Bakım maliyeti? | U-Boot/kernel logosu: yüksek. Her `linux-u-boot` veya `linux-dtb` paket güncellemesinde bölüm, overlay ve logo imajının yeniden doğrulanması gerekir. Ayrıca SPI'daki loader'ın hangi kart için derlendiği ayrı bir değişken. Userspace yolu: Armbian güncellemelerinden bağımsız. |

### `mediabox-console-off` etkisi

Script fbcon'u ayırıyor (unbind) ve `/dev/fb0` içeriğini sıfırlıyor (2560×1440,
32 bpp). Bu adım iki yerde çalışıyor: açılışta (`T_k` ≈ 7,9 s) ve her UI
başlangıcında `hdmi-prepare` içinden. Bunun iki sonucu var:

- Kernel veya U-Boot logosu olsaydı, bu adım onu 7,9 s'de **silerdi**. Böyle bir
  logo yolu açılacak olsaydı script'in açılış çağrısı değiştirilmek zorunda
  kalırdı.
- Erken userspace logosu için doğru kanca bu adım. Açılış çağrısında sıfır
  yerine marka görselini fb0'a yazmak, kernel'e dokunmadan ≈ 7,9 s'de görüntü
  verir. Bedeli: bu görüntü fbdev'in 60 Hz modunda durur ve UI'ın 144 Hz'e
  geçişi, logo ile UI arasında **bir sinyal tazelemesi** yaratır. Bu yüzden
  "kesintisiz" hedefini karşılamaz; önerilen mimariye dahil edilmedi (§4,
  seçenek B). UI başlangıçlarında sıfırlama davranışı (Kodi lastclose izlerini
  temizlemek) aynen kalmalı.

---

## 3. Sinematik tanıtım videosu

### VPU ve GPU ne zaman hazır [ölçüldü]

`mpp_service` (rkvdec2 CCU, vdpu, vepu dahil) `T_k` 3,7–3,8 s'de, Mali
4,3 s'de, DRM/VOP2 3,8 s'de hazır. Hepsi built-in. Donanım tarafında engel
yok. Videonun ne kadar erken başlayabileceğini kernel değil, **userspace sırası**
belirliyor: rootfs 5,8 s'de bağlanıyor, systemd 6,1 s'de başlıyor.

### Mevcut pipeline yeniden kullanılabilir mi — evet [ölçüldü + kaynak]

- UI DRM master'ı elinde tutuyor ve VP0'ın iki penceresini kullanıyor:
  Cluster0 arayüz, Esmart0 ise video düzlemi (NV12/NV15, LINEAR, donanımda
  ölçekleme). Açılış logunda: `interface raised plane::Handle(57)@11`,
  `overlay candidates=2`, `listening on /run/mediabox-ui/video.sock`.
- `mediabox-player`, `mpv --vo=mediabox --hwdec=rkmpp` çağrısıyla dma-buf'ları
  bu sokete gönderiyor. UI, kopyasız biçimde Esmart0'a basıyor
  (`rust/crates/mediabox-tv/src/video.rs`). Ayrı bir renderer, compositor veya
  ikinci DRM master gerekmiyor.
- Önceki açılışta yerel medya oturumuyla başlatılan bir 4K HEVC HDR film için
  **ölçülen süreler:** systemd `Started` → `player connected` 151 ms,
  → `picture seen on plane` 199 ms.
- **Aynı ölçümde iki engel görüldü.** Film başlar başlamaz daemon bir
  `Apply{content:true}` gönderdi (`the film's frame rate chose the next mode`).
  UI bunun üzerine `3840x2160p23.976` modunu kurdu ve HDR10'a geçti. Tanıtım
  videosu bu yolu **kullanmamalı**. Aksi halde 1440p144'te iki modeset, 4K60'ta
  ise bir modeset eklenir. Ayrıca "ilk saniyelerde" (`crystal_ppm_settled`)
  mod geçişi saat ölçümünü bekliyor (`platform.rs`).

### Sorulara cevaplar

| Soru | Cevap |
|---|---|
| VPU hangi aşamada kullanılabilir? | Kernel `T_k` ≈ 3,7 s. Pratikte ise UI'ın video soketi açıldığı anda, çünkü kareleri gösterecek tek DRM master UI. |
| Mevcut pipeline kullanılabilir mi? | Evet: `mediabox-player` → `vo_mediabox` → `video.sock` → Esmart0. Gereken değişiklik yalnız bir "intro" modu (aşağıda). |
| Ağ ve diğer servisler hazırlanırken oynatılabilir mi? | Evet. Yerel dosyanın ağ ile bir ilişkisi yok. Bunun için UI'ın `network-online.target` beklemesinden kurtarılması gerekiyor (§4). |
| Video oynarken Ana Menü arka planda hazırlanabilir mi? | Evet. Menü aynı süreçte ve aynı Slint ağacında hazırlanıyor. Video Esmart0'da, arayüz Cluster0'da duruyor; UI video süresince `film` ekranındaki gibi şeffaf çizer. Katalog getirme ve spatial hazırlık (açılışta ≈ 45 ms) bu sırada sürer. |
| Son kareden Ana Menü'ye atomik geçiş? | Video düzlemi son kareyi **tutar**; UI menünün ilk opak karesini Cluster0'a koyar; Esmart0 **aynı atomic commit içinde** kapatılır. UI tek master olduğu için bu tek bir `drmModeAtomicCommit`. Bugün `Sink` oyuncu ayrılınca düzlemi kendi commit'iyle bırakıyor (`plane released after N frames`), bunun değişmesi gerekir. |
| DRM sahipliği ve framebuffer yaşam döngüsü nasıl korunur? | Hiçbir şey değişmez. Master UI'da kalır. Tanıtım yalnız UI süreci içinden ve yalnız soğuk açılışta başlar. `fdstore` devri (Kodi, tarayıcı) aynı kalır. |

### Video biçimi önerisi [çıkarım; ölçüm gerekli]

- HEVC Main, SDR, BT.709 limited range, 8 bit, 1920×1080, 3–5 s. VOP2 Esmart0
  bunu 1440p ve 2160p'ye donanımda ölçekliyor. HDR kullanılmamalı: HDR hem
  connector property yazımı hem de sink tarafında mod değişimi demek.
- **Kare hızı çıkış tazeleme hızıyla tam bölünmeli,** yoksa yargı (judder)
  oluşur. Mod değiştirmek yasak olduğu için video moda uymak zorunda. Öneri:
  aynı master'dan iki kodlama. `intro-60` (60/120 Hz çıkışlar; 4K60) ve
  `intro-72` (144 Hz = 2 × 72). Gerekirse `intro-50` (50/100 Hz). Çıkışın
  tazeleme hızı UI'da zaten biliniyor.
- Ses isteğe bağlı. HDMI ses yolu `hdmi-prepare` sonrasında hazır, ancak
  sink'in ilk yüzlerce ms içinde sesi kilitleyip kilitlemediği ölçülmedi.

---

## 4. Mühendislik kararı

### Değerlendirilen seçenekler

| | Seçenek | İlk görsel `T_k` | Ek tazeleme | Kapsam / bakım | Karar |
|---|---|---|---|---|---|
| A | U-Boot logosu (`logo.bmp`) | ≈ 2 s **[çıkarım]** | 60→144 Hz (+1) | Bölüm düzeni + DT + bootloader; yüksek | Kapsam dışı, reddedildi |
| B | `console-off` anında fb0'a marka görseli | ≈ 7,9 s | 60→144 Hz (+1) | Yalnız userspace; düşük | Reddedildi: kesintisiz değil |
| C | Kernel boot logosu (`CONFIG_LOGO`) / `video=` | ≈ 3,9 s | — | Kernel değişikliği; `video=` bu kartta panik üretti (`hdmi-prepare` notu) | Kapsam dışı |
| **D** | **UI'ın ilk karesi = marka görseli, ardından aynı süreçte tanıtım** | ≈ 12–13 s **[çıkarım]** (bugün 22 / 33 s) | **0** (UI'ın tek 144 Hz modeset'i zaten var) | Yalnız MediaBox kodu ve birimleri | **Önerilen** |

Gerekçe: kablonun 144 Hz'e geçmesi, kernel komut satırına dokunmadan
kaçınılamayan tek modeset. D seçeneğinde ilk özgün görüntü bu modeset ile
**birlikte** gelir ve ondan sonra kablo bir daha tazelenmez. Erken bir logo
(A/B) ise ancak bir siyah boşlukla UI'a bağlanabilir. D seçeneğinde açılış
süresi kısalır, çünkü ağ ve plan beklemeleri kritik yoldan çıkar. Tanıtım
videosu da menünün hazırlanma süresinin içine gömülür.

### Önerilen akış

```
T_k ≈ 3,8   kernel fbdev 60 Hz (siyah; console-off 7,9 s'de fb0'ı sıfırlar)
            mediabox-display-seed  (plan beklemesi yok)
            mediaboxd-rs           (network.target sonrası; ağ beklemez)
T_k ≈ 12    mediabox-tv-ui: master + tek modeset 144 Hz
            ilk kare = marka görseli (videonun 0. karesiyle aynı)
            soğuk açılış mı?  ─ hayır ─▶ doğrudan Ana Menü
                 │ evet
            intro oynatıcı: yerel dosya → video.sock → Esmart0
            (mod/HDR/geçmiş yok; ilk kare için 500 ms süre)
            menü arka planda hazırlanır (aynı süreç)
            son kare tutulur → menünün opak ilk karesi + Esmart0 kapanışı
            tek atomic commit
            Ana Menü
```

**"Soğuk açılış" kararı** UI başlangıcında verilir. Şu koşulların hepsi
sağlanmalı:

1. `fdstore::adopt()` bir cihaz devralmadıysa. Devraldıysa Kodi/tarayıcıdan
   dönüş veya bir handover var demektir: tanıtım oynatılmaz.
2. `/proc/sys/kernel/random/boot_id` değeri,
   `/var/lib/mediabox-ui/intro-boot-id` dosyasındaki değerden farklıysa. Aynıysa
   bu bir UI restart'ıdır: tanıtım oynatılmaz. Değer **tanıtım başlamadan önce**
   yazılır; böylece bir çökme ve ardından restart tanıtımı tekrar oynatmaz.
3. Bağlı bir ekran varsa, tanıtım dosyası mevcutsa ve ayar açıksa.

**Hata yolları:** Dosya yoksa ya da bozuksa, decoder hata verirse veya 500 ms
içinde ilk kare gelmezse UI doğrudan menüye geçer. Oynatıcı yarıda düşerse düzlem
bırakılır ve menü açılır. Hiçbir durumda tanıtım menüyü bekletmez; uzaktan
kumandada herhangi bir tuş tanıtımı keser.

### Gerekli kaynak kodu değişiklikleri (bu PR'da yapılmadı)

1. **Birim sıralaması** (en büyük kazanç; A0'da ≈ 20 s, A1'de ≈ 9 s):
   - `packaging/systemd/mediabox-tv-ui.service`: `network-online.target`
     `After=` ve `Wants=` satırlarından çıkarılır.
   - `packaging/systemd/mediabox-media-worker.service`:
     `After=stremio-server.service` kaldırılır; `Wants=` kalır.
   - `packaging/systemd/stremio-server.service`: `Before=mediaboxd-rs.service`
     kaldırılır. Stremio kendisi `network-online` beklemeye devam eder.
   - Ön koşul: UI'ın ve worker'ın ağ gelmeden başlayıp sonradan toparlandığı,
     host testleri ve cihazda Ethernet/Wi-Fi olmadan açılışla doğrulanmalı.
2. **Plan beklemesi:** `mediabox-display-changed seed` ve UI'ın ilk
   `ExecStartPre` çağrısı için `MEDIABOX_PLAN_WAIT=0`, ya da master yokken
   observer'ın "plan olamaz" yanıtını beklemeden dönmesi. A0 ve A1'de ≈ 2 × 3 s.
   Seed'in link retrain'i (`modetest -w`, ölçülen 13,9 / 14,5 s) yalnız gerçekten
   farklı bir değer varsa yapılmalı. Script bunu zaten amaçlıyor; açılışta neden
   yine de retrain olduğu ayrıca ölçülmeli.
3. **UI (`rust/crates/mediabox-tv`):**
   - `ui/app.slint`: `Boot` bileşeni marka görselini videonun 0. karesiyle
     aynı yerleşimde çizer. Ayrıca şeffaf bir `intro` ekranı eklenir (`film`
     gibi `#00000000` arka plan).
   - `src/route.rs` / `src/main.rs`: `Route::Intro`, soğuk açılış kararı (yukarıdaki
     üç koşul), 500 ms ilk kare süresi, tuşla kesme.
   - `src/video.rs` (`Sink`): tanıtım sonunda son kareyi tutma ve Esmart0'ı
     menünün ilk opak karesiyle aynı atomic commit'te kapatma.
   - `src/fdstore.rs`: değişiklik yok; yalnız `adopt()` sonucu dışarı verilir.
4. **Kontrol düzlemi (`rust/crates/mediaboxd-rs/src/player.rs`):** bir "intro"
   başlatma türü. Yerel dosya; `Apply{content}` ve HDR yok; geçmiş ve resume
   yok; kısa ömürlü. Alternatif olarak UI oynatıcıyı daemon'a sorarak başlatır.
   Oynatıcının tek başlatıcısı yine daemon olarak kalır.
5. **Oynatıcı (`packaging/mediabox-player`):** bir intro bayrağı. Yerel yol,
   `--hwdec=rkmpp`, ses isteğe bağlı, kare hızına göre dosya seçimi
   (`intro-60` / `intro-72`).
6. **Varlıklar:** `/opt/rk3588-mediabox/share/intro/` altında iki HEVC dosyası
   ve 0. kareyle aynı marka görseli (bu aşamada üretilmedi).
7. **Testler:** `tests/run-host-tests.sh` içinde karar tablosu (soğuk açılış,
   UI restart, fdstore devri, dosya yok/bozuk, süre aşımı) ve birim sıralaması
   kontrolü.

### Bakım, risk ve geri dönüş

- **Bakım:** Bootloader, kernel ve DT'ye dokunulmadığı için Armbian
  güncellemeleri bu özelliği etkilemez. Tek bağımlılık, MediaBox'ın kendi
  video düzlemi sözleşmesi.
- **Riskler:**
  - Ağ beklemesi kaldırılınca ilk katalog isteği yine boş dönebilir. UI'ın
    yeniden deneme davranışı ağsız açılışla doğrulanmalı.
  - 72/60 fps seçimi yanlış olursa judder oluşur.
  - Tanıtım sırasında Cluster0'ın şeffaf çizimi, 144 Hz'te GPU yükü yaratır;
    kare başına maliyet ölçülmeli.
  - SPI'daki Ultra loader (§2), bootloader'a ilişkin ileride yapılacak her işin
    önünde duran ayrı bir risk.
- **Geri dönüş:** Ayar kapatılır ya da tanıtım dosyası silinir; UI bugünkü
  `Boot` → menü yoluna döner. Birim sıralaması değişiklikleri paket düzeyinde
  geri alınabilir. Hiçbirinde boot zincirine dokunulmadığı için cihazın açılmama
  riski yok.

---

## 4b. Uygulama (aynı PR) ve ölçülen sonuç

### Ürün kararı (güncellendi)

Kullanıcı, özgün bir sinematik açılış için Ana Menü'nün **birkaç saniye bilinçli
olarak bekletilmesini kabul ediyor**. Teknik beklemeler (ağ, plan, gereksiz
retrain) kaldırıldı. Gelecekteki marka videosu veya animasyonu, artık kısalmış
sistem bekleme süresine sığdırılmak zorunda değil: tanıtım süresi bir ürün
kararıdır ve ölçülen sistem hazırlığından bağımsız seçilir. Teknik taban
(aşağıda ölçülen ≈ 13,4–14,2 s), tanıtımın en erken başlayabileceği noktadır;
tanıtımın kendisi menüyü bundan sonra kasıtlı olarak geciktirebilir. Nihai logo,
video veya animasyon bu aşamada üretilmedi.

### Değişiklikler

| Dosya | Değişiklik |
|---|---|
| `packaging/systemd/mediabox-tv-ui.service` | `network-online.target` `After=`/`Wants=`'ten çıktı. ExecStartPre: `MEDIABOX_PLAN_WAIT=0 MEDIABOX_HDMI_CONNECTOR_RESET=owner` |
| `packaging/systemd/mediabox-media-worker.service` | `After=stremio-server.service` kaldırıldı (`Wants=` duruyor) |
| `packaging/systemd/stremio-server.service` | `Before=mediaboxd-rs.service` kaldırıldı; kendisi `network-online` beklemeye devam ediyor |
| `packaging/mediabox-display-changed` | `seed`: plan beklemesi 0, connector reset yok (`prepare 0`) |
| `packaging/mediabox-hdmi-prepare` | `owner` modu: EDID varsa connector reset UI'a bırakılır, yoksa eskisi gibi çalışır. Yazılan property'ler loglanır (`connector-reset=…`) |
| `rust/crates/mediabox-tv/src/platform.rs`, `video.rs` | UI'ın ilk karesi: mod + renk property'leri **tek atomic commit** (`atomic_modeset`); reddedilirse eski `SETCRTC` |
| `tests/run-host-tests.sh` | 11 yeni kontrol: seed'in plan ve reset'i, `owner` modu (EDID var/yok), birim sıralaması |

Sistem genelindeki wait-online mekanizmasına dokunulmadı; Stremio hâlâ onu
bekliyor. Kodi'nin ExecStartPre'si (plan 3 s, reset=1) ve hotplug kurtarma yolu
değişmedi.

Retrain'in nedeni ölçüldü: kernel connector'ı açılışta `color_format=1`
(ycbcr444) bırakıyor; seed bunu rgb'ye yazınca link yeniden eğitiliyordu
(`connector-reset=color_format(1)`). UI'ın ilk karesi eskiden `SETCRTC` ile
property'siz kuruluyordu, bu yüzden sıfırlama önceden yapılmak zorundaydı. Artık
property'ler UI'ın modeset'iyle aynı commit'te.

Dolaylı beklemeler incelendi:

- mediaboxd-rs açılışta ağ beklemiyor. `--lan-http 0.0.0.0` adresi ağ olmadan
  da bağlanıyor.
- media-worker başlangıçta upstream'e gitmiyor; Stremio'ya her istek ayrı
  soruluyor.
- UI katalog yükleyicisi 1, 2, 4, 8 s, ardından 15 s aralıkla yeniden deniyor
  ve yalnız bir katalog satırı geldiğinde duruyor.

Bu üçünde kod değişikliği gerekmedi.

### Önce / sonra (`T_k`, kernel saati, §1 ile aynı eksen)

| Açılış | Ağ | UI başlangıcı | İlk kare | **144 Hz modeset (Ana Menü)** | Kablo olayları (fbdev sonrası) |
|---|---|---:|---:|---:|---|
| A0 (önce) | yok | 27,98 | 33,34 | **33,39** | 60 Hz (8,2) + retrain (13,9) + 144 Hz |
| A1 (önce) | var | 16,79 | 21,96 | **22,04** | retrain (14,5) + 144 Hz |
| B2 (yalnız birimler + plan) | var | 11,56 | 13,62 | **13,70** | retrain (11,1) + 144 Hz |
| B3 (tam) | var | 11,32 | 13,37 | **13,45** | yalnız 144 Hz |
| B5 (tam) | yok | 11,83 | 13,81 | **13,89** | yalnız 144 Hz |
| B6 (tam) | yok | 11,91 | 13,85 | **13,94** | yalnız 144 Hz |
| B8 / B9 (tam) | yok | 11,99 / 12,09 | — | **14,04 / 14,22** | yalnız 144 Hz |

**[ölçüldü]** Ana Menü, ağlı açılışta 22,0 s'den 13,45 s'ye, ağsız açılışta
33,4 s'den 13,9–14,2 s'ye indi. Kernel'in fbdev modeset'inden sonra kabloya
giden tek olay UI'ın 144 Hz modeset'i. Raporun önceki sürümündeki 12–13 s
tahmini gerçekleşmedi; ölçülen değer 13,4–14,2 s. Kalan yol: systemd 6,1 s →
seed ≈ 1,0–1,1 s → daemon → UI süreci → ilk kare ≈ 1,1 s.

Açılış sonrası renk durumu **[ölçüldü]**: `bus_format RGB888_1X24`,
`color_format=0`, `Colorspace=0`, `color_depth=8`. UI logunda
`first frame: mode and colour in one commit` satırı görülüyor.

### Ağsız açılış, sonradan gelen ağ ve regresyonlar [ölçüldü]

- **Yöntem:** Wi-Fi tek yönetim yolu olduğu için ağ kapatılmadı. Bunun yerine
  geçici bir drop-in ile `wpa_supplicant` 60 s geç başlatıldı. Birim
  `Type=simple` olduğu için hemen active oluyor ve `network.target` bekletilmiyor.
  SSH yaklaşık 75 s sonra kendiliğinden geri geldi. İlk denemede `ExecStartPre`
  içinde `sleep` kullanılmıştı; bu, `network.target`'ı da beklettiği için ölçüm
  geçersiz sayıldı (B4, tabloda yok). Test birimleri sonra kaldırıldı.
- **Ağ yokken (B7, cihazdaki sürücüyle ekran görüntülü):** Ana Menü, Medya
  ekranı ve Ayarlar açıldı. Daemon `up` 10,5 s'den itibaren yanıt verdi.
- **Ağ geldikten sonra:** DHCP 73,4 s'de geldi. UI yükleyicisi 8. denemesinde
  (76,2 s) kataloğu aldı: 15 satır, kitaplık dışında 204 öğe. Hiçbir servis
  restart edilmedi; dört servisin hepsinde `NRestarts=0`.
- **UI restart:** Modeset ve retrain yok. fdstore devri korundu
  (`took back` … `kept display device is closed`). İlk kare 740 ms'de geldi.
- **Kodi'ye geçiş ve dönüş (film yok):** Modeset yok. Kodi'nin bıraktığı
  `color_depth=10`, UI'ın ilk commit'inde 8'e döndü.
- **Kodi'de HDR10 örneği (yerel dosya) ve dönüş:**
  - Dönüşte renk RGB, 8 bit, SDR; HDR blob'u boş.
  - Dönüşte bir 144 Hz modeset oldu (`tmdsclk 583593`). Nedeni UI'ın kristal
    hatası düzeltmesi: UI, Kodi'nin bıraktığı 583600 kHz yerine 583593 kHz
    istiyor. Eski `SETCRTC` yolu da aynı `wire_mode()` saatini kullandığı için
    bu davranış değişiklikle gelmedi (kod okumasıyla; eski binary ile karşılaştırma
    yapılmadı).
- **Tarayıcı ve dönüş:** Tarayıcı kendi planıyla 120 Hz kurdu; dönüşte UI
  144 Hz'e geçti. Renk RGB, 8 bit. Bu iki modeset, iki ayrı mod seçiminin
  sonucu ve değişiklikten önce de vardı.

### `color_depth` ile ilgili değişikliğin savunması

**Ne değişti, ne değişmedi.**

- `color_depth` için yeni bir karar kuralı **eklenmedi**. Değer eskiden de,
  şimdi de aynı yerden geliyor: açılışta seçilen `ColorMode`
  (`offer.colour(&setting, chosen)`) ve onun bit sayısının kaynak profildeki
  vendor ABI karşılığı (`source.color_depth_value(colour.bits)`). İlk kare yolu
  (`atomic_modeset`, `platform.rs`) ile eski yol (`signal_output_colour`) aynı
  fonksiyonu çağırıyor.
- Değişen yalnız **yazıldığı an ve yazılış biçimi**:
  - Eski davranış: ilk kare `SETCRTC` ile property'siz kuruluyordu. Hemen
    ardından `signal_output_colour`, `HDR_OUTPUT_METADATA`, `Colorspace`,
    `color_depth` ve `color_format`'ı **ayrı ayrı** `set_property` ile, yani
    her biri ayrı bir legacy commit olarak yazıyordu.
  - Yeni davranış: dördü de UI'ın modunu kuran **tek atomic commit'in**
    içinde, `TEST_ONLY` ile denendikten sonra yazılıyor. Ardından
    `applied = colour` atanıyor; bu yüzden `signal_output_colour`'ın
    `applied == format` erken dönüşü aynı değerleri bir daha yazmıyor.
- `hdmi-prepare`'in sıfırlama bloğu `color_depth`'e hiçbir zaman dokunmadı;
  bu değişiklikten sonra da dokunmuyor. Bu yüzden Kodi'nin bıraktığı
  `color_depth`'i eskiden de UI düzeltiyordu, şimdi de UI düzeltiyor; fark
  yalnız düzeltmenin artık modeset'le aynı commit'te olması.

**Neden gerekliydi.** Script'teki ölçüm notuna göre bu sürücüde açılıştan
sonraki ilk property yazımı, değer aynı olsa bile link'i yeniden eğitiyor. İlk
karedeki property'ler moddan ayrı yazıldığında, ya seed (bugüne kadar) ya da
UI'ın sonraki `set_property` çağrıları ayrı bir link olayı üretir. Hepsi modun
commit'ine katıldığında açılışta tek olay kalıyor. Bu ölçüldü: B3, B5, B6, B8 ve
B9'da fbdev'den sonra yalnız bir `final tmdsclk` satırı var.

**Kanıt [ölçüldü].**

| Durum | `color_depth` | Not |
|---|---|---|
| Soğuk açılış sonrası (B3) | 8 | `RGB888_1X24`, `color_format=0`, `Colorspace=0`; fbdev'den sonra tek link olayı |
| Kodi açık, film yok | **10** | Kodi'nin kendi yazdığı değer |
| Kodi'den UI'a dönüş | **8** | `first frame: mode and colour in one commit`; o test penceresinde dmesg'de modeset veya `final tmdsclk` **yok** |
| Kodi HDR10 örneği → dönüş | 8 | HDR blob'u boş, SDR; dönüşteki tek modeset kristal saat düzeltmesinden (yukarıda) |
| UI restart / tarayıcıdan dönüş | 8 | UI restart'ta modeset yok; tarayıcı 120 Hz kurduğu için dönüşte 144 Hz modeset (önceden de vardı) |

**Bilinen sınırlar ve geri dönüş.**

- `colour` değeri `None` ise (EDID yok ya da o modda SDR renk yok) atomic yol
  hiç denenmiyor; eski `SETCRTC` yolu aynen çalışıyor. `hdmi-prepare` de EDID
  yoksa `owner` modunda eskisi gibi sıfırlıyor.
- Sürücü atomic commit'i reddederse UI bunu loglayıp
  (`one commit refused (...); SETCRTC`) eski yola dönüyor. Bu dal cihazda
  tetiklenmedi, yani çalıştığı ölçülmedi.
- Değişiklikten sonra ilk karede `output colour` / `output format` log satırları
  artık basılmıyor, çünkü değerler zaten uygulanmış durumda ve tekrar
  yazılmıyor. Tel üzerindeki durum debugfs (`dri/0/summary`) ve
  `modetest -c` ile doğrulandı.
- 10 bit SDR seçilmiş bir ayarla (`colours` içinde 10 bit) açılış
  denenmedi. Kod yolu aynı fonksiyonu kullanıyor, ama bu ölçülmedi.
- Geri dönüş: `rollback.sh` (mediabox-tv binary'si). Unit tarafında
  `MEDIABOX_HDMI_CONNECTOR_RESET=owner` kaldırılırsa eski sıfırlama geri gelir.

### Açık kalanlar

- Ağsız durumda Medya sayfası boş kalıyor ve bir açıklama göstermiyor. Yerel
  kitaplık ayrı uç noktadan ve ayrı sekmeden çalışıyor; ağsız açılışlarda
  `library failed` görülmedi. Ana sayfa raflarına bilerek konmadığı için bu
  davranış değişiklikten önce de vardı. Bir worker düzeltmesi denendi; TV'ye
  etkisi olmadığı için geri alındı.
- `hdmi-prepare` satır 113'teki `[ ! -s edid ]` kontrolü sysfs'te her zaman
  doğru dönüyor; bu yüzden her çağrıda bir `detect` yazılıyor. Bu da değişiklikten
  önce vardı ve gözlenen bir etkisi yok; kapsam dışı bırakıldı.
- `mediabox-tv` host testlerinde `hit::gl::a_kept_drawing_is_not_drawn_for_every_frame`
  tam paralel koşuda bir kez zaman aşımına düştü; tek başına iki kez geçti.
  `platform.rs` sunum yolunu kullanmıyor.
- **Rollback:** Asıl rollback kopyaları cihazda
  `/root/mediabox-verify/boot-experience/rollback-20261126-*/` ve
  `/opt/rk3588-mediabox/rollback/only-20261126T080958Z/rollback.sh`
  (mediabox-tv) altında.

## 5. Doğrulanamayan hususlar

- Güç → ilk fiziksel görüntü süresi, monitörün sinyale kilitlenme süresi ve
  her tazelemede kaç ms siyah ekran göründüğü. Kamera veya fotodiyot gerekir.
- Çalışan bootloader'ın SPI'daki (Ultra) mı yoksa eMMC'deki (Plus) mı olduğu.
  Seri konsol gerekir.
- A0'daki ikinci 60 Hz modeset'in (`T_k` 8,2 s) kaynağı. A1'de görülmedi.
- Panelde 3,9–7,9 s aralığında systemd durum metninin görünüp görünmediği.
- §4'teki ≈ 12–13 s tahmini. Birim değişiklikleri yapılmadan ölçülemez.
- 4K60 çıkışta davranış. Bu oturumda yalnız 1440p144 monitör bağlıydı. Sony
  girişinin tercih ettiği mod 1080p ise kernel 1080p kurar ve UI 4K60'a geçer:
  yine tek modeset olur, ama doğrulanmadı.
- Tanıtım videosunun ilk kare süresi. Ölçülen 199 ms, HTTP oturumuyla oynatılan
  bir filme ait; yerel dosyada ve soğuk önbellekte farklı olabilir.

---

## Kaynaklar

- [R1] Rockchip, *Boot option* (BootROM ve önyükleme aşamaları):
  https://opensource.rock-chips.com/wiki_Boot_option
- [R2] Radxa/Rockchip U-Boot `next-dev-v2024.10`,
  `drivers/video/drm/rockchip_display.c` (`load_bmp_logo`,
  `rockchip_display_fixup`):
  https://github.com/radxa/u-boot/blob/next-dev-v2024.10/drivers/video/drm/rockchip_display.c
- [R3] Aynı ağaç, `arch/arm/mach-rockchip/resource_img.c` ("partition
  priority: boot/recovery > resource", `PART_LOGO`, ham `blk_dread`):
  https://github.com/radxa/u-boot/blob/next-dev-v2024.10/arch/arm/mach-rockchip/resource_img.c
- [R4] Aynı ağaç, `arch/arm/mach-rockchip/kernel_dtb.c` (`init_kernel_dtb`):
  https://github.com/radxa/u-boot/blob/next-dev-v2024.10/arch/arm/mach-rockchip/kernel_dtb.c
- [R5] Armbian vendor kernel `rk-6.1-rkr5.1`,
  `drivers/gpu/drm/rockchip/rockchip_drm_logo.c` (`rockchip_drm_show_logo`,
  `init_loader_memory`, `setup_initial_state`):
  https://github.com/armbian/linux-rockchip/blob/rk-6.1-rkr5.1/drivers/gpu/drm/rockchip/rockchip_drm_logo.c
- [R6] systemd, `systemd-networkd-wait-online.service(8)` ve
  `network-online.target` önerisi:
  https://www.freedesktop.org/software/systemd/man/latest/systemd-networkd-wait-online.service.html ,
  https://systemd.io/NETWORK_ONLINE/
- [R7] Linux DRM KMS atomic modeset (`DRM_MODE_ATOMIC_ALLOW_MODESET`):
  https://docs.kernel.org/gpu/drm-kms.html
- [R8] Armbian build, `orangepi5-plus` vendor U-Boot kaynağı ve `boot.cmd`:
  https://github.com/armbian/build
- Repo içi: `docs/display-pipeline.md` (kural 10, fdstore devri),
  `packaging/mediabox-hdmi-prepare` (`video=` paniği, retrain notu),
  `rust/crates/mediabox-tv/src/video.rs`, `src/fdstore.rs`, `src/platform.rs`.
