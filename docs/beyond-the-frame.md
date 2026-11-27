# Beyond the Frame — açılış sekansı

Durum: **beşinci sürüm (Ana Menü'nün FilmScene'i), onay bekliyor.** Yalnız Ayarlar > Arayüz ve
Hareket > *Açılışı önizle* ile oynar. Cihazın açılış akışına bağlanmadı,
cihaza kurulmadı.

Reddedilen denemeler geri dönülebilir durumda: `beyond-the-frame-v1`
(`88d2229`, şehir/tren/çöl), `ecfe078` (film şeridi ilk çekimi),
`beyond-the-frame-v3` (`c72673a`, projektör kapısı), `beyond-the-frame-v4`
(`76180e6`, monolit/şehir/motosiklet; ayrı Rust geometri hattı).

## Teknik sapma ve düzeltme (beşinci sürüm)

Dördüncü sürüm (`76180e6`, `beyond-the-frame-v4`) Ana Menü'nün grafik
mekanizmasını kullanmıyordu: `src/intro.rs` kendi pinhole kamerasını, kendi
yol üretimini ve dolgulu cel çizimini kurmuş, `intro.slint` yalnız genel
katman yuvaları çizmişti. `FilmScene`, `SceneBase`, `Plane`, `px/py/zk/fz`,
üç kamera (`d`, `d-far`, `d-near`), `QuietMotion.fade`, `QuietCurves.camera`
ve `QuietGround` hiç kullanılmamıştı; Ana Menü'den yalnız `QuietMotion.on`
alınmıştı.

Şimdi `ui/quiet/intro.slint`:

- `FilmScene`'i (scenes.slint) değiştirmeden, panel genişliğinde örnekler;
  `px`, `py`, `zk`, `fz`, üç `Plane` ve solmaları sahnenin kendisidir.
- Ana Menü'nün zeminini (`QuietGround`, "media" aurası) kullanır.
- Kamerayı Ana Menü gibi sürer: her satır için `d = satır - kamera`, üç
  bağımsız kamera (uzak, ana, yakın), ürünün süreleri (`t-camera-far`,
  `t-camera`, `t-camera-near`) ve eğrisi (`QuietCurves.camera`'nın aynı
  kübik Bézier'i), açılışta `QuietRig.land`'ın `arrive` ilavesi.
- Fark yalnız saat: Ana Menü'de kameralar atanan animasyonlu özellikler,
  burada tek saatin fonksiyonları (`QuietIntro.scrub` ile durdurulabilir).

Sekans (4,1 s): karanlık; kamera dört satır film boyunca ilerler, her satırın
şeritleri derinlikten gelir, kapıdan akar ve geride kalır, düzlemler
derinlikte ayrışır; son satırda durur; kamera onun içinden geçer ve arayüz
açılır. scenes.slint, motion.slint, materials.slint ve src/spatial bu
sürümde değişmedi.

## Arayüz ve Hareket ayarları

Tek kaynak: `interface-preferences.json` (eski `{"layout": …}` dosyası
aynen okunur). Uygulama başlarken okur ve `QuietMotion`'a uygular; her
değişiklikte dosyanın tamamını atomik yazar. Motion Lab
(`MEDIABOX_TV_MOTION_LAB=1`) aynı değerleri çevirir ve aynı dosyaya yazar.

| Kart | Değer | Not |
|---|---|---|
| Tema | Klasik MediaBox / Quiet OS | "Sistem"deki *Arayüz düzeni* kartı buraya taşındı |
| Açılış animasyonu | aç/kapat | Açılış akışına bağlandığında geçerli olacak |
| Oynatılma | Her açılışta / Günde bir kez / Güncellemeden sonra | Karar `Intro::plays` (testli) |
| Kumandayla atlama | Herhangi bir tuş / Tamam veya Geri / Atlanamaz | Önizlemede de geçerli |
| Açılışı önizle | — | O anki ekranın üstünde oynar, bitince aynı ekrana döner |
| Azaltılmış hareket | aç/kapat | `QuietMotion.reduced` |
| İkon animasyonu | aç/kapat | `QuietMotion.icons` |
| Süre, Derinlik, Hareket mesafesi, Atmosfer ışığı, Katman ayrışması | × 0 … × 2 | Motion Lab parametreleri |
| Eğri | Ürün / Çevik / Yumuşak | `QuietMotion.profile` |
| Varsayılan hareket ayarlarına dön | — | Yalnız hareket |

## Önizleme ve kayıt

Host'ta, ürünün gerçek renderer'ı (FemtoVG, GLES, EGL pbuffer) ile kare kare:

```sh
cd rust/crates/mediabox-tv
MEDIABOX_TV_GL=require \
MEDIABOX_TV_INTRO_FRAMES=/var/tmp/rk3588-mediabox/logs/beyond-the-frame/film \
MEDIABOX_TV_INTRO_PANEL=2560x1440@2 MEDIABOX_TV_INTRO_FPS=60 \
  cargo test intro_on_film
ffmpeg -framerate 60 -i .../film/frame-%04d.png -c:v libx264 -pix_fmt yuv420p -crf 16 intro.mp4
```

`MEDIABOX_TV_INTRO_AT=0.4,1.3,2.2` yalnız o anları çizer.

## Doğrulama (host, gerçek renderer)

- `the_opening_is_the_film_scene_and_ends_on_the_interface`: karanlık,
  kamera yolda iken çizgiler, durunca hareketsiz sahne, sonda arayüz piksel
  piksel aynı; 1080p, 1440p@2, 4K@2, 1366×768'de geçti.
- Kayıt: `/var/tmp/rk3588-mediabox/logs/beyond-the-frame/v5/`
  `beyond-the-frame-v5-1080p60.mp4`, `beyond-the-frame-v5-1440p60.mp4`.

## Açık kalanlar

- Kullanıcı onayı: sekansın görsel kalitesi.
- Cihazda gerçek zamanlı ölçüm (onaydan sonra, haber verilerek).
- Açılış akışına bağlanma — kapsam dışı, onay bekler.
