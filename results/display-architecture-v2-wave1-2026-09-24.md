# Display Architecture v2 — Wave 1

Tarih: 2026-09-24 · Dal: `display-architecture-v2-wave1` · Cihaz: Orange Pi 5 Plus `10.27.27.24`, kernel `6.1.115-vendor-rk35xx`

## Baseline

| | Değer |
| --- | --- |
| Başlangıç `main` | `7765f7e7ae63374c93292d4d75181f898dbf3e1d` (= `origin/main`, çalışma ağacı temiz) |
| Owner | UI (`mediabox-tv`, DRM master), oynatma yok |
| Mod / bus | `3840x2160p60`, `RGB888_1X24` (debugfs summary) |
| Sink | SONY TV `*00`, HDMI-A-2, EDID 256 bayt, legacy checkvalue `716b` |
| EDID SHA-256 | `1175a696da42d0dc913a90983653ceef0ba6572ba64f85c1c8826ab43345fc54` |
| Offer | 41 mod; auto `3840x2160p60`; 4K60 HDR `ycbcr422:10` |
| FDStore (UI çalışırken) | boş (`No file descriptors in fdstore`) |
| Observer | yok (bu wave'de eklendi) |

Baseline offer/plan cihazdan alındı ve sonraki karşılaştırmalarda referans olarak kullanıldı.

## Gate 0 decisions respected

- **G0-1:** Observer kalıcı DRM fd tutmuyor. Normal tur sysfs, uevent, debugfs ve DT okuyor. Fiziksel doğrulama: observer'ın fd listesinde `/dev/dri` yok ve observer DRM `clients` tablosunda hiç görünmedi.
- **G0-2:** `drm_query` AM-2 sırasını izliyor: transition lock (`LOCK_NB`) → `clients` tablosunda master var mı → read-only, non-master open → açılıştan sonra "bu pid master mı" kontrolü → `GETCONNECTOR count_modes≥1` (forced probe yok) → close. Master yoksa DEFER. open → master → DROP_MASTER modeli kullanılmıyor.
- **G0-3:** FDStore koduna ve unit'ine dokunulmadı (`git diff main -- fdstore.rs mediabox-tv-ui.service` boş). Observer herhangi bir owner'a sıralanmıyor ve lifetime keeper değil.
- **G0-4:** debugfs dizini dinamik çözülüyor: card → `/sys/class/drm/cardN/dev` → minor → `dri/<minor>` → `name` içindeki `dev=` doğrulaması.
- **G0-5:** Audio ve CEC transmitter'a yapısal bağla (Exact) bağlı. Connector → transmitter bağı hiçbir zaman Exact değil. Kanıt: PA, extcon, PHY clock. Eşit adaylar Ambiguous olur.

## P1 Canonical Timing

**Uygulama**
- `mediabox-core::timing`:
  - `ModeTiming`: `drm_mode_modeinfo`'nun bütün timing alanları ve bit 0–13 bayrakları. Stereo ve aspect ratio kimliğe dahil değil (`drm_mode_match` ile aynı).
  - `Refresh {num, den}`: `drm_mode_vrefresh` semantiği (interlace ×2, DBLSCAN/vscan ile bölme).
  - `TimingKey` biçimi: `t1:<clock>:<hd,hss,hse,ht,hskew>:<vd,vss,vse,vt,vscan>:<flags hex>`.
- Link hızı mainline `drm_hdmi_compute_mode_clock()`'tan kural kural port edildi ve Hz cinsinden karşılaştırılıyor. DBLCLK ×2; vendor `dw_hdmi_rockchip_select_output()` da aynısını yapıyor (kaynak yorumunda belirtildi).
- CTA tablosu, çalışan 6.1.115 ağacının `drm_edid.c` dosyasından tam timing + bayraklarla yeniden üretildi (154 giriş; eski tabloyla eski alanlarda 154/154 aynı).
- VIC eşleştirmesi `drm_match_cea_mode` gibi: `KHZ2PICOS` ile clock, alternate clock, alternate timings.
- DTD ayrıştırması `drm_mode_detailed()` gibi (sync offset/genişlik, interlace quirk, polarite).
- Kodi `screenmode` interlaced modlarda `istd` ve alan hızı kullanıyor; sway alan hızını kullanıyor.
- `OutputModeOffer` alanına `timing_key` ve `timing` eklendi (additive). TV kernel modunu tam timing ile kuruyor ve seçimi key ile eşliyor (`b8ba062`).

**Testler:** core 7 yeni test; platform `tests/timing.rs` (7); `tests/output.rs` +2. Kapsanan senaryolar:
- 1080i60/1080p30: VIC 5/34, i60 ≠ p30.
- 2160p23.976/24/29.97/30/59.94/60: VIC 93/95/97, rasyonel refresh, 6 farklı key.
- VIC 6/7/21/22: DBLCLK, RGB8 = 27 MHz; 25 MHz sink'te `OverSink{27000}`.
- Aynı etiket, farklı timing: polarite veya sync farklıysa key farklı.
- Kodi `0192001080060.00000istd`, sway `1920x1080@60.000Hz`.
- Offer JSON round-trip; key taşımayan eski JSON da okunuyor.

**Sapmalar**
- Kalıcı niyet (`ResolutionChoice` ±5 mHz, renkler etikete göre) P9'a kadar legacy eşleşmeyle kalıyor. Kodda "kimlik değil" diye belgelendi.
- Offer aynı etiketi taşıyan modlardan tek birini gösteriyor (CTA olanı). Bu UX korundu; gösterilen modun key'i taşınıyor.
- Aspect ratio userspace'e gelmediği için VIC 97 ile 107 ayırt edilemiyor. Bu önceden de böyleydi. EDID'de SVD ile beyan edilen VIC'ler için beyan edilen VIC korunuyor.

## P2 EDID / identity

**Uygulama (`mediabox-platform::edid`)**
- Header, base checksum, extension sayısı ve extension başına checksum kontrol ediliyor. Kısmi, eksik ya da beyan edilmemiş bloklar ayrıca raporlanıyor.
- CTA kuralları: revision < 3 ise data block okunmuyor (`cea_db_iter` gibi); DTD offset aralığı kontrol ediliyor; data block taşması koleksiyonu orada durduruyor.
- `EdidStatus` değerleri: Absent, InvalidBase, Truncated, ValidWithDroppedExtensions, Valid. `EdidIssue` teşhis içindir; atılan bir blok asla capability'ye dönüşmez.
- Kernel'den bilinçli bir fark: kernel bozuk checksum'lı bir CTA bloğunu yine kabul eder; bu parser atar.
- Kimlik: tam ve geçerli bir EDID'in alınan byte'larının SHA-256'sı. Truncated okumada kimlik yok.
- `edid_checkvalue` legacy ve teşhis amaçlı kaldı. `OutputOffer.edid_sha256` ve `SinkIdentity.sha256` ek alanlar olarak geldi. `output.json` migrate edilmedi (P9).
- `CtaCapabilities` şunları ayrı alanlarda tutuyor: HDMI VSDB (PA, DC_30/36/48/Y444, Max_TMDS), HF-VSDB, HF-SCDB (0x79), Y420VDB, Y420CMDB, HDR Static (EOTF bitleri, SM Type 1, luminance), Colorimetry (BT.2020 RGB/YCC/cYCC, DCI-P3, …).
- HDR politikası değişmedi (`SinkVideo.st2084/hlg` önceki gibi).
- Yeni teşhis komutu: `mediabox-platform edid [--json]`.

**Testler:** `tests/edid.rs` (16). Senaryolar:
- `hdmi20_600mhz_hdr10_bt2020`, `hf_scdb…`, `hdmi14_no_deep_color`, `sdr_deep_colour_no_hdr`, `y420_only_4k60`
- `invalid_extension_checksum`, `truncated_cta_block`, malformed length, too-short blok, DTD offset sınırları, rev < 3, bilinmeyen extension, beyan dışı blok, invalid base/absent
- `same_legacy_checkvalue_different_sha256`
- mutasyon ve kesme taraması: her bayta 8 değer, checksum onarılmış hâliyle birlikte; 0..(len+130) uzunluk; panik yok ve geçersiz EDID'den capability üretilmiyor

Sony yakalaması yalnız provenance olarak kullanıldı. `SONY_600` fixture'ı, Plus'taki HDMI-A-2'nin yayımladığı EDID ile byte byte aynı (SHA eşleşiyor).

Test fixture'ının base bloğu checksum'sızdı; gerçek bir EDID gibi checksum eklendi.

## P3 Source Profile

**Uygulama (`mediabox-platform::source`)**
- `SourceProfile` üç parçadan oluşuyor: product scope (SoC, kernel serisi, transmitter compatible, KMS sürücüsü), vendor ABI (`color_format`/`color_depth` enum ad=değer, `HDR_OUTPUT_METADATA`, `Colorspace`) ve `SourceCaps`.
- `rk3588-vendor61-dw-hdmi-qp@1` bugünkü değerleri taşıyor: 600 MHz, 10 bpc, 4 format.
- Uyumsuzlukta `conservative-rgb8-sdr@1` kullanılıyor (RGB, 8 bpc, SDR, kernel'in listelediği modlar) ve gerekçe raporlanıyor. Property'ler okunamadıysa sonuç "properties unverified" olur; bu bir uyumsuzluk sayılmaz.
- TV profili, master olarak zaten tuttuğu fd üzerinden property'leri okuyarak çözüyor (yeni DRM open yok). Vendor enum değerleri profil ABI'sinden geliyor. ABI'siz profilde vendor property yazılmıyor.
- `OutputLink.source_profile` ve `mediabox-platform source` komutu eklendi.

**Runtime imza sonucu (Plus):** TV logu ve offer `rk3588-vendor61-dw-hdmi-qp@1 (matched)` diyor. Observer da AM-2 ile okuduğu property'lerle `matched` buldu.

**Offer karşılaştırması (P1–P3 ara deploy):**
- **Beklenen:** baseline ile birebir aynı. **Gerçekleşen:** 41 modun etiket, VIC, clock, total, preferred, auto SDR/HDR ve izinli hücre sayısı aynı; Kodi screenmode, whitelist, browser_mode ve bütün `colour` satırları aynı.

## P4 Topology / debugfs / CEC

**Uygulama**
- `Confidence` beş seviyeli: Exact, Measured, Derived, Ambiguous, Unavailable. `Binding` artık bunun alias'ı.
- `topology.rs` kanıt olarak şunları topluyor:
  - extcon kablo durumu
  - CEC PA (`debugfs cec/<n>/status`) ↔ EDID VSDB PA
  - PHY pixel clock (`supplier:phy` devlink → `clock-output-names` → debugfs clk) ↔ summary dclk
- Karar kuralları:
  - Tek adaptörde PA eşleşmesi Measured olur ve connector sırasını geçer.
  - Tek kablo var ve sıra uyuşuyorsa Measured.
  - Ölçülecek bir şey yoksa Derived.
  - Eşit adaylar veya çelişki Ambiguous olur; hiçbir aday seçilmez.
  - Aynı transmitter'a iki iddia varsa daha güçlü olan kalır.
- `debugfs.rs`: dinamik çözümleyici ve `Unknown` birinci sınıf sonuç. Production'dan `dri/0` literali kaldırıldı: daemon, TV, `mediabox-browser-verify`, `mediabox-kiosk-smoke` (`mediabox-platform dri-debugfs`).
- Daemon: bütün adaptörler dinlenmeye devam ediyor. Gönderim `cec::choose()` üzerinden yapılıyor. Hedef, seçili çıkışın actionable confidence'taki adaptörü; bu adaptörün PA ve LA'sı olmalı ve PA seçili EDID'in PA'sına eşit olmalı. Aksi halde hata döner; başka adaptöre fallback yok.
- `Devices.cec` ambiguous durumda boş dönüyor. Audio, ambiguous durumda da eski davranışı koruyor (sıra adayını izliyor).

**Testler**
- `tests/topology_evidence.rs` (10):
  - 2 DRM cihazı, display minor 1, `dri/0` tuzağı; yanlış `name` → Unknown; debugfs yok
  - Plus topolojisi
  - iki canlı adaptör, seçili ikinci transmitter → cec1
  - PA'nın sırayı geçmesi
  - aynı PA → Ambiguous, CEC yok
  - çelişkili PA; PHY kapalı
  - confidence sıralaması
- `mediaboxd-rs::cec` (5): `cec0 live + cec1 live → cec1`; ambiguous → gönderim yok; f.f.f.f / LA yok → hata, fallback yok; PA uyuşmazlığı; adaptör açık değil.
- `tests/topology.rs` güncellendi. "İki kablo, ayırt edici kanıt yok" durumu artık Ambiguous (spesifikasyona göre).

**Fiziksel eşleme (salt-okur)**
- `HDMI-A-2 → fdea0000.hdmi` measured. Kanıtlar: CEC `3.0.0.0` = EDID PA; `clk_hdmiphy_pixel1` 594 MHz = dclk; `fde80000.hdmi` kablosuz → `cec1` measured.
- debugfs: keşif sonucu `minor 0` (`card0 226:0`, `name dev=display-subsystem`). Bu sabit bir değer değil, keşfin sonucu.
- CEC: `mediaboxctl cec devices` (yoklama; standby/wake gönderilmedi) sonrası daemon logunda `CEC target /dev/cec1 (Measured, selected output)`.
- Dual-sink: **NOT PHYSICALLY VERIFIED** (ikinci sink bağlanmadı; yalnız host testleri var).

## P5 Observer shadow

**Mimari:** `mediabox-display-observer` (platform crate bin, `observer.rs`). Snapshot `/run/mediabox-display-observer/snapshot.json` dosyasına atomik yazılıyor. İçeriği:
- generation, material digest, boottime/realtime
- KMS cihazı, connector, bağlantı, EDID SHA/status, capability ve kernel-mod parmak izleri
- transmitter, audio, CEC ve güvenleri; kaynak profili
- mevcut mod, bus format, debugfs durumu, DRM master
- daemon karşılaştırması

**DRM erişimi:** AM-2 `drm_query` yalnız bu durumlarda çalışıyor: yeni connector+EDID görüldüğünde veya önceki okuma ertelendiyse. Normal turda open yok. Host testi: tekrarlanan turlarda open sayısı 1'de kalıyor.

**Reconcile:** startup + DRM/CEC uevent (hint, birleştirilmiş) + ≤5 sn periyot.
- Generation = `(boot_id, seq)`; yalnız materyal değişince ilerliyor.
- `/run`'da saklanıyor (`RuntimeDirectoryPreserve=yes`), restart'ta sürüyor.
- Ertelenmiş mod okuması bilinen son parmak izini taşıyor. Bu, fiziksel testte bulunan bir kusurun düzeltmesi: ertelemede generation 1→2→3 ilerliyordu.

**Testler:** `tests/observer.rs` (7) ve `drm_query` birim testleri (5):
- event kaybının periyotla yakalanması
- uevent hint
- masterless → primary açılmıyor + "DRM query deferred: no active master"
- geçiş sürerken açılmıyor
- açılırken master olunursa fd hemen kapanıyor
- restart ve generation sürekliliği
- EDID değişimi → yeni generation

**systemd/güvenlik:**
- root, `CapabilityBoundingSet=` boş, `NoNewPrivileges`, `DevicePolicy=closed` + `DeviceAllow=char-drm r`
- `ProtectSystem=strict`, yazılabilir alan yalnız kendi runtime dizini
- `RestrictAddressFamilies=AF_UNIX AF_NETLINK`, `IPAddressDeny=any`; LAN soketi yok
- `SystemCallFilter=@system-service` (ayrıcalıklı gruplar hariç), `MemoryDenyWriteExecute`
- Owner'lara `Before`/`After` bağı yok
- Bu kısıtlamalarla debugfs ve `clients` okunabildiği cihazda doğrulandı.
- `deploy-mediabox-v3.sh`'a bağlandı. Release manifest ve installer değişmedi.

## Host test results

| Paket | Sonuç |
| --- | --- |
| mediabox-core | 42 geçti |
| mediabox-platform | unit 11, color_modes 9, edid 16, observer 7, output 16, source 2, timing 7, topology 20, topology_evidence 10 — hepsi geçti |
| mediaboxd-rs | 84 geçti (+ fan_overlay 3) |
| mediaboxctl / mediabox-cec | 9 / 5 geçti |
| mediabox-tv (native) | 233 geçti; derleme uyarıları önceden var olan kodda |
| mediabox-ui (wasm32 check) | derleniyor |
| `tests/run-host-tests.sh` | `all host tests passed` (her faz sonunda) |

**Flaky test:** `transition::tests::a_dead_daemon_does_not_leave_a_transition_behind` P2 ve P3 gate'lerinde birer kez düştü. `a_transition_is_visible_while_it_runs_and_gone_after` P5 gate'inde bir kez düştü.
- İzole tekrarlar: 10/10, 5/5 ve 5/5 geçti.
- Tam `--lib` tekrarları: 1/5 ve 1/3 düştü.
- Yalnız paralel koşuda görülüyor. Olası neden: diğer testlerin `fork` ettiği çocuk süreçler kilit fd'sini `exec`'ten önceki kısa pencerede miras alıyor. Display ile ilgisiz; düzeltilmedi.

## Plus physical results

Deploy edildi: daemon, platform, TV ve observer. Normal restart'lar dışında reboot yok; kernel, boot arg, paket, ağ ya da kalıcı ayar değişikliği yok.

| Adım | VP / mod / bus | Masterless pencere | Yeni master | Observer |
| --- | --- | --- | --- | --- |
| UI (başlangıç) | ACTIVE 3840x2160p60 RGB888 | — | mediabox-tv | seq 1, `agrees:true` |
| UI → Kodi | sürekli ACTIVE 4K60 RGB888 | 3,6 sn | kodi-gbm | seq 1, aynı SHA; `clients`'ta yok |
| Kodi → UI | sürekli ACTIVE 4K60 | 1,5 sn | mediabox-tv; FDStore fd geri alındı ve kapatıldı | seq 1 |
| UI → browser | sürekli ACTIVE 4K60 | 2,3 sn | sway (logind aracılı) | seq 1 |
| browser → UI | sürekli ACTIVE 4K60 | 1,6 sn | mediabox-tv | seq 1 |
| Observer restart (UI) | değişmedi | — | mediabox-tv | seq korundu |
| Observer restart masterless pencerede | değişmedi | 3,4 sn | kodi-gbm | "DRM query deferred: a display transition is in progress"; sonraki periyotta 41 mod; düzeltmeden sonra seq korundu |

- Hiçbir örnekte 1080p ya da DISABLED yok. Örnekleme 50 ms aralıkla yapıldı, dolayısıyla daha kısa bir flash yakalanmamış olabilir.
- Beklenmeyen DRM master yok. Owner master hatası (`EBUSY`) yok; journal ve kernel logunda SError/panic/DRM hatası yok.
- UI'daki ekran ayarları TV'nin kendi scanout snapshot'ıyla (SIGUSR1; writeback değil) görüntülendi:
  - 4K UHD 60 Hz; renk SDR RGB 8 bit / HDR YCbCr 4:2:2 10 bit; hat yükü 594/600 MHz
  - Gelişmiş ayarlar: 13 boyut, 41 mod, hücre hızları önceki davranışla aynı
- Observer: fd'lerinde `/dev/dri` yok, `NRestarts=0`, daemon ile uyum `true` (connector, EDID SHA, profil).

## Regressions checked

- Offer ve Kodi planı baseline ile birebir aynı (son durumda tekrar karşılaştırıldı).
- 4K60 modu korunuyor; HDR otomatik renk seçimi değişmedi.
- Ethernet: ilgili dosyalarda diff yok; daemon ethernet testleri geçiyor; cihazda `mediaboxctl ethernet status` çalışıyor.
- FDStore: kod ve unit diff'i yok. Davranış: UI dururken `mediabox-tv-drm → /dri/card0` saklandı, UI dönünce geri alınıp kapatıldı.
- UI tasarımı: `ui/*.slint`, `main.rs` ve `screens/settings.rs` değişmedi. `screens/output.rs` (TV) ile `mediabox-ui/screens/settings.rs`'te tek satırlık tip uyarlaması var: hat yükü artık DBLCLK'i bilen `OutputModeOffer::character_rate_khz` ile hesaplanıyor.

## Open physical gates

- **Physical hotplug:** HOST/LOGIC VERIFIED, PHYSICAL HOTPLUG NOT PERFORMED (kabloya erişim yok). P10'dan önce yapılmalı.
- **Dual-sink CEC/topology:** NOT PHYSICALLY VERIFIED.
- **Masterless pencerede "no active master" kapısı:** Fiziksel testte transition kilidi kapısı daha önce devreye girdi. "No active master" yolu host'ta (fake clients) doğrulandı; cihazda tetiklenmedi.
- **Kalan etkileşim riski:** AM-2 sorgusu transition kilidini birkaç ms tutuyor. Tam bu anda Kodi kendiliğinden kapanırsa `mediabox-display-guard` kilidi meşgul görür ve UI kurtarması atlanabilir. Sorgu yalnız yeni ekran veya erteleme sonrası çalıştığı için olasılık çok düşük. Yine de P6/P10'da guard'ın observer kilidini ayırt etmesi gerekir.
- **Release/installer:** Observer, release manifest'e (`create-mediabox-release.sh`) ve installer'a eklenmedi.
- **Ultra:** doğrulanmadı. Oradaki sürücü sysfs EDID'i boş bırakıyor (kod yorumunda belirtilmiş); observer'da EDID `absent` görünecektir.

## Out-of-scope observations

- `mediaboxd-rs` transition testleri paralel koşuda flaky (yukarıda).
- sway her açılışta `Cannot find Xwayland binary "/usr/bin/Xwayland"` hatası veriyor (browser çalışıyor).
- `scripts/` ve `tools/` altındaki geliştirici teşhis betiklerinde ve dokümanlarda hâlâ `/sys/kernel/debug/dri/0` literali var (production değil).
- Operatör notu: property enum'larını görmek için cihazda bir kez `modetest -M rockchip -c` çalıştırıldı. Bu, master varken non-master bir okumaydı ve kernel 6.1 non-master probe'u read-only'ye düşürüyor. Daha sonra kullanılmadı.
- `mediabox-tv` native derlemesinde önceden var olan 34 uyarı.

## Commits

| Faz | Commit |
| --- | --- |
| P1 | `a72a582` display: preserve canonical DRM timing identity |
| P1 takip | `b8ba062` display: find the kernel mode by its timing key |
| P2 | `db161f5` display: validate and fingerprint EDID capabilities |
| P3 | `02e442a` display: introduce versioned source capability profile |
| P4 | `0641bdb` display: bind diagnostics and CEC to selected topology |
| P5 | `d7f83a3` display: add owner-independent observer in shadow mode |
| Rapor | bu dosya (docs commit) |

## Final state

- Dal `display-architecture-v2-wave1`, `main`'in (`7765f7e`) önünde. Push yok.
- Release tag, golden asset, release SHA ve pin değişmedi.
- Plus: Wave 1 ikilileri kurulu; observer etkin (`enabled` + `active`).
- Geri alma yedekleri: `/opt/rk3588-mediabox/bin/{mediaboxd-rs,mediaboxctl,mediabox-platform,mediabox-tv}.pre-wave1`.
- Cihazdaki daemon, TV ve platform ikilileri P5 commit'inden önceki çalışma ağacından derlendi. Aradaki tek fark `observer.rs`; bu modülü yalnız observer ikilisi kullanıyor ve observer güncel koddan yeniden kuruldu.
