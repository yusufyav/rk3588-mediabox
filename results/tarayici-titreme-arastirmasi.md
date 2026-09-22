# Tarayıcıda yırtılma — araştırma raporu

**Tarih:** 21 Eylül 2026 · **Cihaz:** Orange Pi 5 Plus, `6.1.115-vendor-rk35xx`
**Panel:** 2560x1440@143,999 Hz, HDMI-A-2 · **Durum: KAPANDI — bkz. Bölüm 8**

> **Sonrası (22 Eylül 2026).** Bu raporun kapanışı yırtılmayı bitirdi ama
> cadence'ı bitirmedi: 143,999 Hz, 60 fps'in tam katı olmadığı için hiçbir kare
> düşmeden titreme bırakıyordu. O da kapandı — tarayıcı artık panelin sunduğu
> modlardan 60/59,94'ün tam katı olanını alıyor. Aynı çalışmada AV1 donanım
> çözme de açıldı. `results/DURUM.md` bölüm 6b.

Bu rapor önce bir başarısızlık kaydı olarak yazıldı: altı saat, üç mimari
deneme, sonuç yok. Sonradan sebep bulundu ve düzeltildi; Bölüm 8 onu anlatıyor.
Bölüm 1-7 olduğu gibi duruyor, çünkü elenmiş yolların kaydı düzeltmenin kendisi
kadar değerli — ve çünkü sebebin neden bu kadar geç bulunduğunu gösteriyor.

---

## 1. Şikâyet

Kullanıcının kendi ifadeleriyle, değiştirilmeden:

* "sadece video izlerken değil sayfada yukarı aşağı gezinirken bile
  **yırtılmalar** oluyor"
* "720 p de bile titreme var" · "bu çözünürlük sorunu değil farklı birşey"
* "tarayıcı şu an parkinson hastası gibi titriyor. Tearing havada uçuşuyor."
* "Tarayıcı video izlerken yırtılma **kare kare olma** sorunu var. aynı zamanda
  bir sayfada gezerken bile **patlamalar** oluyor."

Üç ayrı belirti: yırtılma, kare kare akış, ve blok blok bozulma. Yalnızca
tarayıcıda. Ürünün kendi arayüzü ve Kodi aynı panelde, aynı modda temiz.

---

## 2. Ölçülenler

Hepsi cihazda, gerçek koşulda. Beklenen/gerçekleşen ayrı.

### 2.1 Flip zamanlaması — temiz

Sürekli kaydırma sırasında VOP2'nin taradığı tampon adresi 1,5 kHz'de
örneklendi (`/sys/kernel/debug/dri/0/summary`, okuma başına 0,03 ms):

| | ölçüm |
|---|---|
| flip sayısı | 572 / 4 s = **143,0/s** (mod 143,999 Hz) |
| farklı tampon | 2, dönüşüm aralığı min 2 / ort 2,00 |
| kare başına bekleme | p50 **6,89 ms** (refresh 6,94 ms), p95 7,57 |

**Sonuç:** flip'ler refresh başına tam bir tane ve vblank'e senkron. Async
page flip yok, alt-refresh bekleme yok, tampon ekrandayken yeniden
kullanılmıyor. *Bu hipotez (tampon geri dönüşümü) elendi.*

Kontrol — native arayüz, aynı yöntemle: 2 tampon, 101 flip / 3 s, bekleme
p50 33 ms. O da temiz ve o titremiyor.

### 2.2 Senkronizasyon yolu — YOK

| kaynak | bulgu |
|---|---|
| çekirdek config | `# CONFIG_MALI_DMA_FENCE is not set` → **örtük fence yok** |
| `libwlroots-0.18.so` | `eglCreateSync`, `eglCreateSyncKHR`, `eglWaitSyncKHR`, `eglDupNativeFenceFDANDROID`, `EGL_ANDROID_native_fence_sync` → **hiçbiri yok** |
| aynı kütüphane | `IN_FENCE_FD`, `OUT_FENCE_PTR`, `in_fence_fd` → **yok** |
| içe aktardığı tüm EGL sembolleri | context / image / query — **tek bir sync fonksiyonu yok** |

Yani Mali'nin kareyi bitirmesi ile VOP2'nin onu taraması arasında **ne açık ne
örtük** bir bariyer var.

### 2.3 Titremeyen iki uygulama ile fark

| | tampon yolu | `eglSwapBuffers` | titriyor mu |
|---|---|---|---|
| `mediabox-tv` (Slint kabuk) | `gbm_surface` | **var** | hayır |
| `kodi-gbm` | `gbm_surface` | **var** | hayır |
| **sway / wlroots 0.18** | `gbm_bo_create` + kendi FBO'su | **yok** | **evet** |

`eglSwapBuffers`, Mali sürücüsünde tamponun hazır olmasının garanti edildiği
yer. Titremeyen iki uygulama oradan geçiyor; wlroots geçmiyor.

Bu, tek tutarlı **çıkarım**: boşluk burada. Kanıtlanmadı — aşağıdaki denemeler
doğrulayamadı.

---

## 3. Elenen yollar

Tekrar denenmesin diye, sebepleriyle:

| Deneme | Sonuç | Kanıt |
|---|---|---|
| **Weston 14.0.2** (kiosk-shell) | **daha kötü** | Chromium CPU %131,5 → **%571,4**; donanım çözme kırılıyor (`PreSandboxInitialization() ... failed to find a suitable render node`); düzlem yine tek |
| **Compositor'süz Chromium** | **imkânsız** | Debian ikilisinde Ozone DRM platformu derlenmemiş — `ozone_platform_drm`, `OzonePlatformDrm`, `DrmThread` yok |
| **`tearing-control` protokolü** | **ilgisiz** | sway sunuyor, Chromium bağlanmıyor; `wp_tearing_control_v1` ikilide geçmiyor |
| **60 Hz mod** | **fark yok** | `2560x1440p60`, dclk 583,6 → 241,5 MHz, kare bütçesi 6,94 → 16,7 ms. Kullanıcı: "Yırtılma devam" |
| **Direct scanout açmak** | **devreye girmiyor** | tamponlar hâlâ wlroots'un kendi ikilisi (`0x1c20000`/`0x3848000`); *neden reddedildiği sorulmadı* |
| **`WLR_RENDERER=pixman`** | **sonuçsuz** | Mali devre dışı (GPU %0 @300 MHz) ama Chromium da yazılıma düşüyor (%667 CPU) ve kullanıcı "deli gibi drop" bildirdi — yırtılma yargılanamadı |

60 Hz'in hiç fark etmemesi önemli: bu bir **zamanlama yarışı değil**. 2,4 kat
pay verilse de değişmiyorsa, eksik olan zaman değil, bariyerin kendisi.

---

## 4. Kullanıcının gönderdiği iki video

İkisi de gerçek ekran kaydı, WhatsApp, 848x478.

| | süre | kare | ses/görüntü süresi |
|---|---|---|---|
| 16.30.52 | 8,90 s | 267 (**30 fps**) | 8,903 / 8,896 |
| 16.41.40 | 15,29 s | 459 (**30 fps**) | 15,281 / 15,294 |

Ses süresi görüntü süresine eşit → **ikisi de ağır çekim değil.** 30 fps kayıt,
144 Hz ekranın her karesinde ~4,8 refresh'i üst üste bindiriyor; bir refresh
süren dikiş bu pozlamada ortalanıp kayboluyor.

Dört ayrı analiz çalıştırıldı ve **hiçbiri yırtılma izi bulamadı**: dikey
süreksizlik taraması (tepe/ort en fazla 1,87), yatay süreksizlik taraması (en
güçlü bulgu YouTube "Sıradaki" kartının kenarıydı, içerik), tüm video
ortalaması, ve sabit yazı bütünlüğü ("San Francisco, California, USA" 16
örneğin hepsinde kırıksız).

**Kullanıcıdan üçüncü video istenmeyecek.** Sınır kayıtta değil, yöntemde.

---

## 5. Bugün repoya giren

Titremeyle ilgisi yok ama gerçek ve ölçülmüş — commit `6f1aae4`:

Birimin `DevicePolicy=closed` ve tüm `DeviceAllow` satırları **tarayıcıya hiç
uygulanmıyordu**. `PAMName=login` yüzünden `pam_systemd` süreçleri birimin
cgroup'undan alıp login session scope'una taşıyor.

| `/dev/loop-control` açma denemesi | beklenen | gerçekleşen |
|---|---|---|
| aynı politikalı temiz kapsam | RED | **RED (EPERM)** |
| çalışan tarayıcının içinde | RED | **AÇILDI** |

Kısıt mount namespace'ine taşındı (`PrivateDevices=yes` + adı adına
`BindPaths=`). Tarayıcının gördüğü `/dev` **196 → 26 düğüm**; `/dev/mem`,
`/dev/mmcblk0`, `/dev/loop-control` artık namespace'te yok. Donanım çözme
çalışmaya devam ediyor (`fdc38100.rkvdec-core`).

Ölçerken çıkan ve dosyaya yazılan iki tuzak: `libseat` oturumdaki her DRM
cihazını `stat` ediyor (NPU'nun *kartı* gizlenirse ekran siyah kalıyor), ve
Mali'nin GBM'i `/dev/dma_heap/system`'den tampon alıyor (bağlanmazsa
`Failed to create GBM device`).

---

## 6. Sıradaki tek somut adım — YAPILDI, bkz. Bölüm 8

**Direct scanout'un neden reddedildiğini öğrenmek.**

Chromium'un kendi tamponu doğru fence'lenmiş bir tampondur. O tampon doğrudan
ekrana gidebilirse compositor'ün fence'siz yolu tamamen devre dışı kalır ve
yırtılma yapısal olarak biter. Bugün açıldı ama devreye girmedi; *neden*
sorulmadı.

sway bunu kendi hata ayıklama günlüğüne yazıyor. Tek yeniden başlatma, günlüğü
oku, sebebi öğren. Üç olası sebep — tampon formatı/modifier uyuşmazlığı, yüzeyin
tam ekran sayılmaması, üstünde başka yüzey olması — ve üçü de düzeltilebilir.

Bu adım kullanıcıdan hiçbir şey istemiyor: ne video, ne bakış, ne yorum.

### Sonraki, eğer o da kapanırsa

wlroots'a fence yolu kazandırmak. Mali kullanıcı alanında
`EGL_ANDROID_native_fence_sync` **var**; eksik olan wlroots 0.18'in onu
kullanmaması. Sürümü sabitlenmiş yeni bir sway/wlroots derlemesi gerekir —
yeni bir upstream bileşen, kullanıcının açık onayıyla.

---

## 7. Açık kalan diğer işler

* **Reklam engelleyici** — hiçbir tarayıcıda çalışmıyor. Chromium 153 yalnız
  MV3, uBO Lite `--load-extension` ile yüklenmiyor; desteklenen yol
  `ExtensionInstallForcelist` kurumsal politikası. Firefox'ta tam uBO çalışıyor
  ama Firefox'un GPU tespiti bu kutuda tamamen başarısız.
* **Üst bar / ayarlar** — yalnızca cihazdaki geçici
  `/opt/rk3588-mediabox/bin/mediabox-browser` betiğinde, **repoda değil**.
* **`rerender` maliyeti** — ~1,2 çekirdek ve GPU tam saatte. Kaldırılınca eski
  içerik kalıntısı geliyor, yani yara bandı ama taşıyıcı.

---

## 8. Sebep bulundu ve kapatıldı (21 Eylül 2026, aynı gün)

Bölüm 6'daki tek açık adım yapıldı ve sorunu kapattı.

### Red sebebi

**Sway yapılandırmasındaki `output * bg #07080b solid_color` satırı.**

O satır bir renk ayarı değil, bir **sahne düğümü**: çıktı boyutunda, her şeyin
altında duran bir dikdörtgen. wlroots bir istemcinin tamponunu doğrudan ekran
denetleyicisine ancak sahne listesinde **tam olarak bir düğüm** varken veriyor.
Chromium'un yüzeyi `AB24` (ABGR8888) — alfa kanallı, dolayısıyla compositor
arkasındakini kapattığını varsayamıyor ve dikdörtgeni eleyemiyor. İki düğüm,
ve direct scanout **hiç denenmiyor**.

Denenmediği için wlroots bir red mesajı da yazmıyor: `Direct scan-out %s`
yalnız durum *değiştiğinde* yazılıyor, hiç etkinleşmediyse hiç yazılmıyor.
Sessizliğin sebebi buydu.

### Elenen sebepler, ölçümle

| aday | sonuç |
|---|---|
| geometri | output 2560x1440 scale 1.0; Chromium `fullscreen=1`, `rect 0,0 2560x1440`, `deco 0x0` — **birebir** |
| tampon import'u | `framebuffer[300]`, format `AB24`, modifier `0x0`, `imported=yes` — wlroots zaten import etmiş |
| düzlem format desteği | plane 98 (PRIMARY, crtc 0x2): `XR24 AR24 XB24 AB24 ...`, modifier `0x0` listesinde `AB24` **var** |
| DMA-BUF | gpu-process 214 dmabuf tutuyor |

### Uygulanan düzeltme

* `config/sway-browser.conf` — `output * bg` satırı kaldırıldı. Görsel kayıp
  yok: wlroots hiçbir düğümün kapatmadığı alanı kareyi kurarken opak siyaha
  temizliyor, yani o satırın boyadığı şeyi zaten boyuyor — ama düğüm olarak
  değil.
* `packaging/systemd/mediabox-browser.service` —
  `Environment=WLR_SCENE_DISABLE_DIRECT_SCANOUT=1` kaldırıldı. O satırın
  gerekçesi ("scanout, Chromium hâlâ çizerken okuyor") yanlış teşhisti: scanout
  zaten hiç olmuyordu, dolayısıyla kapatmak hiçbir şeyi değiştirmiyor, yalnız
  gerçek sebebi gizliyordu.
* `WLR_SCENE_DEBUG_DAMAGE=rerender` **korundu**. Scanout etkinken wlroots
  kompozit yapmıyor, yani bedeli sıfır; scanout düştüğünde (örneğin bir açılır
  pencere) eski-kare sorununa karşı hâlâ gerekli.

### Kanıt

```
00:00:02.006 [DEBUG] [wlr] [types/scene/wlr_scene.c:1858] Direct scan-out enabled
```

VOP2'nin taradığı düzlem artık Chromium'un tamponu:

```
Cluster0-win0: ACTIVE
    format: AB24 little-endian (0x34324241)     ← wlroots'un swapchain'i XR24
    src/dst: 2560x1440
```

Taranan tampon adresleri `0x4658000` / `0x8f95000` — wlroots'un kendi
swapchain'i (`0x1c20000` / `0x3848000`) değil.

### Ölçülen kazanç

| | kompozit (önce) | direct scanout (sonra) |
|---|---|---|
| 4K VP9 Chromium CPU | %131,5 | **%91,0** |
| GPU | %46-57 @ 1000 MHz | **%18 @ 300 MHz** |
| düşen kare (2160p) | — | **0 / 861** |
| düşen kare (720p) | — | 1 / 857 (%0,12) |
| fan pwm | 100 | **50** |

720p'nin CPU'su yüksek (%156,8) çünkü YouTube o çözünürlükte **AV1** veriyor ve
sürücüde AV1 yok — bilinen açık kusur, bu işle ilgisiz.

### Neden bu kadar geç bulundu

İlk gün "tek düzlem aktif, `allocated by = sway`" diye kaydedilmişti. Oradan
doğru soruya — *Chromium'un kendi tamponu neden birincil düzlemde değil* —
gidilmedi; onun yerine video subsurface'leri için overlay düzlemi arandı.
Direct scanout bir kez denendi, devreye girmedi, ve **neden girmediği
sorulmadı**. Ayrıca unit dosyasındaki "direct scanout tearing yapıyor" yorumu
sınanmadan olgu kabul edildi; o yorum yanlış teşhisti ve saatlerce fence
aranmasına yol açtı.

---

## 9. Bölüm 8'in kapanışı tutmadı — elenenler, ölçülerek (22-23 Eylül 2026, Plus)

Kullanıcı fiziksel TV'de gördü: yırtılma ve adres çubuğunda **son karakterin
dans etmesi** hem kompozit yolda (normal pencere, wlroots, düzlem `XR24`) hem
de direct scanout yolunda (tam ekran, düzlem `AB24`) **var**. Bölüm 8'deki
direct scanout düzeltmesi CPU/GPU kazancı olarak geçerli, ama yırtılmanın
sebebi değildi. Bu bölüm o günden sonra neyin ölçülerek elendiğini tutar.

Kurulum: Orange Pi 5 Plus, `6.1.115-vendor-rk35xx`, çıkış 3840x2160@60,
Chromium 153 (Debian), sway + wlroots 0.18, Mali G610 `bifrost_kbase`
g25p0-00eac0 (CSF), `kernel.panic=10`.

### Sonuç tablosu

| deney | sonuç | kanıt |
|---|---|---|
| pencereyi output'a oturtmak (`browser.custom_chrome_frame=false`) | **geometri değişmedi** | xdg geometry 3828x2138 önce ve sonra; sway `zxdg_decoration_manager_v1` sunuyor |
| CDP `windowState=maximized` | **uygulanmadı** | çağrı `{}` döndü, durum `normal` kaldı |
| `--start-maximized` | **uygulanmadı** | `normal`, 3828x2138, `XR24` |
| **software Chromium** (`--disable-gpu`) | **yırtılma YOK, karakter dansı YOK** (kullanıcı) | Chromium'da `mali0`=0, dmabuf=0, tamponlar shm; sway Mali'yi kullanmaya devam etti. Bedeli: videoda ağır kare kaybı |
| kernel'de implicit fence eksik mi | **hayır — fence zaten var** | `bufinfo`: üç 4K pencere tamponunun her birinde `write fence:mali <gpu-pid>-kcpu`. Kaynak `armbian/linux-rockchip fd9f823`; `CONFIG_MALI_DMA_FENCE` yalnız midgard sürücüsüne ait, bifrost ağacında `dma_resv` 0 kez geçiyor; fence'i Chromium'un **browser process'i** `DMA_BUF_IOCTL_IMPORT_SYNC_FILE` ile ekliyor |
| fence commit'ten sonra mı geliyor | **hayır** | 204 `dma_resv_add_fence`'in 204'ü `wl_surface.commit`'ten önce (en az 48 µs); eklenmeyen karelerde fence commit anında zaten signaled |
| release gelmeden tampon yeniden kullanılıyor mu | **hayır** | 1318 karede release'siz yeniden commit 0; kesin eşlenen 88 karede sonraki fence release'ten en az 10,65 ms sonra |
| native EGL (ANGLE'sız) | **bu build'de yok** | `gl_factory.cc:110`: izinli liste yalnız `egl-angle` × {opengl, opengles, vulkan} |
| ANGLE arka ucu OpenGL → **OpenGLES** (`--use-angle=gles`) | **yırtılma sürüyor** (kullanıcı) | `gl=egl-angle,angle=opengles`, renderer Mali-G610, `mali0`=1, dmabuf=121 |

### Ne kaldı

Semptom Chromium'un GPU → DMA-BUF yolunda doğuyor; tamponlar shm'e
düşünce kayboluyor. Ama o yolun ölçülebilen sıralaması doğru: yazma fence'i
commit'ten önce `dma_resv`'de, tampon release gelmeden yeniden kullanılmıyor.
KMS tarafı da fence'i bekliyor: rockchip `drm_atomic_helper_commit`
kullanıyor, VOP2'nin kendi `prepare_fb`'si yok, çekirdek varsayılan
`drm_gem_plane_helper_prepare_fb`'yi çağırıyor.

**Ölçülmemiş olan:** tüketicinin tamponu gerçekten ne zaman okuduğu. Sway'in
Mali'si kompozit sırasında implicit fence'i bekliyor mu, bu hiç izlenmedi.
Hiçbir tamponda **read** fence yok, ama bu tek başına bir bug kanıtı değil.
ANGLE arka ucu elendi (OpenGL ve OpenGLES aynı), native EGL ise bu build'de
denenemiyor. Sıradaki alan: Mali userspace (libmali g24p0) / GBM / `dma_heap`
`system-uncached` tamponları.

### Bu turda kapatılan açık kusur: tarayıcıda ses yoktu

Kutuda ses sunucusu yok; Chromium ALSA `default`'u açıyor, o da
`/etc/asound.conf` olmadığı için card 0'a, yani bağlı olmayan DisplayPort'a
gidiyordu. TV `rockchiphdmi1`'de (card 2). `mediabox-player` aynı sorunu
`mediabox-platform alsa-card` ile çözmüştü; `packaging/mediabox-browser` artık
aynı çözücüyle `--alsa-output-device=hdmi:CARD=<kart>,DEV=0` veriyor.
Ölçüldü: card2 `RUNNING`, S16_LE 2ch 48 kHz; kullanıcı sesi duydu. Bir USB
kulaklığa (`GameBuds`, card 4) geçici yönlendirme de çalıştı; kalıcı bir
"USB takılıysa onu tercih et" kuralı **eklenmedi** — ürün kararı.
