# Rust control plane

`rust/` çalışma alanı cihazın **üretim kontrol düzlemidir**. Yanında doğrulanan
bir V2 olarak başladı; yerini aldığı Python `mediaboxd` servisi `d757d17` ile
ağaçtan çıkarıldı ve `mediaboxd-rs` tek yetkili durumundadır. Kodi build'i,
Stremio alanları ve Python medya çekirdeği (`media/`) bu çalışma alanının
dışında kalır — medya çekirdeği hâlâ çalışan bir bileşendir, kalıntı değil.

## Bileşenler

- `mediabox-core`: serde destekli ortak durum, olay ve wire protocol tipleri.
- `mediaboxd-rs`: yetkili Unix socket sunucusu, opsiyonel loopback HTTP API,
  Kodi JSON-RPC istemcisi, systemd lifecycle adaptörü ve olay yönlendirmesi.
- `mediaboxctl`: yalnız daemon protokolünü kullanan operatör CLI'ı.
- `mediabox-cec`: doğrudan Linux CEC UAPI (`/dev/cecX`) sahibi.
- `mediabox-input`: CEC/USB HID/BT HID/API kaynaklarını `InputAction` olarak
  normalize eden, broadcast eden ve routing kararı üreten katman.

Yerel yetkili socket `/run/mediabox/mediaboxd.sock`'tır. Her bağlantı tek satır
JSON `Request` gönderir ve tek satır JSON `Response` alır. `input_monitor`
komutu ilk yanıttan sonra newline-delimited JSON olay akışını açık tutar. Wire
protokol enum ile kapalıdır; shell/argv/komut satırı taşıyan genel bir çağrı yoktur.

Opsiyonel HTTP sunucusu `--http 127.0.0.1:8788` ile açılır, yalnız loopback
adresini kabul eder ve `POST /v1/control` endpoint'ini sunar. Bu yüzey varsayılan
olarak kapalıdır.

## Kodi

`KodiClient` şu JSON-RPC davranışlarını kapsar: `JSONRPC.Ping`,
`Player.GetActivePlayers`, `Player.GetProperties`, `Player.GetItem`,
`Player.Open`, `Player.PlayPause`, `Player.Stop` ve `Player.Seek`. Her istek iki
saniyelik timeout ile sınırlıdır ve transport/RPC/geçersiz yanıt hataları ayrıdır.
Relative seek önce mevcut zamanı okuyup güvenli mutlak zamana çevirir.

Daemon Kodi süreci spawn etmez. `kodi restart`, doğrulanmış sabit unit adıyla
`/usr/bin/systemctl restart kodi.service` çağırır. Böylece Kodi her zaman kendi
canonical unit'i içinde aynı DRM, udev ve input bağlamıyla başlar.

## Gösterge ışıkları

`leds_status` ve `leds_set` komutları kartın iki GPIO ışığını (`blue_led`,
`green_led`) yönetir; modlar `off`, `on`, `heartbeat`'tir. `SystemStatus` içinde
`leds` alanı aynı durumu taşır, böylece arayüz ayrı bir çağrı yapmadan satırı
çizebilir.

Bu iş daemon'un sahipliğindedir: `/sys/class/leds` root'a aittir ve televizyonu
çizen birim `/sys`'i salt-okunur bağlar. Arayüz yazmaz, ister.

Kalıcılık `/var/lib/mediabox/leds` dosyasıyla sağlanır. Çekirdek her boot'ta
device tree'nin `heartbeat` trigger'ını geri koyar, daemon da her başlayışta bu
dosyayı okuyup modu yeniden uygular. Dosya yoksa mod karttan okunur — hatırlanan
bir seçim yokken durum uydurulmaz. Birim iki satırla bunu mümkün kılar:
`StateDirectory=mediabox` dizini oluşturur, `ReadWritePaths=-/sys/devices/platform/gpio-leds`
ise `ProtectKernelTunables=true` altındaki salt-okunur `/sys` içinde yalnız bu
platform aygıtını yazılabilir bırakır. Baştaki tire, `gpio-leds` aygıtı olmayan
bir kartta birimin yine açılmasını sağlar.

İki davranış koda gömülüdür ve testleri vardır: ışıklar aygıt yolu `gpio-leds`
üzerinden geçtiği için seçilir (dizin taranmaz — `/sys/class/leds` altında USB
klavye kilit ışıkları ve `mmc0::` de listelenir), ve her yazma önce `trigger`
sonra `brightness` uygular.

Kırmızı ışık device tree'de yoktur, besleme hattına bağlıdır ve yazılımdan
kapatılamaz; bkz. [`architecture.md`](architecture.md) § G.

Televizyon arayüzünde bu ayar **Ayarlar > Işıklar** bölümündedir. Satır basış
anında güncellenir: makine yoklaması on saniyelik olduğu için yalnız ona
dayanan bir satır basıştan sonra on saniyeye kadar eski değeri gösterirdi.
Basış satırı aynı karede değiştirir, daemon'un yanıtı onun yerini alır
(reddedilen bir istek satırı geri alır), ve basıştan önce yola çıkmış bir
yoklama bu alan için yoksayılır. Cihazda ölçülen tuş-çizim süresi 3–7 ms'dir.

## Ekran ayarı

Çözünürlük, yenileme ve renk biçimi seçimi dört parçaya ayrılır. Hiçbiri kendi
kuralını hesaplamaz; kurallar tek yerdedir:

- **Kurallar — `mediabox_platform::output` / `video`.** Mainline Linux'un
  `drm_hdmi_compute_mode_clock`, `sink_supports_format_bpc`, `hdmi_clock_valid`
  ve `drm_edid.c` Y420VDB/Y420CMDB ayrıştırmasının birebir karşılığı. Çekirdeğin
  bağlayıcıya verdiği mod listesi ve EDID'den bir `OutputOffer` üretir: her mod
  bir kez, boyuta göre gruplu (TV boyutları önce, VESA/DMT modları
  "Bilgisayar modları" altında); her modda RGB, 4:4:4, 4:2:2, 4:2:0'ın kartın
  her derinliğindeki hücresi, gereken hız ve reddediliyorsa `Refusal` gerekçesi
  (`Only420`, `No420Here`, `DepthNotDeclared`, `OverSink`, `EightBitOnly`, …).
  Kaynak sınırları çalışan vendor sürücünün sunduğudur: 600 MHz, 10 bit.
- **Donanım durumu — `mediabox-display-observer`.** Seçili çıkışı sysfs,
  uevent, debugfs ve device tree'den izler; tek bir snapshot yayımlar
  (`/run/mediabox-display-observer/snapshot.json`): generation, EDID SHA-256,
  topoloji ve ses/CEC yönü, çekirdeğin mod listesi ve ondan hesaplanan teklif,
  sürücünün bildirdiği hat. Daemon donanım gerçeğini yalnız buradan alır.
  Kurallar: [`display-pipeline.md`](display-pipeline.md) § 14.
- **Uygulama — `mediabox-tv`.** Ekranı tutan süreç. Teklifi gözlemciyle aynı
  fonksiyon ve aynı girdilerle hesaplar, daemon'dan gelen ayarı süreç yeniden
  başlamadan uygular: `TEST_ONLY` ile sorulmuş tek bir atomik commit (mod +
  kare + `color_format`/`color_depth`/`Colorspace`); boyut değişirse yeni
  GBM/EGL yüzeyi. Ne uyguladığını (`OwnerReport::Applied`: bağlayıcı, EDID
  SHA-256, timing key, renk, HDR, deneme kimliği) ya da uygulayamadığını
  yalnız yerel sokete bildirir. Daemon'un durumunu her okuyuşunda, orada
  geçerli olan ayar (deneme ya da kayıtlı) hatta değilse onu uygular.
- **Karar ve saat — `mediaboxd-rs` (`src/output.rs`).** Tek kayıt,
  `/var/lib/mediabox/output.json` (şema 2): `{edid_sha256, resolution,
  colours}`. Ekran kimliği doğrulanmış tam EDID'in SHA-256'sıdır; mod ve mod
  başına renk timing key ile tutulur. Referans Android kutusunun modeli
  (`hdmimode`, `<mod>_deepcolor`, `hdmichecksum`, "tv sink changed"): farklı
  EDID'li bir ekranda kayıt o ekrana `Auto` olarak taşınır. Eski kayıt
  (checksum, boyut + mHz, etiket) o checksum'lı ekran ilk görüldüğünde bir kez
  taşınır; birden fazla timing'e uyan seçim tahmin edilmez, `Auto` olur.

Yeni bir ayar **denemedir**: `output_try` bir deneme kimliği üretir, denemeyi
`/var/lib/mediabox/output-trial.json`'a yazar, sonra olay akışıyla sahibine
uygulatır ve 15 saniye sayar. `output_keep {trial}` yalnız o kimlik, aynı ekran
ve sahibin uyguladığını bildirmiş olması halinde diske yazar; yazma başarısızsa
hiçbir şey kaydedilmez. `output_revert {trial}`, süre dolması, sahibin
uygulayamaması, yeniden başlaması ya da ekranı devretmesi ve ekranın değişmesi
denemeyi bitirir ve önceki ayarı geri uygulatır. Yeniden başlayan daemon
günlüğü bulur ve denemeyi geri alır. Deneme yalnız arayüz ekranı tutarken
kabul edilir.

Komutlar: `output_status`, `output_try {resolution, colour}`,
`output_keep {trial}`, `output_revert {trial}`. Donanımı anlatan bir komut
yoktur; sahibin raporu `Request` değil, ayrı bir tiptir (`OwnerReport`) ve
yalnız yerel sokette, root'tan kabul edilir. `Status` içinde `output` alanı
aynı durumu taşır ve istenen (`setting`, `trial`), politikanın seçtiği
(`selected`), DRM'e uygulanan (`applied`) ve sürücünün bildirdiği (`observed`,
okunamıyorsa `unknown`) değerleri ayrı tutar. Olay akışında (`/v1/events`)
`{"output": "apply" | "kept" | "reverted" | "changed", …}` çerçeveleri bütün
istemcilere gider; `apply` hangi bağlayıcı ve EDID için olduğunu taşır.

Daemon gözlemcinin geçerli generation'ı için `/run/mediabox/output-plan` yazar
(şema, boot, generation, bağlayıcı, verici, EDID SHA-256, timing key, kaynak
profili) ve generation değişince kaldırır ya da yeniden yazar.
`mediabox-hdmi-prepare` Kodi'nin başlangıç modunu ve yenileme listesini, tarayıcı
birimi sway'in modunu yalnız `mediabox-platform plan`/`sway-output` üzerinden,
plan şu anki ekran içinse alır. Kodi (`patches/kodi/0013`) her mod için SDR ve
HDR renk biçimini aynı dosyadaki
`colour <G>x<Y>[i]@<saat kHz>/<htoplam>x<vtoplam> <sdr> <hdr|none>` satırlarından
okur; planı her kararda kendisi doğrular (boot, gözlemcinin generation'ı ve
sink'i, sürdüğü bağlayıcı ve o anki EDID'in SHA-256'sı, iki kez okunarak) ve
tutmayan planı kullanmaz. Ayrıntı ve ölçümler: [`display-pipeline.md`](display-pipeline.md) § 9–11, 14.

## Input routing

`Ui` modunda tüm aksiyonlar yalnız event bus'a yayınlanır. `KodiPlayback`
modunda Play/Pause/Stop/Seek/Volume aksiyonları Kodi JSON-RPC'ye yönlenebilir;
ok/OK/Back/Home event bus'ta kalır. `/dev/input/event*` aygıtları sysfs key
capability bitmap'lerinden keyboard/consumer/remote olarak sınıflanır. Daemon
`EVIOCGRAB` çağırmaz ve fiziksel aygıtları exclusive açmaz; mevcut Kodi klavye
yolu korunur.

## Derleme ve doğrulama

```sh
cd rust
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Side-by-side doğrulama için production socket yerine `/tmp` kullanın:

```sh
mediaboxd-rs --socket /tmp/mediaboxd-rs.sock --cec-device "$(mediabox-platform cec-device)"
mediaboxctl --socket /tmp/mediaboxd-rs.sock status
```

`packaging/systemd/mediaboxd-rs.service` üretim birimidir ve
`scripts/deploy-mediabox-v3.sh` tarafından kurulup enable edilir. Eski Python
birimi `packaging/systemd/mediaboxd.service` ile başlattığı modül artık yok.

Üretimde `--cec-device` verilmez: adaptör, ekranın seçildiği çıkışın topolojisinden
çözülür (bkz. [`docs/platform/runtime-discovery.md`](platform/runtime-discovery.md)).
Yukarıdaki elle çalıştırma biçimi yalnızca teşhis içindir.
