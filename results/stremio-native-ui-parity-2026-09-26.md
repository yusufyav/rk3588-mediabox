# Filmler ve Diziler — Stremio arayüz eşitliği (2026-09-26)

- **Baseline:** `302ac2bb6c33f2c6ac286a10d6f8c3ef653d7897`
- **Son kod commit'i:** `51ec39c` (bu rapor ayrı bir commit)
- **Push:** yapılmadı.
- **Kapsam dışı, dokunulmadı:** kernel, bootloader, DRM sahipliği, HDMI düzeltmeleri, Kodi yamaları, fan, release altyapısı.
- **Gömülü web yok:** Stremio Web, Electron veya WebView kullanılmadı. Arayüz Rust + Slint ile yazıldı.
- **Stremio'dan alınmayanlar:** kaynak kodu, CSS, artwork, logo yok. Kurallar stremio-core'dan, ölçüler stremio-web stil dosyasından okunup kendi kodumuza yazıldı. İkonlar burada çizildi.

## Yöntem

- **Referans:** web.stremio.com, headless Chromium ile 1920×1080 (dpr 2) yakalandı. Sayfalar: board, Discover, arama, Drive film sayfası, Ted Lasso dizi sayfası, S1E1 akış listesi.
  - Kitaplık oturum gerektirdiği için yakalanamadı. Ölçüleri stremio-web'in `Library/styles.less` dosyasından alındı: 1 rem = 15 px, 1920 düzeninde 9 sütun.
- **Cihaz:** Plus (10.27.27.24), 3840×2160. Görüntüler arayüzün kendi karesinden alındı (SIGUSR1 anlık görüntüsü).
- **Karşılaştırma:** konumlar ekran oranı olarak ölçüldü, cihaz ve referans yan yana konuldu. Referans görüntüleri Stremio'ya ait olduğu için depoya konmadı.

## Eşitlik matrisi

| Alan | Stremio | Bu cihaz | Durum |
|---|---|---|---|
| Pano (board) | Sol ikon sütunu, üstte arama kutusu, "İzlemeye devam edin" + katalog rafları, "Tümünü Gör" | Aynı geometri: ilk poster %5,47 W, raf aralığı %36,6 H | Eşit ([01](stremio-native-ui-parity-2026-09-26/01-pano.jpg)) |
| Poster odağı | Çerçeve sabit, beyaz halka, görsel 1.05 yakınlaşma, 100 ms | Aynı | Eşit |
| Keşfet | 3 açılır filtre, 7 sütun ızgara, sağda kayıt paneli | Aynı; tüm kurulu kataloglar kendi filtreleriyle | Eşit ([02](stremio-native-ui-parity-2026-09-26/02-kesfet.jpg)) |
| Arama | Kutuda sorgu + ✕; sonuç katalog başına raf; öneriler ve son aramalar kutunun altında | Aynı. Kutunun altındaki panelde harfler de var (TV'de klavye yok) | Eşit ([03](stremio-native-ui-parity-2026-09-26/03-arama-sonuc.jpg), [04](stremio-native-ui-parity-2026-09-26/04-arama-oneri.jpg)) |
| Arama kuralları | LocalSearch (feed.json, 5 öneri, 250 ms), arama yalnızca gönderince, geçmiş 8 | Aynı | Eşit |
| Kitaplık | Tür açılır menüsü, sıralama çipleri (SORT_), 9 sütun, poster menüsü | Aynı. Menü OK ile açılır: Ayrıntılar / Vazgeç / İzlendi / Kaldır | Eşit ([05](stremio-native-ui-parity-2026-09-26/05-kitaplik.jpg), [06](stremio-native-ui-parity-2026-09-26/06-kitaplik-menu.jpg)) |
| Film sayfası | Logo, künye + IMDb rozeti, TÜRÜ/OYUNCULAR/YÖNETMENLER/ÖZET, Fragman + [kitaplık·izlendi] grubu, sağda akış sütunu | Satır konumları referansla ±%0,5 H; rozet 24,4–26,0 % H (referansla aynı) | Eşit ([09](stremio-native-ui-parity-2026-09-26/09-film.jpg)) |
| Dizi sayfası | "‹ Önceki · Sezon 1 ▾ · Sonraki ›", 8×5 rem küçük resimli bölümler, İZLENDİ/YAKLAŞAN bayrakları | Aynı; tarih tr-TR kısa biçim ("14 Ağu 2020") | Eşit ([07](stremio-native-ui-parity-2026-09-26/07-dizi.jpg)) |
| Sezon mantığı | Sıralı, özel bölüm (0) sonda; seçili sezon = seçilen → izlenen bölüm → ilk normal sezon | Aynı | Eşit |
| Kaynak listesi | "‹ S1E3 Başlık" + eklenti açılır menüsü aynı satırda; eklenti adı 7 rem, açıklama en çok 3 satır | Aynı; odaktaki satırda yeşil oynat işareti | Eşit ([08](stremio-native-ui-parity-2026-09-26/08-kaynaklar.jpg)) |
| İzlenme / devam | %70 izlendi, %90 jenerik, 90 sn'de bir hesaba yazma, bitfield, next_video | Aynı (stremio-core'dan); film devamı cihazda doğrulandı | Eşit (dizi devamı aşağıda) |
| İzlemeye devam edin | `is_in_continue_watching`, `_mtime` sırası, 100 öğe | Aynı | Eşit |
| Çözünürlük | Tek düzen | Tüm ölçüler ekran oranı; ayrı 720/1080/4K modu yok | Eşit |

## Doğrulama

| Komut | Beklenen | Gerçekleşen |
|---|---|---|
| `cd rust/crates/mediabox-tv && cargo test` | Hepsi geçer | 274 geçti, 0 hata |
| `cd rust && cargo test` | Hepsi geçer | Tüm crate'ler `ok` (104 + 45 + 20 + … testi), 0 hata |
| `python3 -m unittest discover -s media/tests -t .` | Önceden var olan 1 hata dışında geçer | 275 test, 1 hata: `test_architecture`, `proxy/relay.py: mimetypes`. Bu değişikliklerden önce de vardı. |
| Cihazda arayüz bellek kullanımı | — | VmRSS 230 MB, VmHWM (en yüksek) 344 MB |
| Cihazda boştaki CPU (pano açık, 10 sn) | Düşük | 8 tick / 10 sn ≈ tek çekirdeğin %0,8'i |

## Eksikler ve yapılmayanlar

- **Bölüm arama kutusu yok.** Stremio'nun sezon çubuğu altındaki "görüntüleri arayın" kutusu eklenmedi (TV'de klavye paneli gerekiyor).
- **Olmayan özellikler:** paylaş düğmesi, takvim ve eklenti yerleri. Bu cihazda bu özellikler yok.
- **Dizi bölümü oynatma/devam cihazda denenmedi.** Test gerçek Stremio hesabına yazdığı için bu turda çalıştırılmadı. Film tarafı doğrulandı (Drive): durdurunca timeOffset 1072568 → 1113000.
- **1080p çıkış denenmedi.** Görüntü modu değiştirmek ayrı bir sink testi gerektiriyor; düzen ölçüleri oransal.
- **Önceden not edilenler, bu turda kovalanmadı:**
  - 56 GB 4K DV REMUX kaynağı düzlemde kare göstermiyor.
  - Süre, gerçek 6041,5 sn yerine katalogdaki runtime'dan (6000 sn) yazılıyor.
- **Test aracı:** bir kez, bir `drive.py` çalıştırmasının ilk tuşu kayboldu. Sürüş testlerinde her OK'dan önce ekran görüntüsü alındı.
