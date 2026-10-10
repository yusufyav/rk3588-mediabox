# MediaBox yaşayan proje atlası

Projenin kaynaklarına dayanan, Türkçe, çevrimdışı ve bağımlılıksız mimari uygulaması. Ürün kodlarını değiştirmez; cihaz servislerine bağlanmaz.

## Açma

`index.html` dosyasını tarayıcıda açın. Kurulum, Node paketi, build veya internet gerekmez. Yazı tipi ve kaynak içerikleri bu dizindedir.

İsteğe bağlı yerel HTTP sunucusu, repo kökünden:

```sh
python3 -m http.server 8765 --bind 127.0.0.1 --directory docs/project-atlas
```

Ardından `http://127.0.0.1:8765/`. Sunucuyu bitirmek için Ctrl+C.

## İçerik ve etkileşimler

- **Sistem haritası:** ana yolun on düğümlü diyagramı; kontrol/medya filtresi, seçili bileşen, kaynaklar ve ilişkiler. Tam model 31 bileşen ve 55 bağlantıdır.
- **Veri akışları:** gömülü oynatma, Kodi devri, altyazı AutoSync, TV tarayıcısı, çıkış/hotplug ve derleme/kurulum. İleri/geri ve otomatik adımlama.
- **Donanım:** Ultra/Plus ve çıkış seçimi; verici, ALSA ve CEC eşlemesi; VPU/GPU/VOP2/NPU sorumlulukları ve HDR düzlem sözleşmesi.
- **Servisler:** 12 gerçek unit tanımı ve bir transient player. Satır açıldığında gerçek ExecStart, sıralama ve çatışma alanları.
- **Bileşen kataloğu:** katman ve tam metin filtresi; tüm bileşen ilişkileri. `#components/session` gibi doğrudan adresler.
- **Kanıt defteri:** kabul kapıları, açık kayıtlar, belge/kod farkları, rafa kaldırılan araştırmalar ve korunacak sözleşmeler.
- **Kaynak kütüphanesi:** 26 Markdown belgesi, 114 seçili kaynak içeriği ve Git tarafından izlenen 429 dosyanın envanteri. Tam metin arama, satırlı kaynak okuyucu, dosya sayfalama ve JSON dışa aktarma.

Ctrl+K global aramayı açar; Escape diyalogları kapatır. Tema düğmesi açık/koyu görünümü değiştirir; yalnız bu tercih tarayıcıda saklanır. Dar ekranlarda ana diyagram yatay kaydırılır, menü altta gösterilir. Klavye odağı, etiketler ve azaltılmış hareket tercihi desteklenir.

Sayılar ilk sürümün anlık görüntüsüdür; arayüz çoğunu veri modelinden hesaplar. “Cihaz ölçümü var” etiketi kaynaklarda ilgili ölçüm bulunduğunu söyler; bileşenin her işlevinin veya bugünkü cihazın doğrulandığı anlamına gelmez. Bu çalışma fiziksel kartta yeni test çalıştırmadı.

## Dosya yapısı

| Dosya | Sorumluluk |
| --- | --- |
| `index.html` | Uygulama kabuğu, diyaloglar ve erişilebilirlik işaretleri |
| `styles.css` | Responsive tasarım, açık/koyu tema, diyagram ve okuyucu |
| `app.js` | Görünümler, gezinme, arama, filtreler, akış adımları |
| `atlas-data.js` | İnsan tarafından kürate edilen mimari model |
| `snapshot.js` | Kaynak içerikleri, hash’ler, unit alanları ve dosya envanteri |
| `refresh_snapshot.py` | Repo üzerinde salt okunur tarama; yalnız snapshot çıktısını yazar |
| `ANALYSIS.md` | İnceleme yöntemi, mimari bulgular ve yorum sınırları |
| `tests/validate.mjs` | Kaynak hash’leri, referanslar ve veri bütünlüğü kontrolü |
| `tests/browser.html` | Yerel HTTP üzerinden çalıştırılan davranış kontrolleri |
| `tests/browser-results.json` | İlk sürümün kaydedilmiş tarayıcı test sonucu |
| `assets/InterVariable.woff2` | Repodaki web UI yazı tipinin yerel kopyası |

## Yeni özellik ekleme

1. İlgili kaynak kodunu, unit dosyasını ve yeni ölçüm/belge kaydını birlikte inceleyin.
2. `atlas-data.js` içindeki `nodes` listesine benzersiz kimlik ile bileşen ekleyin. Alanlar sırasıyla `id, name, tech, layer, status, description, details, sources`.
3. `edges` listesine `[from, to, label, type]` ilişkilerini ekleyin. Tür `control` veya `data`.
4. Gerekirse `flows` içine kimlik, başlık, özet ve `[nodeId, title, explanation]` adımları ekleyin. `issues` içine açık iş / fark / ertelenmiş araştırmayı kaynaklarıyla kaydedin.
5. Genel diyagramı değiştirmek gerekiyorsa `app.js` içindeki `positions` ve `mapEdges` yerleşimini güncelleyin. Genel harita bilinçli olarak sade tutulur; katalog tüm ilişkileri gösterir.
6. Snapshot’ı ve doğrulamayı çalıştırın:

```sh
python3 docs/project-atlas/refresh_snapshot.py
node docs/project-atlas/tests/validate.mjs
```

Snapshot yenilemek **mimari yorumları güncellemez**. Eski bir açık sorunu kapatmak veya bir araştırmayı üretim bileşeni olarak etiketlemek insan incelemesi ve kaynak kanıtı gerektirir. Araç yalnız Git tarafından izlenen dosyaları envantere alır; yeni kaynak henüz izlenmiyorsa kürasyon atfı ile içerik kopyasına alınabilir ancak envantere otomatik girmez. Atlasın kendi dosyaları döngüsel büyümeyi önlemek için snapshot dışında tutulur.

## Doğrulama

İlk sürüm: Chromium başsız çalıştırmada **33 başarılı davranış kontrolü**, sıfır hata. Navigasyon, on düğümlü harita, kaynak içi arama, altı akışın ileri/geri adımları, oynatma düğmesi, kart/port eşleme, unit ayrıntıları, katalog filtresi, doğrudan adres, kanıt filtresi, kütüphane araması, sayfalama, global arama ve tema kontrol edildi. Kaynak referansları ve SHA-256 özetleri ayrıca doğrulandı.

Masaüstü 1440×1100 ve mobil 390×844 render’ları ile akış görünümü görsel olarak incelendi. İnteraktif kullanıcı tarayıcısı bağlantısı ortamda bulunmadığı için otomasyon geçici profilli başsız Chromium ile yapıldı. Ürünün Rust/Python testleri çalıştırılmadı; ürün kodu değişmedi.

HTTP sunucusu açıkken `http://127.0.0.1:8765/tests/browser.html` test sayfasını açabilirsiniz. Sonuç sayfanın üstünde JSON olarak görünür. Test profili açık temayla başlamalıdır. Kaydedilmiş sonuç, yalnız oluşturulduğu sürüm için kanıttır; sonraki değişikliklerden sonra yeniden çalıştırın.
