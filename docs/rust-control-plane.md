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
