# S0-A — MediaBox platform mimarisi: Web UI, Stremio, preview→Kodi, display sahipliği, input, HDMI-CEC

| | |
| --- | --- |
| Gate | S0-A (analiz/keşif; implementasyon yok) |
| Tarih | 2026-09-11 |
| Hedef | Orange Pi 5 Ultra / RK3588, `root@10.27.27.25` |
| Baseline | `d02bd6f` = `origin/main`, working tree clean (doğrulandı) |
| Referans repo | `rk3588-screenbridge` @ `582e1d3`, clean, salt-okunur (doğrulandı) |
| Branch | `agent/claude-s0a-architecture` (worktree `/tmp/rk3588-mediabox-claude-s0a`) |
| Hedef üzerinde değişiklik | Yok. Yalnızca okuma: `cat`/`ls`/`ldd`/`strings`, DRM debugfs, CEC `G_` ioctl'leri, Kodi JSON-RPC `Introspect`/`Version` |

Bu gate kabul edilmiş Kodi/RKMPP/DRM PRIME/VOP2/HDR/ALSA yığınını **değiştirmez ve yeniden
tasarlamaz**. Tüm öneriler o yığını sabit girdi kabul eder.

---

## 1. Executive decision

**ARCHITECTURE_READY_FOR_RUNTIME_VALIDATION.**

Beş karar kanıtla kapandı:

1. **Stremio entegrasyonu:** stremio-web fork'lanmaz. Upstream stremio-web *değiştirilmeden*
   çalıştırılır; MediaBox entegrasyonu **streaming server origin'inin önüne konan bir reverse
   proxy** olarak yapılır (`mediaboxd`). stremio-web'in kendi "Streaming server URL" ayarı
   (`URLsManager`) proxy'yi göstermek için yeterlidir — patch gerekmez.
2. **Play on TV kancası:** upstream'de zaten var. `GET /casting` → `type:"external"` cihaz listesi
   ve `POST /casting/{device}/player` `{source, time}`. Bu yol stremio-web'de `platform.shell.active`
   ile **gate'lenmemiştir**; sade tarayıcıda (telefon/laptop) çalışır. `time` alanı pozisyon
   devrini taşır.
3. **Display sahipliği:** MODEL B (compositor) reddedildi. **MODEL A, "Kodi-resident" yönüyle**
   kabul edildi: `mediaboxd` display arbiter'dır, aynı anda tek bir DRM master süreci vardır ve
   Kodi varsayılan sahiptir. Gerekçe: Kodi `drmSetMaster`'ı yalnızca `InitDrm()` içinde alır ve
   `drmDropMaster`'ı yalnızca `DestroyDrm()` içinde bırakır — runtime devri yoktur, devir = süreç
   yaşam döngüsü.
4. **HDMI-CEC:** **OPTION 1 — `mediaboxd` `/dev/cec0`'ı tek başına sahiplenir.** Kodi bu build'de
   `-DENABLE_CEC=OFF` ile derlendiği için CEC'i zaten sahiplenemez, ve libCEC'in Linux adapter'ı
   `CEC_MODE_EXCL_FOLLOWER_PASSTHRU` aldığı için paylaşımlı sahiplik (OPTION 3) kernel API
   seviyesinde imkânsızdır.
5. **Input:** tek normalize aksiyon katmanı `mediaboxd` içinde. CEC uzaktan kumanda tuşları kernel
   tarafından zaten evdev'e çevrilir (`rc0`, `RC_PROTO_CEC`) — libCEC'e gerek yoktur.

Faz 1 için TV-local Web UI **kapsam dışıdır**: bu kutuda kurulu tarayıcı, compositor veya Node.js
yoktur ve TV-local web yüzeyi eklemek kabul edilmiş DRM master sözleşmesine dokunmayı gerektirir.
Faz 1'in TV yüzeyi Kodi'nin kendi GUI'sidir; Web UI telefon/laptop yüzeyidir.

---

## 2. Önerilen hedef mimari

```text
                      KONTROL DÜZLEMİ                          MEDYA DÜZLEMİ
  ┌───────────────────────────────────────────┐     ┌──────────────────────────────┐
  │ tarayıcı (telefon / laptop / ileride TV)  │     │                              │
  │   upstream stremio-web  (değiştirilmemiş) │     │                              │
  │   + MediaBox control shell (ayrı sayfa)   │     │                              │
  └──────────────┬────────────────────────────┘     │                              │
                 │ HTTP + WebSocket                 │                              │
                 v                                  │                              │
  ┌───────────────────────────────────────────┐     │                              │
  │ mediaboxd            (tek uzun ömürlü     │     │                              │
  │                       yerel servis)       │     │                              │
  │  • streaming-server reverse proxy ────────┼────>│ server.js (Node 22, arm64)   │
  │      /casting  override                   │     │  :11470                      │
  │      /casting/{dev}/player  intercept     │     │  torrent → HTTP, /hlsv2,     │
  │      diğer tüm path'ler passthrough       │     │  /proxy, /probe, /stats.json │
  │  • Kodi JSON-RPC istemcisi (TCP 9090) ────┼──┐  └──────────────┬───────────────┘
  │  • WebSocket event bus                    │  │                 │ HTTP dosya/stream
  │  • display arbiter (tek DRM master)       │  │                 v
  │  • input normalizasyon + routing          │  │  ┌──────────────────────────────┐
  │  • CEC sahibi (/dev/cec0)                 │  └─>│ Kodi  (GBM/DRM standalone)   │
  │  • telemetri / ağ / güç / sağlık          │     │  RKMPP → DRM PRIME → VOP2    │
  └──────────────┬────────────────────────────┘     │  → HDMI  (KABUL EDİLMİŞ)     │
                 │                                  └──────────────────────────────┘
                 v   DONANIM / INPUT DÜZLEMİ
  ┌───────────────────────────────────────────┐
  │ /dev/cec0 (dw_hdmi_qp)  •  evdev (USB/BT) │
  │ rc0 → event0 (RC_PROTO_CEC)  •  BlueZ     │
  └───────────────────────────────────────────┘
```

Tek kural: **display'e ve CEC'e yalnızca `mediaboxd` karar verir; Kodi hiçbir zaman
CEC konuşmaz ve hiçbir zaman ikinci bir DRM master ile yarışmaz.**

---

## 3. Stremio entegrasyon kararı (Bölüm A)

### 3.1 Değerlendirilen seçenekler ve kanıtla eleme

| # | Seçenek | Karar | Kanıt |
| --- | --- | --- | --- |
| 1 | Resmî Stremio Web'i iframe'e gömmek | **RED** | Cross-origin iframe; `deepLinks.externalPlayer.streaming` değerine, core transport'una, ayarlara erişilemez. "Play on TV"yi yakalamanın yolu yok. |
| 2 | stremio-web'i fork'layıp kendi shell'imizin bölümü yapmak | **RED (faz 1)** | Sürdürülemez: `development` branch'inde 6.800+ commit, `5.0.0-beta.39`, sürekli hareket. Fork bakım maliyeti, kazanılan şeyden büyük. |
| 3 | stremio-core / addon protokolünü kendi frontend'imizden tüketmek | **RED (faz 1), ileride opsiyon** | Auth, addon state, library sync, notifications, deeplink üretimi hep yeniden yazılır. `stremio-core` Rust→WASM (`@stremio/stremio-core-web` 0.62.1) kullanılabilir ama bu ayrı bir ürün yazmaktır. |
| 4 | **Upstream stremio-web'i değiştirmeden çalıştırmak + MediaBox control shell** | **KABUL** | Aşağıdaki dört kanıt. |
| 5 | "Açıkça üstün upstream-destekli alternatif" | Yok | `stremio-linux-shell` (GTK4 + WebKitGTK + libmpv) masaüstü için tasarlanmıştır; libmpv oynatıcısı bizim kabul edilmiş RKMPP/DRM PRIME yolunu **atlar**. Referans olarak değerli, ürün olarak uygun değil. |

### 3.2 Seçenek 4'ü taşıyan dört kanıt

**(a) Streaming server URL'i kullanıcı ayarıdır.** `stremio-web/src/routes/Settings/Streaming/URLsManager/`
sunucu URL'i ekleme/seçme/silme UI'ı sağlar; varsayılan
`src/common/CONSTANTS.js:4 DEFAULT_STREAMING_SERVER_URL = 'http://127.0.0.1:11470/'`.
Yani stremio-web'i `mediaboxd`'nin proxy origin'ine yöneltmek **patch değil, ayar**dır.

**(b) Çözülmüş oynatılabilir URL zaten UI'a çıkıyor.** `stremio-core` `Stream::convert()` streaming
server URL'i ile `ConvertedStreamSource` üretir; `ExternalPlayerLink.streaming` alanı stremio-web'e
`deepLinks.externalPlayer.streaming` olarak ulaşır ve
`src/routes/Player/usePlayOnDevice.ts` bunu doğrudan okur.

**(c) "Play on TV" kancası upstream'de var ve tarayıcıda gate'li değil.**
`usePlayOnDevice.ts` şunu dispatch eder:

```js
core.transport.dispatch({ action: 'StreamingServer',
  args: { action: 'PlayOnDevice', args: { device: deviceId, source: streamingUrl, time: … } } });
```

`stremio-core/src/models/streaming_server.rs:716 fn play_on_device()` bunu
`POST {server}/casting/{device}/player` gövde `{source, time}` isteğine çevirir.

Kritik ayrım — `stremio-web/src/routes/Player/OptionsMenu/OptionsMenu.js`:

```js
const externalDevices = playbackDevices.filter(({ type }) => type === 'external');   // satır 29
…
streamingUrl && externalDevices.map(({ id, name }) => (
    <Option label={t('PLAYER_PLAY_IN', { device: name })} onClick={playOnDevice} … />))
```

Bu blokta **`platform.shell.active` koşulu yoktur**. Buna karşılık `Player.js:132-137`'de
`chromecast`/`tv` tipli cihazlar `shellCastSupported = platform.shell.active && …` ile
gate'lenmiştir ve `platform.shell.active = !!globalThis?.chrome?.webview`
(`src/common/Platform/shell/useShell.ts`). Sonuç: **`external` tipi cihaz, sade tarayıcıda
görünür; `tv`/`chromecast` görünmez.** Dolayısıyla Kodi'yi bir DLNA renderer (`tv`) olarak değil,
bir `external` cihaz olarak sunmak gerekir.

**(d) `external` grubu sunucu tarafında üretilir, yani proxy ile override edilebilir.**
`server.js` içinde `devices.groups.external`, sabit bir oynatıcı tablosundaki yolların
`fs.existsSync` ile kontrolünden doldurulur (`{ name, type:"external", id, play(src){…} }`);
`"chromecast"`→`ChromecastClient`, `"tv"`→`DLNAClient`. Tablo genişletilemez **ama yanıt
proxy'lenebilir**: `mediaboxd` `GET /casting` cevabına kendi girdisini ekler ve
`POST /casting/mediabox-tv/player` isteğini upstream'e hiç iletmeden kendisi karşılar.

### 3.3 Önerilen mimari

```text
tarayıcı  ──>  mediaboxd :8081        (tek origin)
                 │
                 ├── /            → upstream stremio-web statik build (değiştirilmemiş)
                 ├── /mediabox/*  → MediaBox control shell (dashboard, ayarlar, remote, diag)
                 ├── /ws          → WebSocket event bus
                 └── /server/*    → streaming server reverse proxy
                        ├── GET  /server/casting              → server.js yanıtı + MediaBox TV girdisi
                        ├── POST /server/casting/mediabox-tv/player → Kodi'ye handoff (upstream'e gitmez)
                        └── *                                  → 127.0.0.1:11470 passthrough
```

stremio-web'in streaming server URL'i `http://<box>:8081/server/` olarak ayarlanır.

### 3.4 Bölüm A'nın istediği özel başlıklar

| Başlık | Durum |
| --- | --- |
| Authentication / session | Upstream'de kalır. Stremio hesabı `stremio-core` `Ctx`/`Profile` içinde, tarayıcı storage'ında. `mediaboxd` **auth'a hiç dokunmaz**, authKey görmez. |
| Addon state | Upstream `Ctx` + Stremio API sync. MediaBox tarafında kopya tutulmaz. |
| Stream selection | Tamamen upstream. MediaBox yalnızca *seçilmiş* stream'in çözülmüş URL'ini alır. |
| External-player links | `ExternalPlayerLink { download, magnet, streaming, playlist, openPlayer, … }`. Bizim kullandığımız tek alan `streaming`. `openPlayer` platform tablosu ve `player_type` ayarı MediaBox için kullanılmaz (upstream'de "kodi" diye bir tip yok ve eklemek fork gerektirir). |
| Streaming URL exposure | Çözülmüş URL zaten tarayıcıya ulaşıyor (kanıt b). Proxy bunu değiştirmez; yalnızca origin'i MediaBox'a taşır, böylece URL LAN'da `127.0.0.1` yerine erişilebilir bir host taşır. |
| CORS | `server.js` `Access-Control-Allow-Origin` üretir (16 kullanım) ve `NO_CORS` env'i ile gevşetilebilir (`stremio-linux-shell/src/server.rs`). Yine de **tek origin** önerilir: stremio-web ve proxy aynı origin'de olursa CORS tamamen devre dışı kalır. Upstream shell de aynı hileyi kullanır: `STARTUP_URL = "http://127.0.0.1:11470/proxy/d=https%3A%2F%2Fweb.stremio.com/"` (`stremio-linux-shell/src/config.rs`) — yani web uygulamasını streaming server'ın kendi `/proxy` endpoint'inden servis ederek her şeyi same-origin yapar. |
| Browser sandbox limitleri | `http://<LAN-IP>` secure context **değildir**: service worker, PWA install, `navigator.clipboard` (OptionsMenu'deki "Copy stream" bunu kullanır) ve bazı media API'leri çalışmaz. Upstream'in cevabı `/get-https` → `https://{domain}:{port}` (`*.strem.io` tüneli, `streaming_server.rs:746 get_https_endpoint`), ki bu authKey ve internet ister. **Karar: faz 1 düz HTTP; HTTPS ayrı bir gate.** |
| "Play on TV"yi yakalayabilme | Evet — §3.2(c)(d). |
| Preview desteği | Evet — upstream web player (`@stremio/stremio-video`) preview'i sağlar; MediaBox hiçbir şey yapmaz. |
| Upstream değişikliklerine dayanıklılık | Bağımlı olduğumuz yüzey yalnızca: `GET /casting` şeması (`{id,name,type}`), `POST /casting/{id}/player` gövdesi (`{source,time}`) ve `external` tipinin gate'siz render edilmesi. Fork'a kıyasla çok dar. Bu üç noktanın regresyon testi S2'nin PASS kriteridir. |

### 3.5 Kabul edilen risk: `server.js` kapalı kaynak

`stremio-linux-shell/data/server.js` 6.676.491 baytlık, `linguist-vendored` işaretli webpack
bundle'ıdır; başlığı `/*! For license information please see server.js.LICENSE.txt */`.
Node ile çalıştırılır (`src/server.rs`: `Command::new("node")`, env `NO_CORS`, `SERVER_IPC_KEY`).
Flatpak manifesti **arm64 Node'u pinler** (`node-v22.23.1-linux-arm64.tar.xz`), yani bu bundle
bizim board'da resmî olarak desteklenen bir hedeftir. FFmpeg'i `FFMPEG_BIN`/`ffmpeg-ffprobe-static`
üzerinden bulur.

Sonuç: torrent→HTTP dönüşümünü faz 1'de **kendimiz yazmayız**, `server.js`'i çalıştırırız.
Bağımlılık `mediaboxd`'nin proxy sınırının arkasında izole olduğu için, ileride açık kaynak bir
motorla (ör. Rust tabanlı alternatifler) değiştirmek `mediaboxd` içinde tek bir upstream adresi
değiştirmek demektir.

---

## 4. Preview → Kodi devri (Bölüm B)

### 4.1 Akış

```text
stremio-web (tarayıcı)
  browse → stream seç → Player açılır → PREVIEW web player'da oynar
      │
      │  kullanıcı OptionsMenu → "Play in MediaBox TV"
      v
  core → POST http://<box>:8081/server/casting/mediabox-tv/player
             { source: "<çözülmüş streaming URL>", time: <ms cinsinden pozisyon> }
      │
      v
  mediaboxd
      ├── source URL'ini normalize et (127.0.0.1 → kutunun kendi adresi; zaten yereldeyiz)
      ├── (opsiyonel) /probe veya HEAD ile oynatılabilirliği doğrula
      └── Kodi JSON-RPC (TCP 9090):
            Player.Open { item:{ file: <url> },
                          options:{ resume:{ hours, minutes, seconds, milliseconds } } }
      v
  Kodi → RKMPP → DRM PRIME → VOP2 → HDMI     (kabul edilmiş yol, değişmemiş)
```

### 4.2 Sorulara cevaplar

**1. Çözülmüş oynatılabilir URL nerede yaşıyor?**
`stremio-core` içinde üretilir, `ExternalPlayerLink.streaming` olarak tarayıcıya çıkar, ve
`PlayOnDevice` ile `mediaboxd`'ye **tek seferlik olarak** geri gelir. `mediaboxd` onu kalıcı
olarak saklamaz; yalnızca aktif oturumun state'inde tutar (`current_session { source, meta, started_at }`).
Kaynak-of-truth Stremio'dur, MediaBox değil.

**2. Torrent/magnet kaynakları HTTP'ye nasıl dönüşür?**
Dönüşümü `stremio-core` + streaming server yapar, biz değil. Kanıt —
`stremio-core/src/types/resource/stream.rs`, `Stream::convert()` torrent kolu:

```rust
path.extend([
    &hex::encode(info_hash),
    // fileIndex verilmediğinde -1: sunucu en büyük dosyayı seçer
    &file_idx.map_or_else(|| "-1".to_string(), |idx| idx.to_string()),
]);
// query: her announce için "tr", her file_must_include için "f"
```

Yani URL şekli: `{streaming_server}/{40-hex infoHash}/{fileIdx|-1}?tr=…&f=…`.
`server.js` tarafında aynı desen doğrulanıyor (`src.match(/\/(?<ih>[0-9a-f]{40})\/(?<id>[0-9]+)$/)`,
`enginefs.getFilename(...)`). Streaming server çalışmıyorsa core zaten hata veriyor:
`"Can't play Torrents because streaming server is not running"`.

**3. Stremio streaming server'ının sahibi hangi bileşen?**
`mediaboxd`. `server.js`'i `127.0.0.1:11470`'te, **yalnızca loopback'e bağlı** bir alt süreç olarak
başlatır ve yaşam döngüsünü yönetir (systemd `Requires=`/`After=` veya doğrudan child process).
LAN'a açılan tek yüzey `mediaboxd`'nin proxy'sidir. Bu, hem tek origin'i hem de `/casting`
override'ını mümkün kılan tek düzenlemedir.

**4. Preview ve Kodi aynı kaynağı nasıl güvenle tüketir?**
Aynı HTTP URL'ini iki farklı istemci olarak çekerler. `server.js`/enginefs zaten çoklu range
isteğini destekleyen bir HTTP sunucusudur (torrent için parça önceliklendirmesi dahil).
**Güvenlik kuralı: aynı anda iki tüketici olmamalı.** `mediaboxd` handoff'ta önce preview'e
"durdur" event'i (WebSocket) gönderir, `Player.Open`'ı ondan sonra yapar. Aksi halde tek bir
torrent engine'i iki farklı seek pozisyonunu beslemeye çalışır ve her ikisi de buffer'lar.
Bu sıralama S2'nin PASS kriterlerinden biridir.

**5. Oynatma pozisyonu preview'den Kodi'ye devredilebilir mi?**
**Evet, iki uçta da kanıtlı.**
Gönderen uç: `usePlayOnDevice` `time` alanını gönderir; `PlayOnDeviceArgs { device, source, time: Option<u64> }`
(`stremio-core/src/runtime/msg/action.rs:171`).
Alan uç: bu build'in canlı `JSONRPC.Introspect` çıktısında `Player.Open` `options.resume`
şu tipleri kabul ediyor: `boolean`, `Player.Position.Percentage`, **`Player.Position.Time`**
(`{hours, minutes, seconds, milliseconds}`). Ayrıca `Player.Seek` `{time}`/`{seconds}` destekliyor.
Ters yön (Kodi → preview) `Player.GetProperties` + `Player.OnSeek`/`Player.OnStop` notification'ları
ile mümkündür.

**6. Faz 1 için hangi metadata gerekli?**
`source` (URL), `time` (ms), ve Kodi'nin OSD'si için minimal `title`. `Player.Open` `item.file`
için başka hiçbir şey gerekmez. Başlık `mediaboxd` tarafında `Playlist.Add`/`Player.Open` yerine
basit bir `file` ile geçilir; ekranda dosya adı görünür.

**7. Hangi metadata ertelenmeli?**
Altyazı otomasyonu (**faz-1 blocker değildir, açıkça ertelenmiştir**), IMDb/TMDB artwork,
sezon/bölüm ilişkilendirmesi, Kodi library entegrasyonu, "continue watching" senkronu
(Stremio library'sine geri yazma), audio/subtitle track tercih devri, next-episode.

---

## 5. `mediaboxd` sorumluluk sınırı (Bölüm C)

### 5.1 İçinde olması gerekenler

| Alan | Sorumluluk |
| --- | --- |
| Kodi kontrolü | JSON-RPC istemcisi. **TCP 9090** kullanılır (`127.0.0.1:9090` dinlemede doğrulandı) — kalıcı bağlantı ve notification push için; HTTP 8080 yalnızca fallback. |
| Playback orchestration | Tek "aktif oynatma oturumu" state machine'i: `idle → handoff → playing → stopped`. |
| Stremio handoff | `/casting` override, `/casting/{id}/player` intercept, URL normalizasyonu, preview durdurma sıralaması. |
| Streaming server lifecycle | `server.js` alt sürecini başlat/izle/yeniden başlat; loopback'e bağlı tut. |
| Reverse proxy | stremio-web statikleri + `/server/*` passthrough (tek origin). |
| Event bus | Tarayıcılara WebSocket: playback state, cihaz durumu, CEC olayları, ağ, sağlık. Kodi'de WebSocket transport yok — bu boşluğu `mediaboxd` doldurur. |
| Display arbiter | Aynı anda tek DRM master garantisi; Kodi'yi başlatan/durduran tek yer. |
| Input | evdev okuma + normalize aksiyon + routing (Bölüm E). |
| HDMI-CEC | `/dev/cec0` tek sahibi (Bölüm F). |
| Bluetooth | BlueZ'e D-Bus ile eşleştirme/bağlanma yönetimi (bugün `bluetooth.service` inactive). |
| Telemetri | sıcaklık, CPU/RAM, disk, HDMI bağlantı durumu, DRM mod, ALSA durumu. |
| Ağ | arayüz/IP/DNS durumu okuma; `systemd-networkd` + `wpa_supplicant` mevcut. |
| Güç | reboot/shutdown; Kodi'nin `System.Shutdown` yerine sistem seviyesinde. |
| Sağlık / watchdog | Kodi ve `server.js` canlılık kontrolü, kontrollü yeniden başlatma. |
| Diagnostic log erişimi | Kodi log, journald, DRM debugfs özetlerinin salt-okunur sunumu. |

### 5.2 İçinde **olmaması** gerekenler

- **Video/ses decode, render, ölçekleme, tone mapping.** Medya düzlemi Kodi'nindir.
- **DRM/KMS atomic commit, plane/EOTF/Colorspace/`color_depth` yönetimi.** Kabul edilmiş HDR
  sözleşmesi Kodi patch'lerinde yaşar; `mediaboxd` DRM master **olmaz**, yalnızca kimin olacağını
  seçer.
- **ALSA yapılandırması / mixing.** `config/alsa/rockchip-hdmi1.conf` ve passthrough davranışı
  Kodi'nin işidir.
- **Stremio auth, addon yönetimi, katalog, library sync.** Upstream'in işi.
- **Torrent engine / transcode.** `server.js`'in işi.
- **Kernel, DT, bootloader, kalıcı sistem konfigürasyonu.** Proje kuralı.
- **Harici telemetri / bulut servisi / zorunlu API key.** Proje kuralı.
- **Kodi ayarlarını runtime'da keyfi değiştirmek.** `videoplayer.useprimedecoder`,
  `useprimerenderer`, `videoplayer.usedisplayasclock`, `videoscreen.whitelist` **dokunulmaz**
  kabul edilir.

### 5.3 Runtime / dil önerisi

**Öneri: Rust (tek statik binary, tokio + axum + rustbus/zbus).**

Cihazda ölçülen gerçek durum: Node yok, cargo/rustc yok, Go yok; yalnızca **Python 3.13.5** var.
8 çekirdek, 7.9 GB RAM, 37 GB boş disk.

| Aday | Değerlendirme |
| --- | --- |
| **Rust** | **Seçilen.** `/dev/cec0` ioctl, evdev, DRM arbiter ve HTTP proxy'nin hepsi tek süreçte, GC duraklaması olmadan. Tek dosya deploy; hedefe runtime kurulumu gerekmez (proje kuralı: "install packages on target" yasak — tek binary bunu en iyi karşılayan seçenek). Ek olarak `stremio-core` ile aynı dil, ileride core'u gömme opsiyonu açık kalır. Maliyet: cross-compile (aarch64) host tarafında kurulmalı. |
| Python | Hızlı prototip, cihazda zaten var. Ama ioctl/evdev/CEC döngüleri ve proxy için asyncio + ctypes yığını kırılgan; bağımlılık kurulumu hedefte paket kurmayı gerektirir. **Faz 1 spike'ı için kabul edilebilir, ürün için değil.** |
| Node/TS | `server.js` zaten Node gerektirdiği için runtime "bedava" görünüyor; ama CEC/evdev için native addon derlemek gerekir ve hedefte derleme zinciri istemiyoruz. **Red.** |
| Go | Rust'a yakın; tek binary avantajı aynı. Rust, `stremio-core` ile dil ortaklığı ve mevcut C/C++ probe'larla FFI kolaylığı nedeniyle tercih edildi. |

Not: `server.js` için Node yine de gerekir (arm64 Node 22, upstream flatpak'in pinlediği sürüm).
Bu, `mediaboxd`'nin dilinden bağımsız bir ürün bağımlılığıdır ve S3'ün konusudur.

---

## 6. Display sahipliği (Bölüm D)

### 6.1 Ölçülen gerçek durum

```text
/sys/kernel/debug/dri/0/clients
             command   pid dev master a   uid      magic
            kodi-gbm 40781   0   y    y     0          0     ← tek DRM master
            kodi-gbm 40781 128   n    y     0          0

lsof /dev/dri/card0 → yalnızca kodi-gbm
Video Port0: ACTIVE, HDMI-A-1, 1920x1080p60, SDR[0], Cluster0-win0 ACTIVE (idle GUI durumu)
card1 = RKNPU (display değil)
Kodi TTY'si yok (setsid ile başlatılmış, `ps -o tty` → `?`); getty@tty1 ayrı çalışıyor
loginctl: seat0 mevcut; Kodi'nin logind session'ı yok
kernel drm_lease sembolleri mevcut (kallsyms: 8)
Kurulu compositor yok: weston/sway/cage yok.  Kurulu tarayıcı yok: chromium/firefox/cog yok.
```

### 6.2 Kodi'nin DRM master yaşam döngüsü — belirleyici kanıt

`/var/tmp/kodi-src/xbmc/windowing/gbm/drm/DRMUtils.cpp`:

- satır **589**: `ret = drmSetMaster(m_fd);` — `InitDrm()` içinde, başlangıçta bir kez.
  Başarısız olursa: `"Failed to set drm master, will try to authorize instead"` → `drmGetMagic`
  ile *client* olarak yetkilenmeye çalışır.
- satır **752**: `drmDropMaster(m_fd);` — yalnızca `DestroyDrm()` içinde, kapanışta.
- satır **725** `RestoreOriginalMode()` — çıkarken orijinal CRTC modunu geri koyar.

**Sonuç:** Kodi'de "display'i geçici bırak, sonra geri al" diye bir runtime yolu **yoktur**.
Devir = süreç yaşam döngüsü. Ve fallback yolu (auth-as-client) modeset hakkı vermez — yani
başka bir süreç master iken başlayan Kodi, kabul edilmiş HDR modeset'ini yapamaz. Bu, MODEL B'nin
neden tehlikeli olduğunun doğrudan kanıtıdır.

### 6.3 Modellerin karşılaştırması

| Kriter | MODEL A (tek master, süreç devri) | MODEL B (compositor sahibi) |
| --- | --- | --- |
| Mevcut HDR/23.976/10-bit yoluna risk | **Düşük.** Kodi tam olarak bugünkü gibi, tek master olarak çalışır. | **Kabul edilemez.** Kodi Wayland/X11 client'ına dönüştürülmeden compositor altında `drmSetMaster` alamaz; alamayınca `Colorspace`, `color_depth=30bit`, `HDR_OUTPUT_METADATA`, per-plane `EOTF` ve VOP2 `SDR2HDR_CTRL=0x0000000b` sözleşmesinin tamamı compositor'ın protokolüne devredilir. Repo bu dönüşümü açıkça yasaklıyor. |
| DRM master / lease | Basit: aynı anda bir master. Lease gerekmez. | Compositor master olur, Kodi'ye lease verebilir — ama tek CRTC ve **NV15 alabilen tek plane'in `Esmart0-win0` (cursor tipi)** olması nedeniyle lease'lenecek kaynak zaten tüm çıkışın kendisidir. Compositor katmanı saf ek risk olur. |
| VT / seat | Kodi VT'siz çalışıyor; `getty@tty1` dokunulmuyor. Arbiter VT switch'e hiç bulaşmaz. | Compositor logind session + VT ister; `getty@tty1` ile çakışma yüzeyi açılır. |
| Başlatma gecikmesi | **Bilinen zayıflık.** Kodi soğuk başlatma maliyeti ödenir (`run-kodi-rk3588.sh` JSON-RPC için 120 s'e kadar bekliyor; gerçek süre ölçülmedi). | Düşük (Kodi sürekli client olarak açık kalabilir) — ama yukarıdaki risk bunu anlamsız kılar. |
| Çökme kurtarma | Net: arbiter süreç öldü mü diye bakar, tek sahibi yeniden başlatır. `RestoreOriginalMode()` temiz çıkışta modu geri verir. | Compositor çökerse tüm çıkış gider; iki katman birden kurtarılmalı. |
| UI render seçenekleri | Compositor'sız KMS/GBM tarayıcı gerekir (WPE/Cog `--platform=drm`; upstream'de mevcut, atomic modeset destekli). | Herhangi bir Wayland tarayıcısı çalışır. |
| Appliance güvenilirliği | Yüksek: tek sahip, tek hata noktası, deterministik. | Düşük: iki display yazılımı + doğrulanmış renk yolunun yeniden kanıtlanması. |

### 6.4 Karar

**MODEL A kabul edilir — ancak brief'teki "UI idle'da sahiptir" yönü tersine çevrilerek:
Kodi resident sahiptir, UI transient misafirdir.**

```text
mediaboxd (display arbiter, DRM master DEĞİL)
    │
    ├── varsayılan durum: kodi-gbm çalışıyor, tek master, TV-local yüzey = Kodi GUI
    │
    └── (İLERİDE, S5+) TV-local web yüzeyi istendiğinde:
            stop kodi-gbm  →  DestroyDrm(): RestoreOriginalMode() + drmDropMaster
            start cog --platform=drm (WPE, compositor yok) → yeni master
            "oynat" → stop cog → start kodi-gbm → master geri
```

Gerekçe: pahalı ve doğrulanmış olan bileşen Kodi'dir, tarayıcı değil. Nadir ve ucuz olan geçişi
tarayıcı tarafına yüklemek, sık ve pahalı olanı (HDR modeset'in yeniden kurulması) ortadan kaldırır.

**Faz 1 kapsamı:** TV-local web UI yok. Kodi hiç durmaz, arbiter yalnızca "tek master" kuralını
uygular ve Kodi'yi yeniden başlatmak zorunda kalırsa bunu tek yerden yapar. Web UI telefon/laptop
yüzeyidir. Bu, brief'in "TV-local UI" gereksinimini **erteler, iptal etmez**; erteleme gerekçesi
yukarıdaki risk analizi ve cihazda tarayıcı/compositor bulunmamasıdır.

### 6.5 MODEL A'yı doğrulayacak minimum deney (S1)

Mevcut araçlarla, yeni kod yazmadan:

1. `tools/vop2-sdr2hdr-capture/capture-state.sh` ile **A-öncesi** kabul edilmiş durumu yakala
   (HDR asset oynarken: `SDR2HDR_CTRL`, plane EOTF'leri, HDMI `color_depth`, `Colorspace`).
2. Kodi'yi durdur. `lsof /dev/dri/card0` ve `/sys/kernel/debug/dri/0/clients` boş mu doğrula
   (`drmDropMaster` gerçekten olmuş mu).
3. `tools/gbm-egl-probe` (veya `tools/hdr-signaling-probe`) ile ikinci bir GBM/KMS istemcisini
   master yap, kısa süre tut, çık. Master'ı temiz bıraktığını `clients` ile doğrula.
4. Kodi'yi yeniden başlat, aynı asset'i oynat, **A-sonrası** durumu yakala.
5. **PASS:** adım 1 ve adım 4 capture'ları kritik alanlarda byte-identical
   (`SDR2HDR_CTRL = 0x0000000b`, GUI `SDR[0]`, video `HDR10[2]`, `r2r_mode = BT709_TO_BT2020`,
   `hdr2sdr_en = 0`, `overlay_mode = 0`, HDMI `BT2020_YCC` + `30bit` + `YUYV10_1X20`) ve
   cadence 23.976'da dropped/repeated/late = 0. **Ayrıca Kodi'nin soğuk başlama süresi ölçülür.**
   **STOP:** herhangi bir alan farklıysa veya ikinci istemci master'ı bırakmazsa.

---

## 7. Input mimarisi (Bölüm E)

### 7.1 Ölçülen giriş yüzeyi

```text
/proc/bus/input/devices:
  event0  "dw_hdmi_qp"   /devices/platform/fdea0000.hdmi/rc/rc0/input0   Handlers: kbd event0
  event3  "headset-keys" (es8388)
  (+ ALSA jack switch'leri event1/2/4, input5 "bt-powerkey")
/sys/class/rc/rc0/protocols → [cec]
Kodi linkleri: libinput.so.10, libudev.so.1, libgudev-1.0 (ldd kodi-gbm)
bluetooth.service = inactive; bluetoothctl/hciconfig/rfkill kurulu değil
```

**Kritik bulgu:** HDMI-CEC uzaktan kumanda tuşları **libCEC olmadan** normal evdev tuşlarına
dönüşür. Kernel `CONFIG_MEDIA_CEC_RC=y` ile `rc-cec` keymap'ini kaydeder
(`dmesg: "Registered IR keymap rc-cec"`, `rc rc0: dw_hdmi_qp`) ve
`drivers/media/cec/core/cec-adap.c` `CEC_MSG_USER_CONTROL_PRESSED` → `rc_keydown(adap->rc, RC_PROTO_CEC, …)`
yapar. Yani CEC kumandası, USB/BT kumandayla **aynı taşıma yolunu** kullanır: evdev.

İki koşul var ve ikisi de bugün sağlanmıyor:
- adaptörün yapılandırılmış olması (`num_log_addrs = 0` — bkz. §8),
- `cec_log_addrs.flags` içinde **`CEC_LOG_ADDRS_FL_ALLOW_RC_PASSTHRU`** set edilmiş olması
  (kaynak: `cec-adap.c:2190-2196` ve `:2185` civarı; bugün `flags = 0x00000000`).

### 7.2 Önerilen katman

```text
  BT HID ──┐
  USB HID ─┼─> evdev (/dev/input/event*)  ─┐
  CEC RC ──┘   (rc0 → event0)              │
                                           ├─> mediaboxd input manager
  tarayıcı remote ──> WebSocket ───────────┘        │
                                                    │  normalize: UP DOWN LEFT RIGHT OK BACK HOME
                                                    │             PLAY PAUSE PLAY_PAUSE STOP
                                                    │             SEEK_FORWARD SEEK_BACK
                                                    │             VOLUME_UP VOLUME_DOWN MUTE POWER
                                                    v
                                            routing (focus'a göre)
                                   ┌────────────────┴────────────────┐
                                   v                                 v
                     Kodi JSON-RPC (TCP 9090)              WebSocket → aktif Web UI
                     Input.Up/Down/Left/Right/Select/
                     Back/Home/ContextMenu/Info/ShowOSD/
                     ExecuteAction/ButtonEvent/SendText
                     Player.PlayPause/Stop/Seek/SetSpeed
                     Application.SetVolume/SetMute
                     (hepsi bu build'in Introspect çıktısında doğrulandı)
```

**Çakışmayı önleyen kural:** bir input cihazı ya `mediaboxd`'nindir ya Kodi'nin, ikisinin birden
değil. `mediaboxd` sahiplendiği cihazlara `EVIOCGRAB` uygular — bu exclusive'dir, yani Kodi'nin
libinput'u o cihazdan olay alamaz. Deterministik ve gözlemlenebilir.

**Fazlama.** Grab, Kodi'ye giden her tuşa bir JSON-RPC gidiş-dönüşü ekler. Bu yüzden:

- **Faz 1 (S2):** grab **yok**. Kodi USB/BT/CEC tuşlarını bugünkü gibi doğrudan libinput ile alır
  (gecikme = bugünkü gecikme). `mediaboxd` yalnızca *ağ* uzaktan kumandasını ekler
  (WebSocket → JSON-RPC) ve evdev'i yalnızca **gözlemler** (grab'siz okuma) — cihaz keşfi ve
  teşhis için.
- **Faz 2 (S6):** tam arbiter. `mediaboxd` remote sınıfı cihazları grab eder, normalize eder,
  focus'a göre yönlendirir. PASS kriteri: tuş→ekran gecikmesinin faz 1'e göre ölçülebilir şekilde
  kötüleşmemesi.

**Cihaz modeli sabit kodlanmaz.** Cihaz sınıflandırması `EVIOCGBIT` yeteneklerinden ve udev
özelliklerinden türetilir (`ID_INPUT_KEY`, `KEY_PLAYPAUSE`/`KEY_HOMEPAGE` gibi tuşların varlığı),
VID/PID tablosundan değil. Keymap normalize aksiyonlara `KEY_*` seviyesinde bağlanır; böylece
`rc-cec` keymap'i, jenerik BT kumandalar ve Android TV kumandaları aynı tabloyu paylaşır.

---

## 8. HDMI-CEC sahipliği (Bölüm F)

### 8.1 Ölçülen durum — salt okunur `G_` ioctl'leri ile

```text
/dev/cec0  (crw-rw---- root:video, 247:0)
  driver            : dwhdmi-rockchip
  name              : dw_hdmi_qp
  available_log_addrs: 4
  capabilities      : 0x0000001e  = LOG_ADDRS | TRANSMIT | PASSTHROUGH | RC
  version           : 0x00060173  (6.1.115)
  physical_address  : 1.0.0.0  (0x1000)
  num_log_addrs     : 0
  log_addr_mask     : 0x0000
  log_addr[0]       : 0xff  (CEC_LOG_ADDR_INVALID)
  vendor_id         : 0xffffffff (CEC_VENDOR_ID_NONE)
  flags             : 0x00000000
  osd_name          : ''
```

Kernel: `CONFIG_CEC_CORE=y`, `CONFIG_CEC_NOTIFIER=y`, `CONFIG_MEDIA_CEC_SUPPORT=y`,
`CONFIG_MEDIA_CEC_RC=y`, `CONFIG_DRM_DW_HDMI_CEC=y`.
Platform cihazı: `/sys/devices/platform/fdea0000.hdmi/dw-hdmi-qp-cec.1.auto`.
`v4l-utils` (`cec-ctl`) kurulu **değil**; `libcec` ldconfig'de **yok**.

Üç sonuç:

1. **CEC donanımı çalışıyor ve TV'ye bağlı.** `physical_address = 1.0.0.0` EDID'den türetilmiş —
   yani TV'nin HDMI-1 portundayız ve CEC bloğu okunmuş.
2. **`CEC_CAP_PHYS_ADDR` set DEĞİL.** Fiziksel adresi sürücü (`cec_notifier` üzerinden EDID'den)
   yönetir. Userspace `CEC_ADAP_S_PHYS_ADDR` **çağırmamalıdır**; yalnızca `S_LOG_ADDRS` çağırır.
3. **Bugün CEC'i kimse sahiplenmiyor.** `num_log_addrs = 0`, `log_addr_mask = 0x0000` → hiçbir
   mantıksal adres talep edilmemiş, hiçbir CEC mesajı alınmıyor/gönderilmiyor. Temiz sayfa.

### 8.2 Kodi CEC konuşamaz — build kanıtı

`scripts/build-kodi.sh:108` → `-DENABLE_CEC=OFF`.
Hedefteki binary'de tek `libcec` geçen dize:
`"{} - libCEC support has not been compiled in, so the CEC adapter cannot be used."`
`peripherals.xml` CEC girdilerini içeriyor ama bunlar yalnızca uyarı üretmek için var
(dosyanın kendi yorumu: *"This entry will not create a CPeripheralCecAdapter instance… it will
ensure that a warning is displayed when an adapter is inserted, but libCEC is not present"*).

### 8.3 Paylaşımlı sahiplik neden imkânsız

libCEC'in Linux CEC framework adapter'ı (`src/libcec/adapter/Linux/LinuxCECAdapterCommunication.cpp`)
şunu yapar:

```cpp
uint32_t mode = CEC_MODE_INITIATOR | CEC_MODE_EXCL_FOLLOWER_PASSTHRU;
… ioctl(m_fd, CEC_ADAP_S_LOG_ADDRS, &log_addrs)
```

Kernel dokümanı: `CEC_MODE_EXCL_FOLLOWER` — *"This is an exclusive follower and only this file
descriptor will receive CEC messages for processing"*; ikinci bir istekli **`EBUSY`** alır.
Ayrıca `CEC_ADAP_S_LOG_ADDRS`, başka bir fd exclusive initiator/follower modundaysa hata döner.

Yani **OPTION 3 (shared ownership) bir tasarım tercihi değil, kernel API'sinde var olmayan bir
şeydir.** Exclusive follower rolü tektir.

### 8.4 Karar: OPTION 1 — `mediaboxd` `/dev/cec0`'ı sahiplenir

Yapılandırma önerisi:

| Ayar | Değer | Gerekçe |
| --- | --- | --- |
| Mod | `CEC_MODE_INITIATOR \| CEC_MODE_EXCL_FOLLOWER` | Exclusive follower: başka hiçbir süreç mesaj işleyemez. **PASSTHRU değil.** |
| Neden PASSTHRU değil | `cec-adap.c:2105-2109`: passthrough açıkken core, `GET_CEC_VERSION`, `GIVE_PHYSICAL_ADDR`, `GIVE_DEVICE_VENDOR_ID`, `GIVE_OSD_NAME`, `GIVE_FEATURES`, `GIVE_DEVICE_POWER_STATUS`, `ABORT` mesajlarını **cevaplamayı bırakır**; hepsini userspace yazmak zorunda kalır. Non-passthrough'da bunları kernel halleder, `mediaboxd` yalnızca ürün semantiğiyle ilgilenir. |
| RC passthrough | `cec_log_addrs.flags |= CEC_LOG_ADDRS_FL_ALLOW_RC_PASSTHRU` | **Zorunlu.** Bu bayrak olmadan `USER_CONTROL_PRESSED/RELEASED` evdev'e çevrilmez (`cec-adap.c:2190-2193`). İyi haber: `REPORT_PHYSICAL_ADDR` ve `USER_CONTROL_*` **passthrough modundan bağımsız olarak** her zaman core tarafından işlenir (`cec-adap.c:2083-2087`), yani bu tercih RC'yi riske atmaz. |
| Mantıksal adres | primary device type = Playback Device → `0x4`, sırayla `0x8`, `0xB` fallback | `available_log_addrs = 4`; Playback Device semantiği ürüne doğru olan. |
| Fiziksel adres | **dokunulmaz** | `CEC_CAP_PHYS_ADDR` yok; sürücü EDID'den yönetiyor (bugün `1.0.0.0`). |
| OSD name | ürün adı (≤14 karakter) | `GIVE_OSD_NAME`'e kernel cevap verir. |
| Kodi build | `-DENABLE_CEC=OFF` **kalıcı kural** | Exclusivity'nin hiç tartışmaya düşmemesi için. Bu, mimari bir invariant olarak yazılmalı. |

### 8.5 Yarış / çakışma analizi

| Konu | Risk | Azaltma |
| --- | --- | --- |
| Mantıksal adres | İki süreç `S_LOG_ADDRS` çağırırsa ikincisi hata alır; daha kötüsü, bir süreç adresi bırakıp yeniden alırsa TV cihazı kaybeder. | Tek yazar: `mediaboxd`. Kodi'de CEC derlenmemiş. Çalışma zamanında `cec-ctl` gibi bir araç **kurulmaz**. |
| Active Source | En yüksek ping-pong riski. İki kaynak `<Active Source>` yayınlarsa TV kaynak arasında gidip gelir. | Tek yazar. `mediaboxd`, Kodi'yi başlatmadan **önce** `<Image View On>` + `<Active Source>` gönderir; Kodi'nin kendi lifecycle'ı CEC'e hiç dokunmaz. |
| Standby | TV `<Standby>` yayınlar; cihaz kapanmalı mı? Ürün kararı. | `mediaboxd` bunu bir **olay** olarak ele alır, davranış kullanıcı ayarıdır (yoksay / Kodi'yi durdur / sistemi kapat). Varsayılan: yoksay. |
| Image View On | TV uykudayken uyandırma. | Yalnızca kullanıcı eylemiyle veya açık ayarla. Bu gate'te **hiçbir güç/kaynak komutu gönderilmedi**. |
| Routing / source switching | `<Routing Change>`/`<Set Stream Path>` geldiğinde MediaBox kendini aktif kaynak yapmalı. | `mediaboxd` follower olarak yakalar, tek noktadan cevap verir. |
| Remote passthrough | CEC tuşları hem `mediaboxd`'ye (CEC mesajı olarak) hem Kodi'ye (evdev olarak) ulaşır → çift işleme. | Faz 1: `mediaboxd` `USER_CONTROL_*` mesajlarını **işlemez**, evdev yolunu tek gerçek kabul eder. Faz 2'de grab ile tek yol kalır. |
| Kodi lifecycle | Kodi durup başlarken CEC durumu bozulmamalı. | `mediaboxd` uzun ömürlüdür; CEC oturumu Kodi'den bağımsız yaşar. Kodi'nin yeniden başlaması CEC'e görünmez. |
| HPD / kablo | HDMI kopunca fiziksel adres `f.f.f.f` olur ve mantıksal adres düşer. | `mediaboxd` `CEC_EVENT_STATE_CHANGE` dinler, yeniden yapılandırır. |

### 8.6 Ürün yetenekleri karşılığı

| İstenen | Nasıl |
| --- | --- |
| TV kumandası MediaBox UI'ını kontrol eder | CEC → `rc0` → evdev → (faz 2) `mediaboxd` normalize → WebSocket → Web UI |
| TV kumandası Kodi oynatmasını kontrol eder | CEC → `rc0` → evdev → Kodi libinput (faz 1, doğrudan) |
| MediaBox TV'yi uyandırır | `<Image View On>` / `<Text View On>` — `mediaboxd` initiator |
| MediaBox Active Source olur | `<Active Source>` + `physical_address 1.0.0.0` |
| MediaBox kapanınca TV standby | `<Standby>` — **varsayılan kapalı**, kullanıcı ayarı |
| Tüm güç davranışı yapılandırılabilir | Politika `mediaboxd` ayarında; hiçbiri derleme zamanı sabiti değil |

---

## 9. Ürün modül haritası (Bölüm G)

### Kontrol düzlemi

| Modül | Barınak | Notlar |
| --- | --- | --- |
| Web frontend — Stremio | upstream `stremio-web` statik build | Değiştirilmez. `mediaboxd` servis eder. |
| Web frontend — MediaBox control shell | yeni, `/mediabox/*` | Dashboard, ayarlar, uzaktan kumanda, teşhis, cihaz yönetimi. Stremio'dan ayrı sayfa. |
| Stremio entegrasyonu | `mediaboxd` proxy | `/casting` override + `/casting/{id}/player` intercept. |
| `mediaboxd` | yeni, Rust | §5. |
| Event bus | `mediaboxd` WebSocket | Kodi'de WS yok; boşluğu bu doldurur. |
| Kodi kontrolü | `mediaboxd` → TCP 9090 | `Player.*`, `Input.*`, `Application.*`, `GUI.*`, `Settings.*` mevcut (Introspect). |
| Servis başlatma | systemd | `mediaboxd.service` (`Wants=`) → `stremio-server.service`, `kodi.service`. Bugün hiçbiri yok; hepsi gate konusudur. |
| Recovery / watchdog | `mediaboxd` | Kodi + `server.js` canlılık; `systemd Restart=` ile birlikte. |

### Medya düzlemi

| Modül | Barınak | Notlar |
| --- | --- | --- |
| Preview player | tarayıcı, `@stremio/stremio-video` | Upstream. MediaBox kod yazmaz. |
| Streaming server | `server.js` (Node 22 arm64) | `127.0.0.1:11470`, yalnızca loopback. `/hlsv2`, `/proxy`, `/probe`, `/settings`, `/stats.json`, `/casting`, `/device-info`, `/network-info` doğrulandı. |
| Üretim oynatıcı | **Kodi** | KABUL EDİLMİŞ. RKMPP → DRM PRIME → VOP2 → HDMI. Dokunulmaz. |
| FFmpeg/RKMPP | `/opt/rk3588-screenbridge` | Mevcut bağımlılık; `MEDIABOX_FFMPEG_PREFIX`. `server.js` için `FFMPEG_BIN` buraya yöneltilebilir (S3 konusu). |

### Donanım / input düzlemi

| Modül | Barınak | Notlar |
| --- | --- | --- |
| HDMI-CEC | `mediaboxd`, `/dev/cec0` | Tek sahip. §8. |
| Input manager | `mediaboxd`, evdev | Faz 1 gözlem, faz 2 grab+route. §7. |
| Bluetooth | `mediaboxd` → BlueZ D-Bus | `bluetooth.service` bugün inactive; `bt-powerkey` input node'u var. |
| Display arbiter | `mediaboxd` | DRM master **olmaz**; kimin olacağını seçer. §6. |
| Telemetri | `mediaboxd` | sysfs/debugfs/journald okur. |
| Ağ | `mediaboxd` → networkd/wpa_supplicant | Bugün `enP3p49s0` 10.27.27.25/24. |
| Güç | `mediaboxd` | reboot/shutdown; CEC standby politikası ile birlikte. |

---

## 10. Risk register

| # | Risk | Etki | Olasılık | Azaltma | Sahip gate |
| --- | --- | --- | --- | --- | --- |
| R1 | Kabul edilmiş HDR/plane/EOTF sözleşmesinin bozulması | Kritik | Düşük (bu mimaride Kodi'ye dokunulmuyor) | Her gate'te `vop2-sdr2hdr-capture` ile before/after karşılaştırma; MODEL B reddi | S1 |
| R2 | İkinci bir DRM master'ın Kodi'yi client'a düşürmesi (`drmGetMagic` fallback) | Kritik | Orta (arbiter hatası) | Tek master invariant'ı; arbiter Kodi'yi başlatmadan önce `clients` boş mu doğrular | S1 |
| R3 | `server.js` kapalı kaynak, upstream'e bağımlılık | Yüksek | Orta | Proxy sınırı; motor değişimi tek adres değişikliği. Arm64 desteği upstream flatpak ile pinli. | S3 |
| R4 | stremio-web'in `/casting` + `external` kontratını değiştirmesi | Orta | Düşük-Orta | Bağımlılık yüzeyi 3 noktaya indirgendi; S2 PASS kriterine regresyon testi olarak yazılır; upstream revizyonu raporlanır | S2 |
| R5 | Preview ve Kodi'nin aynı torrent'i eşzamanlı çekmesi | Orta | Yüksek (sıralama yapılmazsa) | Handoff'ta önce preview stop, sonra `Player.Open` | S2 |
| R6 | Düz HTTP'de secure-context kayıpları (clipboard, SW, PWA) | Orta | Kesin | Faz 1'de kabul edilir ve dokümante edilir; HTTPS ayrı gate | S4 |
| R7 | CEC exclusivity'nin ileride Kodi tarafından ihlali | Yüksek | Düşük | `-DENABLE_CEC=OFF` kalıcı invariant; build script'e yorum olarak yazılır | S5 |
| R8 | `CEC_LOG_ADDRS_FL_ALLOW_RC_PASSTHRU` unutulması → TV kumandası sessizce çalışmaz | Orta | Orta | S5 PASS kriteri: `flags` okunarak doğrulanır ve evdev'de gerçek tuş görülür | S5 |
| R9 | CEC güç komutlarının istenmeden gönderilmesi (TV'yi kapatma/uyandırma) | Orta | Orta | Varsayılan kapalı; her güç mesajı açık kullanıcı ayarı arkasında | S5 |
| R10 | Input grab'in Kodi gecikmesini kötüleştirmesi | Orta | Orta | Faz 2'ye ertelendi; PASS kriteri ölçülebilir gecikme regresyonu yok | S6 |
| R11 | Kodi soğuk başlatma süresinin appliance UX'ini bozması (MODEL A bedeli) | Orta | Bilinmiyor — **ölçülmedi** | S1'de ölçülür; kabul edilemezse Kodi hiç durdurulmaz (faz 1 zaten böyle) | S1 |
| R12 | Node runtime'ın hedefe kurulması gereği (paket kurulumu yasağı ile gerilim) | Orta | Kesin | `server.js` + arm64 Node **kendi prefix'ine** açılır (`/opt/rk3588-mediabox/…`), sistem paketi kurulmaz — `install-mali-runtime.sh` ile aynı desen | S3 |
| R13 | `mediaboxd` proxy'sinin LAN'a açık olması (kimlik doğrulama yok) | Orta | Kesin | Faz 1: yalnızca LAN, auth yok, dokümante edilir. Token/pairing ayrı gate. | S4 |

---

## 11. Önerilen gate dizisi (Bölüm H)

Her gate tek değişken taşır ve kabul edilmiş medya yığınını korur.

### S1 — Display arbiter feasibility (kod yazılmaz, ölçüm yapılır)

- **Amaç:** MODEL A'nın bedelini ve güvenliğini ölçmek.
- **Tek değişken:** DRM master'ın Kodi'den başka bir GBM/KMS istemcisine geçip geri dönmesi.
- **Yöntem:** §6.5.
- **PASS:** before/after VOP2+HDMI capture'ları kritik alanlarda aynı; ikinci istemci master'ı
  temiz bırakıyor; cadence 23.976 @ 0 dropped/repeated/late; Kodi soğuk başlatma süresi ölçülmüş
  ve raporlanmış.
- **STOP:** herhangi bir renk/HDR alanı değişti, master sızdı, veya Kodi ikinci başlatmada
  `drmSetMaster` alamadı.
- **Kapsam dışı:** tarayıcı kurulumu, compositor, `mediaboxd` kodu, TV-local UI.

### S2 — `mediaboxd` v0: Kodi köprüsü + Play-on-TV intercept

- **Amaç:** "preview → Play on TV → Kodi aynı kaynağı pozisyonla açar" akışını uçtan uca kanıtlamak.
- **Tek değişken:** `mediaboxd`'nin varlığı (proxy + JSON-RPC istemcisi). Kodi, `server.js`,
  stremio-web hepsi olduğu gibi.
- **Kapsam:** `GET /casting` override, `POST /casting/mediabox-tv/player` intercept,
  `Player.Open` + `options.resume`, preview stop sıralaması, WebSocket event bus iskeleti.
- **PASS:** sade bir tarayıcıda (telefon) upstream stremio-web'de "Play in MediaBox TV" görünüyor;
  tıklandığında Kodi aynı stream'i açıyor; pozisyon ±2 s içinde korunuyor; oynatma sırasında
  VOP2/HDMI durumu kabul edilmiş değerlerde; iki tüketici aynı anda çekmiyor.
- **STOP:** `external` tipi UI'da görünmüyor; `Player.Open` torrent HTTP URL'ini açamıyor;
  oynatma durumu kabul edilmiş capture'dan sapıyor.
- **Kapsam dışı:** CEC, input grab, TV-local UI, HTTPS, auth, altyazı.

### S3 — Streaming server'ı appliance'a yerleştirmek

- **Amaç:** `server.js` + arm64 Node'u kendi prefix'inde, loopback'e bağlı, yönetilen bir servis
  olarak çalıştırmak.
- **Tek değişken:** streaming server'ın kalıcı yaşam döngüsü.
- **PASS:** servis yeniden başlatmaya dayanıklı; torrent→HTTP gerçek bir asset'te çalışıyor;
  `/stats.json` beklenen değerleri veriyor; sistem paket veritabanına hiçbir şey kurulmamış
  (`/opt/rk3588-mediabox/...` dışında değişiklik yok, apt manifest'i değişmemiş).
- **STOP:** sistem geneline paket sızdı; `FFMPEG_BIN` çözülemedi; disk/RAM bütçesi aşıldı.
- **Kapsam dışı:** transcode politikası, HDR transcode, CEC.

### S4 — MediaBox control shell (Web UI, telefon/laptop)

- **Amaç:** dashboard, ayarlar, ağ, uzaktan kumanda, teşhis.
- **Tek değişken:** yeni frontend yüzeyi. Backend kontratı S2'den değişmez.
- **PASS:** telefondan oynatma kontrolü (play/pause/stop/seek/volume), canlı telemetri, log
  görüntüleme; stremio-web aynı origin'den bozulmadan servis ediliyor.
- **STOP:** stremio-web davranışı değişti; kontrol yüzeyi oynatmayı etkiledi.
- **Kapsam dışı:** TV-local render, auth/HTTPS (açıkça "LAN-only, auth yok" olarak dokümante edilir).

### S5 — HDMI-CEC bring-up

- **Amaç:** `mediaboxd`'yi `/dev/cec0`'ın tek sahibi yapmak.
- **Tek değişken:** CEC adaptörünün yapılandırılması.
- **Kapsam:** `S_LOG_ADDRS` (Playback Device, `ALLOW_RC_PASSTHRU`), `CEC_MODE_INITIATOR |
  CEC_MODE_EXCL_FOLLOWER`, olay döngüsü, `<Active Source>` / `<Image View On>` (kullanıcı ayarı
  arkasında), `<Standby>` politikası (varsayılan yoksay).
- **PASS:** `G_LOG_ADDRS` `num_log_addrs = 1`, geçerli `log_addr`, `flags` içinde
  `ALLOW_RC_PASSTHRU`; TV kumandasının tuşları `event0`'da gerçek `KEY_*` olarak görülüyor;
  Kodi bu tuşlarla kontrol edilebiliyor; **hiçbir TV güç/kaynak komutu kullanıcı ayarı olmadan
  gönderilmiyor**; Kodi durup başladığında CEC oturumu etkilenmiyor.
- **STOP:** `S_LOG_ADDRS` `EBUSY`/hata; RC tuşları gelmiyor; TV istenmeden kaynak değiştiriyor
  veya kapanıyor.
- **Kapsam dışı:** input grab, libCEC (kalıcı olarak kapsam dışı), HDMI-CEC üzerinden ses kontrolü
  (ARC/eARC).

### S6 — Normalize input arbiter

- **Amaç:** tek aksiyon katmanı; BT/USB/CEC/WebSocket aynı tabloya.
- **Tek değişken:** `EVIOCGRAB` ve routing.
- **PASS:** dört kaynak da aynı normalize aksiyonları üretiyor; focus'a göre doğru hedefe
  gidiyor; tuş→ekran gecikmesi S5 ölçümüne göre regresyon göstermiyor.
- **STOP:** gecikme regresyonu; grab sonrası bir cihaz sessiz kalıyor; Kodi çift tuş alıyor.
- **Kapsam dışı:** TV-local UI.

### S7 — Bluetooth cihaz yönetimi

- **Amaç:** BlueZ ile eşleştirme/yeniden bağlanma, Web UI'dan yönetim.
- **PASS:** Android TV kumandası eşleşiyor, yeniden başlatmada otomatik bağlanıyor, S6 tablosuna
  düşüyor.
- **STOP:** BT stack HDMI ses veya oynatma performansını etkiliyor.

### S8 — TV-local web yüzeyi (MODEL A geçişi)

- **Ön koşul:** S1 PASS ve Kodi soğuk başlatma süresi kabul edilebilir bulunmuş olmalı.
- **Tek değişken:** compositor'sız KMS/GBM tarayıcı (WPE/Cog `--platform=drm`) ile display devri.
- **PASS:** UI→Kodi ve Kodi→UI geçişlerinin her birinden sonra kabul edilmiş VOP2/HDMI capture'ı
  byte-identical; geçiş süresi ölçülmüş; çökme sonrası arbiter tek sahibi geri getiriyor.
- **STOP:** herhangi bir geçiş sonrası renk/HDR durumu sapıyor.
- **Kapsam dışı:** Wayland/X11'e geçiş (kalıcı olarak reddedildi).

### S9 — Güvenlik ve HTTPS

- Pairing/token, remote erişim politikası, secure-context gerektiren özelliklerin açılması.

---

## 12. Kanıt / kaynaklar

### Hedef cihaz (salt okunur, 2026-09-11)

| Kanıt | Kaynak |
| --- | --- |
| Kodi tek DRM master, pid 40781 | `/sys/kernel/debug/dri/0/clients`, `lsof /dev/dri/card0` |
| Idle display durumu 1920x1080p60 SDR, Cluster0-win0 ACTIVE | `/sys/kernel/debug/dri/0/summary` |
| CEC adaptör yetenekleri ve yapılandırılmamış durumu | `/dev/cec0` üzerinde `CEC_ADAP_G_CAPS` (`_IOWR('a',0)`), `CEC_ADAP_G_PHYS_ADDR` (`_IOR('a',1)`), `CEC_ADAP_G_LOG_ADDRS` (`_IOR('a',3)`) |
| `rc-cec` keymap ve rc0 kaydı | `dmesg`: `Registered IR keymap rc-cec`, `rc rc0: dw_hdmi_qp as /devices/platform/fdea0000.hdmi/rc/rc0` |
| CEC kernel config | `/boot/config-6.1.115-…`: `CONFIG_CEC_CORE=y`, `CONFIG_CEC_NOTIFIER=y`, `CONFIG_MEDIA_CEC_RC=y`, `CONFIG_DRM_DW_HDMI_CEC=y` |
| Input envanteri | `/proc/bus/input/devices`, `/sys/class/rc/rc0/protocols` → `[cec]` |
| Kodi libCEC'siz | `strings kodi-gbm \| grep libcec` → tek satır: *"libCEC support has not been compiled in"*; `scripts/build-kodi.sh:108 -DENABLE_CEC=OFF` |
| Kodi JSON-RPC yüzeyi | canlı `JSONRPC.Introspect` — 181 method; `Player.Open.options.resume` ∈ {boolean, Percentage, **Time**}; `Input.*`, `Application.SetVolume/SetMute`, `System.*`, `GUI.*` mevcut; 42 notification |
| Kodi transportları | `ss -lntp`: `0.0.0.0:8080` (HTTP), `127.0.0.1:9090` + `[::1]:9090` (TCP). TCP 9090 `JSONRPC.Version` ile canlı doğrulandı. |
| Kurulu olmayanlar | `node`, `chromium`, `firefox`, `cog`, `weston`, `sway`, `cage`, `cargo`, `rustc`, `go`, `bluetoothctl`, `rfkill`, `cec-ctl`, `libcec` — hiçbiri yok |
| Mevcut runtime | Python 3.13.5; 8 çekirdek; 7929 MB RAM; `/` 57 G, 37 G boş; `bluetooth.service` inactive |
| Ağ | `enP3p49s0` 10.27.27.25/24 |

### Upstream kaynaklar (bu gate'te klonlanan revizyonlar)

| Depo | Revizyon | Tarih | Kullanılan dosya / davranış |
| --- | --- | --- | --- |
| `Stremio/stremio-core` | `3ac269b470854d14033f1971dfbf4022831d57f5` | 2026-09-09 | `src/types/resource/stream.rs` → `Stream::convert()` torrent kolu (`{server}/{hex infoHash}/{fileIdx\|-1}?tr=&f=`); `src/deep_links/mod.rs` → `ExternalPlayerLink{download,magnet,streaming,playlist,openPlayer,…}`; `src/models/streaming_server.rs:450/467/483/499/716/746` → `/settings`, `/casting`, `/network-info`, `/device-info`, `casting/{device}/player`, `/get-https`; `src/runtime/msg/action.rs:171` → `PlayOnDeviceArgs{device,source,time}`; `src/constants.rs:89` → `STREAMING_SERVER_URL = http://127.0.0.1:11470` |
| `Stremio/stremio-web` | `509023270583077538caed04ffde0686cfa19bd9` | 2026-09-11 | `package.json` → `5.0.0-beta.39`, `@stremio/stremio-core-web 0.62.1`, node ≥22 / pnpm ≥11; `src/routes/Player/usePlayOnDevice.ts`; `src/routes/Player/OptionsMenu/OptionsMenu.js:29,141-150` (**`external` gate'siz**); `src/routes/Player/Player.js:132-140` (`chromecast`/`tv` **shell-gated**); `src/common/Platform/shell/useShell.ts` → `active = !!globalThis?.chrome?.webview`; `src/common/CONSTANTS.js:4`; `src/routes/Settings/Streaming/URLsManager/` |
| `Stremio/stremio-linux-shell` | `c6e7cd2` (v1.2.0) | 2026-08-03 | `src/config.rs` → `STARTUP_URL = http://127.0.0.1:11470/proxy/d=https%3A%2F%2Fweb.stremio.com/`, `IPC_KEY = "LINUX"`; `src/server.rs` → `Command::new("node")` + `NO_CORS`/`SERVER_IPC_KEY`; `src/app/ipc/preload.js` → `globalThis.chrome = { webview: {…} }` enjeksiyonu (shell aktivasyonunun tam mekanizması); `flatpak/…json` → `node-v22.23.1-linux-arm64`; `data/server.js` → 6.676.491 bayt, `linguist-vendored`, endpoint'ler `/casting/ /device-info /hlsv2 /network-info /probe /proxy /settings /stats.json`, cast tipleri `chromecast`→Chromecast, `tv`→DLNA (`upnp-mediarenderer-client`, `SetAVTransportURI`), `external`→yerel oynatıcı tablosu (`fs.existsSync`) |
| `torvalds/linux` | `master` (2026-09-11 fetch) | — | `drivers/media/cec/core/cec-adap.c:2083-2087` (REPORT_PHYSICAL_ADDR + USER_CONTROL_* passthrough'tan bağımsız), `:2105-2109` (passthrough'da core cevapları atlanır), `:2149-2196` (`rc_keydown`/`rc_keyup`, `CEC_CAP_RC` + `CEC_LOG_ADDRS_FL_ALLOW_RC_PASSTHRU` koşulu) |
| kernel.org docs | `userspace-api/media/cec/cec-ioc-g-mode.html` | — | `CEC_MODE_EXCL_FOLLOWER` semantiği ve `EBUSY`; `CEC_MODE_EXCL_FOLLOWER_PASSTHRU`; monitor modları için `CAP_NET_ADMIN` |
| `Pulse-Eight/libcec` | `master` | — | `src/libcec/adapter/Linux/LinuxCECAdapterCommunication.cpp` → `mode = CEC_MODE_INITIATOR \| CEC_MODE_EXCL_FOLLOWER_PASSTHRU`, `ioctl(m_fd, CEC_ADAP_S_LOG_ADDRS, …)` |
| Kodi (bu build'in kaynağı) | `/var/tmp/kodi-src` @ 22.0b2-Piers `e513e0f` + `patches/kodi/0001..0010` | — | `xbmc/windowing/gbm/drm/DRMUtils.cpp:589` `drmSetMaster`, `:725` `RestoreOriginalMode`, `:752` `drmDropMaster` |

Doğrulanmamış / kasıtlı olarak ölçülmeyenler: Kodi soğuk başlatma süresi (S1), `server.js`'in
bu board'da gerçek torrent performansı (S3), CEC mesaj alışverişinin fiilen çalışması (S5 —
bu gate'te hiçbir CEC mesajı gönderilmedi ve hiçbir mantıksal adres talep edilmedi).

---

## 13. Tam olarak bir sonraki deney

**S1 — display arbiter feasibility.** Hiçbir kod yazılmaz, hiçbir paket kurulmaz.

```text
1. Kodi'yi kabul edilmiş HDR asset'i ile oynat; tools/vop2-sdr2hdr-capture/capture-state.sh
   ile "A" durumunu yakala (+ cadence istatistikleri).
2. scripts/run-kodi-rk3588.sh stop
3. /sys/kernel/debug/dri/0/clients ve lsof /dev/dri/card0 boş mu doğrula.
4. tools/gbm-egl-probe (veya hdr-signaling-probe) ile ikinci bir KMS istemcisini kısa süre
   master yap, çıkart, clients'ı yeniden doğrula.
5. scripts/run-kodi-rk3588.sh start  — başlangıçtan JSON-RPC'ye kadar geçen süreyi ÖLÇ.
6. Aynı asset'i aynı noktadan oynat; "B" durumunu yakala.
7. A ve B'yi karşılaştır.
```

PASS/STOP kriterleri §6.5 ve §11-S1'de.

---

## ARCHITECTURE_READY_FOR_RUNTIME_VALIDATION
