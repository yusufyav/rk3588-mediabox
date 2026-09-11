# M1A — mediaboxd control plane uygulaması

## Sonuç

`MEDIABOXD_M1A_PASS`

İlk gerçek MediaBox backend'i host ve Orange Pi 5 Ultra target üzerinde çalıştı.
Browser → `mediaboxd` → Kodi/system sınırı kuruldu; browser'ın Kodi JSON-RPC ile
doğrudan konuşmasına gerek kalmadı. Kalıcı target install, service enable, push,
merge, kernel/DT, CEC, Bluetooth, Wi-Fi write veya Stremio değişikliği yapılmadı.

| Alan | Sonuç |
| --- | --- |
| Baseline | `HEAD = origin/main = d02bd6f31b305daa10decb4eb4d2adf87879fdf6`, ana tree clean |
| Branch | `agent/codex-m1a-mediaboxd` |
| Worktree | `/tmp/rk3588-mediabox-codex-m1a` |
| Target | `root@10.27.27.25`, `orangepi5-ultra`, AArch64 |
| Backend | Python 3 stdlib; sıfır runtime dependency |
| Event transport | Gerçek SSE `/api/v1/events` |
| Host tests | **16/16 PASS** |
| Target smoke | **PASS** |
| Final target | staged daemon stopped; production Kodi running, idle, DRM master |

## Teknoloji kararı

Python 3 stdlib seçildi. Target'ta `/usr/bin/python3 3.13.5` zaten vardı; HTTP,
JSON, TOML (`tomllib`), procfs/sysfs telemetrisi ve SSE için ek package gerekmedi.
Bu seçim mediaboxd ile sonraki Stremio Node runtime'ını birbirine bağlamaz ve
deployment footprint'ini yalnız repo kaynaklarına indirir.

## Mimari

```text
Web UI / reverse proxy (gelecek milestone)
             |
             | versioned same-origin HTTP + SSE
             v
        mediaboxd :8787 (default 127.0.0.1)
          |          |              |
          |          |              +-- procfs/sysfs/debugfs telemetry
          |          +-- fixed-argv Kodi/system lifecycle
          +-- discovered loopback Kodi HTTP JSON-RPC
                         |
                         v
              accepted Kodi GBM/RKMPP/DRM PRIME stack
```

Kod bölümleri:

- `mediaboxd/api.py`: route, JSON envelope, body limit, SSE;
- `mediaboxd/kodi.py`: endpoint resolver, reusable JSON-RPC client, playback;
- `mediaboxd/lifecycle.py`: Kodi ve gated host lifecycle;
- `mediaboxd/telemetry.py`: bounded Linux system/network/display collectors;
- `mediaboxd/events.py`: fan-out queue ve Kodi state monitor;
- `mediaboxd/config.py`: typed TOML config ve fail-fast güvenlik doğrulaması.

Kodi portu hardcode edilmedi. Resolver önce çalışan Kodi process `HOME`'undan,
sonra allowlisted settings path'lerinden `guisettings.xml` içindeki webserver
port/TLS/auth değerlerini okur. Explicit `kodi.endpoint` override da vardır.

Lifecycle önceki accepted evidence'ten alındı: production owner bir Kodi unit
değil repo launcher sözleşmesidir. mediaboxd aynı `kodi-gbm` executable/argv,
working directory, `HOME`, Mali/RKMPP library path ve ALSA environment'ını shell
olmadan uygular. Stop-service, `Player.Stop`'tan ayrıdır: önce
`Application.Quit`, sonra exact PID + proc start-time doğrulamalı timeout cleanup.

## Endpoint listesi

| Method | Endpoint |
| --- | --- |
| GET | `/api/v1/health` |
| GET | `/api/v1/system` |
| GET | `/api/v1/network` |
| GET | `/api/v1/kodi` |
| GET | `/api/v1/display` |
| GET | `/api/v1/events` |
| POST | `/api/v1/kodi/playpause` |
| POST | `/api/v1/kodi/stop` |
| POST | `/api/v1/kodi/seek` |
| POST | `/api/v1/kodi/open` |
| POST | `/api/v1/kodi/start` |
| POST | `/api/v1/kodi/stop-service` |
| POST | `/api/v1/kodi/restart` |
| POST | `/api/v1/system/reboot` |
| POST | `/api/v1/system/shutdown` |

RPC/network failure sınıfları: `KODI_UNREACHABLE`, `KODI_RPC_ERROR`,
`INVALID_REQUEST`. `Player.Open` yalnız `http`, `https`, `file` kabul eder;
`magnet` reddedilir. Seek absolute saniyeyi Kodi 22'nin canlı introspection ile
doğrulanan `value.time` şemasına çevirir.

## Host doğrulaması

Komut:

```text
tests/run-mediaboxd-tests.sh
```

Sonuç: `Ran 16 tests ... OK`.

Kapsam:

- health ve basic system endpoint;
- mock Kodi RPC success ve gerekli status method zinciri;
- Kodi unavailable → `KODI_UNREACHABLE`;
- runtime XML endpoint/port discovery;
- invalid `magnet` URL;
- seek validation ve Kodi `value.time` serialization;
- procfs memory/load/uptime/temperature parser;
- network argv/JSON parser;
- vendor VOP2 display summary → colour/depth/HDR parser;
- loopback dışı bind için explicit opt-in;
- system actions default disabled;
- shell metacharacter içeren lifecycle argümanının command olarak çalışmaması;
- gerçek localhost HTTP ve SSE stream.

Ek host daemon smoke'unda health/system/network/kodi/display yanıtları ve SSE
`connected` olayı görüldü; daemon SIGINT ile temiz kapandı.

## Target smoke

Kaynaklar `/tmp/mediaboxd-m1a/` altına kopyalandı. Daemon enable edilmeyen,
`--collect` transient `mediaboxd-m1a-smoke.service` ile loopback
`127.0.0.1:8787` üzerinde çalıştırıldı. Target Python compile kontrolü geçti.

Read-only sonuçlar:

- health: `{"status":"ok"}`;
- system: `orangepi5-ultra`, accepted kernel, `aarch64`, RAM/root FS/load/uptime,
  SoC sıcaklığı `45.307 C`;
- network: `enP3p49s0 up`, IPv4 `10.27.27.25`, gateway `10.27.27.1`;
- Kodi: PID `99128`, JSON-RPC reachable, başlangıçta active player boş;
- display idle: `/dev/dri/card0`, `card0-HDMI-A-1`, `1920x1080p60`,
  `RGB888_1X24`, `BT.709`, Full, 8 bit/component, SDR EOTF tag 0;
- SSE: `connected`, `control.action` ve gerçek `kodi.state` değişim olayları.

Kısa functional playback smoke, mevcut güvenli lokal
`file:///var/tmp/mp1b/past-lives.mkv` ile yapıldı. HDR/OSD yeniden acceptance'a
sokulmadı; yalnız kontrol API'si doğrulandı:

1. `Player.Open` → `OK`, speed 1 ve current item doğru;
2. `playpause` → speed 0;
3. `playpause` → speed 1;
4. `seek {"seconds":30}` → Kodi time tam `00:00:30.000`, ardından ilerledi;
5. `Player.Stop` → `OK`, active player tekrar boş.

İlk seek denemesi Kodi'nin 502'ye çevrilen invalid-params cevabını gösterdi.
Canlı `JSONRPC.Introspect` yalnız `Player.Seek` için salt-okunur sorgulandı;
Kodi 22 v13.200.0 şeması `value: {time: ...}` istedi. Client düzeltildi, regresyon
testi eklendi ve target tekrarında PASS alındı. Hata sessizce yutulmadı.

Smoke sonunda transient unit durduruldu. `:8787` listener kalmadı. Production
Kodi aynı PID `99128` ile çalıştı, JSON-RPC `pong`, active players `[]`; debugfs
clients yalnız Kodi'yi `/dev/dri/card0 master=y` olarak gösterdi. Production
Kodi lifecycle endpoint'leri smoke sırasında çağrılmadı; kullanıcı görüntüsünü
gereksiz kesmemek için implementation host testleri ve accepted lifecycle
evidence ile sınandı.

## Güvenlik kararları

- Default bind loopback; non-loopback literal bind ayrıca `allow_lan=true`
  ister. M1A LAN opt-in yalnız trusted-LAN içindir; authentication sonraki UI
  milestone'ında eklenmelidir.
- CORS kapalı, wildcard yok, same-origin tasarım.
- Host reboot/shutdown default disabled; enable edilse de source CIDR allowlist.
- Shell kullanılmıyor; lifecycle, `ip`, `iwgetid`, `systemctl` explicit argv.
- Arbitrary filesystem browser/log/command endpoint'i yok.
- JSON body 64 KiB limitli; URL, seek ve resume doğrulamalı.
- Display telemetry yalnız mevcut accepted vendor debugfs summary ve connector
  sysfs kaynaklarını okur; DRM/KMS commit yapmaz.
- systemd unit explicit `/usr/bin/python3`, restart policy, network dependency,
  `NoNewPrivileges` ve makul filesystem/kernel hardening içerir. Kodi'ye
  `After=`/`Requires=` bağımlılığı yoktur.

## Teslimat ve sonraki noktalar

- Config: `config/mediaboxd.example.toml`
- Unit: `packaging/systemd/mediaboxd.service`
- Kullanım/operasyon: `docs/mediaboxd.md`
- Test runner: `tests/run-mediaboxd-tests.sh`

Sonraki milestone'lar M1A sınırlarının üzerine eklenebilir: same-origin Web UI
ve auth, Stremio casting intercept → mevcut `Player.Open`, Kodi push
notification transport, privilege-separated power/lifecycle broker. CEC,
Bluetooth/input ve Wi-Fi writes ayrı gate olarak kalır.

## MEDIABOXD_M1A_PASS
