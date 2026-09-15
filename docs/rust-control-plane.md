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
mediaboxd-rs --socket /tmp/mediaboxd-rs.sock --cec-device /dev/cec0
mediaboxctl --socket /tmp/mediaboxd-rs.sock status
```

`packaging/systemd/mediaboxd-rs.service` üretim birimidir ve
`scripts/deploy-mediabox-v3.sh` tarafından kurulup enable edilir.
`packaging/systemd/mediaboxd.service` eski Python birimidir; başlattığı modül
artık repoda yok, dosyanın kendisi henüz silinmedi.
