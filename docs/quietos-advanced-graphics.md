# QuietOS grafik altyapısı: renderer, GPU-resident sahne, kapasite

Ölçülen kart: Orange Pi 5 Ultra, kernel 6.1.172-vendor-rk35xx, Mali-G610 MP4
(g24p0, OpenGL ES 3.2), 2560x1440 @ 143,997 Hz (bütçe 6,944 ms), 2026-11-25.
Varsayılan frekans politikaları (CPU ondemand, GPU simple_ondemand 300–1000
MHz, DDR 2400 MHz). Bağlı monitörün mod listesinde **3840x2160 yok**: 4K60
ölçülmedi. Orange Pi 5 Plus'ta çalıştırılmadı.

## Karar

1. **Renderer FemtoVG kalır.** Skia GPU bu kartta 3,3× CPU komutu, 3,7× GPU
   çevrimi ister ve karelerin ~%20'sini kaçırır; kısmi çizimi çalıştırıldığında
   da durum değişmez.
2. **Karmaşık sahneler GPU-resident olur** (`src/spatial`): çalışıyor, Slint'in
   kendi context'inde, tek geçişle. Sahne CPU maliyeti nesne sayısından bağımsız.
3. **AFBC taraması bu libmali ile mümkün değil.** Neden aşağıda; kapandı.
4. RGA: raster darboğazı bulunmadı, denenmedi. NPU/VPU: kapsam dışı.

Deneysel her şey cargo feature'ı (`skia`, `spatial`, `gl-count`; ürün
derlemesinde yok) **ve** ortam değişkeniyle kapalıdır. Üretimde kalan tek fark:
`ui/quiet/spatial.slint` (kullanılmayan eleman) ve `Spatial.on == false` koşulu.

## 1. FemtoVG ↔ Skia GPU

Skia, Slint'in hazır `OpenGLSurface`'i değil (kendi EGL display/context/swap'ını
kurar), bu platformun context'i üstünde bir `Surface` (`src/skia.rs`): aynı GBM,
DMA-BUF, atomik commit. `softbuffer` derlenmez; yazılım yolu yoktur.

Kısmi çizim üç ayrı nedenle **sessizce kapalıydı**; API'yi açmak yetmiyor:

| Engel | Kanıt | Deneydeki çözüm |
|---|---|---|
| `SkiaRenderer::partial_rendering_state()` bir rendering notifier varsa `None` döner | ürünün kare sayacı (`main.rs`) notifier takıyor; dirty bölge hiç hesaplanmadı | kareyi platform bildirir |
| glutin `buffer_age()` yalnız `EGL_EXT_buffer_age` listeliyse sorar; Mali yalnız `EGL_KHR_partial_update` listeler | yaş hep 0 → her kare tam ekran | `eglQuerySurface(0x313D)` doğrudan: yaş 2–6 |
| Slint 3 karelik geçmiş tutar | yaş 4+ → tam ekran | çözülmedi |

Üçünden ikisi giderilince kısmi çizim **gerçekten** çalıştı. Kanıt donanım
sayacı: kategori gezinmesinde gölgelenen quad 9,8 M → 3,2 M/kare, GPU 3,44 →
2,15 Mçevrim. Ana ekranda ise bölge her karede 1280x720 (mantıksal) = %100:
900 ms ışık geçişi zemini her kare değiştirir; kazanç sıfır. Süre kısaltılmadı.

| | FemtoVG | Skia | Skia kısmi (çalışır hâlde) | Skia kısmi + DMSAA |
|---|---|---|---|---|
| tek adım: Mkomut/kare | 3,6 | 12,1 | 12,5 | 4,6 |
| GPU Mçevrim/kare | 1,06 | 3,98 | 3,99 | 4,16 |
| kaçan vblank % | 0,5–1,0 | 20,7 | 19,7 | 15,0 |
| sunum p50/95/99 ms | 6,94/6,99/7,04 | 6,95/13,9/20,8 | 6,95/13,9/20,8 | 6,95/13,9/20,8 |
| draw · doku yükleme/kare | 140 · 1 KB | 29 · **8,7 MB** | 29 · 8,8 MB | 51 · 1 KB |
| dış okuma / yazma MB/kare | 1,6 / 4,4 | 9,4 / 8,4 | 9,4 / 8,4 | 11,0 / 8,8 |
| tuş tekrarı: Mkomut · Mçevrim · kaçan % | 5,3 · 1,11 · 2,8 | 12,3 · 3,95 · 22,5 | 13,1 · 4,05 · 25,1 | 5,7 · 4,69 · 8,2 |
| kategori: Mkomut · Mçevrim · kaçan % | 2,7 · 0,58 · 0,1 | 6,2 · 3,44 · 10,5 (tam) | 6,8 · 2,15 · 10,3 | 5,2 · 6,16 · 42,4 |
| GPU belleği · RSS MB | 28–36 · 111–121 | 153 · 246 | 151–297 · 243–395 | 131–224 · 221–320 |

(Skia sütunları 1–2 tur; farklar tur saçılmasının çok üstünde. "Skia" kategori
hücresi yaş=0'lı derlemedendir: fiilen tam çizim.)

Neden: yolları CPU'da maskeye çevirip yüklüyor (8,7 MB/kare); DMSAA bunu GPU'ya
taşır, bu kez fragment maliyeti artar. Path cache yardım etmiyor, sahnelerin
yolları her kare değişir. FemtoVG'nin 139 draw'u Mali'de pahalı değil. Binary: ürün
32,65 MB (kurulu 32,59), skia+spatial+sayaç 37,31 MB (+4,7); yeni bağımlılık `skia-safe/skia-bindings` (önceden derlenmiş C++
ikilisi) ve `libstdc++`.
Görsel: aynı düzen; FemtoVG'ye 32–34 dB (gradyan dither'ı, glif ve kenar raster
farkı; yakalamalar farklı dakikada, saat da farklı).

**FemtoVG'de kısmi çizim.** Mali yazmayı zaten eliyor (transaction elimination:
14 400 karonun ~11 000'i yazılmadan atılıyor; 14,7 MB yerine 4,4 MB). Kalan
kazanç yalnız gölgelemededir. FemtoVG'de kısmi çizim **ölçülmedi**; ana ekranda
kazanç beklenmemesi, aynı ekranda Skia ile ölçülen %100 bölgeden çıkarımdır.
Uygulamak `i-slint-renderer-femtovg`'nin fork'unu ister: `GLItemRenderer`'a
`ItemRendererFeatures` ve `PartialRenderer` sarmalaması, `clear_rect` yerine
bölge scissor'u, `RendererSealed`'in `mark_dirty_region`/kaynak bırakma
kancaları, `OpenGLInterface`'e buffer age, layer önbelleğiyle etkileşim, notifier
ile dışlama. Yapılmadı: ana ekranda karşılığı beklenmiyor.

## 2. GPU-resident spatial sahne

Entegrasyon. Yalnız seçilen yöntem ölçüldü; ilk ikisi kaynak incelemesiyle elendi:

- `set_rendering_notifier`: FemtoVG'de kareyi ikiye böler (temizleme ayrı
  flush); arada başka FBO'ya geçmek ana karenin store+load'ı demektir; Skia'da
  kısmi çizimi kapatır. Kullanılmadı.
- Ana framebuffer'a çizmek: MSAA yok, metnin altına/üstüne sırası yok.
- **Seçilen:** elemanın `image`'ı kameranın saf callback'idir. Slint onu çizim
  sırasında, kendi komutları daha gönderilmemişken ister; sahne o anda, **ana
  geçişten önce** kendi FBO'suna çizilir ve `BorrowedOpenGLTextureBuilder` ile
  kopyasız gösterilir. Ek kare gecikmesi, ikinci context, ikinci döngü yok.

Geometri kalıcı VBO/IBO; instance = parametre; bütün dönüşümler vertex
shader'da. Karede CPU: birkaç uniform. Aynı kamera tekrar sorulursa GL hiç
çağrılmaz (model yenilenmesi dahil; testli). Bağlam değişirse (EGLContext
kimliği) her şey yeniden kurulur, boyut değişirse yalnız doku; ekran verilirken
hepsi silinir. GL durumu: framebuffer ve viewport saklanıp geri konur, kalanını
FemtoVG her flush'ta kurar; VAO'lar yalıtır. Küre için shader derleme ve kurulum 45–47 ms,
açılışın ilk karesinde (hareketin ilk karesinde değil).

İki Mali bulgusu:

- Slint ödünç dokuyu **düz alfa** sayar, harmanlama önçarpımlı üretir. L1 bunu
  shader'da `EXT_shader_framebuffer_fetch` ile karışarak çözer. MSAA'da aynı
  yöntem örnek başına çalışır: 4,51 Mçevrim; ayrı 1× geçiş 2,15 Mçevrim.
- 4× MSAA `EXT_multisampled_render_to_texture` ile karo içinde: 1×'ten ~+0,25
  Mçevrim; derinlik karo dışına hiç yazılmaz (`glInvalidateFramebuffer`).

### Seviye 1 — küre (ABC/CBA/BAC, 3 tur; ortalama)

A Slint vektörü · T sahne hiç yok (tavan) · G GPU-resident

| | A | T | G |
|---|---|---|---|
| tek adım: süreç Mkomut/kare | 3,63 | 2,67 | 2,75 (**−%24**, tavan −%26) |
| sistem Mkomut/s | 666 | 548 | 555 (−%17) |
| GPU Mçevrim/kare · ms · meşgul % | 1,06 · 1,69 · 19,9 | 0,92 · 1,64 · 19,4 | 1,11 (**+%5**) · 1,73 · 20,3 |
| kaçan vblank (3 tur, ≈1993'te) | 8 9 8 | 1 8 0 | 7 14 5 |
| giriş→commit p50 ms | 7,7 9,5 7,4 | 6,4 8,6 5,1 | 8,1 8,5 8,3 |
| giriş→flip p50 ms | 10,9 14,2 11,3 | 10,7 12,0 9,0 | 11,1 13,0 11,8 |
| küre sürekli (80 ms): Mkomut · Mçevrim | 4,28 · 0,99 | 3,67 · 0,93 | 3,78 (−%12) · 1,03 |
| ters yön (150 ms): Mkomut · Mçevrim | 4,85 · 1,02 | 3,78 · 0,96 | 3,81 (−%21) · 1,08 |
| DDR yükü % · dış okuma/yazma MB/kare | 7,6 · 1,6/4,4 | 7,3 · 1,2/4,0 | 7,8 · 1,6/4,5 |
| GPU belleği · RSS MB | 28 · 110 | 28 · 112 | 36 · 122 |
| boşta kare (20 s) | 2 2 2 | 2 1 2 | 2 2 1 |

CPU'da sahne neredeyse bedava (tavanın %3 içinde); GPU'da **kazanç yok, %5
fazla**: bugünkü küre birkaç ince çizgidir ve ayrı bir geçişin bedeli çizgilerin
kendisinden büyüktür. %50 hedefi bu sahnede tutmaz; kazanç ölçeklenmededir.

Görsel eşdeğerlik (host, ürünün renderer'ı, aynı an; 1080p/1440p/4K/768p,
dinlenme + 5 hareket anı): PSNR 43,8–70 dB, >16 düzey fark ≤ %0,19 — FemtoVG'nin
eğri parçalaması ile GPU kenarı arasındaki fark. Slint 1.17.1'in stroke kayması
(FemtoVG `draw_path` mantıksal ofseti fiziksel uygular) bilerek taklit edilir.
Cihaz karesi göz kontrolünden geçti.

### Seviye 2 ve 3 — sahne sürekli hareketli, 144 Hz, 1440p

`advanced`: perspektif kamera, derinlik, küre (shader ızgara), 5 iç içe halka, 24
uydu, 2 cam kabuk, 4× MSAA. `stress`: +200 uydu, 5000 parçacık. Aşağısı
`advanced` üstünde tek değişken. Nokta başına 3 s; taban ve seviyeler iki kez (ortalama).
"CPU µs" sahnenin karedeki bütün CPU süresi; Mkomut bütün süreç.

| | fps | kaçan % | GPU Mçevrim | fragment / vertex M | CPU µs | süreç Mkomut | dış GB/s | draw · geçiş | nesne · instance | vertex · üçgen | alan Mpx |
|---|---|---|---|---|---|---|---|---|---|---|---|
| sahne yok (taban) | 144 | 0 | 0,54 | 0,44/0,03 | – | 1,87 | 0,14 | – | – | – | – |
| **L1 `globe`** | 144 | 0 | 0,89 | 0,74/0,08 | 60–79 | 1,98 | 0,27 | 1·1 | 33·33 | 8,5 b·8,4 b | 0,62 |
| **L2 `advanced`** | 144 | 0 | 2,11 | 1,65/0,35 | 179–196 | 2,10 | 1,35 | 5·2 | 32·32 | 253 b·84 b | 1,23 |
| **L3 `stress`** | 144 | 0 | 2,89 | 2,03/0,76 | 111–243 | 2,08 | 2,6 | 6·2 | 208·5208 | 578 b·196 b | 1,23 |
| nesne 1000 | 144 | 0 | 5,31 | 3,0/2,2 | 200 | 2,07 | 5,2 | 5·2 | 1008 | 1,9 M·647 b | 1,23 |
| nesne 3000 | 83,7 | 41,7 | 11,5 | 5,5/5,9 | 209 | 2,07 | 7,1 | 5·2 | 3008 | 5,4 M·1,8 M | 1,23 |
| nesne 6000 | 48,7 | 66,1 | 20,7 | 9,1/11,5 | 170 | 2,15 | 7,0 | 5·2 | 6008 | 10,6 M·3,5 M | 1,23 |
| nesne 10000 | 30,7 | 78,5 | 33,5 | 14,9/19,2 | 340 | 2,30 | 7,2 | 5·2 | 10008 | 17,5 M·5,8 M | 1,23 |
| parçacık 20 b | 144 | 0 | 2,87 | 2,1/0,7 | 210 | 2,08 | 2,9 | 6·2 | 32·20 b | 333 b·124 b | 1,23 |
| parçacık 100 b | 144 | 0 | 5,69 | 3,5/2,1 | 157 | 2,10 | 6,4 | 6·2 | 32·100 b | 653 b·284 b | 1,23 |
| parçacık 100 b, boy 0,3 | 144 | 0 | 5,30 | 3,1/2,1 | 150 | 2,10 | 5,8 | 6·2 | 32·100 b | 653 b·284 b | 1,23 |
| parçacık 200 b | 103 | 28,3 | 9,18 | 5,3/3,8 | 222 | 2,12 | 7,4 | 6·2 | 32·200 b | 1,1 M·484 b | 1,23 |
| doldurma: MSAA 1× | 144 | 0 | 1,96 | 1,5/0,34 | 168 | 2,05 | 1,4 | 5·2 | 32 | 253 b·84 b | 1,23 |
| doldurma: tam ekran | 144 | 0 | 3,25 | 2,8/0,35 | 145 | 2,11 | 1,8 | 5·2 | 32 | 253 b·84 b | **3,69** |
| tam ekran, 8 cam kat | 141,7 | 1,6 | 4,43 | 3,9/0,44 | 123 | 2,08 | 2,1 | 5·2 | 38 | 327 b·109 b | 3,69 |
| tam ekran, 32 cam kat | 65,3 | 54,7 | 15,3 | 14,3/0,9 | 216 | 2,08 | 2,2 | 5·2 | 62 | 622 b·207 b | 3,69 |

("b" = bin. Sunum p95/p99 144 fps satırlarında ≤ 7,04 ms. Arayüz çizim süresi,
CPU, beklemeler hariç: p50 0,4–1,0, p95 ≤ 2,2, p99 ≤ 3,0 ms. Karede doku
yüklemesi 1 KB, hepsi arayüzün; sahneden sıfır.)

Frekanslar tavana çıkmadan okunan ms yanıltır: kıyas çevrimdedir; 1000 MHz'te
6,94 ms bütçe = 6,94 Mçevrim.

**Sınır.** 144 Hz'te ~650 bin üçgen/kare (1000 nesne) veya 100 bin parçacık
%71–76 GPU meşguliyetiyle kaçırmadan geçer; 1,8 M üçgende 84, 200 bin parçacıkta
103 fps'e düşer. Tam ekranda 2 cam kat rahat, 8 kat sınırda, 32 kat 65 fps.
Geometri darboğazı vertex/tiler ve dış bellek (7 GB/s'te doyar), doldurma
darboğazı fragmenttir. **CPU darboğaz olmaz**: 32 → 6000 nesnede süreç komutu 2,10 → 2,15 M/kare;
sahnenin CPU payı bütçenin %2–5'i. Bütün `stress` noktalarında 1000 MHz'e çıkılır;
kalıcı frekans artışı yapılmadı. Bellek: L2 49, L3 52 MB GPU belleği; ağır
noktalardan (10 bin nesne, tam ekran) sonra sürücü havuzu geri vermez (175–230 MB
kalır): üretimde sahne karmaşıklığı tavanla sınırlanmalıdır. Sıcaklık 49 → 62 °C.

## 3. RK3588 kabiliyetleri

- EGL 1.5: `KHR_partial_update` (EXT_buffer_age **listelenmez**, sorgu
  çalışır), `EXT_image_dma_buf_import_modifiers`, `ANDROID_native_fence_sync`,
  `IMG_context_priority`. GLES: `EXT_multisampled_render_to_texture(2)`,
  `EXT_shader_framebuffer_fetch`, pixel local storage, `EXT_disjoint_timer_query`,
  `EXT_buffer_storage`, geometry/tessellation, ASTC. Tam liste
  `tools/mali-afbc-probe.c` çıktısında.
- Donanım sayaçları **erişilebilir** (kinstr_prfcnt, libGPUCounters;
  `tools/spatial-bench/mali-counters.cpp`): fragment/tiler çevrimi, karo, quad,
  dış bellek baytı.
- VOP2: VP0 `PLANE_MASK` = Cluster0 (arayüz) + Esmart0 (film); ek katman yok.
  Cluster0 `IN_FORMATS` AR24 için 10 AFBC modifier'ı kabul eder; Esmart0 yalnız
  LINEAR.

**AFBC neden olmuyor.** (a) Ekran denetleyicisi kabul ediyor: dumb tampon +
AFBC modifier → `ADDFB2` OK. (b) `eglQueryDmaBufModifiersEXT(AR24)` yalnız
LINEAR döner. (c) GBM her AFBC modifier'ını (bo, bo+RENDERING, bo+SCANOUT,
surface) reddeder ve **errno yazmaz** (0 kalır): önceki `ENOENT` bayat
errno'ydu, bir dosya aranmıyor. (d) GBM atlanıp tampon doğrudan verilince de
`eglCreateImage` `EGL_BAD_MATCH` (0x3009); aynı yol LINEAR ile çiziyor. Yani
engel kernel/VOP2 değil, libmali g24p0'ın dışa açtığı modifier kümesi. İç
FBO dokuları için bir şey söylemez.

## 4. Önerilen mimari

- 2B arayüz, metin, düzen, kamera, zamanlama: Slint + FemtoVG (bugünkü yol).
- Çok öğeli/3B sahne: `src/spatial`, sahne başına tek doku, sıkı dikdörtgen,
  karo içi 4× MSAA, instancing; sahne = f(kamera), boşta sıfır çizim.
- Az çizgili bugünkü sahneleri taşımak için GPU gerekçesi yok; CPU gerekçesi
  var (sahne başına ~1 Mkomut/kare).
- Film/HDR/Kodi plane'leri olduğu gibi. Skia, AFBC, ek katman: yok.

## Sınırlar

- Ölçülmeyenler: `home-settings`; Skia'nın FemtoVG ile kıyaslanabilir ilk kare
  süresi; geometri taramasının `detail=4`, doldurmanın MSAA 8× noktası. Skia
  sonuçları 1–2 tur; kapasite sınırları yaklaşıktır.
- Üretime geçmeden: sahne karmaşıklığı tavanı, iki eşzamanlı sahne için doku
  havuzu, uzun süreli ısı testi, HDR/Kodi/CEC/Canlı TV regresyonu.
- Yalnız Ultra ve 1440p144; 4K60, enerji, input-to-photon ölçülmedi.
- Kapasite süpürmesi tek turdur (seviyeler iki kez), nokta başına 3 s; uzun
  süreli ısıl davranış sınanmadı. Arada gerçek kumanda girişi gelen bir
  süpürme geçersiz sayılıp atıldı.
- Skia + spatial birlikte desteklenmez (Skia'nın GL durumu sıfırlanmıyor).
- Sahne tek dokudur: aynı anda iki GPU sahnesi (kamera iki uygulama arasındayken)
  için elemanların doku havuzu gerekir.
- İşlevsel (Ultra): `spatial` binary'siyle `mediabox-playback-smoke` 13/13 PASS;
  küre ekrandayken 1440p144 → 1080p60 → geri, sahne iki modda doğru, hata yok.
  HDR film, Kodi, CEC, Canlı TV deney binary'siyle **sınanmadı**. Cihaz ürün
  binary'sine döndürüldü (`abde3ea5…`, drop-in yok, NRestarts=0).
- Host testi (NVIDIA) framebuffer fetch'siz yolu çizer; fetch'li Mali yolunun
  eşdeğerliği cihaz yakalamasında gözle doğrulandı, PSNR'ı ölçülmedi.
- Skia ilk açılış süresi FemtoVG ile aynı noktadan bildirilmediği için
  kıyaslanmadı (`first_launcher_ms` FemtoVG 559–680, Skia 409–467).

## Yeniden üretmek

```
scripts/build-mediabox-tv.sh   # ürün; deney: --features skia,spatial,gl-count
tools/spatial-bench/build-tools.sh <libGPUCounters>
variant.sh idle.py <binary> MEDIABOX_TV_SPATIAL=globe|advanced|stress
variant.sh idle.py <binary> MEDIABOX_TV_RENDERER=skia|skia-partial
bench.py <çıktı> ; sweep.py <çıktı> <s> "<sahne> run ..."
```

Ham veri (git'te değil): cihazda `/root/mediabox-verify/advgfx`, geliştirmede
`/var/tmp/rk3588-mediabox/logs/advgfx`. PR #6'nın sonuçları
`experiment/quietos-hardware-animation` dalında.
