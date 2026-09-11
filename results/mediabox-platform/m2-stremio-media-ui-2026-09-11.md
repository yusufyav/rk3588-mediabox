# M2 — Stremio medya deneyimi, ön izleme ve Kodi devri (2026-09-11)

## 1. Başlangıç revizyonu

`main` = `origin/main` = `df6de14e2b51d3288be74497936268eb8922900a`, working tree temiz.
Tüm çalışma doğrudan `main` üzerinde yapıldı; yeni branch veya worktree açılmadı.

## 2. Reddedilen eski UI'dan kaldırılanlar

Kullanıcı mevcut ürün tasarımını reddetti. Kaldırılanlar:

| Kaldırılan | Yerine ne geçti |
| --- | --- |
| Dashboard "Genel Bakış" ana ekranı | Gerçek Stremio medya deneyimi |
| Sol admin sidebar (`components/Shell.tsx`) | Sağ üstte tek kontrol kümesi |
| Standalone "Oynatıcı" sayfası | Now Playing overlay'i |
| Standalone "Cihaz" sayfası | Ayarlar → Tanılama |
| Standalone "Ağ" sayfası | Ayarlar → Ağ / Tanılama |
| "Stremio — Yakında" placeholder'ı | Gerçek Stremio |
| Hash router (`nav/router.ts`) | Navigasyon medya uygulamasının |

Silinen dosyalar: `src/pages/*` (7), `components/Shell.*`, `components/PlayerControls.*`,
`nav/router.ts`, ve bunlara ait 4 test dosyası. Dead component, route veya CSS bırakılmadı.

Faydalı telemetri **çöpe gitmedi**: CPU, RAM, depolama, sıcaklık, çekirdek, ağ, DRM/ekran,
HDR, Kodi ve servis sağlığı **Ayarlar → Tanılama** altına taşındı.

## 3. Yeni ürün information architecture

```
http://10.27.27.25:8787/ui/          tek origin, tek doküman, iframe yok
  │
  ├── medya deneyimi ................ upstream Stremio Web (değiştirilmemiş)
  │     Board / Discover / Library / Calendar / Addons / Search / Detail / Player
  │
  └── MediaBox appliance shell ...... sağ üstte tek kontrol kümesi
        ├── [gear] Ayarlar ......... Oynatma · Ağ · Bluetooth · HDMI/CEC ·
        │                            Ekran · Ses · Sistem · Tanılama
        ├── Now Playing pill ....... yalnız TV'de bir şey oynarken görünür
        └── Kodi'de Oynat .......... yalnız ön izleme ekrandayken görünür
```

Ana ekranda kernel sürümü, RAM yüzdesi, CPU sıcaklığı, DRM connector adı, servis durumu
**yok**; hepsi Tanılama'da. Bu test ile zorlanıyor (`webui/tests/shell.test.tsx`).

Shell `position: fixed` + `pointer-events: none` köküne sahiptir; layout talep etmez ve
medya uygulamasının sayfasını sarmalamaz. Tasarım token'ları `:root` yerine shell
elemanına scope'lanmış ve `--mb-` ön ekli — tek dokümanda iki tasarım sistemi var.

## 4. Stremio upstream entegrasyon yöntemi

**Fork yok, iframe yok, patch yok.** Upstream stremio-web pinned revizyondan kaynaktan
derleniyor ve üretilen sayfaya yalnız **iki satır** ekleniyor:

```html
<link href="mediabox/shell.css" rel="stylesheet">   <!-- </head> öncesi -->
<script defer src="mediabox/shell.js"></script>      <!-- </body> öncesi -->
```

Kompozisyon `scripts/build-media-ui.sh` içindedir. Upstream'in hiçbir baytı değiştirilmez.

Shell medya uygulamasıyla **yalnız `window.core` üzerinden** konuşur — bu, stremio-web'in
`CoreProvider.tsx:72`'de kendi yayımladığı transport'tur. Kullanılan üç şey de upstream
UI'ın kendi dispatch ettiği belgelenmiş core action'larıdır:

| İhtiyaç | Kullanılan upstream yüzeyi |
| --- | --- |
| Akış sunucusunu bu kutuya yöneltmek | `Ctx/AddServerUrl` + `Ctx/UpdateSettings` |
| Ön izlemede seçili stream'i okumak | `getState('player')` → `deepLinks.externalPlayer.streaming` |
| Kodi'ye devretmek | `StreamingServer/PlayOnDevice` |

### iframe neden kullanılmadı

iframe iki ayrı doküman demek; bunun bedeli tam da işi mümkün kılan üç şeydir: kumanda
için tek odak ağacı, CORS'suz tek origin, ve pozisyon devri için `window.core` erişimi.
Aynı dokümanı paylaşmak üçünü de verir; bedeli yalnız shell'in neyi style edebileceği
konusundaki disiplindir ve bu disiplin kodda zorlanmaktadır.

### Akış sunucusu adresinin otomatik ayarlanması

stremio-core varsayılanı `http://127.0.0.1:11470` — masaüstü uygulaması için doğru, ağ
üzerinden bağlanan her cihaz için yanlış. Shell açılışta mevcut ayarı okur ve **yalnız
hâlâ upstream varsayılanlarından biriyse** bu origin'e yöneltir. Kullanıcının Stremio'nun
kendi ayarlarından seçtiği bir sunucuya dokunulmaz.

Doğrulandı (target, gerçek tarayıcı): `streamingServerUrl: http://10.27.27.25:8787/`

## 5. Stremio revizyonları

S0-A'da incelenen ve kabul edilen revizyonlar korundu; yeni revizyona geçilmedi.
Hepsi `packaging/upstream.env` içinde pinlidir; "latest" bağımlılık yoktur.

| Bileşen | Pin |
| --- | --- |
| `Stremio/stremio-web` | `509023270583077538caed04ffde0686cfa19bd9` (5.0.0-beta.39) |
| `Stremio/stremio-linux-shell` | `c6e7cd22e23ed6401e573fe7fe1a023fc07399a2` (v1.2.0) |
| `server.js` | sha256 `82175d7982bce864df071df93b4b3d567a401e65881a8ac579d7db0ce71dafd7`, 6.676.491 bayt |
| Node (arm64) | v22.23.1, sha256 `0294e8b915ab75f92c7513d2fcb830ae06e10684e6c603e99a87dbf8835389c1` |
| pnpm (yalnız build) | 11.26.0, `--frozen-lockfile` |

Node sürümü upstream'in kendi arm64 paketlemesinin pinlediği sürümdür; yani cihaz
Stremio'nun fiilen desteklediği bir kombinasyonu çalıştırıyor.

`server.js` upstream'de de kaynaktan derlenmeyen, vendored bir artefakttır; bu yüzden
build ile değil içerik hash'i ile pinlenmiştir. Her iki artefakt da cihaza inmeden
**önce** checksum ile doğrulanır (`scripts/deploy-mediabox.sh`).

## 6. Streaming server mimarisi

```
tarayıcı ──> mediaboxd :8787 ──> 127.0.0.1:11470 (server.js, Node 22 arm64)
                   │                    └── torrent → HTTP, /hlsv2, /probe, /settings
                   └── Kodi JSON-RPC (loopback)
```

- systemd unit: `packaging/systemd/stremio-server.service`, `stremio` kullanıcısı,
  `enabled`, `Restart=on-failure`, açık yollar, `ProtectSystem=strict`.
- `FFMPEG_BIN`/`FFPROBE_BIN` tanımlı (`ffmpeg 7:7.1.5-0+deb13u1`, dağıtım paketi).
  Bunlar olmadan sunucu stream'i probe edemiyor, transcode'a düşüyor ve transcode de
  yapamadığı için oynatılabilir medyada **"video is not supported"** veriyordu.

### Proxy neden origin kökünde

Proxy önce `/server/` altına konmuştu ve bu transcoding'i bozdu: `stremio-video`
HLS adresini `url.resolve(streamingServerURL, '/hlsv2/…')` ile kurar; baştaki `/`
origin'e çözülür ve subpath atılır. Subpath arkasındaki bir akış sunucusu bu yüzden
transcode **edemez**. Bu nedenle akış sunucusu origin kökünü işgal eder; mediaboxd
kendine `/`, `/api/` ve `/ui/` ayırır, gerisi akış sunucusudur.

### Torrent → HTTP

Mimari yerinde ve doğrulandı: engine oluşuyor, tracker cevap veriyor
(`swarmSize: 55`), HTTP uç noktası proxy üzerinden erişilebiliyor. Ancak bu ağda
**peer bağlantısı hiç denenmiyor** (`connectionTries: 0`, `sources: []`, 120 s sonra
`downloaded: 0`) — tracker peer'ları biliyor ama giden BitTorrent bağlantıları
kuruluyor değil. Bu bir ürün kusuru değil, ağ koşuludur; bkz. §21.

Kodi'ye **hiçbir durumda** magnet, torrent metadata objesi veya blob URL verilmez;
yalnız oynatılabilir HTTP/file URL'i verilir (`validate_media_url`, testlerle zorlanır).

## 7. Ön izleme mimarisi

Stremio'da bir stream'e tıklamak medya uygulamasının **kendi** oynatıcısını açar. Ön
izleme budur: TV'yi bağlamadan önce doğru içerik mi, kalite yeterli mi görmek için.
MediaBox bunun için hiçbir şey yapmaz ve **Kodi başlamaz** (TEST C ile kanıtlı).

## 8. Kodi devri mimarisi

```
Stremio ön izleme
   │  kullanıcı "Kodi'de Oynat" der
   ▼
shell: canlı playhead'i okur → ön izlemeyi durdurur → StreamingServer/PlayOnDevice
   ▼
POST /casting/mediabox-tv/player  { source, time }      (upstream sözleşmesi)
   ▼
mediaboxd: URL doğrula → cast.handoff olayı → Player.Open + options.resume
   ▼
Kodi → RKMPP → DRM PRIME → VOP2 → HDMI   (kabul edilmiş yol, dokunulmadı)
```

MediaBox cihazı Stremio'ya `external` tipiyle sunulur: upstream stremio-web
`chromecast`/`tv` tiplerini yalnız kendi masaüstü shell'inde gösterirken `external`
tipini sade tarayıcıda da gösterir (S0-A §3.2c).

**Kaynak kimliği:** Kodi'ye aynı path ve query verilir. Akış sunucusundan gelen bir
URL'de origin loopback'e çevrilir — böylece production oynatmada tek bir medya baytı
Python proxy'sinden geçmez; ön izleme proxy'yi kullanmaya devam eder çünkü uzaktaki
tarayıcı cihazın loopback'ine erişemez. Origin yeniden yazımı **host kontrolü** ile
sınırlıdır: bir addon'un kendi host'undan verdiği stream olduğu gibi devredilir.

Ön izleme, `Player.Open`'dan **önce** durdurulur. Tek bir torrent engine'inin iki ayrı
seek pozisyonunu beslemesi her iki tarafı da buffer'a sokar (S0-A §4.2.5).

## 9. Pozisyon devri

Ölçülen (target, gerçek tarayıcı, iki bağımsız koşu):

| Koşu | Ön izleme pozisyonu | Kodi'ye verilen resume | Fark |
| --- | --- | --- | --- |
| 1 | 30,178 s | 30,237 s | **0,059 s** |
| 2 | 30,351 s | 30,395 s | **0,044 s** |

Kodi devirden sonra `state: playing`, `duration: 120`, örnekleme anında `position`
38,0–38,3 s — yani ~30 s'den devam edip oynamaya devam etmiş.

**Upstream sınırı, gizlenmiyor:** upstream'in kendi cast menüsü pozisyonu **her zaman 0**
gönderir. `OptionsMenu.js:149` `onClick={playOnDevice}` yazar, `Option` bileşeni ise
`onClick(deviceId)` diye tek argümanla çağırır, dolayısıyla `usePlayOnDevice`
`time: 0` varsayılanına düşer. Shell'in ayrı bir kontrol sunmasının sebebi tam olarak
budur: aynı action, pozisyon doldurulmuş hâlde.

## 10. Uzaktan kumanda gezinmesi

Aday kümesi yalnız shell'in kendi kontrolleri değildir: Stremio kontrollerini olağan
yolla (`tabindex="0"`, gerçek `button`) işaretler ve koltuktan bakınca tek bir UI vardır.
Kutusu olmayan elemanlar elenir.

İki istisna medya uygulamasını bütün bırakır: Stremio oynatıcısında ok tuşları ona
aittir (orada seek ederler), ve geri tuşu ancak shell gerçekten tükettiyse sahiplenilir.

**Düzeltilen gerçek kusur:** medya uygulaması arama kutusu odaklı açılıyor ve ok tuşları
metin alanında caret'e bırakılmıştı — yani televizyonda kumanda ana ekrana geliyor ve
arama kutusundan **hiç çıkamıyordu**. Artık yukarı/aşağı tek satırlık bir input'tan
odağı çıkarır; sol/sağ caret'te kalır.

Target'ta klavye-only doğrulama (fare kullanılmadan):

```
focus walk: DIV → A[#/detail/movie/tt37275]:The Secret Woman
                → A[#/detail/movie/tt32332]:I Want Your Sex
                → A[#/detail/movie/tt14173]:The Invite
                → A[#/detail/series/tt1196]:The Mentalist
Enter      → #/detail/series/tt1196946      (detay açıldı)
Backspace  → board                          (geri döndü)
Enter (gear) → Ayarlar açıldı               ArrowDown → panel içinde odak hareket etti
Escape     → panel kapandı, arkası bozulmadı
```

`results/mediabox-platform/evidence/remote-nav.json`

## 11. Settings / Diagnostics ayrımı

Ayarlar ekranda duran bir yer değil, köşedeki tek kontrolün arkasındaki bir paneldir.
Bölümler (target'tan okundu): `Oynatma · Ağ · Bluetooth · HDMI / CEC · Ekran · Ses ·
Sistem · Tanılama`.

Servis desteği olmayan bölümler (Bluetooth, HDMI/CEC, Ses) bunu açıkça yazar;
çalışıyormuş gibi görünecek anahtar **üretilmez**. Ana ekranda "yakında" kartı yoktur.

Tanılama ürünün en derin ekranıdır ve telemetrinin taşındığı yerdir.

## 12. Backend değişiklikleri

| Değişiklik | Neden |
| --- | --- |
| `mediaboxd/stremio.py` (yeni) | Akış sunucusu sınırı, cast hedefi, Kodi devri |
| `GET /api/v1/stremio` | Akış sunucusu erişilebilirliği/sürümü, Tanılama için |
| `GET /api/v1/cast` | Devredilen aktif oturum |
| `POST /api/v1/cast/kodi` | Shell'den pozisyonlu devir |
| `health.media` | Shell'e mount ve cast device id'sini bildirir |
| Statik serving'de Range | Medya cihazı medya sunar; Range olmadan hiçbir oynatıcı seek edemez |
| Proxy timeout'unun ikiye ayrılması | Kontrol çağrısı saniyeler, soğuk torrent dakikalar sürer |
| Medya yüzeyi için ayrı CSP | Stremio tasarımı gereği üçüncü taraf addon istemcisidir |

Sürüm `0.1.0` → `0.2.0`.

## 13. Güvenlik

- Tarayıcı ne Kodi'ye ne de akış sunucusuna doğrudan bağlanır; tek yol mediaboxd'dir.
  Frontend'de `:8080` referansı yoktur (test ile zorlanır).
- Proxy hedefi config'ten gelen bir **loopback** origin'dir, istekten alınmaz ve
  load-time'da doğrulanır. Mount open proxy'ye dönüşemez.
- `server.js`'in kendi "herhangi bir URL'i getir" `/proxy` uç noktası **iletilmez**
  (`403 PROXY_DENIED`).
- Yalnız `GET`/`HEAD`/`POST`. `server.js`'in ürettiği `Access-Control-Allow-*` başlıkları
  iletilmez. Wildcard CORS yok, `OPTIONS` hâlâ reddediliyor.
- Kodi'ye yalnız `http`/`https`/absolute `file` kabul edilir; `magnet` reddedilir.
- Medya yüzeyi CSP'si: script ve worker yalnız same-origin (+ stremio-core için
  `wasm-unsafe-eval`), `unsafe-eval` yok, inline script yok, `object-src 'none'`,
  `frame-ancestors 'none'`. `img-src`/`connect-src`/`media-src` https'e açıktır çünkü
  Stremio'nun katalog/poster/stream kaynakları ancak çalışma anında bilinir.

**Kapatılmayan borç:** Kodi'nin LAN'a açık, auth'suz JSON-RPC `:8080` portu ve akış
sunucusunun `*:11470` dinlemesi bu milestone'da kapatılmadı. Gerekçe §21'de.

## 14. Host testleri

| Test | Sonuç |
| --- | --- |
| Backend suite (`tests/run-mediaboxd-tests.sh`) | **57/57 PASS** |
| Frontend TypeScript | **PASS** |
| Frontend suite (vitest) | **68/68 PASS** |
| Frontend production build | **PASS** |
| Upstream stremio-web build (pinned rev, `--frozen-lockfile`) | **PASS**, 2 uyarı (asset boyutu) |

Yeni backend kapsamı: casting listesi birleştirme, `external` tip, ölü akış sunucusu,
devir, kaynak kimliği, ms→s pozisyon, geçersiz cast hedefi, magnet reddi, eksik source,
header injection, negatif/metin `time`, devir öncesi olay sırası, shell cast endpoint'i,
Range iletimi, CORS sızıntısı yok, open relay reddi, 502, method allowlist, mount
sınırları, upstream doğrulama, Range ayrıştırma.

Yeni frontend kapsamı: ana yüzeyde telemetri yok, admin navigasyonu yok, "yakında" yok,
shell layout talep etmiyor, ayarlar ikincil, desteklenmeyen bölümler dürüst, tanılama
telemetriyi taşıyor, ön izleme ile Kodi ayrı aksiyonlar, pozisyon iletimi, akış sunucusu
URL benimseme kuralları, iki uygulamanın kontrolleri üzerinde kumanda gezinmesi, metin
alanından çıkış, oynatıcıda ok tuşlarının medya uygulamasına bırakılması.

## 15. Target testleri

`http://10.27.27.25:8787/ui/`, gerçek Chromium 152, 1920×1080.

| Test | Sonuç | Kanıt |
| --- | --- | --- |
| **A — Web** | **PASS** | Admin dashboard değil, gerçek medya yüzeyi. `adminWords: []` |
| **B — Stremio** | **PASS** | 60 gerçek katalog bağlantısı; detay: tür/yıl/puan/oyuncu/özet |
| **C — Ön izleme** | **PASS** | `readyState: 4`, oynuyor; Kodi `idle` kaldı |
| **D — Kodi devri** | **PASS** | `source == kodiSource`; Kodi `playing`, `duration: 120` |
| **E — Pozisyon** | **PASS** | 0,044 s ve 0,059 s tolerans (iki koşu) |
| **F — Kumanda** | **PASS** | Faresiz: home → başlık → detay → geri → ayarlar → geri |

Servis durumu: `mediaboxd` ve `stremio-server` **active/enabled**, `NRestarts=0`.
Node `v22.23.1`, `server.js` sha256 `82175d79…` (pinlenmiş değerle aynı).
Akış sunucusu `/settings` → `serverVersion 4.21.0`.

## 16. Screenshot / kanıt yolları

Görsel kabul **yapıldı**. Screenshot'lar build artefaktı olduğu için repoya
commit edilmedi; yol ve checksum'lar aşağıdadır.

Dizin: `/var/tmp/mediabox-m2/evidence/` (9,3 MB)

| Dosya | sha256 |
| --- | --- |
| `01-home-1920x1080.png` | `489406b0154da3deca12eee4615a03d63f3bd00e6eb7a1dcf0f55ab49fec81b7` |
| `02-detail-1920x1080.png` | `3ff88e605f2a34c306db05eff744e36188b0098112e6b0074ca69fad4fc9448b` |
| `03-preview-1920x1080.png` | `fe00864556af1add17a2a32171858f982170a75c5293b57d0009de5ac81458c8` |
| `04-now-playing-1920x1080.png` | `c9200d958dafadd117f15350856f6e426bd0cdd0f9cc774a8fed962ef2ba54d8` |
| `05-settings-1920x1080.png` | `e110972556995cbceda0163616842f34acde3f7bae71b9fc3276e1bb3713917f` |
| `06-diagnostics-1920x1080.png` | `b5d41432922967ab4b707f7ff35c3a20b25016e68039ec7d1a5b98844da8e082` |
| `07-mobile-390x844.png` | `cd460b01da380884f85f12d2f5226fd685bfcf70a107057083c57ded25cd185f` |
| `08-remote-nav-detail-1920x1080.png` | `2896d5b3cf3b9a045ae63159957f96b41cfba3f61b60ef26a0e7ddb6474cd72f` |

Makine-okunur sonuçlar repoda: `results/mediabox-platform/evidence/visual.json`,
`results/mediabox-platform/evidence/remote-nav.json`.

Mobil (390×844): shell var, ayarlar erişilebilir, **yatay kaydırma yok**.

## 17. Gerçek Web UI URL'i

```
http://10.27.27.25:8787/ui/
```

## 18. Servis durumu

| Birim | Active | Enabled | NRestarts |
| --- | --- | --- | --- |
| `mediaboxd.service` | active | enabled | 0 |
| `stremio-server.service` | active | enabled | 0 |

Kurulum: `/opt/rk3588-mediabox/{mediaboxd,webui/dist,node,stremio-server}`,
durum `/var/lib/stremio-server`, config `/etc/mediaboxd.toml`.

## 19. Final HEAD

Rapor öncesi entegre kod/kanıt HEAD'i: `cbc837353175b16f103c43c67219d0e640c884c1`.
Bu rapor onun üzerine tek commit olarak eklendi ve `main` HEAD'i odur.

`df6de14`'ten bu yana 8 commit, hepsi doğrudan `main` üzerinde:

```
docs: document the MediaBox M2 media experience
fix(webui): let a remote get out of the search box
fix(stremio-server): give the server an ffmpeg to work with
fix(mediaboxd): give the streaming server the origin root it needs
feat(mediaboxd): serve static assets by range, and give media its own timeout
build: pin and compose the Stremio media surface
redesign(webui): replace the admin dashboard with an appliance shell
feat(mediaboxd): add Stremio streaming-server boundary and Kodi cast target
```

## 20. origin/main

`git push origin main` yapıldı; `main` ve `origin/main` aynı commit'tedir ve
working tree temizdir.

## 21. Bilinen eksikler

**HIGH — Torrent peer bağlantısı bu ağda kurulmuyor.** Akış sunucusu engine'i
oluşturuyor ve tracker 55 peer bildiriyor, ama 120 saniyede tek bir bağlantı denemesi
bile olmuyor (`connectionTries: 0`, `sources: []`). Giden BitTorrent trafiğinin bu ağda
engellendiğini gösteriyor. Bu nedenle **torrent kaynağının uçtan uca oynatılması
doğrulanamadı**; doğrulanan şey mimarinin yerinde olduğudur. Preview→Kodi kabul akışı,
bu koşuldan bağımsız olarak, yerel olarak üretilmiş yasal bir test klibiyle (120 s,
`testcard.mp4`, ffmpeg ile sentetik olarak üretildi — üçüncü taraf içerik değil) uçtan
uca koşturuldu. Gerçek torrent oynatması, giden BitTorrent'e izin veren bir ağda
tekrar denenmelidir.

**HIGH — Kodi `:8080` ve akış sunucusu `:11470` hâlâ LAN'da dinliyor.** Ürün yolu
artık tektir (`Browser → mediaboxd → Kodi`) ve frontend bu portlara hiç gitmez, ama
portların kendisi kapatılmadı. `server.js` bind adresi için desteklenen bir seçenek
sunmuyor (`.listen(port)`, hostname yok); Kodi'nin HTTP sunucusunu kapatmak ise
mediaboxd'nin endpoint keşfini kırar ve TCP 9090 transportuna geçmeyi gerektirir.
İkisi de bu milestone'un güvenli kapsamı dışında kaldı ve net bir rollback'i yok.
Ayrı bir hardening işi olarak ele alınmalıdır.

**MEDIUM — Ön izleme baytları Python proxy'sinden geçiyor.** Production oynatma
(Kodi) proxy'yi atlar, ama tarayıcı ön izlemesi `http.server` tabanlı proxy üzerinden
akar. Kısa ön izleme için ölçüldüğü kadarıyla yeterli; uzun/yüksek bit hızlı ön izleme
için ayrı bir ölçüm yapılmadı.

**MEDIUM — Donanım hızlandırmalı transcoding yok.** `hls-converter` qsv/nvenc/vaapi
profillerinin üçünü de bu kartta eleyor; RKMPP profili sunmuyor. Transcode gereken
stream'ler CPU ile kodlanır. Kabul edilen Kodi oynatma yolu bundan etkilenmez.

**LOW — Upstream'in iki harici script'i CSP tarafından engelleniyor**
(`gstatic` cast sender, Apple ID). Chromecast gönderimi ve Apple ile giriş çalışmaz;
e-posta ile giriş, katalog, addon ve oynatma etkilenmez.

**LOW — Upstream cast menüsü pozisyon taşımaz.** §9'da açıklandı; shell'in kendi
kontrolü taşır.

**Kapsam dışı bırakılanlar (bilinçli):** CEC probe'u tekrarlanmadı, Bluetooth eşleştirme
araştırılmadı, HDR/OSD kabul kampanyası tekrarlanmadı, kernel/DT/VOP2/Kodi'ye
dokunulmadı, reboot yapılmadı.

---

## MEDIABOX_M2_MEDIA_EXPERIENCE_PASS
