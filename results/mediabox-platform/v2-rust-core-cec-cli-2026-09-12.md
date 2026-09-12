# MediaBox V2 — Rust control plane, CLI, CEC ve unified input

Tarih: 2026-09-12
Target: Orange Pi 5 Ultra / RK3588, `root@10.27.27.25`
Başlangıç: `f3dd8f3` (detached worktree `/tmp/mediabox-codex`)

## Sonuç

V2 system core, mevcut Python `mediaboxd` ve production Kodi servisleri
değiştirilmeden bağımsız `rust/` workspace olarak implemente edildi. Host kalite
kapıları ve AArch64 cross-build geçti. İki binary target `/tmp` alanında
side-by-side çalıştırıldı; Rust daemon `/dev/cec0` üzerinde Playback logical
address aldı, Kodi JSON-RPC'yi kullandı, gerçek TV kumandası olaylarını normalize
etti ve outbound CEC komutlarını başarıyla iletti.

Accepted Kodi/RKMPP/DRM PRIME/HDR/10-bit/SDR2HDR/ses/DRM lifecycle baseline'ı
yeniden test edilmedi veya değiştirilmedi. Media/Stremio/transcode path'lerine
dokunulmadı.

## Rust mimarisi

```text
CEC /dev/cec0 ─┐
USB/BT sysfs ──┼─> mediabox-input ─> InputAction broadcast/routing
HTTP/API ──────┘                         │
                                        v
mediaboxctl ─Unix socket─> mediaboxd-rs ─> Kodi JSON-RPC
                              │
                              ├─> Linux CEC UAPI
                              └─> systemctl kodi.service (fixed argv)
```

- `mediabox-core`: `SystemStatus`, `KodiStatus`, `PlaybackState`,
  `InputAction`, `CecStatus`, `CecDevice`, `CecEvent`, `ServiceHealth` ve kapalı
  request/response enum'u.
- `mediaboxd-rs`: `/run/mediabox/mediaboxd.sock` Unix control, opsiyonel
  loopback-only HTTP, Kodi client/lifecycle, CEC manager, diagnostics ve input
  broadcast.
- `mediaboxctl`: iş mantığı taşımayan Unix socket CLI istemcisi.
- `mediabox-cec`: doğrudan Linux kernel CEC UAPI.
- `mediabox-input`: grab yapmayan device discovery, kaynak sınıflandırma,
  normalized event bus ve `Ui`/`KodiPlayback` routing.

Wire protocol enum ile fail-closed çalışır. Shell, argv veya caller-controlled
system command endpoint'i yoktur. Kodi restart yalnız doğrulanmış unit adı ve
`/usr/bin/systemctl restart <unit>` sabit argv'siyle yapılır.

## CLI

İmplemente edilen komutlar:

```text
mediaboxctl status
mediaboxctl system
mediaboxctl kodi status|play-pause|stop|seek|open|restart
mediaboxctl cec status|devices|active-source|wake-tv|standby-tv
mediaboxctl input monitor
```

`--json` newline-delimited machine-readable çıktı verir. Daemon olmayan socket
ayrı testte açık hata ve exit code `2` üretti. Host gerçek daemon kabulünde Unix
`system/status/cec status`, loopback `POST /v1/control` ve streaming
`input monitor` + API injection çalıştı.

## CEC implementasyonu

Target'ta:

```text
adapter          /dev/cec0 (dw_hdmi_qp)
driver           dwhdmi-rockchip
capabilities     logical-addresses, transmit, passthrough, remote-control
physical address 3.0.0.0
logical address  4 (Playback Device)
OSD name         MediaBox
mode             INITIATOR | EXCL_FOLLOWER
RC flag          ALLOW_RC_PASSTHRU
```

Fiziksel adres yalnız `G_PHYS_ADDR` ile okundu; EDID/driver-owned adres
yazılmadı. Başlangıçta wake/standby/active-source gönderilmedi. Drop/SIGINT
sırasında boş `S_LOG_ADDRS` ile logical address bırakılır.

Topology taraması 0..14 adreslerine poll yapar. Düzeltilen eşzamanlı receive/TX
yolu sonrası target ölçümü yaklaşık 7 ms sürdü. TV `logical=0`,
`physical=0.0.0.0`, `device_type=tv` olarak gözlendi.

## TV kumandası kabulü

Gerçek TV kumandasından aşağıdaki raw CEC `User Control Pressed/Released`
mesajları Rust receive loop'a ulaşıp `InputAction` eventlerine dönüştü:

| Aksiyon | Press | Release | Kaynak |
| --- | ---: | ---: | --- |
| Up | 5667984929044 | 5668195187693 | CEC |
| Down | 5667290847011 | 5667445997975 | CEC |
| Left | 5702806219497 | 5702962952824 | CEC |
| Right | 5668654485337 | 5668850532297 | CEC |
| Ok | 5911207684559 | 5911364386595 | CEC |
| Back | 5904838924396 | 5905035146688 | CEC |

`/dev/input/event0` ayrıca `remote_control`, kaynak `cec`, `grabbed=false`
olarak sınıflandırıldı. Kullanıcı yön, OK ve Back tuşlarının production Kodi
üzerinde çalıştığını doğruladı. Daemon fiziksel input aygıtı açmadı/grab etmedi.

## MediaBox → TV

- `cec active-source`: başarılı. Son TX `initiator=4`, `destination=15`,
  `opcode=0x82`, operands `30:00`; error counters sıfır.
- Kontrollü final güç testi: `standby-tv`, üç saniye sonra `wake-tv`, iki saniye
  sonra `active-source`; üç komut da `transmitted=true` döndürdü.
- Restore sonrasında TV `Routing Change 0.0.0.0 -> 3.0.0.0` mesajı yayınladı.

## Kodi entegrasyonu

Reusable client `JSONRPC.Ping`, `Player.GetActivePlayers`,
`Player.GetProperties`, `Player.GetItem`, `Player.Open`, `Player.PlayPause`,
`Player.Stop` ve `Player.Seek` uygular. Timeout, transport, RPC, invalid response
ve invalid request hataları ayrıdır. Relative seek mevcut zamanı okuyup güvenli
mutlak Kodi zamanına çevirir.

Target kabulü:

```text
running=true
jsonrpc_reachable=true
state=idle
```

Daemon Kodi child process spawn etmez; lifecycle yalnız canonical
`kodi.service` unit'ini kullanır. Kabul sırasında Kodi restart/stop/playback
komutu çalıştırılmadı.

## Host kalite kapıları

- `cargo fmt --all -- --check`: PASS
- `cargo clippy --workspace --all-targets --offline -- -D warnings`: PASS
- `cargo test --workspace --offline`: PASS, 17 test
- `aarch64-unknown-linux-gnu` release cross-build: PASS

Test kapsamı: tüm `InputAction` serde round-trip, Kodi mock HTTP JSON-RPC,
CEC key mapping/parser/invalid message/transmit bytes/UAPI layout, Unix socket,
CLI JSON/unavailable daemon, input routing/broadcast, lifecycle unit validation
ve arbitrary command rejection.

## Target testleri ve restore

Binary'ler yalnız `/tmp/mediaboxd-rs` ve `/tmp/mediaboxctl` olarak stage edildi;
target'a paket kurulmadı. Son AArch64 SHA-256 değerleri daemon için
`232738b4f46653cae7f0431251d681ce82abfcc3cc92c6c597192c1a6d9241e4`, CLI
için `badd8014ec7497136996834eab4768c6ba7aa0b9365080c1cb47126cff49441f` idi.
Production `mediaboxd.service` ve `kodi.service` acceptance öncesi ve sonrası
aktif kaldı; Python unit disable/stop edilmedi. Rust daemon farklı
`/tmp/mediaboxd-rs.sock` kullandı.

Final cleanup'ta staging Rust daemon'ın executable kimliği doğrulandı ve SIGINT
ile kapatıldı. `cec-ctl`, `Logical Address Mask = 0x0000` gösterdi; `/dev/cec0`
owner kalmadı. Production Kodi JSON-RPC tekrar `pong` verdi. İki binary, socket,
PID ve log olmak üzere yalnız staging `/tmp` dosyaları kaldırıldı. Target normal
production durumuna döndü.

## Production migration planı

1. Rust binary'lerini versioned `/opt/rk3588-mediabox/bin/` dizinine deploy et.
2. `mediaboxd-rs.service` dosyasını yükle fakat enable etme.
3. `/tmp` socket ile smoke/CEC testini tekrarla.
4. Kısa bakım penceresinde Python unit'i durdur; Rust unit'i başlat ve canonical
   `/run/mediabox/mediaboxd.sock` CLI kabulünü çalıştır.
5. CEC owner, Kodi input ve API consumer'larını doğruladıktan sonra Rust unit'i
   enable et. Python unit'i rollback için kurulu fakat disabled tut.
6. Sorunda Rust unit'i durdurup Python unit'i yeniden başlat; Kodi unit'ine ve
   medya baseline'ına dokunma.

## Bilinen sınırlamalar

- HTTP API varsayılan kapalı ve yalnız loopback'tir; LAN auth/TLS bu teslimatın
  kapsamında değildir.
- Fiziksel evdev aygıtları bilinçli olarak yalnız enumerate edilir; Faz 1'de
  read/grab/reroute edilmez. Bu, production Kodi input invariant'ıdır.
- Bluetooth pairing/userspace kurulumu kapsam dışıdır.
- HDMI hot-unplug sonrasında physical/logical address otomatik reacquire ayrı
  bir dayanıklılık adımıdır; stabil bağlantıdaki gerçek CEC yolu kabul edilmiştir.
- Systemd unit teslim edildi fakat bu side-by-side gate'te kurulmadı/enabled
  edilmedi.
