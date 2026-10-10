# Kaynak incelemesi ve mimari çıkarım

İnceleme tarihi: **10 Ekim 2026**, kaynak revizyonu **80b5021**. Bu rapor ve atlas, yerel çalışma ağacını açıklar. Kartlara bağlanılmadı, donanım ayarı yapılmadı, servis çalıştırılmadı, ürün kodu değiştirilmedi.

## Yöntem ve kanıt sırası

README, `docs/`, platform notları, `results/` durum ve araştırma belgeleri, araç README’leri; ardından ilgili Rust/Python/C/C++ kaynakları, systemd birimleri, uygulama yapılandırması ve build/deploy betikleri karşılaştırıldı. Belgelerdeki her cümle eşit güncellikte değildir. Çalıştırılacak komut için gerçek unit/launcher, davranış için güncel fonksiyon, fiziksel özellik için bağlamı belirtilmiş cihaz ölçümü esas alındı. Güncel kodun varlığı cihazda kabul kanıtı yerine kullanılmadı.

Örnek: `media/policy/decide.py::mode_when_chosen` açık kullanıcı seçimini eski P5 ret belgesinden farklı ele alıyor. Buna karşılık Dolby Vision donanım desteği, o fonksiyon nedeniyle var kabul edilmedi. Her iki bilgi de atlasın fark defterinde korunuyor.

## Mimari omurga

Ürün üç ayrı katmanı birleştirir:

1. **Deneyim:** Rust/Slint native TV yüzeyi, Rust/WASM LAN arayüzü, `mediaboxctl`. TV ve web paketleri ana Cargo workspace üyeleriyle aynı şey değildir; kendi build akışları vardır.
2. **Kontrol:** Rust daemon tek otoritedir. Tipli Unix JSON protokolü, loopback HTTP ve özel ağ istemcilerine açık LAN HTTP sunar. Ekran sahipliği, uygulama yaşam döngüsü, oynatma gözetimi, CEC ve cihaz ayarları bu otoritededir.
3. **Medya:** Python worker güncel üretim bileşenidir; kaldırılmış eski Python kontrol daemon’u ile karıştırılmamalıdır. Katalog ve Stremio bridge, probe/policy/ranking, session/relay, ses dönüşümü ve altyazı analizi burada bulunur.

Node tabanlı Stremio server torrent/stream çözümleyicisidir; native arayüz değildir. Worker yalnız loopback dinler. Web istemcisi worker’a doğrudan ikinci bir LAN otoritesi üzerinden bağlanmaz; daemon relay yüzeyini kullanır.

## Oynatma yolları ve donanım ilişkisi

**Gömülü yol:** transient systemd player → mpv → özel FFmpeg/RKMPP → NV12/NV15 DMA-BUF → `vo_mediabox.c` / SCM_RIGHTS → TV `video.rs` PRIME import / framebuffer / SetPlane → VOP2. Native TV DRM master’ı korur. Film bir GLES texture’a çevrilip UI içine kopyalanmaz. Mali G610 yalnız UI render tarafındadır; VOP2 video ölçekleme ve scanout’u yapar. Buffer ömrü, ekrandan kalktığının doğrulandığı ana kadar korunur.

**Kodi yolu:** daemon mevcut film konumunu ve kaynak bilgisini alır, player’ı durdurur, ekran sahipliğini devreder, Kodi JSON-RPC üzerinden devam ettirir. Kodi bağımsız GBM/DRM uygulamasıdır. HDR10 için kabul edilmiş referans yoludur; çıkışında native yüzey geri gelir. Dönüştürülen bir pipe’a geç katılan ikinci okuyucu doğru handoff değildir; kaynak/origin ayrımı bu yüzden vardır.

**Tarayıcı yolu:** ayrı browser surface → VT8 / sway → Chromium. Native UI’nin “compositor yok” özelliği bu uygulamaya genellenemez. V4L2-to-MPP preload runtime AV1 yolunu da açar; VA-API kodunun bulunması onun seçili backend olduğu anlamına gelmez. Fullscreen/direct scanout, pencere dekorasyonu ve yüzey geometrisine bağlıdır.

**Donanım keşfi:** DRM connector sahibi, ilişkili render aygıtı, Device Tree vericisi, ALSA ve CEC birbirinden bağımsız seçilmez. Numara/isim sabitleri taşınabilirlik sorunudur. Ultra’nın `HDMI-A-1` → `rockchiphdmi1` eşlemesi, Plus’ın aynı adlı konektörü için geçerli değildir. DisplayPort’ta CEC yoktur.

**HDR:** GUI pikseli ve EOTF etiketi uyumlu olmalıdır. Kabul edilen Kodi/VP0 zinciri SDR GUI + PQ video + VOP2 SDR2HDR’dir. VP1/VP2 aynı dönüştürme yeteneğini sunmaz. Çıkış renk formatı ve bit derinliği alıcının EDID/TMDS sınırına bağlıdır. Eski tek-CRTC ve plane ID örnekleri iki kart için evrensel sabit olarak kullanılmadı.

## Medya yaşam döngüsü

`MediaInfo`, video/ses/kapsayıcı kararlarına ve gerekçe kodlarına dönüşür. Sıralama çözünürlüğü tek ölçüt yapmaz. Risk uyarısı ile açık kullanıcı seçiminin uygulanması farklı fonksiyonlardır.

Genel worker ses dönüşümü video kopyalayarak AC-3 üretebilir; `assert_video_copy` bu sınırı korur. Ancak gömülü player ses planını kendisi uygular: passthrough, PCM decode ve seçildiğinde `lavcac3enc`. Dolayısıyla “her DTS dosyası worker’da AC-3 olur” güncel ürünün doğru tanımı değildir.

Relay’nin ayrı gerekçesi TLS’dir: donanım decoder FFmpeg’i TLS taşımadığından HTTPS kaynakları worker tarafından yerel HTTP’ye aktarılır. Range, yeniden bağlanma ve doğru bayttan devam seek için kritiktir. Sahibi veya açık okuyucusu olan session korunur; durmuş oturum 410 verir.

Oynatmanın bitişine video plane’in kararması karar vermez. `playback::Supervisor` mpv/Kodi olaylarını konum ve süreyle birlikte ele alır; normal bitiş, istenen durdurma ve erken EOF/hata ayrılır. Hata ve handoff hesapta yanlışlıkla “kapandı” olmaz.

## Altyazı alt sistemi

Kaynaklar: dosyanın izleri, addon/OpenSubtitles v3 ve isteğe bağlı OpenSubtitles.com. Arama, indirme, sağlayıcı kotası ve kimlik bilgileri ayrı sorumluluklardır.

Uygunluk önce Matroska indeksindeki referansla değerlendirilir. Birden çok izin uzlaşması, kısmi altyazı, zaman tabanı farkı ve farklı kurgu reddi önemlidir. Gömülü referans yoksa sınırlı ses pencerelerinin enerji VAD kanıtı kullanılır; konuşma tanıma yoktur. Çocuk süreç/öncelik ve indirme bütçeleri film relay işini korur. Uygulanan dönüşüm yalnız sabit offset’tir. ASS stil, bitmap çizimi, Kodi handoff ve isteğe bağlı ASR/NPU desteği mevcut ürün yeteneği olarak gösterilmedi.

## Cihaz, servis ve teslimat sorumlulukları

`output.rs` tercih/trial/planı, observer fiziksel gözlemi sahiplenir. EDID kimliği, gözlem nesli ve owner commit raporu birlikte denetlenir. Trial kalıcı ayar değildir; journal ve geri dönüş süresi vardır.

CEC UAPI adaptörleri daemon tarafından dinlenir; gönderim seçili çıkışa gider. IR ve BLE HID girdileri ayrı yollarla libinput’a gelir. UR-02 mikrofonunun ADPCM çözümleyicisi araştırma aracıdır; üründe sesli arama değildir.

Ses cihazları, softvol, Ethernet, Wi-Fi/netplan, Bluetooth/BlueZ, fan ve GPIO LED’ler daemon modülleridir. Eski “Wi-Fi/BT yok” listesinin güncel kaynakla farkı açıkça kaydedildi. BT eşleme agent’ı, radyo başlangıç sırası, ekran seed/observer/changed ve konsol hizmetleri servis envanterine dahil edildi.

MPP/librga/FFmpeg ve player/runtime pinleri ürün prefix’ine aittir; ScreenBridge runtime bağımlılığı yoktur. İki ürünün ekran sahipliği systemd Conflicts + After ve kalıcı tercih ile sıralanır. Rust/WASM host cross-build ile üretilirken runtime/player target build akışları bulunur. Üretim kurucusu prebuilt release kullanır; temiz kurulumda build zorunluymuş gibi gösterilmedi.

## Depo kapsamı

| Alan | Atlas içindeki karşılık |
| --- | --- |
| `rust/crates/mediabox-tv` | Native UI, platform/video, ekranlar, giriş, fdstore |
| `rust/crates/mediabox-ui` | WASM arayüz, daemon API istemcisi |
| `rust/crates/mediaboxd-rs` | Kontrol, sahiplik, lifecycle, player/Kodi, playback, output, audio, CEC, ağ, fan/LED |
| `mediabox-core`, `mediabox-platform`, `mediabox-input`, `mediabox-cec`, `mediaboxctl` | Ortak sözleşme, keşif, input, Linux CEC ve operatör CLI |
| `media/` | API, katalog/hesap, inspector/policy, proxy/FFmpeg, subtitles ve ilgili testler |
| `packaging/`, `config/` | Unit’ler, launcher’lar, udev/IR overlay/keymap, ALSA, uygulamalar ve runtime pinleri |
| `patches/` | Kodi üretim serisi ve tanı açıklamaları |
| `src/`, `tools/`, `tests/` | C/C++ HDR/KMS/decoder/cadence temeli, ölçüm araçları, UI preview ve host doğrulamaları |
| `scripts/`, `releases/`, `dist/`, `assets/`, `bin/` | Derleme, release/install/deploy, dağıtım çıktıları ve test medyası / giriş komutları |
| `docs/`, `results/` | Mimari gerekçeler, kabul kanıtları, tarihsel ve açık kayıtlar |

429 dosyalık envanter dizinleri tek tek gezilebilir kılar. 31 mantıksal bileşen, her yardımcı fonksiyonu ayrı kutuya dönüştürmeden bu sorumlulukları gruplar. Kaynak kopyaları seçilidir; tüm repo içeriklerinin kopyalandığı iddia edilmez.

## Açık ve tarihsel bulgular

Atlasın kanıt defterinde kaynaklarıyla ayrı kayıtlar vardır: yüksek-TMDS SCDC/hotplug kaybı; debugfs summary OOPS; browser tearing/direct-scanout ve hover ayrımı; USB ses tercihi ve relay sonrası mpv durması; altyazı kalibrasyonu; player UI/performance ve web metadata borcu.

P5 ret kararı, gömülü ses dönüşümü, Wi-Fi/BT, Plus kurulum tablosu, HTTP portları ve HDR doğrulamasının kapsamındaki belge/kod farkları işaretlendi. Eski tabloya bakıp Plus’ı hâlâ kurulmamış saymak yerine 19 Eylül kurulum kanıtı dikkate alındı. Aynı biçimde tarayıcıda erken “kapandı” kaydı daha sonraki geri alınma notuyla birlikte ele alındı.

AI upscale 4 Ekim kaydına göre raftadır. MBSR hız hedefini yakalasa da gerçek film sürüm çiftinde kalite hedefini karşılamadı. Araç/ağırlıklar git geçmişindedir. RGA3 denemeleri geometrik kayma nedeniyle reddedildi. Üretimin VOP2 ölçekleme yolu değişmiş gibi gösterilmedi.
