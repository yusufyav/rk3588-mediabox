# QuietOS donanım animasyonu: küre deneyi

Durum: deney, varsayılan kapalı. Öneri: **üretime alınmasın** (gerekçe sonda).
Ölçülen kart: Orange Pi 5 Ultra, kernel 6.1.172-vendor-rk35xx, 2560x1440 @
144 Hz (dclk 583,6 MHz), 2026-11-25. Orange Pi 5 Plus'ta çalıştırılmadı.

## Nasıl çalışır

`MEDIABOX_TV_GLOBE_OFFLOAD` yoksa hiçbir parçası çalışmaz.

| Değer | Yöntem |
|---|---|
| (yok) | A: Slint çizer (ürün) |
| `still` | B: tek XRGB tampon; konum/ölçek/alfa VOP2'de. Küre **dönmez** |
| `decode` | C: istenen kare o an VPU'da çözülür (HEVC, MPP), DMA-BUF olarak taranır |
| `cache` | D: 129 kare başlangıçta VPU'da çözülür, sınırlı havuzda tutulur |
| `nothing` | ölçüm tavanı: küre hiç yok |

- Kareler `tools/globe-offload/make-frames.sh` ile sahnenin kendisinden, ürünün
  renderer'ıyla üretilir; her kare ayrı IDR. Depoya konmaz (3,53 MB).
- Hangi kare, nerede, ne büyüklükte, ne parlaklıkta: hepsi yalnız kameranın
  `d` değerinden (`src/globe.rs`). Oynatma yok; ters yön ve kesinti başka bir
  `d`'dir.
- Plane, arayüz karesiyle **aynı atomik commit'te** gider.
- NV12'de alfa yok; küre, zemine eklediği ışık olarak pişirildi. Arayüzün o
  dikdörtgende alfası silinip rengi bırakılır; VOP2
  `arayüz + (1 − alfa)·plane` uyguladığından ışık Slint'in canlı zeminine
  eklenir. Sönme, alt katmanın global alfası.
- Film plane'i istediği anda alır; sonra küre geri gelir.

Cihazda: `variant.sh` yöntemi `/run` altındaki geçici drop-in ile seçer
(birim `/dev/mpp_service`'i üründe açmaz).

## VOP2'nin fiilî sınırları

- VP0 `PLANE_MASK` = Cluster0 + Esmart0: animasyona tek plane var, o da filmin.
  Sürücü diğerlerini de `possible_crtcs=1` gösterir.
- Esmart0: yalnız LINEAR, 1/8–8× ölçek, dönme yok (reflect-y), tam piksel konum.
- Cluster0: AFBC kabul eder, NV12 etmez.
- BT.709 **tam** aralık yok: istenince `y2r[BT.601_F]` uygulanır. Kareler
  bu yüzden BT.601 tam aralık.
- Alt katman alfası donanımda: hareket boyunca `MIX0_DST_COLOR_CTRL`
  (0xfdd90654) `00680061 … 00de0061`, plane'in `glb_alpha`sını bir kare geriden
  izler.

## Sonuçlar

Üç tur (1.–2. ABBA), tur başına aynı betik (`tools/globe-offload/measure-globe.py`,
PR #5 `measure.py` üstünde), varsayılan frekans politikaları. Ortalama; Δ
A'ya göre. 1.–2. tur `0372b17f`, 3. tur `41526f47`: fark yalnız
`COLOR_ENCODING` değeri, donanımın uyguladığı dönüşüm aynı.

**Tek adım (Canlı TV ↔ Tarayıcı, 16 tuş)**

| | A | tavan | B | C | D |
|---|---|---|---|---|---|
| süreç Mkomut/kare | 3,64 | 2,84 (−%22) | 2,86 (−%21) | 3,12 (−%14) | 2,87 (−%21) |
| sistem Mkomut/s | 639 | 560 (−%12) | 558 (−%13) | 594 (−%7) | 562 (−%12) |
| GPU Mçevrim/kare | 1,06 | 0,95 (−%10) | 0,94 (−%11) | 0,97 (−%9) | 0,95 (−%10) |
| GPU ms/kare · meşgul | 1,69 · %19,8 | 1,66 · %19,6 | 1,67 · %19,6 | 1,69 · %19,9 | 1,67 · %19,7 |
| DDR yükü % | 7,6 | 7,5 | 7,6 | 7,6 | 7,5 |
| kaçan vblank | 43/5970 | 8/5984 | 30/5975 | 14/5977 | 16/5981 |
| giriş→commit p50 ms (3 tur) | 7,6 8,9 7,7 | 8,2 6,0 7,7 | 8,6 8,2 7,7 | 8,3 6,5 7,7 | 7,9 8,1 7,9 |
| giriş→flip p50 ms (3 tur) | 12,8 12,5 12,9 | 11,5 9,6 10,9 | 12,1 12,9 11,6 | 12,6 11,5 11,9 | 11,2 11,2 11,5 |
| rkvdec kesmesi / tur | 0 | 0 | 0 | 739–744 | 0 |

**Tuş tekrarı (80 ms, 48 tuş)** Mkomut/kare 4,35 → tavan 3,83 · B 3,81 · C 4,01
(−%8) · D 3,84 (−%12). GPU Mçevrim/kare 0,97 → 0,87 · 0,93 · 0,89 · 0,90.
Kaçan vblank 4 · 1 · 3 · 5 · 6 (≈1660'ta).

**Ters yön (150 ms, 28 tuş)** Mkomut/kare 4,80 → tavan 3,89 · B 3,92 · C 4,37
(−%9) · D 3,93 (−%18). GPU Mçevrim/kare 1,02 → 0,93 · 0,91 · 0,89 · 0,96.
Kaçan vblank 0 · 3 · 7 · 0 · 0 (≈1890'da).

GPU ms/kare ve CPU % frekans politikasıyla oynar: gelen iş azalınca saat de
düştüğünden ms değişmez; CPU % tur içinde 0,4–1,8 GHz taban farkıyla anlamsız
saçılır. Kıyas komut ve çevrimdedir. GPU çevriminin turlar arası saçılması
tekrar/ters yönde ±%10: oradaki GPU farkları bu büyüklüktedir, tek adımınki
(±%3) değildir. Fiziksel input-to-photon ve enerji ölçülmedi.

| | A | B | C | D |
|---|---|---|---|---|
| DMA-BUF (sistem geneli) | 42,2 MB | 42,2 MB (+1,1 MB dumb) | 44,9 MB (+9 tampon) | **109,7 MB** (+67,5) |
| GPU belleği · RSS | 28 · 109 | 28 · 111 | 28 · 114 | 28 · 112 |
| depolama | 0 | 1,1 MB | 3,53 MB | 3,53 MB |
| soğuk başlangıç | – | dosya okuma | ilk kare 12 ms | ilk kare 12–28 ms, havuz 102–138 ms |
| boşta (küre ekranda) Mkomut/s · DDR % | 16,0 · 5,2 | 15,9 · 5,9 | 16,0 · 5,3 | 16,0 · 5,3 |

### VPU ve sıfır kopya

- D: 129 kare → tam 129 rkvdec kesmesi (65 + 64, iki çekirdek). Hareket
  sırasında 0: hazırlık maliyeti başlangıçta, harekette yok.
- C: çözülen 740/743/744 kare → 739/740/744 kesme. Çözme p50 0,68–0,74 ms, p95 en
  çok 1,6 ms, en uzun 4,7 ms (tek adım). İstek çizimden önce verilir, kare çizim bitince
  alınır: çizim başında yeni olan 739 karenin hiçbiri commit'te eksik değildi.
- MPP'nin DMA-BUF'ı PRIME ile alınıp olduğu gibi `NV12` framebuffer olur
  (528x528, pitch 576); tampon başına bir import. CPU piksel kopyası, CPU renk
  dönüşümü, GPU doku yüklemesi yok. CPU'nun kopyaladığı yalnız ~27 KB'lık
  sıkıştırılmış paket.

### 144 Hz'te gerçek kare

Plane'in yeri, ölçeği ve alfası her karede yeni. Dönmenin görüntüsü 129 ayrı
kare (d'ye göre dinlenmeye yakın sık). Hareket eğrisinden: gelen adımda dönen 60
karenin 51'i, gidende 50'nin 44'ü, kısa hareketlerde 33'ün 30'u yeni görüntü;
açı hatası en çok 0,76°. Yani **dönüş gerçek 144 FPS değil, ~%85–90'ı**; A'da
her kare yeni.

### Görsel doğruluk

- Dinlenmede zemin + çözülmüş ışık, Slint'in kendi karesine karşı: 46,5 dB; en
  büyük fark 24/255, örneklerin %0,19'u 8/255'i geçer (4:2:0 + HEVC). Bu hesap
  host'ta; **cihazda kompozit geri okunamaz** (writeback HDMI'yi bozuyor),
  ekrandan ölçüm yapılmadı.
- Hareket sırasında: konum tam piksele yuvarlanır; d<0'da 1,42×'e büyüyen kare
  VOP ölçekleyicisiyle yumuşar; ışık, dinlenme zemini için hesaplandı, zemin
  renk değiştirirken çizgi altı birkaç düzey sapar.
- Kare seti mod başına ayrı (yalnız 1440p üretildi; başka modda Slint çizer).
- Deliğin üstüne gelen her şey (pencere, bildirim) küreyi toplamalı gösterir;
  üretimde kapsanması gerekirdi.

### İşlevsel (Ultra)

Küre plane'deyken `mediabox-playback-smoke`: 13/13 PASS; film BT.709_L, alfa
0xff, tam ekran; bitince küre geri geldi. Tarayıcı'dan çıkınca plane kapandı.
Küre ekrandayken servis yeniden başlatıldı: plane kapalı, alfa 0xffff. HDR film,
Kodi geçişi ve mod değişimi küre ekrandayken **sınanmadı**.

## Mutlak tavan

Bütün sahneler çizilmeden (commit'lenmeyen ölçüm derlemesi `148f5db6`, 2 tur,
tek adım): 2,15 Mkomut/kare (−%41), 0,835 Mçevrim/kare (−%21), GPU %19. Yani
sahnelerin **tamamı bedava olsa bile %50 hedefi tutmaz**. Kalan maliyet sahnede
değil: 144 Hz'te tam ekran karenin kendisi (zemin dokularının bindirilmesi,
arayüz parçaları, commit) ve küre durduktan sonra 900 ms'ye kadar süren ışık
geçişi.

## Karar

| | Sonuç |
|---|---|
| B | Tavana ulaşır, ama küre dönmez: aynı görüntü değil. Elendi |
| C | En az bellek, gecikmesiz; CPU kazancı −%14 (sistem −%7). Her karede VPU çalışır; kazanımın üçte biri MPP'ye gider |
| D | Tavana oturur: CPU −%21 (sistem −%12), GPU çevrimi −%10 |

D teknik olarak en iyisi, yine de üretime önerilmiyor: GPU meşguliyeti ve DDR
ölçülebilir değişmiyor, karşılığında +67,5 MB, filmle paylaşılan tek plane,
mod başına asset, dönüşte daha az ayrık kare ve birime yeni aygıt izni geliyor.

Sahne donanıma ancak **arayüz karesi o sırada hiç çizilmiyorsa** değer:
tam ekran, kendi kendine akan bir animasyonda (ör. açılış). Bu durumda doğru
mimari D'nin buradaki hâlidir: intra HEVC → MPP → sınırlı DMA-BUF havuzu →
Esmart0, kare = f(zaman). Ana menüde sıradaki kazanç donanım kod çözücüde değil,
karenin kendisinde (kısmi yeniden çizim, ışık geçişinin süresi) aranmalı.
