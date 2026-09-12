# MediaBox V2 Core Integration — Final Kapanış Raporu

Tarih: 2026-09-12
Kapsam: Codex + Claude V2 entegrasyonunun doğrulanması, raporlanması ve güvenli kapatılması.
Bu rapor **yeni implementasyon içermez**; mevcut başarılı entegrasyonun kanıta dayalı kapanışıdır.

---

## 1. Başlangıç revision

Bu kapanış oturumunun devraldığı durum:

| Alan | Değer |
|---|---|
| Branch | `main` |
| Local HEAD | `28aefd467266b66109a268023964ca3149f76a3f` (`28aefd4`) |
| `origin/main` | `f3dd8f317438b41e49183a7ea497eb905be412cd` (`f3dd8f3`) |
| Working tree | clean (`git status --short` boş) |
| Fark | main, origin/main'in 16 commit önünde |

Beklenen durumla birebir eşleşti; hiçbir reset/rollback uygulanmadı.

## 2. Entegrasyon öncesi main

`f3dd8f3` — `docs: report the M2 CPU and Kodi input regression fixes`

Bu commit, M2 fazının (CPU + Kodi input regression fix) son noktasıdır ve entegrasyonun temel aldığı bilinen-iyi durumdur.

## 3. Final local HEAD

`28aefd4` — `fix: make Rust the production HTTP authority`

(Bu raporun commit'i eklendikten sonraki final HEAD Bölüm 24'te.)

## 4. Codex commit listesi

Author e-postası `yusufyav@hotmail.com` olan, Codex ajanı tarafından üretilen commitler:

| SHA | Konu |
|---|---|
| `9d51692` | feat: add Rust MediaBox control plane and CEC |
| `b7c4653` | docs: report Rust core and CEC acceptance |
| `dc0918b` | chore: normalize Rust manifests |
| `427d0f2` | feat: integrate media worker into Rust control plane |
| `8378123` | feat: route media session lifecycle through Rust |
| `86ac97b` | fix: handle systemd shutdown cleanly |
| `28aefd4` | fix: make Rust the production HTTP authority |

Toplam: 7 commit.

## 5. Claude commit listesi

Author e-postası `yusufyav@gmail.com` olan, Claude ajanı tarafından üretilen commitler:

| SHA | Konu |
|---|---|
| `cfc9b7f` | feat: add a headless Stremio adapter |
| `926e961` | feat: add the media inspector |
| `e991e0a` | feat: add the media capability policy |
| `f3d8079` | feat: add audio-to-AC3 media sessions |
| `80e61af` | feat: expose the media core over HTTP and from a terminal |
| `a5d2ed2` | feat(mediaboxd): mount the media core at /media/ |
| `c37a4a2` | test: cover the media core |
| `121a030` | docs: document the MediaBox V2 media core |
| `cc9f0b8` | docs: report the V2 media core |

Toplam: 9 commit.

## 6. Entegrasyon commitleri

`f3dd8f3..28aefd4` aralığı = 16 commit, kronolojik sıra:

```
9d51692 feat: add Rust MediaBox control plane and CEC          [Codex]
b7c4653 docs: report Rust core and CEC acceptance              [Codex]
dc0918b chore: normalize Rust manifests                        [Codex]
cfc9b7f feat: add a headless Stremio adapter                   [Claude]
926e961 feat: add the media inspector                          [Claude]
e991e0a feat: add the media capability policy                  [Claude]
f3d8079 feat: add audio-to-AC3 media sessions                  [Claude]
80e61af feat: expose the media core over HTTP and from a terminal [Claude]
a5d2ed2 feat(mediaboxd): mount the media core at /media/       [Claude]
c37a4a2 test: cover the media core                             [Claude]
121a030 docs: document the MediaBox V2 media core              [Claude]
cc9f0b8 docs: report the V2 media core                         [Claude]
427d0f2 feat: integrate media worker into Rust control plane   [Codex]
8378123 feat: route media session lifecycle through Rust       [Codex]
86ac97b fix: handle systemd shutdown cleanly                   [Codex]
28aefd4 fix: make Rust the production HTTP authority           [Codex]
```

Diff hacmi: **75 dosya, 14 879 ekleme, 4 silme**.

Doğrulanan bileşen kapsamı (diff'ten okunarak teyit edildi, PR/rapor iddiasından değil):

- Rust control plane: `rust/crates/mediaboxd-rs/` (daemon, kodi, lifecycle, media, main)
- Rust CEC: `rust/crates/mediabox-cec/src/lib.rs` (612 satır)
- Rust input: `rust/crates/mediabox-input/src/lib.rs` (253 satır)
- Rust core protokolü: `rust/crates/mediabox-core/src/lib.rs`
- CLI: `rust/crates/mediaboxctl/src/main.rs` (372 satır)
- Headless Stremio: `media/stremio/` (adapter, addons, api, models, server)
- Media inspector: `media/inspector/` (ffprobe, model, parse)
- Media policy: `media/policy/` (audio, video, capabilities, decide, preview, ranking, reasons)
- AC3 session pipeline: `media/proxy/` (ffmpeg, session, security)
- Production servisleri: `packaging/systemd/mediaboxd-rs.service`, `packaging/systemd/mediabox-media-worker.service`
- Testler: `media/tests/` + `tests/test_media_core_mount.py` + `tests/run-media-core-tests.sh`

Eksik bileşen tespit edilmedi; ek cherry-pick gerekmedi.

## 7. Final architecture

```
                 ┌─────────────────────────────────────────┐
                 │  mediaboxd-rs  (Rust, PID 28966)         │
                 │  HTTP  127.0.0.1:8787   ← tek otorite    │
                 │  UNIX  /run/mediabox/mediaboxd.sock      │
                 └───┬───────────┬───────────────┬──────────┘
                     │           │               │
           CEC       │     Kodi  │        Media  │ (HTTP proxy)
        /dev/cec0    │  JSON-RPC │      127.0.0.1:8790
                     │  :9090    │               │
                     ▼           ▼               ▼
              dw_hdmi_qp     kodi-gbm     mediabox-media-worker
              (3.0.0.0)      (PID 954)    (python3 -m media.server)
                                          ├─ stremio adapter → :11470
                                          ├─ inspector (ffprobe)
                                          ├─ policy (capabilities)
                                          └─ proxy/session (ffmpeg AC3)
```

Ana ilke: **Rust control plane tek dış otoritedir.** Python tarafı yalnızca
loopback'e bağlı (`127.0.0.1:8790`) bir media worker'a indirgenmiştir ve
doğrudan dışarıya servis vermez.

## 8. Rust control plane ownership

`mediaboxd-rs` sahiplendiği alanlar:

- **HTTP otoritesi**: `127.0.0.1:8787` (kanıt: `ss -lntp` → `users:(("mediaboxd-rs",pid=28966,fd=11))`)
- **UNIX kontrol soketi**: `/run/mediabox/mediaboxd.sock` (`srwxr-x--- root root`)
- **CEC**: `--require-cec` ile başlar; adapter/logical/physical address yönetimi
- **Kodi lifecycle**: sabit `systemctl` argv üzerinden `kodi.service` çağırır (unit yorumunda açıkça belirtilmiş)
- **Media session lifecycle**: `--media-endpoint http://127.0.0.1:8790` üzerinden worker'a proxy

Unit ExecStart (production'dan okundu):

```
/opt/rk3588-mediabox/bin/mediaboxd-rs --require-cec \
  --http 127.0.0.1:8787 --media-endpoint http://127.0.0.1:8790
```

Systemd hardening aktif: `NoNewPrivileges`, `ProtectSystem=strict`, `ProtectHome`,
`ProtectKernelTunables/Modules/Logs`, `ProtectControlGroups`, `ProtectClock`,
`RestrictSUIDSGID`, `LockPersonality`, `PrivateTmp`, `UMask=0027`,
`RuntimeDirectoryMode=0750`. Kodi'nin bağımsız direct-KMS servis olarak kalması
için `KillMode=process`.

## 9. Media worker ownership

`mediabox-media-worker` (PID 27911) ExecStart:

```
/usr/bin/python3 -m media.server --bind 127.0.0.1 --port 8790 \
  --streaming-server http://127.0.0.1:11470 \
  --state /var/lib/mediabox-media-worker/stremio-session.json \
  --base-url http://127.0.0.1:8790 --loopback-base-url http://127.0.0.1:8790
```

Sahiplendiği alanlar: Stremio adapter/addon çözümleme, ffprobe inspector,
capability policy, AC3 transcode session'ları. **Yalnızca loopback'e bind.**

## 10. Eski Python daemon durumu

| Unit | Active | Enabled |
|---|---|---|
| `mediaboxd.service` (eski Python control plane) | **inactive** | **disabled** |

Unit dosyası diskte korunmuştur (`/etc/systemd/system/mediaboxd.service`,
947 byte, 11 Eyl 21:22) — silinmedi, yalnızca devre dışı bırakıldı. Rollback
yolunun açık kalması için bilinçli tercih (bkz. Bölüm 22).

Eski daemon hiçbir port dinlemiyor (`ss -lntp` çıktısında yok). HTTP otoritesi
çakışması yok.

## 11. mediaboxctl

Kurulum yolu: `/opt/rk3588-mediabox/bin/mediaboxctl` (1 811 792 byte, 12 Eyl 16:16).

Doğrulanan komutlar ve gerçek çıktılar:

```
$ mediaboxctl status
Sistem: orangepi5-ultra (aarch64)
Çalışma süresi: 8564 saniye
Kodi: idle
CEC: hazır
Medya: hazır
Torrent ağı: TORRENT_NETWORK_BLOCKED

$ mediaboxctl kodi status
Kodi: idle (JSON-RPC: erişilebilir)

$ mediaboxctl cec status
CEC adaptörü: /dev/cec0 (dw_hdmi_qp)
Fiziksel adres: 3.0.0.0
Mantıksal adresler: [4]
```

### Bilinen kozmetik sorun (blocking değil)

`mediaboxctl media status` insan-okunur çıktısı yanlış şablon kullanıyor:

```
$ mediaboxctl media status
CEC adaptörü: yok
Fiziksel adres: yok
Mantıksal adresler: null
```

**Kök neden (kaynaktan doğrulandı):** `rust/crates/mediaboxctl/src/main.rs`
içindeki `print_human()`, yanıt tipini alan varlığına göre ayırt ediyor. CEC
dalının koşulu `result.get("available").is_some()`. Media status yanıtı da
`available` alanı taşıdığı için bu dala düşüyor ve CEC etiketleriyle basılıyor.

**Etki:** yalnızca terminal render. Veri katmanı doğru — JSON modunda tam ve
sağlıklı yanıt dönüyor:

```
$ mediaboxctl --json media status
{"ok":true,"result":{"available":true,
 "capabilityProfile":"rk3588_orangepi5_production",
 "provider":{"addonCount":7,"apiReachable":true,"authenticated":false,
   "streamingServer":{"reachable":true,"version":"4.21.0"}},
 "sessions":0,
 "torrentNetwork":{"directHttpAvailable":true,"status":"TORRENT_NETWORK_BLOCKED"}}}
```

Üst seviye `mediaboxctl status` de medyayı doğru raporluyor (`Medya: hazır`),
çünkü orası ayrı bir dalda ele alınıyor. Bu kapanış görevinin kapsamı gereği
**düzeltilmedi**; ayrı ve küçük bir takip işi olarak kayda alınmıştır.

## 12. Production CEC

| Alan | Değer |
|---|---|
| Adapter | `/dev/cec0` (`dw_hdmi_qp`) |
| Fiziksel adres | `3.0.0.0` |
| Mantıksal adresler | `[4]` (Playback Device 1) |
| `mediaboxctl status` özeti | `CEC: hazır` |
| Active Source | `{"transmitted": true}` |

Daemon `--require-cec` ile çalıştığı için CEC kaybı servisi fail ettirir —
sessiz degradasyon yok.

TV standby/wake döngüsü bu oturumda **tekrarlanmadı**; fiziksel remote
acceptance önceki ajan tarafından zaten doğrulanmıştı.

## 13. Kodi durumu

| Alan | Değer |
|---|---|
| `kodi.service` | active / enabled |
| Process | `kodi-gbm` PID 954 |
| JSON-RPC | `127.0.0.1:9090` + `[::1]:9090` — erişilebilir |
| Web | `0.0.0.0:8080` + `[::]:8080` |
| State | `idle` |

Kodi direct-KMS bağımsız servis olarak korunmuştur; Rust daemon ona yalnızca
sabit `systemctl` argv ve JSON-RPC üzerinden dokunur.

## 14. Physical input durumu

Regression riski: Rust daemon'ın USB klavyeyi `EVIOCGRAB` ile exclusive alıp
Kodi'nin fiziksel klavye girişini bozması.

**Sonuç: regression yok.** Process/device ownership seviyesinde kanıt:

`mediaboxd-rs` (PID 28966) açık dosya tanıtıcıları arasında **hiçbir
`/dev/input/event*` yok**:

```
$ ls -l /proc/28966/fd | grep -i input
  (çıktı boş)
```

Event device sahipliği (`fuser /dev/input/event*`):

```
/dev/input/event0  -> 782 systemd-logind, 954 kodi-gbm    (dw_hdmi_qp)
/dev/input/event3  -> 954 kodi-gbm                        (headset-keys)
/dev/input/event5  -> 782 systemd-logind, 954 kodi-gbm    (bt-powerkey)
/dev/input/event6  -> 782 systemd-logind, 954 kodi-gbm    (HP OMEN SPACER Keyboard)
/dev/input/event7  -> 782 systemd-logind, 954 kodi-gbm    (… Keypad)
/dev/input/event8  -> 782 systemd-logind, 954 kodi-gbm    (… Consumer Control)
/dev/input/event9  -> 954 kodi-gbm                        (… Mouse)
/dev/input/event11 -> 954 kodi-gbm                        (adc-keys)
/dev/input/event12 -> 782 systemd-logind, 954 kodi-gbm    (rk805 pwrkey)
```

USB klavye (HP OMEN SPACER, event6–event9) yalnızca `kodi-gbm` ve
`systemd-logind` tarafından tutuluyor. Rust tarafı hiçbir input cihazına
sahiplik iddia etmiyor. `mediabox-input` crate'i derlenmiş durumda ancak
production daemon'ında grab yapan bir yol aktif değil.

## 15. Headless Stremio

| Alan | Değer |
|---|---|
| Streaming server | `http://127.0.0.1:11470`, reachable, **v4.21.0** |
| Stremio API | reachable |
| Addon sayısı | 7 |
| Auth | oturum açılmamış — Stremio varsayılan addon koleksiyonu kullanımda |

Canlı katalog doğrulaması (gerçek Stremio, mock değil):

```
$ mediaboxctl --json media search "interstellar"
{"ok":true,"result":{"query":"interstellar","rows":[
  {"addonId":"com.linvo.cinemeta","addonName":"Cinemeta","catalogId":"top",
   "items":[
     {"id":"tt0816692","name":"Interstellar","type":"movie","releaseInfo":"2014", …},
     {"id":"tt5083736","name":"Interstellar Wars","type":"movie","releaseInfo":"2016", …}
   ]}, …]}}
```

Gerçek Cinemeta addon'undan gerçek IMDB ID'leriyle sonuç döndü.

## 16. Media inspector / policy

Canlı uçtan uca doğrulama (`https://download.samplelib.com/mp4/sample-5s.mp4`):

**Inspector** (`media inspect`) — `ok:true`, ffprobe çıktısı tam parse edildi:

```
container: mov,mp4,m4a,3gp,3g2,mj2 (QuickTime / MOV)
           bitRate=3 956 841  duration=5.758549s  size=2 848 208 B
video:     h264 (avc1)
audio:     aac, stereo, 2ch, 44 100 Hz, bitRate=127 998, lang=eng
subtitles: []
probeDurationSeconds: 7.0
```

**Policy** (`media policy`) — `ok:true`, aynı medya modelini üretip karar
katmanına besledi (probeDurationSeconds 6.894).

**Sessions** (`media sessions`) — `{"sessions":[]}`, sızan session yok.

Capability profili: `rk3588_orangepi5_production`.

## 17. AC3 audio pipeline

AC3 transcode pipeline'ı `media/proxy/` altında (ffmpeg + session + security)
implement edilmiştir ve önceki ajan tarafından production'da doğrulanmıştır.

Bu oturumda **yeniden uzun encode benchmark çalıştırılmadı** (görev kapsamı
gereği). Dayanılan kanıtlar:

- `results/mediabox-platform/v2-media-core-stremio-policy-2026-09-12.md` — AC3 pipeline acceptance kaydı
- `media/tests/test_session.py` (518 satır) — bu oturumda yeniden çalıştırıldı, PASS
- Canlı `media sessions` sorgusu — temiz, sızıntı yok

## 18. ffmpeg / ffprobe process durumu

Smoke öncesi:

```
$ pgrep -a ffmpeg   → yok
$ pgrep -a ffprobe  → yok
```

`media inspect` + `media policy` çalıştırıldıktan **sonra**:

```
$ pgrep -a ffmpeg   → yok
$ pgrep -a ffprobe  → yok
```

Orphan process yok; inspector ffprobe çocuklarını temiz reap ediyor. Bağımsız
sistem process'lerine dokunulmadı.

## 19. Torrent — bilinen network limitation

`mediaboxctl status` çıktısı: `Torrent ağı: TORRENT_NETWORK_BLOCKED`

JSON detayı:

```json
"torrentNetwork": {"directHttpAvailable": true, "status": "TORRENT_NETWORK_BLOCKED"}
```

Bu **bilinen ve kabul edilmiş bir ağ kısıtıdır**, kod hatası değildir: mevcut
ağda BitTorrent peer trafiği bloklu. Doğrudan HTTP erişimi çalışıyor
(`directHttpAvailable: true`), bu yüzden HTTP tabanlı stream'ler ve Stremio
katalog/addon çözümlemesi etkilenmiyor. Sistem bu durumu sessizce yutmak
yerine açıkça raporluyor. Uzun torrent testi bu oturumda yapılmadı.

## 20. Test sonuçları

Son 4 commit (`427d0f2`, `8378123`, `86ac97b`, `28aefd4`) mevcut rapor
evidence'ından **sonra** geldiği için regression suite bu oturumda bir kez
yeniden çalıştırıldı.

### Rust (`rust/` workspace)

| Komut | Expected | Actual |
|---|---|---|
| `cargo fmt --check` | exit 0, çıktı yok | **exit 0**, çıktı yok |
| `cargo clippy --all-targets --all-features -- -D warnings` | exit 0, 0 uyarı | **exit 0**, `Finished dev profile`, uyarı yok |
| `cargo test --all` | tüm testler ok | **exit 0** |

`cargo test --all` dağılımı: 5 + 2 + 3 + 3 + 6 = **19 test passed, 0 failed,
0 ignored** (5 crate ayrıca 0-test hedefleriyle birlikte).

### Python

| Komut | Expected | Actual |
|---|---|---|
| `tests/run-media-core-tests.sh` | OK | **exit 0** — `Ran 206 tests in 8.213s` / `OK` |
| `tests/run-mediaboxd-tests.sh` | OK | **exit 0** — `Ran 88 tests in 39.137s` / `OK` |

### Toplam

**313 test PASS, 0 FAIL** (19 Rust + 206 media core + 88 mediaboxd).

### Devralınan evidence

Aşağıdaki testler bu oturumda tekrarlanmamış, önceki raporlara dayanılmıştır:

- AC3 encode benchmark → `v2-media-core-stremio-policy-2026-09-12.md`
- CEC discovery kampanyası + fiziksel remote acceptance → `v2-rust-core-cec-cli-2026-09-12.md`
- HDR/KMS/VOP2 test kampanyası → `s1-direct-kms-lifecycle-handoff-2026-09-11.md`, `s1a-direct-kms-probe-handoff-2026-09-11.md`
- Uzun torrent testi → yapılmadı (Bölüm 19'daki ağ kısıtı)

## 21. Production service listesi

Hedef: `10.27.27.25` (`orangepi5-ultra`, aarch64), uptime 2s 22dk.

| Unit | Active | Enabled | Rol |
|---|---|---|---|
| `mediaboxd-rs.service` | **active** | **enabled** | Rust control plane — HTTP otoritesi |
| `mediabox-media-worker.service` | **active** | **enabled** | Python media worker (loopback) |
| `kodi.service` | **active** | **enabled** | Direct-KMS oynatıcı |
| `mediaboxd.service` | **inactive** | **disabled** | Eski Python control plane (emekli) |
| `kodi-standalone.service` | inactive | not-found | Kullanılmıyor |

Dinlenen soketler:

```
127.0.0.1:8787   mediaboxd-rs  (pid 28966)   ← kontrol düzlemi HTTP
127.0.0.1:8790   python3       (pid 27911)   ← media worker, loopback-only
127.0.0.1:9090   kodi-gbm      (pid 954)     ← Kodi JSON-RPC
  0.0.0.0:8080   kodi-gbm      (pid 954)     ← Kodi web
/run/mediabox/mediaboxd.sock  (srwxr-x--- root root)
```

Deploy edilmiş binary'ler:

```
/opt/rk3588-mediabox/bin/mediaboxctl    1 811 792 B   12 Eyl 16:16
/opt/rk3588-mediabox/bin/mediaboxd-rs   4 440 496 B   12 Eyl 16:18
```

## 22. Rollback bilgisi

Rollback yolu **açık ve test edilebilir durumda**; hiçbir eski bileşen silinmedi.

Kod tarafı:

```sh
git revert --no-commit f3dd8f3..HEAD   # veya
git checkout f3dd8f3                   # entegrasyon öncesi bilinen-iyi nokta
```

Production tarafı (eski Python control plane'e dönüş):

```sh
systemctl disable --now mediaboxd-rs mediabox-media-worker
systemctl enable  --now mediaboxd
```

Bunu mümkün kılan koşullar (doğrulandı):

- `/etc/systemd/system/mediaboxd.service` diskte duruyor (947 B, silinmedi)
- Eski daemon'ın konfigürasyonu `/etc/mediaboxd.toml` yerinde
- `kodi.service` her iki mimaride de bağımsız — rollback Kodi'yi etkilemez
- Rust daemon ve media worker yalnızca loopback dinliyor; durdurulmaları dış
  bağımlılık kırmaz

Rollback sonrası port 8787 boşalır ve eski daemon kendi portunu geri alır.

## 23. Final branch / worktree durumu

Bu bölümün nihai değerleri kapanış adımları (push + cleanup) sonrasında
Bölüm 24-25 ile birlikte geçerlidir.

Kapanış öncesi worktree envanteri:

```
/home/yu/Projeler/rk3588-mediabox   28aefd4 [main]
/tmp/mediabox-claude                eef4b79 (detached HEAD)
/tmp/mediabox-codex                 7ff5e3c (detached HEAD)
```

Branch envanteri:

```
* main 28aefd4 [origin/main: 16 önünde]
```

Ajan branch'i oluşmamıştır (çalışma detached worktree üzerinden yürütüldü) —
beklenen durum. Cleanup sonucu final cevapta raporlanmıştır.

## 24. Final HEAD

Bu rapor commit'lendikten sonraki `main` HEAD'i, Bölüm 25 ile birlikte final
cevapta kanıtlanmıştır.

## 25. origin/main

Kapanış öncesi `origin/main` = `f3dd8f3` (16 commit geride).

Kapanış hedefi: `git push origin main` sonrası
`git rev-parse HEAD` == `git rev-parse origin/main`.

Force push kullanılmamıştır ve kullanılmayacaktır.

---

## Özet

MediaBox V2 core entegrasyonu (Codex 7 + Claude 9 = 16 commit) doğrulanmıştır.
Rust control plane production'da tek HTTP otoritesidir, media worker loopback'e
indirgenmiştir, eski Python daemon rollback edilebilir şekilde emekli
edilmiştir. CEC, Kodi, headless Stremio, inspector ve policy canlı olarak
sağlıklıdır. Fiziksel klavye sahipliği Kodi'de kalmıştır — input regression
yoktur. Orphan ffmpeg/ffprobe process'i yoktur. 313 test PASS.

Açık kalan tek kalem: `mediaboxctl media status` insan-okunur çıktısının CEC
şablonuna düşmesi — kozmetik, veri katmanı doğru, ayrı takip işi.
