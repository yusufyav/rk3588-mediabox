# mediaboxd

`mediaboxd`, MediaBox tarayıcı yüzeyi ile kabul edilmiş Kodi medya motoru
arasındaki kontrol sınırıdır. M1A uygulaması Python 3.11+ standard library ile
çalışır; Node, framework, veritabanı veya üçüncü taraf Python paketi kullanmaz.

```text
browser
          |
          | versioned HTTP + same-origin SSE
          v
      mediaboxd
       |  |  |  |
       |  |  |  +-- Stremio streaming server reverse proxy (loopback upstream)
       |  |  +----- allowlist tabanlı Linux sysfs/procfs telemetrisi
       |  +-------- sabit argv ile Kodi/host lifecycle
       +----------- keşfedilmiş loopback Kodi HTTP JSON-RPC
```

Tarayıcı Kodi'nin LAN'a açık, auth'suz JSON-RPC portuna doğrudan bağlanmaz.
`mediaboxd`, production Web UI bundle'ını `/ui/` altında ve API'yi aynı origin'de
`/api/v1/` altında sunar. Varsayılan bind yalnız `127.0.0.1:8787`'dir; güvenilir
LAN kurulumu `allow_lan = true` ile açıkça etkinleştirilir.

## Çalıştırma

Host veya geçici target staging'i:

```sh
PYTHONPATH=/opt/rk3588-mediabox \
  /usr/bin/python3 -m mediaboxd --config /etc/mediaboxd.toml
```

Repo içinden geliştirme çalıştırması:

```sh
python3 -m mediaboxd --config config/mediaboxd.example.toml
```

Örnek config `config/mediaboxd.example.toml` dosyasındadır. Config verilmezse de
güvenli built-in varsayılanlar kullanılır: loopback bind, kapalı system actions,
kapalı CORS.

Production unit `packaging/systemd/mediaboxd.service` dosyasındadır. Unit şu
yerleşimi bekler:

- Python paketi: `/opt/rk3588-mediabox/mediaboxd`
- Config: `/etc/mediaboxd.toml`
- Web UI: `/opt/rk3588-mediabox/webui/dist`
- Python: `/usr/bin/python3`

Unit bu milestone sırasında target'a kurulmadı veya enable edilmedi.

## Kodi endpoint keşfi

`kodi.endpoint` doluysa bu explicit override kullanılır. Boşsa resolver sırasıyla:

1. çalışan exact-name `kodi-gbm`/`kodi.bin` sürecinin `/proc/<pid>/environ`
   içindeki `HOME` değerinden `.kodi/userdata/guisettings.xml` yolunu;
2. config'teki sabit `kodi.settings_paths` listesini

okur. `services.webserverport`, TLS ve varsa Basic Auth alanlarından loopback
JSON-RPC URL'i üretilir. Dolayısıyla port `8080` daemon kodunda hardcode değildir.
Her JSON-RPC çağrısının config ile sınırlı timeout'u vardır.

## API

Başarılı mutation yanıtı şu envelope'u kullanır:

```json
{"status":"ok","result":{}}
```

Hata yanıtı örneği:

```json
{"error":{"code":"KODI_UNREACHABLE","message":"Kodi JSON-RPC endpoint is unreachable"}}
```

Zorunlu Kodi sınıfları `KODI_UNREACHABLE`, `KODI_RPC_ERROR` ve
`INVALID_REQUEST` olarak korunur.

| Method | Path | Davranış |
| --- | --- | --- |
| GET | `/api/v1/health` | Daemon liveness, sürüm ve action capability map |
| GET | `/api/v1/system` | Hostname, kernel, arch, uptime/load, RAM, root FS, CPU sıcaklığı |
| GET | `/api/v1/network` | Arayüzler, IPv4, link, default route, bulunursa Wi-Fi SSID |
| GET | `/api/v1/kodi` | Process/PID, RPC erişimi, player/item/speed/time/total |
| GET | `/api/v1/display` | DRM card, HDMI connector, mod/refresh, vendor colour/depth/HDR özeti |
| GET | `/api/v1/events` | Server-Sent Events stream |
| POST | `/api/v1/kodi/playpause` | Aktif player'ı toggle eder |
| POST | `/api/v1/kodi/stop` | Yalnız aktif `Player.Stop` |
| POST | `/api/v1/kodi/seek` | `{"seconds":30}` ile absolute time seek |
| POST | `/api/v1/kodi/open` | Güvenli URL ile `Player.Open` |
| POST | `/api/v1/kodi/start` | Kodi process lifecycle start |
| POST | `/api/v1/kodi/stop-service` | Kodi process lifecycle stop |
| POST | `/api/v1/kodi/restart` | Stop + JSON-RPC-ready start |
| POST | `/api/v1/system/reboot` | Policy izin verirse systemd reboot |
| POST | `/api/v1/system/shutdown` | Policy izin verirse systemd poweroff |
| GET | `/api/v1/stremio` | Akış sunucusu erişilebilirliği, sürümü, cast cihazı |
| GET | `/api/v1/cast` | Devredilen aktif kaynak oturumu |
| POST | `/api/v1/cast/kodi` | Shell'den pozisyonlu Kodi devri |

## Stremio sınırı

Tarayıcı ne Stremio akış sunucusuna ne de Kodi'ye doğrudan bağlanır. Stremio web
uygulamasının ihtiyaç duyduğu her şey tek origin üzerinde tek bir mount altında
sunulur (varsayılan `/server/`), ve bu mount altında yalnız iki yol upstream'e
iletilmeden mediaboxd tarafından cevaplanır:

| Yol | Davranış |
| --- | --- |
| `GET {mount}casting` | Upstream cihaz listesi + MediaBox cihazı başa eklenir |
| `POST {mount}casting/{device}/player` | MediaBox cihazı ise Kodi'ye devredilir |
| diğer her şey | Loopback upstream'e olduğu gibi iletilir |

MediaBox cihazı Stremio'ya `external` tipiyle sunulur. Bu bilinçli bir seçimdir:
upstream stremio-web `chromecast` ve `tv` tiplerini yalnız kendi masaüstü
shell'inde gösterirken, `external` tipini sade tarayıcıda da gösterir.

### Kaynak kimliği ve medya yolu

Devirde Kodi'ye **aynı path ve query**, farklı origin ile verilir. Tarayıcı
cihazın loopback'ine erişemediği için Stremio stream URL'lerini proxy mount'una
göre çözer; Kodi ise cihazın üzerinde çalıştığı için doğrudan loopback akış
sunucusu adresini alır. Sonuç: production oynatmada tek bir medya baytı bile
Python proxy'sinden geçmez, ön izleme ise proxy'yi kullanmaya devam eder.

`time` alanı upstream sözleşmesi gereği milisaniyedir ve Kodi'nin
`Player.Open` `options.resume` alanına saniyeye çevrilerek verilir.

Devir sırasında `Player.Open`'dan **önce** `cast.handoff` olayı yayımlanır;
shell bunu görünce ön izlemeyi durdurur. Tek bir torrent engine'inin iki ayrı
seek pozisyonunu beslemesi hem ön izlemeyi hem Kodi'yi buffer'a sokar.

### Proxy sınırları

- `upstream` yalnız loopback olabilir ve config'ten gelir; istekten alınmaz.
  Load-time'da doğrulanır, dolayısıyla mount asla open proxy'ye dönüşemez.
- `server.js`'in kendi `/proxy` (herhangi bir URL'i getiren) endpoint'i
  iletilmez, `403 PROXY_DENIED` döner.
- Yalnız `GET`, `HEAD`, `POST` kabul edilir.
- `Range` iletilir (seek), yanıt parça parça aktarılır; gövde belleğe alınmaz.
- `server.js`'in ürettiği `Access-Control-Allow-*` başlıkları **iletilmez**.

`open` örneği:

```json
{"url":"https://media.example/movie.mkv","resume_seconds":90.5}
```

Yalnız `http`, `https` ve absolute local `file` URL'leri kabul edilir. `magnet`
ve diğer şemalar reddedilir. `seek.seconds` finite, sıfır veya pozitif ve en
fazla yedi gündür; Kodi'ye `value.time` şemasıyla gönderildiği için absolute
pozisyondur.

SSE stream ilk olarak `connected`, ardından durum değişince `kodi`, başarılı
mutation sonrasında `control.action` üretir. Her `data:` satırı frontend ile ortak
`{"type":"…","payload":{…}}` envelope'unu taşır. On beş saniyelik yorum heartbeat'i
idle bağlantıyı canlı tutar. M1A'da ek WebSocket dependency yerine SSE seçildi.

## Kodi lifecycle

S0/S1 target kanıtındaki production sözleşmesi kullanılır:

- executable `/opt/rk3588-mediabox/kodi/lib/kodi/kodi-gbm`;
- argv `--standalone --debug`;
- working directory ve `HOME` `/var/tmp/kodi-home`;
- `AE_SINK=ALSA`, `MEDIABOX_GPU=mali` ve kabul edilmiş iki runtime library path'i;
- graceful stop için önce `Application.Quit`, timeout sonrası yalnız aynı
  `/proc` start-time kimliğine sahip exact Kodi PID'ine `SIGTERM`/`SIGKILL`.

Hiçbir lifecycle veya system action `shell=True` kullanmaz. Daemon Kodi config,
ALSA config veya medya dosyası provision etmez; mevcut kabul edilmiş production
runtime'ı tüketir. `start` ancak process göründükten ve JSON-RPC `Ping` başarılı
olduktan sonra başarılı döner.

## Güvenlik politikası

- `bind_address` literal IP olmalıdır. Loopback dışı değer ancak
  `allow_lan = true` ile başlar; bu opt-in yalnız güvenilir LAN deployment'ı
  içindir ve M1A authentication eklemez.
- CORS header üretilmez; wildcard CORS yoktur. `OPTIONS` reddedilir.
- System actions default kapalıdır. Açıldığında da çağıran adres
  `system_actions_allow_cidrs` listesine uymalıdır.
- Request body 64 KiB ile sınırlıdır ve mutation body'leri JSON object olmalıdır.
- Static dosyalar yalnız configured `webui_root` altından sunulur; traversal ve
  symlink escape reddedilir, SPA fallback hiçbir zaman `/api` isteklerine uygulanmaz.
- Medya yüzeyinin CSP'si ayrı ve açıkça tanımlıdır (`MEDIA_APP_CSP`). Stremio
  tasarımı gereği üçüncü taraf addon istemcisidir: katalog metadata'sı, posterler
  ve stream tanımları ancak çalışma anında bilinen hostlardan gelir. Bu yüzden
  `img-src`/`connect-src`/`media-src` https kaynaklarına açıktır; ama neyin
  **çalıştığına** karar veren kısımlar kapalı kalır — script ve worker yalnız
  same-origin (artı stremio-core'un ihtiyacı olan wasm), `unsafe-eval` yok,
  inline script yok, `object-src 'none'`, `frame-ancestors 'none'`. Bu CORS
  değildir: mediaboxd hâlâ hiçbir `Access-Control-Allow-*` başlığı üretmez ve
  `OPTIONS` reddedilir.
- Telemetri yalnız önceden tanımlı procfs/sysfs/debugfs yollarını okur. Filesystem
  browse, komut veya path parametresi sunan endpoint yoktur.
- URL şeması ve seek/resume değerleri RPC'den önce doğrulanır.
- systemd unit `NoNewPrivileges`, filesystem/kernel/control-group korumaları,
  dar address-family listesi ve restart policy uygular. Kodi endpoint keşfi ve
  lifecycle için `/var/tmp/kodi-home` iki süreç arasında bilinçli olarak paylaşılır;
  unit genel `PrivateTmp=true` izolasyonunu koruyup yalnız bu exact yolu `BindPaths`
  ile görünür kılar.

## Test

```sh
tests/run-mediaboxd-tests.sh
```

Suite gerçek localhost HTTP server'larıyla API/SSE ve mock Kodi JSON-RPC
entegrasyonunu; ayrıca config, telemetry parser, endpoint discovery, unavailable
Kodi, URL/seek doğrulaması ve command-injection regresyonunu kapsar.

## Sonraki entegrasyon noktaları

- Authentication/session policy
- Kodi TCP/WebSocket notification ingest; polling yerine push
- Privilege-separated lifecycle/power broker
- HDMI-CEC, Bluetooth/input ve Wi-Fi write işlemleri için ayrı milestone'lar
