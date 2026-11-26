# QuietOS grafik altyapısı: renderer, GPU-resident sahne, kapasite

İki cihazda ölçüldü; sonuçlar eşdeğer sayılmaz:

| | Orange Pi 5 Plus (bölüm 5–7) | Orange Pi 5 Ultra (bölüm 1–3) |
|---|---|---|
| Panel | Sony BRAVIA, **3840x2160 @ 60 Hz** (VOP: 594 MHz, RGB 8 bit) | 2560x1440 @ 144 Hz; 4K modu yok |
| Kernel · Mali | 6.1.115-vendor · Mali-G610, g24p0-00eac0, GLES 3.2 | 6.1.172-vendor · aynı |
| Politika | CPU ondemand, GPU simple_ondemand 300–1000, DDR dmc_ondemand 528–2112 MHz | CPU ondemand, GPU aynı, DDR 2400 |
| Kod | üretim altyapısı (`src/spatial`, bu raporun 4. bölümü) | deney sürümü (`f3a3810`) |

Üretim altyapısı Ultra'da **çalıştırılmadı**.

## Karar

1. **Renderer FemtoVG kalır.** Skia GPU 3,3× CPU komutu, 3,7× GPU çevrimi ister
   ve karelerin ~%20'sini kaçırır. Kodu ve bağımlılıkları çıkarıldı (`f3a3810`'da
   durur); ölçümleri bölüm 1'dedir.
2. **Karmaşık sahneler GPU-resident olur** (`src/spatial`). Üründe ayarların
   gövdeli amblemlerini çizer (bölüm 9); Ana Menü sahneleri Slint'indir.
   `MEDIABOX_TV_SPATIAL=globe` Tarayıcı küresini de GPU'dan çizer, `off` hiçbirini;
   ikisi de karşılaştırma içindir. Bölüm 1–8 amblemlerden öncedir.
3. **AFBC taraması bu libmali ile mümkün değil.** RGA, NPU, VPU: kapsam dışı.

Ürün dışı: `spatial-lab` (yük sahneleri, kontrol dosyası, hata enjeksiyonu) ve
`gl-count` cargo özellikleri.

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
en olası neden zemini her kare değiştiren 900 ms ışık geçişi (bölge ölçüldü, neden
koddan çıkarıldı); bu ekranda kazanç sıfır. Süre kısaltılmadı.

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
32,65 MB (kurulu 32,59), yalnız spatial 32,72, skia+spatial+sayaç 37,31 (+4,7); yeni bağımlılık `skia-safe/skia-bindings` (önceden derlenmiş C++
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

Bu bölümdeki ölçümler deney sürümüyledir; bugünkü entegrasyon bölüm 4'tedir.
Yöntem aynıdır: elemanın `image`'ı kameranın saf callback'idir, sahne ana geçişten
önce kendi FBO'suna çizilir ve `BorrowedOpenGLTextureBuilder` ile kopyasız
gösterilir. Elenenler: rendering notifier içinde çizmek (kareyi böler) ve ana
framebuffer'a çizmek (MSAA ve katman sırası yok).

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
uydu, 2 cam kabuk, 4× MSAA. `stress`: 24 yerine 200 uydu, 5000 parçacık. Aşağısı
`advanced` üstünde tek değişken. Nokta başına 3 s; taban ve seviyeler iki kez (ortalama).
"CPU µs" sahnenin karedeki CPU süresidir ve CPU saatiyle oynar (kıyas için Mkomut); Mkomut bütün süreç.

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
103 fps'e düşer. Tam ekranda 2 cam kat rahat, 8 katta %1,6 kayıp (GPU %61, saat 960 MHz: tek ölçüm), 32 kat 65 fps.
Geometri darboğazı vertex/tiler ve dış bellek (7 GB/s'te doyar), doldurma
darboğazı fragmenttir. **CPU darboğaz olmaz**: 32 → 6000 nesnede süreç komutu 2,10 → 2,15 M/kare;
sahnenin CPU payı kare bütçesinin %1–5'i. Bütün `stress` noktalarında 1000 MHz'e çıkılır;
kalıcı frekans artışı yapılmadı. Bellek: L2 49, L3 52 MB GPU belleği; ağır
noktalardan (10 bin nesne, tam ekran) sonra sürücü havuzu geri vermez (175–230 MB
kalır): üretimde sahne karmaşıklığı tavanla sınırlanmalıdır. Sıcaklık 49 → 62 °C.

## 3. RK3588 kabiliyetleri

- EGL 1.5: `KHR_partial_update` (EXT_buffer_age **listelenmez**, sorgu
  çalışır), `EXT_image_dma_buf_import_modifiers`, `ANDROID_native_fence_sync`,
  `IMG_context_priority`. GLES: `EXT_multisampled_render_to_texture(2)`,
  `EXT_shader_framebuffer_fetch`, pixel local storage, `EXT_disjoint_timer_query`,
  `EXT_buffer_storage`, geometry/tessellation, ASTC. Tam liste
  `results/quietos-advanced-graphics/mali-probe.txt`.
- Donanım sayaçları **erişilebilir** (kinstr_prfcnt, libGPUCounters;
  `tools/spatial-bench/mali-counters.cpp`): fragment/tiler çevrimi, karo, quad,
  dış bellek baytı.
- VOP2: VP0 `PLANE_MASK` = Cluster0 (arayüz) + Esmart0 (film); ek katman yok.
  Cluster0 `IN_FORMATS` AR24 için 10 AFBC modifier'ı kabul eder; Esmart0 yalnız
  LINEAR.

**AFBC neden olmuyor.** (a) Ekran denetleyicisi kabul ediyor: dumb tampon +
AFBC modifier → `ADDFB2` OK. (b) `eglQueryDmaBufModifiersEXT(AR24)` yalnız
LINEAR döner. (c) GBM her AFBC modifier'ını (bo, bo+RENDERING, bo+SCANOUT,
surface) reddeder ve **errno yazmaz** (0 kalır): önceki `ENOENT`in bayat
errno olduğu buradan çıkarımdır; o denemenin kodu yeniden koşulmadı. (d) GBM atlanıp tampon doğrudan verilince de
`eglCreateImage` `EGL_BAD_MATCH` (0x3009); aynı yol LINEAR ile çiziyor. Yani
engel kernel/VOP2 değil, libmali g24p0'ın dışa açtığı modifier kümesi. İç
FBO dokuları için bir şey söylemez.

## 4. Üretim altyapısı (`src/spatial`)

```
Slint elemanı (SpatialScene)  ── name, kind, cam ──►  saf callback
   │                                                    │ kare içinde ve yalnız gerekirse
   │  Spatial.kind == 0 ise Slint'in kendi sahnesi      ▼
   ▼                                          havuz: ad başına bir doku (Slot)
FemtoVG karesi ◄── ödünç doku ──              ortak: shader + geometri (Scene), MSAA/derinlik hedefi (Scratch)
```

| Konu | Ne yapılıyor |
|---|---|
| Çoklu sahne | Eleman adı başına doku; bir doku karede en çok bir kez çizilir. Shader, geometri ve MSAA/derinlik hedefi bağlam başına tektir |
| Ne zaman çizilir | Kamera, yer veya renk değiştiyse. Boşta 0 çizim (testli, cihazda 20 s'de 1–2 arayüz karesi, A ile aynı) |
| Kim hâlâ gösteriyor | Slint eleman yok olunca haber vermez; her çizilen kare yoklama yapar (`Spatial.tick`). Yoklamaya yanıt vermeyen doku 1,5 s dinlenmede (en geç 600 karede) bırakılır; bunun için kare istenmez |
| Ödünç doku ömrü | Değişen doku, gösterildiği karenin komutları gönderildikten sonra silinir; ona bağlı eleman o karede yenisini almıştır. Bir karede sorulmamış doku başka ada verilebilir |
| GL durumu | FBO (draw/read), viewport, program, VAO, tamponlar, doku birimi, harmanlama, derinlik, yüz, maske, temizleme değerleri saklanır; **her çıkışta** geri konur (`Kept`, Drop) |
| FemtoVG sırasına bağımlılık | Yalnız maliyettir, doğruluk değildir: görüntü callback dönmeden çizilir, durum tam geri konur. Sıra testle kilitli (callback anında FemtoVG kareye başlamamış olmalı) |
| Kare dışı çağrı | GL yok; var olan görüntü döner, sonraki kare yeniden sorar |
| Bağlam | Kimlik = EGL adresi + platformun bağlam sayacı (adres yeniden kullanılabilir). Başka bağlamda her şey yeniden kurulur; eskinin adları silinmez, unutulur (`Owned`, RAII). Kısmi kurulum hataları da kaynak sızdırmaz |
| Geri dönüş | Shader, tampon, doku, FBO, çizim, bağlam veya bütçe hatasında o kare **sunulmaz**; eleman Slint sahnesine döner ve kare hemen yeniden çizilir. Shader hatası bağlam boyunca, diğerleri panel modu değişene dek kalıcıdır; tekrar tekrar denenmez |
| Sınır | İkinci context, döngü, compositor, piksel kopyası, readback, `glFinish` yok. `cache-rendering-hint` altına sahne konmaz |

Yeni sahne: `Scene` trait'i + `Kind` + bir `SpatialScene`.

### Giderilen gerçek hatalar

| Hata | Nerede görüldü |
|---|---|
| Tek `Stage`/doku: kamera iki uygulama arasındayken ikinci sahne birincinin içeriğini ezerdi | kod; çoklu sahne testi |
| Doku büyürken (`drop_target`) Slint'in o karede kuyruğa aldığı çizim silinmiş dokuyu gösterebilirdi | kod |
| Hata sonrası `Spatial.on` açık, görüntü boş: sahne kaybolurdu | kod; hata enjeksiyonu |
| `program`/`Mesh`/`Target` hata yollarında shader, tampon, FBO sızıntısı | kod |
| Bağlam kimliği yalnız EGL adresi: aynı adreste yeni bağlamda eski adlar geçerli sanılırdı | kod |
| Kare dışı ilk çağrının boş görüntüsü binding'de kalırdı | kod |
| Bağlam değişince dinlenmedeki sahne yeniden istenmezdi (boş kalırdı) | yeni altyapının kendi testi |
| Doku hatasından sonra panel modu değişse de sahne GPU'ya dönmezdi | yeni altyapının kendi testi |
| Rendering notifier FemtoVG'nin temizlemesini ayrı flush yapıyordu; sahne geçişi ana kareyi bölüyordu. Kare sayacı artık platformda (`platform::on_drawn`) | kaynak (i-slint-renderer-femtovg 1.17.1) |

### Bütçe

Ölçülen en çok eşzamanlı sahne **4**'tür (24 uygulama, 16–220 ms tuş tekrarı, ters
yön, sıçrama; `QuietReach`, host). Ürün bütçesi (`budget.rs`):

| Kaynak | Sınır | Gerekçe |
|---|---|---|
| Eşzamanlı sahne | 6 | ölçülen 4 + pay |
| Doku + derinlik + geometri | 192 MiB | 4K'da sahne dokusu 11,8 MiB, ortak hedef 23,6; dört MSAA'lı sahne 71 MiB; iki tam ekran sahne ~127 MiB |
| GPU süresi | karenin %55'i (1 GHz'te 60 Hz: 9,2, 144 Hz: 3,8 Mçevrim) | 4K60'ta arayüzle birlikte 10,2 Mçevrim (%59 meşgul) sistematik kayıpsız ölçüldü |

GPU süresi sahne maliyetinden hesaplanır: `0,45·piksel + 0,05·piksel·katman +
6·üçgen + 40·instance` çevrim. Plus ve Ultra'da ölçülen dokuz yükün −%10…+%34'ü içinde (test). Çözünürlük
ve yenileme hızıyla kendiliğinden ölçeklenir; sabit nesne sayısı yoktur. Aşan eleman
bütünüyle Slint'e döner.

Önceki rapordaki 175–230 MB: **sızıntı değil, sürücü havuzu.** Kendi sahipliğimiz
(`held`) sahne kalkınca geometriye (≤ 0,8 MB) iner; GPU belleği ilk süpürmenin sonunda 109,6, ikinci
aynı süpürmenin sonunda 109,7 MB'dır. Tepe, eski hedefler serbest kalmadan
yenileri ayrıldığında görülür (187–209 MB) ve geri iner.

## 5. Plus, native 4K60

Sıra A P G | G P A | P A G (3 tur, süreç yeniden başlatılarak). A: PR #5 üretim
referansı (`main` a816d14, `abde3ea5`) · P: bu dal, varsayılan · G: aynı ikili, küre
GPU'da. `h1` derlemesi; sonrasında yalnız bütçe modeli değişti (`h2`).

| Senaryo | | Mkomut/kare | GPU Mçevrim (hw) | GPU ms · MHz · meşgul % | kaçan vblank | giriş→commit p50 | giriş→flip p50 · p95 | dış oku/yaz MB | GPU MB · RSS MB |
|---|---|---|---|---|---|---|---|---|---|
| browser | A | 3,02 | 2,17 (2,12) | 4,04 · 536 · 20,4 | 0 1 0 / 815 | 10,7 10,2 7,6 | 23,4 16,9 20,6 · 24,9 32,5 21,8 | 2,6/10,6 | 48 · 132 |
|  | P | 2,98 | 2,14 (2,14) | 3,95 · 541 · 19,9 | 0 0 0 / 816 | 7,9 10,5 10,1 | 21,3 20,7 20,4 · 22,4 22,0 21,6 | 2,6/10,7 | 48 · 131 |
|  | G | 2,05 | 2,24 (2,23) | 4,00 · 562 · 20,3 | 1 0 0 / 824 | 9,6 9,8 10,2 | 13,4 12,4 24,1 · 29,7 29,6 25,7 | 2,7/10,7 | 61 · 147 |
| repeat | A | 5,09 | 2,14 (2,49) | 3,75 · 570 · 23,4 | 1 0 0 / 205 | 8,5 8,9 7,8 | 24,3 25,0 24,1 · 31,3 32,2 31,5 | 3,6/20,9 | 50 · 134 |
|  | P | 5,02 | 1,90 (2,47) | 3,24 · 588 · 20,2 | 0 4 0 / 206 | 9,3 8,5 7,6 | 25,7 24,6 24,0 · 32,6 32,1 31,7 | 3,6/20,8 | 50 · 133 |
|  | G | 4,53 | 2,02 (2,61) | 3,30 · 617 · 20,3 | 0 1 1 / 202 | 8,3 8,0 9,3 | 24,4 24,6 24,6 · 32,5 32,5 32,7 | 3,8/21,3 | 63 · 149 |
| reverse | A | 5,68 | 2,26 (2,50) | 3,84 · 589 · 23,7 | 0 0 0 / 242 | 1,6 9,3 6,3 | 18,1 25,8 22,8 · 19,3 27,0 24,5 | 3,8/22,2 | 50 · 134 |
|  | P | 5,64 | 2,11 (2,51) | 3,68 · 573 · 22,6 | 0 0 0 / 242 | 1,1 15,6 10,0 | 17,7 32,0 26,5 · 33,1 32,9 27,4 | 3,9/22,2 | 50 · 133 |
|  | G | 5,56 | 2,13 (2,50) | 3,65 · 584 · 22,7 | 0 0 0 / 244 | 12,6 11,9 11,2 | 28,9 28,2 27,6 · 30,5 29,5 28,5 | 3,8/22,0 | 63 · 149 |
| globe-repeat | A | 4,55 | 1,93 (2,47) | 3,30 · 585 · 20,3 | 0 0 0 / 201 | 9,0 8,8 7,8 | 24,9 25,0 24,2 · 32,2 32,4 31,1 | 3,3/17,6 | 50 · 134 |
|  | P | 4,42 | 2,15 (2,43) | 3,74 · 574 · 23,2 | 0 0 0 / 203 | 8,2 8,2 8,9 | 24,6 24,2 25,4 · 32,2 31,9 32,2 | 3,3/17,5 | 50 · 133 |
|  | G | 3,42 | 2,28 (2,61) | 3,86 · 591 · 23,7 | 1 0 0 / 201 | 8,4 8,9 8,2 | 24,7 25,2 24,4 · 32,6 32,2 31,6 | 3,6/17,8 | 63 · 149 |
| home-settings | A | 3,57 | 1,74 (1,74) | 4,06 · 431 · 15,1 | 0 0 0 / 359 | 22,3 24,0 21,7 | 33,9 34,8 33,0 · 83,5 52,1 65,9 | 2,5/10,5 | 55 · 139 |
|  | P | 3,52 | 1,75 (1,76) | 4,12 · 426 · 15,3 | 0 0 0 / 360 | 22,6 28,5 24,6 | 36,5 37,5 35,7 · 85,7 86,6 36,8 | 2,5/10,5 | 54 · 139 |
|  | G | 3,52 | 1,74 (1,74) | 3,93 · 444 · 14,8 | 0 0 0 / 361 | 18,9 15,3 19,6 | 25,4 24,8 35,6 · 42,4 40,4 37,2 | 2,5/10,5 | 67 · 155 |
| category | A | 2,09 | 1,33 (1,29) | 4,30 · 309 · 18,9 | 0 0 0 / 610 | 14,6 13,2 12,1 | 26,8 18,3 27,8 · 61,3 35,6 30,3 | 1,9/1,4 | 60 · 146 |
|  | P | 2,01 | 1,31 (1,29) | 4,20 · 312 · 18,7 | 0 0 0 / 609 | 14,0 13,4 13,6 | 22,8 24,4 23,5 · 40,7 26,5 42,1 | 1,8/1,4 | 60 · 145 |
|  | G | 2,02 | 1,33 (1,31) | 4,13 · 323 · 18,1 | 0 0 0 / 604 | 13,7 14,2 14,1 | 28,3 27,8 16,7 · 44,6 45,7 49,2 | 1,8/1,4 | 73 · 161 |

Sunum p50/p95 her satırda 16,67 / ≤16,73 ms; p99 bir tur dışında ≤16,76 ms (P `repeat`
2. tur: 207'de 4 kaçan, p99 33,3). Bunun için `repeat` ayrıca `h2` ile koşuldu (A P P A A P P A, varyant başına
16 tur): kaçan P **0/3232**, A 3/3235; varsayılan üründe regresyon yok. Giriş gecikmesi tuşun vblank içindeki yerine
bağlı; varyantlar arası fark tur saçılması içindedir.
Boşta (20 s): A 2 2 2, P 2 2 2, G 2 2 1 arayüz karesi; CPU %0,5–0,65 hepsinde.

- **P ≈ A**: varsayılan ürün maliyeti değişmedi (Mkomut −%1…−%4, GPU ±%1 hw). Notifier'ın kalkması
  bunun içindedir; ayrı ölçülmedi.
- **G**: küre hareketindeki senaryoda CPU **−%32** (browser), −%25 (globe-repeat); GPU hw **+%5**, +%6.
- Hazırlık açılışın ilk karesinde: context 6,5 ms, küre shader'ları 38 ms (`advanced` 22 ms).

### Seviyeler ve hedefli kapasite (sahne sürekli hareketli, 5 s, iki tur)

| | fps | kaçan % | GPU Mçevrim | fragment / vertex M | GPU ms · meşgul % · MHz | sahne CPU µs | süreç Mkomut | dış GB/s | üçgen · instance | Mpx | held / GPU MB |
|---|---|---|---|---|---|---|---|---|---|---|---|
| **L2 `advanced`** | 60,0 60,0 | 0,00 0,00 | 3,62 | 3,01/0,34 | 4,25 · 26 · 828 | 199 | 1,29 | 0,83 | 84 b · 32 | 2,76 | 36 / 88–112 |
| **L3 `stress`** | 60,0 60,0 | 0,00 0,00 | 4,67 | 3,62/0,76 | 4,53 · 27 · 1000 | 188 | 1,27 | 1,67 | 196 b · 5208 | 2,76 | 36 / 90–112 |
| advanced full | 60,0 60,0 | 0,00 0,00 | 5,92 | 5,40/0,33 | 6,51 · 39 · 898 | 219 | 1,27 | 1,26 | 84 b · 32 | 8,29 | 96 / 187–209 |
| stress full | 59,6 59,8 | 0,67 0,33 | 6,83 | 5,92/0,78 | 6,98 · 42 · 945 | 227 | 1,25 | 2,37 | 196 b · 5208 | 8,29 | 96 / 147–169 |
| advanced full layers=4 | 60,0 60,2 | 0,00 0,00 | 6,41 | 5,86/0,38 | 6,34 · 38 · 982 | 179 | 1,25 | 1,24 | 93 b · 34 | 8,29 | 96 / 147–169 |
| advanced full layers=8 | 60,0 60,2 | 0,00 0,00 | 8,28 | 7,73/0,49 | 7,92 · 48 · 1000 | 145 | 1,25 | 1,58 | 109 b · 38 | 8,29 | 96 / 146–169 |
| stress objects=1000 | 59,8 60,0 | 0,33 0,00 | 7,15 | 4,82/2,22 | 6,97 · 42 · 980 | 216 | 1,28 | 3,04 | 657 b · 6008 | 2,76 | 36 / 109–194 |
| stress objects=2000 | 60,0 59,6 | 0,00 0,67 | 10,20 | 6,08/4,04 | 9,91 · 59 · 988 | 196 | 1,31 | 4,56 | 1233 b · 7008 | 2,76 | 36 / 109–109 |
| stress particles=50000 | 60,0 60,0 | 0,00 0,00 | 6,21 | 4,50/1,47 | 6,18 · 37 · 982 | 164 | 1,32 | 2,91 | 286 b · 50208 | 2,76 | 37 / 110–110 |
| advanced msaa=1 | 60,0 60,0 | 0,00 0,00 | 3,34 | 2,74/0,34 | 4,22 · 25 · 770 | 141 | 1,29 | 0,88 | 84 b · 32 | 2,76 | 36 / 110–110 |

Sunum p95/p99 hepsinde ≤ 16,74 / ≤ 16,79 ms. Kaçan %0,33–0,67 = 300 karede
1–2; nedeni ayrılmadı, bir satır dışında (tam ekran L3) tur başına tekrarlamıyor.

**Ultra ile** (aynı sahne, eşdeğer koşul değil): geometri maliyeti aynı, doldurma
pikselle büyüyor.

| | Ultra 1440p144: Mçevrim (fragment / vertex), Mpx | Plus 4K60 |
|---|---|---|
| L2 | 2,11 (1,65 / 0,35), 1,23 | 3,62 (3,01 / 0,34), 2,76 |
| L3 | 2,89 (2,03 / 0,76), 1,23 | 4,67 (3,62 / 0,76), 2,76 |
| 1000 nesne | 5,31 (3,0 / 2,2) → 144 Hz bütçesinin %76'sı | 7,15 (4,8 / 2,2) → 60 Hz bütçesinin %42'si |

### Aynı anda birden fazla sahne (bütün uygulamalar GPU'dan)

| | senaryo | Mkomut/kare | GPU Mçevrim (hw) | GPU ms · meşgul % | kaçan vblank | giriş→flip p50 · p95 | GPU MB · RSS MB |
|---|---|---|---|---|---|---|---|
| globe | repeat | 2,90 | 2,81 (3,22) | 4,02 · 25,3 | 0 / 207 | 24,2 · 31,7 | 87 · 172 |
|  | reverse | 2,95 | 2,48 (3,00) | 3,73 · 23,1 | 0 / 244 | 23,5 · 24,9 | 87 · 173 |
|  | browser | 1,65 | 2,62 (2,61) | 4,22 · 21,4 | 0 / 819 | 23,1 · 24,9 | 87 · 173 |
| advanced | repeat | 3,08 | 5,36 (5,89) | 5,63 · 34,4 | 0 / 198 | 24,5 · 31,9 | 107 · 194 |
|  | reverse | 3,06 | 4,33 (5,13) | 4,80 · 29,2 | 0 / 240 | 30,1 · 31,3 | 108 · 195 |
|  | browser | 1,72 | 4,56 (4,40) | 5,87 · 29,8 | 1 / 816 | 16,2 · 17,1 | 108 · 195 |
| stress | repeat | 3,12 | 6,40 (6,91) | 6,98 · 42,3 | 0 / 198 | 23,9 · 31,6 | 114 · 201 |
|  | reverse | 2,96 | 4,99 (5,76) | 5,59 · 35,2 | 1 / 250 | 28,1 · 29,6 | 114 · 202 |
|  | browser | 1,72 | 5,27 (5,16) | 6,70 · 33,6 | 5 / 816 | 18,4 · 19,8 | 114 · 203 |

## 6. Uzun koşu ve ısı

45 dakika, 57 tur, laboratuvar ikilisi `h2`, **ürün bütçesiyle** (`bounded`): 20 s sürekli
L3, ardından bütün uygulamalar L3 iken tuş tekrarı ve ters yön, 8 s dinlenme.

| | |
|---|---|
| Sürekli L3 (57 × 20 s) | 60,0 fps, **0 kaçan**; sunum p99 ≤ 16,77 ms; GPU 4,44 → 4,49 Mçevrim, meşgul %27,1 → 27,4 |
| Gezinme, beş uygulama da L3 | kaçan 97 / 16510 (%0,59), tur başına en çok 4; bütçe aşımı veya geri dönüş: 0 |
| GPU belleği, hareket | 89,7–91,4 MB; çeyrek ortalamaları 91,1 91,3 91,2 91,2 |
| GPU belleği, dinlenme | 106–122 MB (iki düzey); çeyrekler 113,7 113,5 111,6 115,4 |
| RSS · fd | hareket 176–179, dinlenme 193–208 MB · 46 (bir an 48) |
| Başta → sahne kaldırılınca | GPU 48,0 → 52,5 MB, RSS 132 → 140 MB |
| Sıcaklık | GPU 41–45 °C, SoC ≤ 45, en sıcak bölge (Wi-Fi) ≤ 57; soğutma aygıtı durumu hep 0 |

Birikme ve ısıl kısma görülmedi. Dinlenmedeki 106–122 MB tek sahnenin 36 MB'ı + sürücü
havuzudur; sahne kaldırılınca geri verildi. Kare bütçesi 45 dakika boyunca sabit kaldı.
Çekirdek günlüğünde GPU hatası yok (bir kez, kapasite süpürmesi sırasında, DVFS ölçüm
penceresi uyarısı).

## 7. Regresyon

### Görsel

Cihazda (Mali, fetch'li yol), 4K, dinlenme, aynı kare bölgesi:

| | PSNR | >16 düzey fark |
|---|---|---|
| Küre GPU ↔ Slint, yalnız sahne bölgesi (1540x1450) | 38,1 dB (tam kareye oranla 43,8) | %0,71 |
| Hata enjeksiyonu (12 durum) ↔ A'nın Slint sahnesi | ≥ 105 dB (en çok 1 düzey) | 0 |
| Quiet → Klasik → Quiet sonrası ↔ öncesi | 105 dB | 0 |

Host (1080p/1440p/4K/768p, dinlenme + 5 hareket anı): testler tam karede ≥ 40 dB.
Varsayılan üründe sahne Slint'indir: A ile aynı yol.

### İşlevsel (Plus, ürün ikilisi `h2` + GPU küre)

| | Beklenen | Gerçekleşen |
|---|---|---|
| SDR oynatma (kendi oynatıcı) | hevc_rkmpp, NV12 düzlem, dönüşte sahne | geçti; aynı süreç, aynı bağlam |
| HDR10 | NV15, PQ/BT.2020 tel, dönüşte RGB 8 | geçti (tel 10 bit 4:2:2 HDR10, düzlem `HDR10`) |
| `mediabox-playback-smoke` | PASS | 13/13 |
| 10 kez oynat/dön | birikme yok | GPU 56,1 MB, 46 fd sabit |
| Canlı TV (TRT 1 HD) | NV12 düzlem, dönüş | geçti; aynı süreç |
| Kodi'ye geçiş ve dönüş | mod 2160p60 korunur | geçti |
| HDMI-CEC | adres, giriş aygıtı | mantıksal 4, aktif; TV kumandasına basılmadı |
| 1080p60, 2160p30, 1080p50 ↔ 2160p60 | geri, sahne doğru, kullanıcı seçimi `Otomatik` | geçti; `keep` edilmedi |
| Quiet ↔ Klasik (8 kez) | sahne geri, kaynak birikmez | geçti; ana ekranda GPU belleği 66–77 MB (artan eğilim yok; kontrol A 56–60, fark sahnenin dokusu + havuz) |
| Azaltılmış hareket | iki gezinmede birkaç kare | 5 kare (kontrol A: 4) |
| Fare (uinput), tıklama; kumanda | imleç, hover, seçim | geçti (Ayarlar açıldı) |
| Hata enjeksiyonu, gerçek sürücü | 12/12 Slint sahnesi, süreç ayakta | geçti, NRestarts=0 |

Motion Lab açılınca GPU belleği +40 MB oluyor; kontrol olarak A'da da aynı (41,8 → 81,9),
sahneyle ilgisi yok.

### Testler

| | Beklenen | Gerçekleşen (son HEAD) |
|---|---|---|
| `cargo test`, `MEDIABOX_TV_GL=require`, varsayılan | 0 hata | **603** geçti, 2 yok sayıldı |
| aynı, `spatial-lab,gl-count` | 0 hata | 603 geçti, 2 yok sayıldı |
| Grafik regresyon testleri (tam takımın içinde) | 0 hata | 18 geçti |
| Clippy `--all-targets`, tam liste, main ile | fark yok | 38 = 38 satır, `diff` boş (laboratuvar: 38, boş) |
| `tests/run-host-tests.sh` | 0 FAIL | 299 ok, 0 FAIL |
| ARM64 release, uyarısız: ürün / laboratuvar | derlenir | derlendi (`25b8bd8d…` / `df9b2283…`) |

Önceki raporun eksiği (düz-alfa geçişinden sonra tam takım koşulmamıştı) bununla kapandı.
Yeni testler: çoklu sahne içeriği, ortak derinlik hedefi, eşzamanlılık tavanı, kaynak geri
verme, 11 hata yolunda sunulan karenin Slint sahnesi olması, bağlam yenilenmesi, iki gerçek
bağlamda ad silme, GL durumu, karenin sırası, bütçe, azaltılmış hareket.

## 8. Sınırlar ve merge

- Ultra'da yeni kod, 1440p144'te yeni kod ve enerji ölçülmedi. Bütçe modeli Ultra'nın deney
  verisinden de uyarlandı, orada doğrulanmadı.
- Ekran görüntüsü arayüzün kendi karesidir; panelin kendisi fotoğraflanmadı. VOP durumu
  (mod, düzlem, renk) çekirdekten okundu.
- `mali-probe.txt`'teki EGL/GLES uzantı listesi Plus'ta **aynı** (`plus-4k60/mali-extensions.txt`).
- Deney süresince cihazın **kurulu** ikilisi (`ce481d7d`, main'den eski) değişmedi; deney
  ikilileri geçici drop-in ile koştu, her koşu sonunda kaldırıldı.
- CEC'te TV kumandasına basılmadı; ses çıkışı dinlenmedi (smoke kart eşleşmesini okur).
- Gerçek EGL context kaybı oluşturulmadı; bağlam yenilenmesi testte, iki gerçek bağlamla ve
  kimlik değişimiyle sınandı. FemtoVG kendi bağlamını kaybederse süreç systemd ile yeniden başlar (değişmedi).
- L3 dört sahnede aynı anda (yalnız çok hızlı gezinme) GPU bütçesini aşabilir; aşan sahne o an Slint'e döner.

**Merge.** Bilinen engel yok: varsayılan üründe görünüm ve maliyet A ile aynı, yeni yol kapalı.
Karar kullanıcınındır; GPU sahnesini kalıcı açmak ayrı bir karardır. Merge edilmedi, Golden
yayınlanmadı.

## 9. Spatial Refinement: ikonlar ve ayar amblemleri

Ana Menü'de seçili uygulamanın açıklaması kalktı; "kurulu değil" durumu başlığın altında
kaldı. Başlık sahnenin ortasına hizalandı (72 tasarım pikseli aşağı).

**İkonlar** (`ui/quiet/marks.slint`, Slint). Bir hareket 0→1 tek sayıdır ve iki ucunda da
ikon tam olarak eski ikondur: dişli bir diş (45°), film bir kare, küre iki meridyen arası
kadar döner; Canlı TV'de iki dalga çıkar; Kodi'nin dört parçası sırayla toplanır; tanınmayan
uygulama hafifçe küçülüp geri gelir. Seçilen satırın ve bırakılanın ikonu önbellekli listenin
(`Still`) **üstünde** çizilir: hareket ederken liste yeniden rasterize edilmez (testli).
Dinlenmedeki beş ikon cihazda önceki kareyle piksel piksel aynıdır.

**Amblemler** (`ui/quiet/emblems.slint`, `src/spatial/emblem.rs`). Sahne, `Group::id` ve
`Page::id` ile seçilir; sıra numarası veya görünen başlıkla değil.

| Yer | Kimlik | Sahne | Çizen |
|---|---|---|---|
| Medya | `media` | karşılıklı dönen iki makara, film şeridi | GPU |
| Görüntü ve Ses | `picture` | katmanlı ekran, önünden çıkan halkalar | GPU |
| Bağlantılar · Tanılama/Ağ | `links` | düğümler, aralarında akan veri | GPU |
| TV ve Kumanda | `remote` | kumanda, televizyon, aralarındaki dalga | GPU |
| Canlı TV | `live` | uydu, yörünge, çanak, demet | GPU |
| Cihaz | `device` | kart, soğutucu, ledler | GPU |
| Sistem · Tanılama/Makine | `system` | işlemci ve devresi | GPU |
| Tanılama | `diagnostics` | örgünün üstünde büyüteç | GPU |
| Görüntü ve Ses/Ekran · Tanılama/Ekran ve GPU | `output` | kafes, filtre, ön cam; geçen ışık | GPU |
| Görüntü ve Ses/Ses | `audio` | koni halkaları, içinden geçen dalga | GPU |
| Cihaz/Soğutma | `cooling` | çerçevesinde fan | GPU |
| Tanılama/Depolama | `storage` | plakalar ve kol | GPU |
| Medya/Oynatma · Tanılama/Oynatma | `playback` | kareler ve oynat işareti | Slint |
| Medya/Hesap | `account` | kimlik, çevresinde eklentiler | Slint |
| TV ve Kumanda/HDMI-CEC · Tanılama/CEC | `cec` | kabloyla bağlı iki cihaz, geçen işaret | Slint |
| Bağlantılar/Ethernet | `ethernet` | soket, kontaklar, bükümlü çift | Slint |
| Bağlantılar/Wi-Fi ağ sayfası | `wifi` | derinliğe ayrılan dalgalar | Slint |
| Tanılama/Servisler | `services` | birbirine oturan modüller | Slint |

Wi-Fi ve Bluetooth listeleri ile Tanılama/Makine kendi enstrümanını çizer (`emblem: false`
değişmedi). Akan ışık ve dalga yalnız geçiş sırasında vardır, ölçüm değildir.

- **Geçiş**: sahneler bir deste üstündedir (`QuietDeck`): eskisi geldiği yöne çıkar, yenisi
  ters yönden katmanlarıyla toplanır. Kuyruk yok: yarı yolda dönülürse bırakılan sahneye
  döner; sahne daha yeni giriyorsa yerini yenisi alır. Ayarlara uçuşta dişli mekanizma
  durur, yerine varırken sahneye dönüşür; dönüşte tersi.
- **GPU**: 12 model, iki ortak program (gövde, çizgi), instancing; geometri 0,84 MB.
  Bütün dokular tek boydur (4K'da 880²). Her modelin Slint sahnesi geri dönüş olarak durur.
- Kesilme testi: her model yolunun tamamında kendi yerinin içinde (testte üç model kesiliyordu, düzeltildi).

### Ölçüm — Plus, 3840x2160p60

A: kurulu main (`25b8bd8d`), R: bu dal. Ana Menü ve `category-repeat` 2'şer, diğerleri A 1 · R 2 tur.

| Senaryo | Mkomut/kare A → R | GPU Mçevrim (hw) A → R | GPU ms/kare A → R | kaçan vblank A · R | GPU MB A → R |
|---|---|---|---|---|---|
| film-live | 3,84 → 3,89 | 2,11 → 2,11 | 3,98 → 3,87 | 9/1629 · 9/1641 | 50 → 50 |
| repeat | 5,18 → 5,10 | 2,54 → 2,62 | 3,52 → 3,30 | 3/401 · 0/398 | 50 → 50 |
| reverse | 5,62 → 5,52 | 2,50 → 2,48 | 3,67 → 3,40 | 2/485 · 1/489 | 50 → 50 |
| home-settings | 3,56 → 3,26 | 1,73 → 1,84 | 4,14 → 4,02 | 0/363 · 0/727 | 54 → 68 |
| category | 1,98 → 2,21 | 1,30 → 1,71 | 4,18 → 3,93 | 0/601 · 0/1215 | 60 → 85 |
| category-repeat | 2,69 → 3,19 | 1,40 → 1,73 | 3,14 → 3,87 | 0/348 · 0/330 | 62 → 91 |
| category-reverse | 2,27 → 2,62 | 1,36 → 1,70 | 3,82 → 3,63 | 0/240 · 0/480 | 60 → 75 |
| page (Ekran editörü aç/kapa) | 6,90 → 5,38 | 1,76 → 1,95 | 4,18 → 4,07 | 4/484 · 1/966 | 61 → 77 |

| Boşta (20 s) | arayüz karesi A · R | CPU % | Mkomut/s | GPU MB |
|---|---|---|---|---|
| Ana Menü | 2 · 2 2 | 0,6 · 0,6 0,5 | 5,3 · 5,2 | 61 · 84 79 |
| Ayarlar | 2 · 2 2 | 0,6 · 0,55 0,6 | 5,3 · 5,3 | 61 · 77 77 |

- Ana Menü: fark yok. Kategori değişimi: kare başına +%12–19 CPU komutu ve +0,3–0,4 Mçevrim
  (1 GHz'te 60 Hz karesinin %2,5'i); kaçan yok. Kategori başına çizilen kare de arttı: uzak
  katman 760 ms sürer (önceki amblem 560 ms).
- Bellek: +16–31 MB, tek sahnenin dokusu ve ortak hedef (üçü ≤ 10 MB) ile sürücü havuzu. 12 tur
  Ayarlar giriş-çıkışında ana ekranda 55–95 MB arasında salınır, artmaz; fd 46 sabit. İlk
  derlemede (`r2`) doku uçuşla büyüyor ve havuz 120–138 MB'a çıkıyordu; sahne boyu sabitlendi.
- Hazırlık: shader ve geometri 49–61 ms, bağlam 7–32 ms. Yalnız QuietOS için yapılır: Klasik
  açılışta hiçbir şey hazırlanmaz (cihazda journal'da `tv.spatial` satırı yok, GPU 19,7 MB).
  QuietOS'a geçişte Klasik'in son karesinde hazırlanır; yeni yüz ondan sonra konur, yani
  hiçbir QuietOS animasyonu hazırlık sırasında başlamaz (testli; cihazda geçişte 35 flip'te 1
  kaçan vblank). Sonraki geçişlerde hazırlık yok.
- Bütçe aşımı veya Slint'e geri dönüş: 0 (journal, bütün koşular).

Kategori geçişlerinde **tuş başına** toplam CPU komutu (süreç; A 1, R 2 tur, `r3`). Tuş başına
kare de arttı: uzak katman 760 ms sürer.

| Senaryo | Mkomut/tuş A → R | kare/tuş A → R | GPU Mçevrim/tuş A → R |
|---|---|---|---|
| category (800 ms arayla) | 71,5 → 105,9 (+%48) | 36,1 → 47,9 | 47,8 → 82,3 |
| category-repeat (80 ms) | 14,4 → 16,4 (+%14) | 5,3 → 5,1 | 6,4 → 8,3 |
| category-reverse (150 ms) | 21,9 → 25,1 (+%15) | 9,6 → 9,6 | 12,2 → 14,6 |
| page (Ekran aç/kapa) | 248,1 → 253,3 (+%2) | 35,9 → 47,2 | 63,0 → 94,3 |
| home-settings | 161,5 → 195,0 (+%21) | 45,4 → 59,8 | 78,5 → 111,8 |

Tek tuşla kategori değişimi bir çekirdeğin 1,8 GHz'te yaklaşık 50 ms'sine eşittir; kare
başına artış %12'dir, gerisi daha uzun süren harekettir.

**Panel modu değişince** (`what_scenes_hold_is_given_back`'in kararsızlığı): bir elemanın eski
moddan kalan dokusu, yeni modda da yetiyorsa yeniden kullanılıyordu; ortak hedef de hiç
küçülmüyordu. Sonuç zamanlamaya bağlıydı: eleman 4K'dayken bir kez çizildiyse 1080p'de 4K
dokuyla kalıyordu (37,3 MB), çizilmediyse kalmıyordu (28,1 MB; testteki iki değer bu ikisi).
main'de de vardı. Artık panelin boyu değişince eski dokular gösterilmeye devam eder ama içine
çizilmez, ilk çizimde yenisiyle değişir; ortak hedef bırakılır. Test, değeri doğrudan sınar
(tek doku ve hedef, 880², bu modun boyunda); 17 koşu, 4'ü eşzamanlı, hepsi geçti.

**Oynatma regresyonu** (Plus, kurulu `be70d2fa`, Ayarlar'da Soğutma'nın GPU amblemi
açıkken):

| | Beklenen | Gerçekleşen |
|---|---|---|
| SDR 4K | hevc_rkmpp, NV12, SDR; dönüşte RGB888, aynı süreç | geçti |
| HDR10 4K | NV15, HDR10 tel; dönüşte RGB888, aynı süreç | geçti |
| Dönüşte amblem ve gezinme | geri dönüş ve GL hatası yok | geçti; GPU 58–73 MB, fd 46 |
| Kodi'ye geçiş ve dönüş | 2160p60 korunur, hata yok | geçti |

Ham veri ve görseller `results/quietos-spatial-refinement/`. Görseller host'ta ürünün
renderer'ından alındı; panel fotoğraflanmadı.

## Yeniden üretmek

```
scripts/build-mediabox-tv.sh      # ürün; laboratuvar: --features spatial-lab,gl-count
tools/spatial-bench/build-tools.sh <libGPUCounters>
variant.sh idle.py <ikili> [MEDIABOX_TV_SPATIAL=globe]     # ikili mediabox-tv adında, /var/lib altında (servis ProtectHome)
bench.py <çıktı> ; sweep.py <çıktı> <s> "<sahne> run ..." ; soak.py ; functional.py ; faults.py
```

Laboratuvar: `MEDIABOX_TV_SPATIAL="advanced|stress|globe on=*|on=a+b objects= particles= layers=
msaa= full run bounded fault=shader|buffer|texture|framebuffer|draw|context"`.

## Ham veri

`results/quietos-advanced-graphics/`: Ultra — `capacity`, `straight-alpha`, `globe-3rounds`,
`skia-round1..3` (`.jsonl`), `mali-probe.txt`; Plus — `plus-4k60/rounds-apg`, `levels`,
`scenes-at-once`, `repeat-a-p`, `soak`,
`soak-frames`, `mali-extensions.txt`. Ekran yakalamaları git'te değildir (ağ adı görünüyor). PR #6'nın
sonuçları `experiment/quietos-hardware-animation` dalında.
