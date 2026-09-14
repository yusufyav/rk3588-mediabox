# Gömülü oynatıcı — 15 Eylül 2026

Kaldığımız yer ve yapılacaklar. Buradaki her sayı cihazdan ölçüldü; ölçülmemiş
olan "ölçülmedi" diye yazıldı.

Cihaz: Orange Pi 5 Ultra (RK3588), `6.1.115-vendor-rk35xx`, panel HDMI-A-1
2560x1440p60 (SDR monitör, EDID 0 bayt).

Commit'ler: `9b831e9`, `b524807`, `46249a1`, `ac06e30` — hepsi `main`'de ve
push edilmiş.

---

## 1. Oynatıcı neden yoktu

`d12f24f` oynatıcıyı kurmuştu: mpv 0.41, cihazda Rockchip ffmpeg'e karşı
derlenmiş, iki yama, `--vo=dmabuf-wayland`. `67b8d8d` TV arayüzünü (sway içinde
Chromium kiosk) ürünün kendi Slint binary'siyle değiştirdi ve **oynatıcının tek
satırına dokunmadı** — ama `dmabuf-wayland` bir Wayland compositor'ına çizer, o
commit de compositor'ı kaldırdı.

```
/run/mediabox-ui/                       boş — wayland soketi yok
mediabox-player.service: Main process exited, status=2/INVALIDARGUMENT
```

İkinci yarısı: detay ekranındaki tek oynatma düğmesi "Kodi'de Oynat"tı. Daemon'da
`MediaPlayHere` hazır duruyordu, arayüzden hiç çağrılmıyordu.

## 2. Filmin gittiği yer

```
Video Port0  PLANE_MASK  Cluster0=0x1 … Esmart0=0x4 …  value: 5
plane 57  Cluster0      zpos 0…11   XR24 AR24 …
plane 73  Esmart0-win0  zpos 0…11   FEATURE scale=0x1
          NV12 NV21 NV16 NV61 NV24 NV42 NV15 NV20 NV30 …
          modifiers  NV12: LINEAR  NV15: LINEAR
```

Bir video portu iki pencere sürüyor. Arayüz DRM master'da kalıyor, film MPP'nin
çözdüğü tamponla Esmart0'a gidiyor, ölçekleme ve renk dönüşümü donanımda.

Kolay olmayan iki nokta:

* `DRM_CLIENT_CAP_UNIVERSAL_PLANES` istenmedikçe çekirdek birincil ve imleç
  düzlemlerini gizliyor, **ve bu sürücü Esmart0'ı `type: Cursor` bildiriyor** —
  yani video taşıyan tek pencere sürece görünmüyordu.
* Portun hangi düzlemi kabul edeceği userspace'ten okunamıyor: `PLANE_MASK` ve
  `NAME` bitmask özellikler, drm 0.14 isimleri yalnız enum'lar için tutuyor.
  `possible_crtcs` bakan ilk seçim Esmart1'i seçti, bu port onu sürmüyor.
  Adaylar yeteneğe göre sıralanıp `SetPlane` ile deneyerek karara bağlanıyor.

## 3. Tel

`packaging/mpv/vo_mediabox.c` + `rust/crates/mediabox-tv/src/video.rs`. Kare
başına tek sabit boyutlu mesaj (88 bayt), dma-buf tanıtıcıları `SCM_RIGHTS` ile.
Kare, yerine bir sonraki geçtikten sonra geri veriliyor — `SetPlane` bir sonraki
dikey boşlukta etkili oluyor, daha erken bırakmak MPP'yi panelin hâlâ okuduğu
tampona çizmeye davet ediyor.

Kare kendi rengini taşıyor: düzlem BT.601 sınırlı başlıyor, `COLOR_ENCODING` ve
`COLOR_RANGE` çözücünün söylediğinden ayarlanıyor.

## 4. Arayüz filmin üstünde

İki pencerenin de `zpos`'u 0–11 aralığında ve ikisi de pre-multiplied harmanlıyor.
Birincil 0'da, Esmart0 11'de başlıyordu — film arayüzü tamamen örtüyordu.

```
Cluster0-win0  AR24  zpos 11   ← arayüz, üstte
Esmart0-win0   NV15  zpos 0    ← film, altta
```

Tarama tamponları ARGB8888. Ekran devredilirken sıra geri alınıyor: bu özellik
bağlayıcıya ait, Kodi kendi pencerelerini kendi diziyor.

## 5. Düzeltilen kusurlar

| Kusur | Kök neden |
|---|---|
| Kumanda oynatıcıyı hiç kontrol etmiyor | Film bayrağı 500 ms'lik "bitti mi" saati tarafından ilk kareden önce siliniyordu; her tuş çalışmayan Kodi'ye gidiyordu |
| Şeritlerde ana ekranın duvar kâğıdı | Aynı kök: arayüz Home'a dönüyordu |
| Ad yerine `73c91b7f…` | Adı oturum adresinden türetiyordum; katalog filmi onaltılık kimlikli bir oturumdan okunuyor |
| 99 dakikalık film 3:45 | mpv'nin `duration`'ı konteynerin; vekillenen oturumda konteyner süresi yok, mpv aktardığından tahmin ediyor |
| Sarma imkânsız | ±10/±30 sn iki saatlik filmi geçemez |
| Sarma filmi bitiriyordu | Katalog süresi dosyadan uzun olabiliyor; son saniyeye gidince EOF |

## 6. Ölçümler

Katalogdan, tam ürün yolu (arayüz → denetim düzlemi → medya oturumu → oynatıcı →
düzlem):

```
Sexual Chronicles of a French Family   85 min → katalogdan 5100 s
NV12 1920x1080 → 2560x1440, Esmart0
14 × Sağ      0:15 → işaret 15:13 ;  Ok → 915 s
60 × Sağ + Ok 5088 s, hâlâ oynuyor, düzlem ayakta

In the Mood for Love                   duration 5940 s = 1:39:00
```

Yerel 4K dosya:

```
Esmart0-win0  NV15  src 3840x2080  dst 2560x1383+0+28
              color-encoding[BT.2020]  csc mode[3]
mpv %6-9 CPU · arayüz ~%0 · 1845 kare · 0 düşük · 23.96 fps
Ok "30 sn ileri"  150.4 → 184.2
Ok "Duraklat"     paused true, iki saniye arayla 184.559 / 184.559
Ok tekrar         devam, 186.5
Geri (açık)       342 kareden sonra düzlem bırakıldı, oynatıcı birimi gitti
```

93 test, `mediabox-kiosk-smoke` 16/16.

---

## Yapılacaklar

### Önce

1. **Oynatıcı katmanının görünümü — referans bekliyor.**
   İki deneme de reddedildi: ilki herkesin kullandığı hap düğmeler ve VCR
   simgeleri; ikincisi onların çıkarılmışı — bir dil değil, bir eksiklik.
   Kullanıcının vereceği referansa (Apple TV / Netflix / Plex / Stremio ya da
   bir fotoğraf) bakarak yapılacak, sonra ekran görüntüsüyle yan yana
   karşılaştırılacak. **Kendi zevkimle üçüncü bir deneme yapılmayacak.**

2. **CPU.** mpv %6-9, Kodi %2. Mazeret değil, ölçülüp düşürülecek iş. İlk
   bakılacak yer: `vo_mediabox` kare başına `PRIME_FD_TO_HANDLE` + `ADDFB2`
   yapıyor; MPP havuzu 3-4 tampon döndürdüğü için bunlar tampon başına bir kez
   yapılıp önbelleklenebilir (arayüzün kendi tarama tamponlarında zaten öyle).

3. **Detay ekranının düğmeleri.** Hâlâ hap + simge. 1 numaradaki referans
   geldikten sonra aynı dille yapılacak; önce değil.

### Sonra

4. **Ses çıkışı seçimi.** Çekirdek USB kulaklığı görüyor
   (`card 3: GameBuds [Arctis GameBuds]`), üründe seçecek yer yok.
5. **Wi-Fi.** NetworkManager yok, arayüz yok.
6. **Bluetooth.** `bluetooth.service` inactive, eşleştirme arayüzü yok.
7. **Web UI'da donanım hızlandırmalı önizleme/oynatma** (Jellyfin gibi).
8. **Home yoğunluğu** — sağda ve altta boş alan.
9. **Süreç içi DRM yeniden modeset** (hot plug şu an birim yeniden başlatmayla
   telafi ediliyor).
10. **HDR regresyonu** — takılı ekran SDR monitör, `edid` 0 bayt. Bu panelde
    ölçülemez; HDR TV takılınca.

### Bilinen açık kusurlar

* Sarmada çubuk yalnız işaretin yerini gösteriyor; filmin gerçek konumu çubuk
  üzerinde işaretlenmiyor (büyük rakamın yanında küçük olarak yazılıyor).
* Web UI'dan başlatılan film sahipleniliyor ama adı ve süresi gelmiyor —
  `MediaPlayHere`'ın `title`/`duration_seconds` alanlarını web arayüzü henüz
  göndermiyor.
* Kodi'nin kendi "Şimdi Oynatılan" ekranı hâlâ eski simge setini kullanıyor.
