# MediaBox V3 — First-Party Product UI

**Tarih:** 2026-09-12
**Hedef:** Orange Pi 5 Ultra / RK3588, `10.27.27.25` (`orangepi5-ultra`, Armbian 26.11 trixie, kernel 6.1.115-vendor-rk35xx)
**Başlangıç:** `4b240add734450117515453e2344c3fae17b723e`

---

## 1. Sonuç

Bu milestone **PASS token ile kapatılmamıştır.** Ürün hedeflerinin büyük bölümü
hedef cihazda gerçek veriyle doğrulandı ve production'a alındı; fakat brief'in
PASS için şart koştuğu iki kabul kalemi **doğrulanamadı**:

1. **Görsel acceptance (TV):** Kullanıcı, TV'deki arayüzde ölçek bozukluğu,
   yırtılma ve yavaşlık bildirdi (bkz. §11). Üç kök neden bulunup düzeltildi ve
   son durumda arayüzün TV'de gerçekten yüklendiği ölçüldü, fakat düzeltme
   sonrası görüntü kullanıcı tarafından onaylanmadı.
2. **Gerçek CEC uzaktan kumanda acceptance:** Fiziksel kumanda basımı
   gözlemlenemedi. 70 saniyelik `mediaboxctl input monitor` penceresinde
   **0 basım** kaydedildi (kimse basmadı), dolayısıyla fiziksel zincir
   doğrulanmadı.

Bu iki kalem dışındaki her şey hedefte çalışır durumdadır ve aşağıda kanıtıyla
verilmiştir. `MEDIABOX_V3_PRODUCT_UI_PASS` bilinçli olarak yazılmamıştır.

---

## 2. Seçilen stack ve gerekçesi

**Leptos 0.8.20, CSR (client-side rendering), `wasm-bindgen 0.2.128` ile
paketlenmiş; trunk/dx gibi ek build aracı yok.**

| Karar | Gerekçe |
|---|---|
| Leptos (Yew/Dioxus yerine) | İnce taneli reaktivite; VDOM diff'i yok. RK3588 sınıfı bir cihazda saniyede bir yoklanan bir ekran, VDOM ile her turda ağaç karşılaştırır. Bu proje o farkı fiilen yaşadı (§11.1). |
| CSR (SSR değil) | Sunucu zaten Rust denetim düzlemi; ikinci bir render runtime'ı ikinci bir authority demekti. Bundle statik dosya olarak servis edilir. |
| Router crate yok | Ekran durumu tek bir `Route` enum'u + geri yığını. TV'de URL yok; `leptos_router` saf bağımlılık olurdu. |
| `wasm-bindgen-cli`, trunk yok | Zincir iki komut: `cargo build --target wasm32` + `wasm-bindgen`. Sürüm `Cargo.toml`'da `=0.2.128` olarak sabitlenir ve build betiği CLI ile crate sürümünü karşılaştırıp uyuşmazlıkta durur. |
| `gloo-net` yok | `fetch` doğrudan `web-sys` ile (~40 satır). Bir bağımlılık eksik. |
| Ayrı workspace | UI kendi `[workspace]`'inde; Leptos'un bağımlılık ağacı denetim düzleminin `Cargo.lock`'una girmez, `cargo test --workspace` hızlı kalır. |

**Bundle:** `mediabox_ui_bg.wasm` ~694 KB + `mediabox_ui.js` 44 KB + `style.css`
17 KB ≈ **780 KB**. WASM mimariden bağımsızdır: geliştirme makinesinde derlenir,
hedefte Rust toolchain yoktur.

---

## 3. Mimari ve veri akışı

```
 TV (1920x1080)            LAN tarayıcı / telefon
   sway + Chromium kiosk        │
        │ 127.0.0.1:8787        │ <lan-ip>:8788
        └───────────┬───────────┘
                    v
         mediaboxd-rs  (TEK authority)
          ├── GET  /            statik WASM ürün arayüzü
          ├── POST /v1/control  tipli komut zarfı
          ├── GET  /v1/events   SSE — CEC girişleri UI'a
          └── GET  /v1/media/session/{id}  oturum baytları röle
                    │ (yalnız loopback)
      ┌─────────────┼──────────────┐
      v             v              v
 Kodi JSON-RPC  media worker   CEC /dev/cec0
 127.0.0.1:8080 127.0.0.1:8790  (mediabox-cec)
                    │
            headless Stremio + inspector + policy + ffmpeg
```

Değişmeyen kurallar:

* **Media worker LAN'a açılmadı.** Tarayıcının ihtiyaç duyduğu her şey —
  önizleme oturum baytları dahil — `mediaboxd-rs` üzerinden röle edilir.
* **LAN dinleyicisi yalnız özel ağ eşlerini kabul eder** (RFC1918 / ULA /
  link-local + v6-mapped IPv4). Genel internet eşleri `accept` sonrası
  reddedilir. Birim testleri: `web.rs::lan_listener_admits_private_and_refuses_public`.
* **Ekran sahipliği tek yerde.** Kodi ve TV arayüzü aynı anda DRM master
  olamaz; geçiş yalnız `SurfaceManager` üzerinden yapılır ve giriş kipi
  (`InputMode`) ekranla birlikte değişir.

---

## 4. Eklenen API ve gateway noktaları

**Yeni `Request` varyantları** (`mediabox-core`, hepsi tipli; serbest komut yok):

`Diagnostics`, `MediaCapabilities`, `MediaHome`, `MediaCatalog`, `MediaMeta`,
`MediaSubtitles`, `MediaLibrary`, `MediaLibraryItem`, `MediaResolve`,
`MediaStreamPlan`, `MediaPlayOnKodi`, `SurfaceStatus`, `SurfaceSwitch`.

**`MediaPlayOnKodi` — tek yetkili oynatma yolu.** Sırası kasıtlıdır:

1. mevcut tüm medya oturumlarını durdur (önizleme kodlayıcısı ekranı geçemez),
2. yeni oturumu oluştur,
3. ekranı Kodi'ye devret + giriş kipini `kodi_playback` yap,
4. Kodi JSON-RPC hazır olana kadar bekle (≤30 sn),
5. `Player.Open`.

Adım 2'den sonraki her hata oturumu durdurur — reddedilen bir `Player.Open`
arkasında okuyucusuz ffmpeg bırakmaz.

**HTTP yüzeyi** (`mediaboxd-rs/src/web.rs`, el yazımı; framework eklenmedi):
statik servis (yol kaçışı testli), SSE, oturum rölesi, ETag/304.

**Media worker'a eklenen:** `GET /media/library`, `GET /media/library/{id}` —
operatörün bildirdiği kitaplık (`/etc/mediabox-library.json`). Kitaplık
kaynakları da aynı inspect → policy → session yolundan geçer; kestirme yoktur.

---

## 5. Gerçek Stremio içerik kanıtı

Kurulu eklenti koleksiyonu (hesap yok, Stremio varsayılanı, 7 eklenti):
Cinemeta, YouTube, WatchHub, **Public Domain Movies**, OpenSubtitles v3,
OpenSubtitles, Local Files.

```
$ mediaboxctl media status
Medya: hazır
Yetenek profili: rk3588_orangepi5_production
Sağlayıcı: 7 eklenti, oturum yok, akış sunucusu erişilebilir
Etkin oturum: 0
Torrent ağı: TORRENT_NETWORK_BLOCKED
```

Ana ekrandaki raylar gerçek Cinemeta kataloglarıdır (`Popüler Filmler`,
`Popüler Diziler`, `Öne Çıkan Filmler` …). Ekran görüntülerindeki her poster,
başlık, yıl, IMDb puanı ve özet addon'dan gelir. **Mock katalog yoktur; boş
dönen bir katalog hiç çizilmez.**

Detail ekranı `GET /media/meta/...` ve `GET /media/streams/...` sonuçlarını
gösterir. Örnek (`tt0063350`): 2 kaynak — biri `external` (MUBI, abonelik),
biri `torrent` (1080p, 1.51 GB). İkisi de gerçek durumlarıyla etiketlenir.

---

## 6. Kaynak seçici ve policy entegrasyonu

Seçili kaynak için `POST /media/plan` çalıştırılır; ekranda gösterilen her
teknik değer **gerçek ffprobe + policy çıktısıdır**, sürüm adından
çıkarılmaz. Hedefte ölçülen gerçek bir örnek:

```
mode: Direct | needsSession: False | videoCopied: True
audio: DecodeToPCM | video: Direct SDR
video: h264 High 1280x720 yuv420p 8-bit bt709
container: mov,mp4,m4a,3gp,3g2,mj2  duration 6047.3 s  size 536 MB
```

UI'daki karşılığı: `Doğrudan oynatma` · `720p · 1280×720` · `H264 High` ·
`8-bit` · `29.970 fps` · `SDR` · `MP4` · `AAC stereo` + policy gerekçeleri.

**Dürüstlük kuralları kodda:**

* **Dolby Vision uydurulmaz.** `dolbyVision` alanı varsa HDR rozeti
  DV profiliyle çizilir. `bl_signal_compatibility_id == 0` (Profil 5 gibi
  çapraz uyumsuz hâller) **kırmızı `desteklenmiyor`** olur; asla HDR10 gibi
  gösterilmez. Yetenek profilinde `dolbyVisionPipeline: false`.
* **Ses politikası korunur.** `TranscodeToAC3` durumunda "Ses AC-3 5.1'e
  dönüştürülür; özgün kayıpsız akış korunmaz. Video kopyalanır, yeniden
  kodlanmaz." yazılır; nesne tabanlı ses varsa kaybı açıkça söylenir.
  4K yazılımsal video dönüştürme profilde kapalıdır ve Ayarlar > Oynatma'da
  böyle gösterilir.
* **Torrent kaynak sağlığı gizlenmez.** Torrent kaynakları "oynatma, eş
  bağlantısı kurulabilmesine bağlıdır; ağ engelliyse veri akmaz" notuyla
  işaretlenir; `external` kaynaklar "MediaBox bu servisi cihazda oynatamaz"
  der. Probe başarısız olursa sessiz düşme yerine hatanın kendisi gösterilir.

**Ön İzle** yalnız `preview.mode == BrowserDirect` **ve** çözülen URL'in
tarayıcı tarafından gerçekten getirilebildiği (http/https, loopback değil)
durumda etkindir; aksi hâlde düğme devre dışıdır ve gerçek gerekçe listelenir.
Sahte önizleme üretilmez ve bu yol hiç ffmpeg başlatmaz.

---

## 7. Kodi oynatma acceptance (hedefte, gerçek içerik)

Gerçek public-domain kaynak, tam yetkili yoldan:

```
$ mediaboxctl media play "https://archive.org/download/night-of-the-living-dead_202311/…mp4"
ok: True
mode: Direct | needsSession: False | videoCopied: True
audio: DecodeToPCM | video: Direct SDR
surface: {'active': 'kodi', 'kodi_active': True, 'ui_active': False, 'ui_installed': True}

$ mediaboxctl kodi status          # 15 sn sonra
state: playing speed: 1
time: {h:0, m:0, s:8}  total: {h:1, m:40, s:47}
file: https://archive.org/download/night-of-the-living-dead_202311/…
```

Transport kontrolleri (hepsi hedefte, gerçek oynatma üzerinde):

| Komut | Beklenen | Gerçekleşen |
|---|---|---|
| `kodi play-pause` | duraklar | `state=paused speed=0 t=0:16` |
| `kodi play-pause` | devam eder | `state=playing speed=1 t=0:19` |
| `kodi seek 300` | +5 dk | `t=3:06 → 5:04` |
| `kodi seek -- -120` | −2 dk | `t=5:04 → 3:06` |
| `kodi stop` | durur | `state=idle` |
| oynatma sonrası | kodlayıcı kalmaz | `pgrep -c ffmpeg = 0` |

### Bu milestone'da bulunup düzeltilen iki gerçek hata

1. **`Player.Open` zaman aşımı.** Kodi istemcisi tüm çağrılar için 2 sn
   kullanıyordu; `Player.Open` ise akış fiilen açılana kadar cevap vermez.
   Uzak bir kaynakta bu her seferinde `KODI_ERROR` üretiyordu. Artık durum
   yoklamaları hızlı başarısız olur, `Player.Open` 90 sn bütçe alır.
2. **`Player.Seek` geçersiz parametre.** `value` bir union; çıplak bir zaman
   nesnesi `Received value does not match any of the union type definitions`
   ile reddediliyordu. `{"time": {...}}` olarak sarmalandı + regresyon testi.

---

## 8. Navigasyon acceptance

**Geometrik yön çözümü.** Aday elemanlar, odaklı elemanın dikdörtgenine göre
puanlanır: istenen eksendeki mesafe + eksen dışı kaymanın 4 katı. Puanlama
**yerleşim koordinatlarında** (offset zinciri) yapılır, viewport'ta değil —
böylece süren bir kaydırma animasyonu cevabı değiştiremez.

Klavye, 120 ms tuş tekrarı ile (hedef üzerinde ölçüldü):

```
start        Aç (hero birincil düğmesi)      ← data-autofocus doğru çalışıyor
ArrowRight   Ara
ArrowRight   The Whisper Man aç
ArrowRight   Star Wars: The Mandalorian…
ArrowLeft    Star Wars… → The Whisper Man    ← geri dönüş simetrik
ArrowDown    Silo aç (x=278, aynı sütun)     ← alttaki raya hizalı iniyor
ArrowUp      The Whisper Man aç              ← aynı elemana dönüyor
```

**Daemon giriş veriyolu üzerinden** (CEC'in yayın yaptığı aynı normalize
eylem otobüsü; `input_inject` ile enjekte edilip SSE ile UI'a ulaştı):

```
inject down/down/right/right/down/up/left  → raylar arası doğru gezinme
inject ok    → Detail açıldı
inject back  → Detail
inject home  → Home (Aç)
```

**Odak hiç kaybolmuyor.** Yoklama altındaki dört ekranda odaklı DOM düğümünün
kimliği ölçüldü:

```
Home (boşta)                  6 sn → 0 hata
Şimdi Oynatılan (1.2 sn yoklama) 8 sn → 0 hata
Ayarlar / Oynatma             7 sn → 0 hata
Ayarlar / HDMI-CEC (düğmeli)  7 sn → 0 hata
```

**Doğrulanmayan:** fiziksel CEC kumanda basımı (§1.2). CEC adaptörü canlıdır
(`/dev/cec0`, `dw_hdmi_qp`, fiziksel adres `3.0.0.0`, mantıksal `[4]`), giriş
aygıtları `EVIOCGRAB` ile **exclusive alınmaz** (`mediabox-input` bunu açıkça
yapmaz), dolayısıyla USB klavye doğrudan tarayıcıya ulaşır.

---

## 9. Responsive ve LAN acceptance

Hedef üzerinde ölçülen yerleşim:

| Genişlik | Kök yazı | Kart | Yatay kaydırma |
|---|---|---|---|
| 3840×2160 | 42.0 px | 407 px | yok |
| 1920×1080 | 21.1 px | 204 px | yok |
| 412×915 (telefon) | 15.0 px | 116 px | yok |

LAN geçidi, geliştirme makinesinden (`10.27.27.x`):

```
index=200  wasm=200  control=200   (http://10.27.27.25:8788/)
```

Tarayıcıdan MediaBox/Kodi kontrolü çalışır: aynı `POST /v1/control` yüzeyi,
aynı oynatma yolu, ayrıca Şimdi Oynatılan ekranında "Televizyonda Arayüze Dön".

---

## 10. Ekran görüntüleri (hedeften, gerçek veriyle)

`results/mediabox-platform/evidence/v3-product-ui/`

| Dosya | İçerik |
|---|---|
| `01-home.png` | Hero + gerçek Cinemeta rayları + Kitaplık |
| `02-detail-library.png` | Kitaplık başlığı, doğrudan HTTP kaynağı, policy rozetleri |
| `03-detail-catalog.png` | Katalog başlığı, kaynak seçici (external + torrent), sağlık notları |
| `04-settings-diagnostics.png` | CPU/RAM/depolama/sıcaklık, DRM, Kodi, CEC, ffmpeg oturumları |
| `05-settings-playback.png` | Yetenek profili, HDR biçimleri, DV hattı kapalı |
| `06-search.png` | Gerçek arama sonuçları |
| `08-mobile-home.png` | 412×915 responsive |

Görüntüler, TV kiosk'unun yüklediği **aynı origin, aynı bundle, aynı veri**
üzerinden `tools/ui-screenshot.py` (CDP) ile hedef cihazda alınmıştır.

---

## 11. TV-local kiosk: bulunan gerçek hatalar

Kullanıcı TV'de titreme bildirdi. Üç ayrı kök neden bulundu:

1. **Yoklama odağı düşürüyordu.** Şimdi Oynatılan 1.2 sn'de bir tüm ekran
   ağacını yeniden kuruyordu; odaklı düğüm yok olunca `focusout` → yeniden
   odaklama → sıçrama. Ekran, yapısı yalnız `Phase`'e bağlı olacak şekilde
   yeniden yazıldı; tikleyen her değer kendi sinyaline bağlı. Ayarlar'da
   yoklama yalnız Tanılama sekmesinde (tek hareketli ve tek düğmesiz panel)
   çalışır. Ölçüm: §8.
2. **Arayüz 4K'da çiziliyordu.** Panel 3840×2160 tercih ediyor; 1080p için
   tasarlanmış arayüz hem yarı fiziksel boyutta kalıyor hem de her odak adımı
   4K yeniden rasterleme yaptırıyordu. Çıkış `sway` ile **1920×1080@60**'a
   sabitlendi (`cage` çıkış modu ayarlayamıyor). Ayrıca tüm CSS tavanları
   viewport'la ölçeklenecek şekilde yükseltildi.
3. **Pahalı odak çizimi.** Odaklı kart `transform: scale()` + geniş bulanık
   gölge ile çiziliyordu; bu, poster görselini her karede yeniden rasterletir.
   Odak düz bir `outline`'a çevrildi, overlay'deki `backdrop-filter` kaldırıldı,
   `WLR_SCENE_DISABLE_DIRECT_SCANOUT=1` ile compositor tam kare sunar.

Ara denemelerde iki hata daha üretildi ve düzeltildi (dürüstlük için kayda
geçiyor): `--force-device-scale-factor=2` sayfayı ekranın sol üst çeyreğine
sıkıştırdı (geri alındı) ve `WLR_SCENE_DEBUG_DAMAGE=rerender` her kareyi tam
yeniden çizdirerek arayüzü yavaşlattı (geri alındı).

`sway` yapılandırmasında ayrıca iki parser tuzağı: satır devamı (`\`)
desteklenmiyor ve `,` / `;` komut ayırıcı sayılıyor. Bu yüzden tarayıcı
argümanları sessizce düşüyor ve Chromium varsayılan başlangıç sayfasını
açıyordu. Tarayıcı `packaging/mediabox-kiosk-browser` betiğine taşındı.

Son ölçülen durum (kullanıcı onayı beklemede):

```
kiosk: running  chromium=9   yapılandırma hatası=0
chromium argv URL: http://127.0.0.1:8787/
DRM: crtc-pos=1920x1080+0+0
media worker: GET /media/home, GET /media/library   ← arayüz TV'de gerçekten yüklendi
```

---

## 12. systemd / deploy durumu

| Birim | Durum |
|---|---|
| `mediaboxd-rs.service` | active, **enabled** — `--http 127.0.0.1:8787 --lan-http 0.0.0.0:8788 --ui-root /opt/rk3588-mediabox/ui --ui-unit mediabox-tv-ui.service` |
| `mediabox-media-worker.service` | active, **enabled** — `--library /etc/mediabox-library.json --allow-file-prefix /opt/rk3588-mediabox/assets/` |
| `stremio-server.service` | active |
| `kodi.service` | bağımsız direct-KMS servisi, değiştirilmedi |
| `mediabox-tv-ui.service` | kurulu, **kasten enabled değil** — ekranı kimin kullanacağına boot'ta systemd değil, çalışma anında `mediaboxd-rs` karar verir; tty1'deki bir konsol oturumu kendiliğinden ezilmez |

Kiosk birimi: `PAMName=login` (libseat'in DRM aygıtını alabilmesi için gerçek
oturum şart), kendi sanal terminali (`tty7`), ve `NoNewPrivileges`,
`ProtectSystem=strict`, `ProtectHome`, `PrivateTmp`, `DevicePolicy=closed` +
adlandırılmış aygıtlar ile sınırlandırılmış.

Deploy tek betikle tekrarlanabilir: `scripts/deploy-mediabox-v3.sh`
(aarch64 cross-derleme + wasm build + paketler + birimler + doğrulama).
Eski Python `mediaboxd` diskte rollback için duruyor, authority değil.

---

## 13. `mediaboxctl media status` formatter hatası

**Sebep:** `print_human()` içindeki dal seçimi `available` alanına bakıyordu.
Bu alanı hem CEC durumu hem medya çalışanı taşır, bu yüzden `media status`
CEC formatter dalına düşüp boş bir adaptör listesi yazdırıyordu.

**Düzeltme:** dal seçimi artık yalnız o yüke özgü alanlara bakar. `CecStatus`
her zaman `logical_addresses` serileştirir; medya durumu ise `provider` /
`capabilityProfile` taşır ya da `available` ile birlikte `logical_addresses`
taşımaz. `print_human` bir `Write` üzerine yazan `render()`'a ayrıldı, böylece
çıktı test edilebilir oldu.

**Hedefteki sonuç — beklenen / gerçekleşen:**

```
$ mediaboxctl media status         # beklenen: medya özeti
Medya: hazır
Yetenek profili: rk3588_orangepi5_production
Sağlayıcı: 7 eklenti, oturum yok, akış sunucusu erişilebilir
Etkin oturum: 0
Torrent ağı: TORRENT_NETWORK_BLOCKED

$ mediaboxctl cec status           # beklenen: değişmemiş CEC çıktısı
CEC adaptörü: /dev/cec0 (dw_hdmi_qp)
Fiziksel adres: 3.0.0.0
Mantıksal adresler: [4]
```

`--json` davranışı değiştirilmedi; `json_output_is_unchanged_by_the_formatter_fix`
testi bunu sabitler. Toplam 5 regresyon testi eklendi.

---

## 14. Test sonuçları

| Süit | Sonuç |
|---|---|
| `cargo test --workspace` (Rust) | **34 passed, 0 failed** |
| `media/tests` (Python, unittest) | **220 passed** (14'ü bu milestone'da eklenen kitaplık testleri) |
| Odak/navigasyon ölçümü (hedef, CDP) | 120 ms tuş tekrarında yön çözümü doğru; 4 ekranda odak kaybı 0 |
| Yerleşim ölçümü (hedef, 3 genişlik) | yatay taşma yok |

Bu milestone'da eklenen kayda değer testler: LAN eş politikası (özel kabul /
genel ret, v6-mapped dahil), statik yol kaçışı, HTTP başlık ayrıştırma,
kitaplık manifest ayrıştırma ve rota testleri, formatter regresyonları,
`Player.Seek` union şekli, surface birim adı doğrulama, tanılama anlık görüntüsü.

---

## 15. Bilinen gerçek sınırlamalar

1. **Torrent ağı engelli** (`TORRENT_NETWORK_BLOCKED`). Torrent kaynaklarının
   üst verisi çözülebiliyor; oynatma eş bağlantısına bağlı. UI bunu kaynak
   başına açıkça gösterir, sessiz düşmez.
2. **`BrowserRemux` önizleme üretilmiyor.** Mevcut oturum API'si yalnız Kodi
   hedefli bir `PlaybackDecision` ile oturum kurar; tarayıcı hedefli remux için
   ayrı bir karar yolu gerekir. Sahte önizleme üretmek yerine düğme devre dışı
   bırakılır ve gerçek gerekçe gösterilir.
3. **Kiosk tarayıcısı `--no-sandbox` ile çalışıyor.** `cage`/`sway` DRM master
   için root ister (Kodi ile aynı düzen), Chromium ise root altında kendi
   setuid sandbox'ını reddeder. Telafi: adres çubuğu yok, eklenti yok, tek
   sayfa (loopback), ayrı profil ve systemd sınırlandırması. Ayrı kullanıcıyla
   çalıştırma denendi; wlroots/Mali GBM yolunda tıkandı.
4. **Devam Et (Continue Watching) yerel.** Stremio hesabı olmadığı için
   sunucu tarafı kitaplık yok; ray, o tarayıcıda fiilen başlatılan başlıklardan
   kurulur ve boşken hiç çizilmez. Uydurma satır yoktur.
5. **Bluetooth paneli yalnız gözlemler.** Eşleştirme arayüzü yoktur; MediaBox'ın
   HID olarak gerçekten gördüğü aygıtlar listelenir, yoksa "yok" der.
6. **Kitaplıktaki iki referans klip 3 saniyeliktir** (HDR10 ve SDR, MP1b'de
   kabul edilmiş dosyalar). Ürün içeriği değil, HDR/SDR karar yolunu gerçek bir
   kaynak üzerinde göstermek için tutulur ve adlandırması bunu söyler.
7. **§1'deki iki kabul kalemi doğrulanmadı.**

---

## 16. Final commit

`COMMIT_SHA_PLACEHOLDER`

---

## 17. PASS token

Yazılmadı. Gerekçe §1'dedir. Eksik olan iki kalem:

* TV'deki görsel sonucun kullanıcı tarafından onaylanması,
* fiziksel CEC kumandayla Home → ray → Detail → kaynak → Kodi'de Oynat →
  Şimdi Oynatılan → play/pause/seek/back akışının gözlenmesi.

Bu ikisi karşılandığında token bu raporun sonuna eklenmelidir.
