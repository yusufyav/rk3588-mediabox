# MediaBox Native Shell — 2026-09-14

- Final commit: `474cd13` — `main == origin/main`, çalışma ağacı temiz, push edildi.
- Hedef: Orange Pi 5 Ultra / RK3588, `mediabox-tv-ui.service`.
- Panel (bu oturumda takılı olan): **2560x1440@60**, HDMI-A-1, EDID 0 bayt.
  Önceki raporlardaki 3840x2560@50 paneli takılı değil; ölçümler bu yüzden
  doğrudan karşılaştırılamaz.

## P0 — Ok → reboot

**Sebep bizim sürecimizde değildi.** HDMI bloğu CEC kumandasını bir rc-core
giriş aygıtı olarak kaydediyor, udev ona `power-switch` etiketi takıyor,
systemd-logind de etiketli her aygıtı izliyor. Varsayılanlar
`HandlePowerKey=poweroff`, `HandleRebootKey=reboot`. Cihaz günlüğünden (boot -5):

```
systemd-logind: Watching system buttons on /dev/input/event0 (dw_hdmi_qp)
systemd-logind: Power key pressed short.
systemd-logind: Powering off...
```

Ayrıca boot -6, beş `Back` basışından bir saniye sonra: `systemd[1]: Received
SIGINT` → `Activating special unit reboot.target` — `ctrl-alt-del.target`
bu sistemde `reboot.target` takma adı.

Dört kilit, dördü de cihazda doğrulandı:

| Kilit | Beklenen | Gerçekleşen |
|---|---|---|
| udev kuralı `80-mediabox-no-power-switch.rules` | event0 etiketsiz | `CURRENT_TAGS` boş; event12 (rk805 pwrkey) etiketini korudu |
| logind drop-in | power/reboot ignore | `HandlePowerKey=ignore`, `HandleRebootKey=ignore` |
| `ctrl-alt-del.target` | masked | `masked` |
| `src/actions.rs` yönlendirme tablosu | Ok/Back/yön asla güç değil | 6 regresyon testi |

`TAG-="power-switch"` **işe yaramıyor** (udev 257 kabul ediyor, uygulamıyor —
kural eşleşiyordu, etiket kalıyordu). `TAG=""` kullanıldı.

Regresyon testleri (`cargo test`, 77 geçti):
`ok_is_never_power_and_never_restart`, `back_is_never_power_and_never_restart`,
`navigation_never_reaches_the_machine`, `only_the_power_key_offers_power`,
`no_intent_has_a_system_effect`, `nothing_destructive_happens_on_the_first_press`,
`restarting_takes_three_deliberate_presses`, `no_key_code_maps_to_power`.

Yeniden başlatma/kapatma hâlâ mümkün: `SystemPower` iki değerli kapalı bir enum,
Ayarlar → Sistem içinden **iki onay** arkasında.

### İkinci giriş hatası (aynı aileden)

sway kalkınca CEC aygıtını kapatan mekanizma da kalkmıştı: bir kumanda basışı
hem libinput'tan hem daemon'dan geliyordu. Tekilleştirme tek yönlüydü ve kumanda
libinput üzerinden daha hızlı — yani **Ok iki kez işleniyordu** (bir liste açılıp
aynı basışta seçiliyordu). Platform artık rc-core aygıtlarını `/sys` yoluna
bakarak atlıyor, tekilleştirme de hangi yol önce gelirse gelsin çalışıyor.

### Terminal sızıntısı — iki ayrı sebep

Kullanıcı bildirimi: Kodi'den çıkar çıkmaz panelin üstünde iki beyaz satır.

**Sebep 1 — tuşlar VT'ye yazılıyordu.** Kabuk giriş aygıtlarını kilitlemiyordu.
Düzeltme: `EVIOCGRAB` (girdi çekirdeği düzeyinde kilit; konsol dahil diğer tüm
işleyiciler aygıtı görmez, `/dev/cec0`'a dokunmaz).

**Sebep 2 — framebuffer'ın içinde açılış metni duruyordu.** fbcon'u çözmek
belleği temizlemiyor, ve DRM çekirdeği **son master kapandığında** (yani filmin
bittiği an) fbdev modunu geri yüklüyor. Ölçüm: `/dev/fb0`'ın ilk 40 satırında
1095 sıfır-olmayan bayt — fotoğraftaki iki çizgi tam olarak bu.

Düzeltme: `mediabox-console-off` fbcon'u çözüyor **ve** `/dev/fb0`'ı sıfırlıyor;
açılışta `mediabox-console-off.service` (sysinit) ve her kabuk başlangıcında
`mediabox-hdmi-prepare` içinden çalışıyor — ikincisi, fbdev geri yüklemesinden
hemen sonraki an.

Doğrulama, tam Kodi gidiş-dönüşü boyunca: `fb0 non-zero: 0` (Kodi ekrandayken,
dönüşün ilk saniyesinde ve yerleştikten sonra), `(M) frame buffer device
bind=0`. Smoke'a iki kontrol eklendi.

## Render başarımı

Ölçüm `MEDIABOX_TV_BENCH=1` ile sürekli çizimde, 2560x1440@60 panelde.

| | FPS | p95 kare | p99 kare | sahne | EGL swap | sayfa çevirme | CPU (tek çekirdek) |
|---|---|---|---|---|---|---|---|
| A: her karede PRIME+ADDFB2 | 60.0 | 17.8–18.4 ms | 18.6–22.1 ms | 1.7–3.9 ms | 4.4 ms | 8.3–10.6 ms | %15.0–30.2 |
| B: framebuffer tamponla kalıyor | **60.0** | **17.2–18.4 ms** | **17.4–18.8 ms** | 1.5–2.8 ms | 4.4 ms | 9.4–10.8 ms | **%10.8–19.2** |

- Hedef 60 Hz panelde ~60 FPS: **tutuldu**, `long_frames` 0–2.
- p95 hedefi 16.7 ms; ölçülen 17.2–18.4 ms. FPS tam 60.0 ve düşen kare yok —
  fark, kare aralığının vblank'e göre ölçüm noktasındaki titreşimi.
- Import sayısı: 32 saniyede **0** (üçü açılışta), A'da kare başına bir tane.
- Boşta CPU: %0.0–0.3. Tuş→çizim p95: 0 ms (ölçüm çözünürlüğünün altında).
- İlk kare 413–486 ms, ilk veri 1010–1974 ms, RSS 150–176 MB, resim önbelleği
  16–19 MB.

Metrik satırı artık kare fazlarını da taşıyor:
`{"fps":60.0,"target_fps":60,"p95_frame_ms":17.2,"p99_frame_ms":17.4,"long_frames":0,"draw_ms":2.53,"flip_wait_ms":0.00,"swap_ms":4.49,"present_ms":9.64,...}`

## Ekranlar

- **Home** — özellikli launcher: uygulama kutucukları, makine göstergeleri
  (saat, işlemci/bellek halkaları, depolama, ağ), tek "Devam Et" rafı. Katalog
  Home'a basılmıyor.
- **Filmler ve Diziler** — kendi barı (Ara, Kitaplık), raflar, her posterin adı
  altında, odaklanan başlığın künyesi üstte.
- **Detail** — logo, `yıl · süre · IMDb`, TÜRÜ/OYUNCULAR/YÖNETMENLER/ÖZET,
  yuvarlak aksiyon sırası, **kalıcı sağ kaynak sütunu** + sağlayıcı filtresi.
  Kaynakta Ok doğrudan oynatır. Uydurma "Kaynak Seç" düğmesi kaldırıldı.
- **Search** — 7 sütunlu Türkçe harf ızgarası (İ/I ayrı), poster ızgarası,
  USB klavye desteği, 350 ms debounce.
- **Library** — Devam Et / Filmler / Diziler / Kitaplık sekmeleri, poster
  ızgarası, sağda odaklanan başlığın kaydı.
- **Now Playing** — artwork, süre/ilerleme, dört kumanda, kodek/HDR ikincil.
- **Settings** — Oynatma, Ağ, Bluetooth, HDMI ve CEC, Ekran, Ses, Sistem,
  Tanılama. Yıkıcı satırlar onaylı.
- **Diagnostics** — Makine, Depolama, Ağ, Ekran ve GPU, Oynatma, CEC, Servisler.

Odak modeli 77 testle sabitlendi (odak kaybolmaz, her satır kendi sütununu
hatırlar, boş raflar atlanır, modal arka planı kıpırdatmaz).

## Kodi devri

Cihazda iki kez uçtan uca: kabuk `DRM released` yazıp çıktı, Kodi ekranı aldı
(`Ekran: Kodi`), `surface switch ui` ile kabuk geri geldi
(`first_frame_ms=829`, `present dma-buf=active`), konsol panelde görünmedi,
artık `mpv`/`ffmpeg` kalmadı, pozisyon `tv-state.json`'dan geri yüklendi.

## HDR regresyonu — ÇALIŞTIRILAMADI

`/opt/rk3588-mediabox/assets/hdr10-4k-2398-main10.mp4` Kodi'de açıldı,
`state=playing`, `total_time=3 s`, Rockchip MPP donanım çözücüsü yüklendi
(`mpp_info: mpp version: 0986d01`), klip bitip idle'a döndü.

**Ancak HDR10 sinyallemesi doğrulanamaz:** takılı ekran 2560x1440 bir monitör,
`/sys/class/drm/card0-HDMI-A-1/edid` 0 bayt ve `hdr_panel_metadata` boş.
Bağlayıcı `SDR[0] BT.709 Full` kaldı — bu, HDR bildirmeyen bir panelin doğru
davranışı, regresyon değil. Kabul edilmiş HDR baseline'ı **HDR TV takılıyken**
tekrar koşulmalı.

## Deploy temizliği

Kaldırılanlar (hem repodan hem cihazdan): `mediabox-tv-native`,
`mediabox-kiosk-browser`, `mediabox-display-watch`, `mediabox-display-settle`,
`config/sway-kiosk.conf`. TV-yerel kabukta sway/Chromium yok; sway ve Chromium
yalnız tarayıcı uygulamasının kapsamında kuruluyor. Unit doğrudan
`/opt/rk3588-mediabox/bin/mediabox-tv` çalıştırıyor.

`mediabox-ui-reap` süreçleri komut satırına göre eşleştiriyordu ve ikiliyi kurup
aynı komutta unit'i yeniden başlatan her deploy'da **yöneticinin SSH kabuğunu
öldürüyordu**. Artık `/proc/<pid>/exe` ile eşleştiriyor.

Güvenlik modeli değişmedi: `DevicePolicy=closed`, dar `DeviceAllow` listesi,
`/dev/cec0` sahipliği mediaboxd-rs'te.

## Smoke

```
== native shell smoke
  PASS unit / interface / platform (renderD128 -> card0) / gpu (ARM / Mali-G610)
  PASS scanout (PRIME FB active) / home / native mode 2560x1440 / compositor none
  PASS scanout buffers (3 imported once)
  PASS cec power tag (event0 untagged) / logind buttons / logind keys / ctrl-alt-del
  PASS framebuffer console (off the panel) / framebuffer content (empty)
  PASS orphan players
== native shell smoke PASS            (16/16)
```

## Kanıt

`mediabox-tv-drive` cihazda: gezinme control plane'in girdi veriyolundan,
ekran görüntüsü kabuğun kendisinden (SIGUSR1 → sonraki tarama karesi PNG).
`/var/lib/mediabox-ui/snapshots/{home,media,detail,search,library}.png`,
2560x1440.

## Kalan bloker

1. **HDR regresyonu**: takılı panel 1440p, EDID 0 bayt, HDR metadata yok.
   HDR TV takılmadan koşulamaz.
2. **Native gömülü oynatıcı (VOP2)**: bu oturumda başlanmadı. Kodi oynatma yolu
   sağlam ve tek oynatma yolu. Shell'in kabulünden ayrı tutulmalıdır.
