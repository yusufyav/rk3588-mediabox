# Gate S1A — minimal direct-KMS probe ve lifecycle handoff doğrulaması

## 1. Sonuç

`DIRECT_KMS_LIFECYCLE_HANDOFF_PASS`

Orange Pi 5 Ultra üzerindeki production Kodi ile küçük bir direct-KMS istemci
arasında process-lifecycle tabanlı display ownership devri gerçek donanımda
doğrulandı.

- Kodi normal kapanışta DRM master'ı bıraktı.
- Probe `drmSetMaster()` sonucunu `drmIsMaster()` ile doğruladı.
- Probe XRGB8888 dumb framebuffer oluşturdu, primary plane'e bağladı ve gerçek
  atomic KMS commit yaptı.
- Probe sonlu süre sonunda plane/connector/CRTC bağlarını söküp framebuffer,
  GEM handle, mode blob, DRM master ve fd kaynaklarını bıraktı.
- Production Kodi normal launcher ile DRM master'ı geri aldı.
- 4K TV üzerinde üç lifecycle cycle'ın üçü de geçti.
- Canonical HDR asset'in PRE, ilk-cycle POST ve üçüncü-cycle final capture'ları
  kabul edilmiş HDR/display invariant'larını korudu.
- Gate penceresinde yeni kernel atomic/VOP2/IOMMU/fatal veya Kodi DRM/RKMPP
  hatası görülmedi.
- Finalde probe yok, Kodi çalışıyor, `card0` master ve JSON-RPC `pong`.

Ön gözlem sırasında bağlı ekran 4K TV değildi. Kullanıcı ekranı Sony 4K TV ile
değiştirdi; bu sırada Kodi'yi kendisi kapattığını açıkladı. 1080p ekrandaki ilk
deneme `PRELIMINARY_1080_*` adıyla saklandı ve resmi 3-cycle sonucuna dahil
edilmedi. Resmi Gate ölçümü 4K EDID doğrulandıktan sonra sıfırdan başladı.

## 2. İzolasyon ve baseline

| Öğe | Gözlem | Sonuç |
| --- | --- | --- |
| Ana repo `HEAD` | `d02bd6f31b305daa10decb4eb4d2adf87879fdf6` | PASS |
| `origin/main` | `d02bd6f31b305daa10decb4eb4d2adf87879fdf6` | PASS |
| Ana working tree | temiz | PASS |
| Referans repo | `582e1d30c6762c134118445bf660c0784aaffe58` | PASS; salt-okunur kaldı |
| Branch | `agent/codex-s1a-kms-probe` | PASS |
| Worktree | `/tmp/rk3588-mediabox-codex-s1a` | PASS |
| Kernel | `6.1.115-vendor-rk35xx-screenbridge-hdmirx-audio` | PASS |
| Kodi source revision | log: `Git:20260831-e513e0ff43` | PASS |
| Kodi executable SHA-256 | `bfa5b1cb2ed73d139f0bcdb8652e73ea489f71edac51534fbeceddd73b1f71fb` | PASS |

S0-A (`fc0cfa2`), S0-B
(`c589d3bfd8248619865806e7d9b041e4186154f6`) ve S1
(`0668bbb128907c49781f81331861e09e8f852ae4`) raporları `git show` ile
okundu. Hiçbir branch merge edilmedi.

## 3. Probe tasarımı

Kaynak: `tools/kms-handoff-probe/kms-handoff-probe.c`

Araç tek C11 kaynak dosyasıdır; GUI framework, compositor, GBM, EGL, Kodi,
kernel veya DT kodu içermez. Tek runtime bağımlılığı libdrm'dir.

### `--inspect`

- `/dev/dri/card0`…`card15` arasında runtime discovery yapar.
- Bağlı `DRM_MODE_CONNECTOR_HDMIA` connector'ı bulur.
- Driver, encoder, CRTC, aktif mode/refresh, primary plane ve formatları basar.
- Universal-plane ve atomic client capability durumunu raporlar.
- `drmIsMaster()` ile yalnız kendi fd'sinin pasif durumunu okur.
- `drmSetMaster`, modeset, framebuffer veya atomic commit çağırmaz.

### `--acquire-only`

- Runtime'da bulunan display card'ı açar.
- `drmSetMaster()` çağırır ve sonucu `drmIsMaster()==1` ile doğrular.
- Varsayılan bir saniye sonlu hold sonrası `drmDropMaster()` ve fd close yapar.

### `--scanout-test`

- Bağlı HDMI'nın halen aktif CRTC/mode'unu kullanır; aktif mode yoksa keyfi bir
  resolution/refresh seçmek yerine hata verir.
- İlgili CRTC ile uyumlu ve XRGB8888 kabul eden `Primary` plane'i runtime'da
  bulur.
- `DRM_IOCTL_MODE_CREATE_DUMB` ile 32-bit buffer yaratır, `drmModeAddFB2()` ile
  XRGB8888 framebuffer kaydeder ve gri/renk barı test deseni doldurur.
- Mode blob üretir.
- Atomic request içinde connector `CRTC_ID`; CRTC `MODE_ID` ve `ACTIVE`;
  primary plane `FB_ID`, `CRTC_ID`, `SRC_*` ve `CRTC_*` alanlarının tamamını
  yazar.
- Önce `DRM_MODE_ATOMIC_TEST_ONLY | DRM_MODE_ATOMIC_ALLOW_MODESET`, sonra
  gerçek `DRM_MODE_ATOMIC_ALLOW_MODESET` commit yapar.
- Varsayılan iki saniye görüntü verir.
- Cleanup sırasında kendi plane'ini, connector'ı ve CRTC'yi atomic olarak
  detach eder; mapping, FB, dumb GEM handle, mode blob, master ve fd'yi bırakır.
- `SIGINT`, `SIGTERM` ve `SIGHUP` hold'u kesip aynı normal cleanup yoluna sokar.
- Hold aralığı 1–10 saniye ile sınırlıdır; sonsuz loop yoktur.

Makinece okunabilir `PROBE_STARTED`, `DRM_MASTER_ACQUIRED`,
`ATOMIC_SCANOUT_COMMITTED` ve `DRM_MASTER_RELEASED` event'leri timing için
`CLOCK_MONOTONIC` nanosecond değeri taşır.

## 4. Build ve host/source doğrulaması

Host build:

```text
cc -I/usr/include/libdrm -O2 -g -std=c11 -Wall -Wextra -Wpedantic -Werror \
  kms-handoff-probe.c -o kms-handoff-probe -ldrm
```

- GCC build sıfır warning ile geçti.
- Clang static analyzer sıfır bulgu ile geçti.
- `--help`, çakışan mod, geçersiz timeout ve DRM'siz host fail-fast exit-code
  kontrolleri geçti.
- `git diff --check` geçti.
- Kaynakta framebuffer/GEM/blob/master/fd cleanup ve signal/exit yolları ayrıca
  gözden geçirildi.

Hedefte `/usr/bin/gcc`, `/usr/bin/make`, `/usr/bin/pkg-config`, libdrm
development header'ları ve libdrm `2.4.124` zaten mevcuttu. Yeni paket
kurulmadı. Kaynak yalnız `/tmp/kms-handoff-probe-build` altında native AArch64
olarak derlendi; test binary'si `/tmp/kms-handoff-probe` idi.

```text
ELF 64-bit LSB pie executable, ARM aarch64
libdrm.so.2 => /lib/aarch64-linux-gnu/libdrm.so.2
libc.so.6   => /lib/aarch64-linux-gnu/libc.so.6
```

| Artifact | SHA-256 |
| --- | --- |
| Target test binary | `f0bc0f52f503c62093b44d9879897825b9b9d646deb32374d74bc33839e0b33f` |
| `kms-handoff-probe.c` | `fc194194e1dbe6905c4bb714d7d103161194ec1254f23738a3041e7e7ab4d1dd` |
| `Makefile` | `1c04e5bffce5cf17ce093db2c1cdf867c3cf3a99b5bd5a7adfb34319845ad350` |
| `README.md` | `5ce9f713be5beb4e0826e6fe71faf7c45b1957a108c2b8d8ae1491686782dfdd` |

Probe kaynak commit'i: `dcb9df3` (`test: add direct-KMS lifecycle handoff probe`).

## 5. Passive target inspection ve DRM topology

Kodi çalışırken `--inspect` sonucu:

```text
DRM card            : /dev/dri/card0
driver              : rockchip
connector           : HDMI-A-1
connector id/state  : 201 / connected
encoder id          : 200
CRTC id/index       : 89 / 0
current mode         : 1920x1080 1920x1080
current refresh      : 60.000 Hz
universal planes     : yes
atomic capability    : yes
this fd DRM master   : no
primary plane        : id=57 possible_crtcs=0x1
plane formats        : XR24 AR24 XB24 AB24 RG24 BG24 RG16 BG16 YU08 YU10 YUYV Y210
selected plane       : 57 (Primary)
INSPECT RESULT: PASS
```

Probe öncesi/sonrası Kodi aynı PID `84385` idi, `/dev/dri/card0` master olarak
kaldı ve JSON-RPC `pong` döndü. Passive inspection modeset veya ownership
değişikliği yaratmadı.

Sony TV EDID'i 41 mode sundu. Canonical playback için kullanılan mode satırı:

```text
3840x2160 23.98 Hz; clock=296703 kHz; htotal=5500; vtotal=2250
```

Bu timing `296703000 / (5500 * 2250) = 23.976 Hz`'dir.

## 6. PRE_HANDOFF_GOLDEN

Canonical asset:

```text
/var/tmp/mp1b/past-lives.mkv
12081238172 bytes
SHA-256 f4e32b8d3feb7efdebaaefaf7338a25bcc86ee144bf82bd4b93da6b80906f96a
```

Hash Gate sonunda 12 GB dosyanın tamamı tekrar okunarak doğrulandı.

Kodi logu:

```text
Video: hevc (Main 10), yuv420p10le(tv, bt2020nc/bt2020/smpte2084),
3840x2080, 23.98 fps
CDVDVideoCodecDRMPRIME::Open - using decoder Rockchip MPP ... HEVC decoder
SetGuiPlaneEotf plane=57 hdr_encoded=false eotf=0
VideoLayerBridge:Configure plane=73 plane_fourcc=NV15 ... BT.2020 ... eotf=2
```

Accepted capture aracı OSD görünürken şunları ölçtü:

| Invariant | PRE |
| --- | --- |
| RKMPP / DRM PRIME | aktif |
| Codec | HEVC Main10 |
| Video framebuffer | NV15, plane 73 |
| Display mode | 3840x2160p24; gerçek timing 23.976 Hz |
| Output HDR | `HDR10[2]`, BT.2020 limited |
| Fiziksel bus | `YUYV10_1X20` — 10-bit YCbCr 4:2:2 |
| GUI plane | AR24, plane 57, `SDR[0]`, EOTF 0 |
| Video plane | NV15, `HDR10[2]`, EOTF 2 |
| `SDR2HDR_CTRL` | `0x0000000b`, eotf+r2r+oetf, `BT709_TO_BT2020` |
| HDR2SDR | off (`hdr2sdr_en=0`) |

OSD gizliyken GUI plane detach olduğu için `SDR2HDR_CTRL=0x100` bypass olması
beklenen durumdur; video tek başına HDR10 taşır. Gate'in `GUI EOTF=0` ve
`SDR2HDR=0x0b` invariant'ı OSD görünür capture ile doğrulandı.

## 7. Kodi release

Production lifecycle sahibi repo içindeki `scripts/run-kodi-rk3588.sh` olarak
önceki Gate'te belirlenmişti. Playback her ölçüm öncesi `Player.Stop` ile temiz
durduruldu; `Player.GetActivePlayers` boş döndü. Ardından launcher'ın normal
`stop` yolu kullanıldı: `Application.Quit`, altı saniyelik bekleme ve yalnız
stale süreç kalırsa exact-name cleanup.

Stop çağrısı öncesi başlatılan sonlu watcher, process ve debugfs DRM-master
durumlarını 20 ms aralıkla ölçtü. Her cycle sonunda:

- Kodi process yoktu;
- Kodi DRM fd'si yoktu;
- `/sys/kernel/debug/dri/0/clients` boştu;
- stale `kodi-gbm` yoktu;
- Kodi `RestoreOriginalMode()` ile güvenli idle mode'u 1920x1080p60'a geri
  koymuştu.

`KODI_DRM_RELEASE_FAIL` oluşmadı.

## 8. Acquire-only sonucu

4K TV formal Gate koşulunda:

```text
event=PROBE_STARTED          monotonic_ns=14769755516796
event=DRM_MASTER_ACQUIRED    monotonic_ns=14769755529046
event=DRM_MASTER_RELEASED    monotonic_ns=14770756345796
ACQUIRE-ONLY RESULT: PASS
```

Master `0.012 ms` içinde doğrulandı. Sonlu bir saniyelik hold sonrası probe
çıktı, DRM client listesi boş kaldı ve fd/process leak görülmedi.

## 9. Atomic scanout sonucu

Üç cycle'da da Kodi'nin normal stop sırasında geri koyduğu aktif 1920x1080p60
idle mode yeniden kullanıldı. Connector/CRTC/plane ID hardcode edilmedi.

| Cycle | FB | Boyut/format | TEST_ONLY | Gerçek commit | Cleanup |
| --- | ---: | --- | --- | --- | --- |
| 1 | 247 | 1920x1080 XR24, pitch 7680 | PASS | PASS | clients boş |
| 2 | 250 | 1920x1080 XR24, pitch 7680 | PASS | PASS | clients boş |
| 3 | 253 | 1920x1080 XR24, pitch 7680 | PASS | PASS | clients boş |

Cycle 1 örnek gerçek commit event'i:

```text
event=DRM_MASTER_ACQUIRED monotonic_ns=14770773766289 card=/dev/dri/card0
framebuffer_created fb=247 handle=1 1920x1080 pitch=7680 size=8294400 format=XR24
atomic_test_only=success
event=ATOMIC_SCANOUT_COMMITTED monotonic_ns=14770819121460 connector=201 crtc=89 plane=57 fb=247 mode=1920x1080
event=DRM_MASTER_RELEASED monotonic_ns=14772916608821
SCANOUT RESULT: PASS
```

Bu sonuç yalnız master alma veya GBM buffer yaratma değildir: framebuffer
primary plane'e bağlandı; connector doğru CRTC'ye bağlandı; CRTC `ACTIVE=1`,
mode blob ve tüm source/destination rectangle alanlarıyla gerçek atomic modeset
commit kernel tarafından kabul edildi.

`TRANSIENT_DRM_ACQUIRE_FAIL` ve `TRANSIENT_KMS_SCANOUT_FAIL` oluşmadı.

## 10. Kodi reacquire

Her probe çıkışından sonra aynı production launcher `start` yolu kullanıldı.
Her cycle'da:

- executable `/opt/rk3588-mediabox/kodi/lib/kodi/kodi-gbm` idi;
- args `--standalone --debug` idi;
- `card0` ve `renderD128` fd'leri açıldı;
- debugfs clients içinde `card0 master=y` görüldü;
- Mali EGL/GBM runtime yüklendi;
- JSON-RPC `pong` döndü;
- auth/client-only fallback görülmedi.

Final Kodi PID'i `99128` oldu. `KODI_DRM_REACQUIRE_FAIL` oluşmadı.

## 11. POST golden ve PRE/POST karşılaştırması

İlk cycle sonrası `POST_HANDOFF_GOLDEN` ve üçüncü cycle sonrası
`FINAL_CYCLE3_GOLDEN` aynı asset, aynı JSON-RPC açma yolu ve aynı
`tools/vop2-sdr2hdr-capture/capture-state.sh` aracıyla alındı.

| Invariant | PRE | POST cycle 1 | FINAL cycle 3 | Sonuç |
| --- | --- | --- | --- | --- |
| RKMPP / DRM PRIME | aktif | aktif | aktif | aynı |
| HEVC Main10 | evet | evet | evet | aynı |
| Video format | NV15 | NV15 | NV15 | aynı |
| Mode | 3840x2160p24 / 23.976 | aynı | aynı | aynı |
| HDR / colorspace | HDR10 / BT.2020 | aynı | aynı | aynı |
| HDMI bus | YUYV10_1X20 | aynı | aynı | aynı |
| GUI EOTF | 0 | 0 | 0 | aynı |
| Video EOTF | 2 | 2 | 2 | aynı |
| SDR2HDR OSD açık | `0x0000000b` | `0x0000000b` | `0x0000000b` | aynı |
| HDR2SDR | off | off | off | aynı |

`ACCEPTED_DISPLAY_REGRESSION` oluşmadı.

### Horizontal-shift regresyon kanıtı

Kabul edilmiş read-only `scripts/vop-geometry-watch.py`, final HDR playback
sırasında OSD off→on ve pause→resume geçişini 25 Hz'de izledi. İki farklı
composition state yakalandı; video plane her ikisinde de:

```text
Esmart0 pos=(0,42) dst=3840x2076 src=3840x2080
DSP_ST=002a0000 DSP_INFO=081b0eff ACT_INFO=081f0eff
atomic crtc=3840x2076+0+42 src=3840x2080+0+0
```

Video `x=0`, scaler ve atomic rectangle alanları değişmedi. GUI plane'in
1x1/inactive durumdan 3840x2160 active duruma geçmesi video geometrisini
kaydırmadı. Kaynak/VOP2 tarafında horizontal-shift regresyonu yoktur.

## 12. Üç-cycle repeatability ve timing

Tüm değerler gerçek event timestamp farklarıdır. Stop/start request değerleri,
production launcher çağrısından hemen önce alınan wrapper timestamp'idir; SSH
ve launcher process oluşturma overhead'ini içerdiği için konservatiftir.

| Ölçüm | Cycle 1 | Cycle 2 | Cycle 3 |
| --- | ---: | ---: | ---: |
| Kodi stop → process gone | 3.234 s | 4.230 s | 4.236 s |
| Kodi stop → DRM released | 3.242 s | 4.237 s | 4.244 s |
| Probe start → DRM master | 2.372 ms | 1.666 ms | 2.330 ms |
| Probe start → atomic scanout | 47.727 ms | 34.144 ms | 24.183 ms |
| Probe start → master release | 2.145 s | 2.120 s | 2.120 s |
| Kodi start → process | 6.270 s | 6.148 s | 6.118 s |
| Kodi start → DRM master | 7.147 s | 7.144 s | 6.878 s |
| Kodi start → JSON-RPC ready | 7.718 s | 7.636 s | 7.446 s |

Probe master release ile process exit aynı cleanup/return yolundadır; ayrı bir
external exit timestamp örneklenmediği için `probe exit → DRM released` için
uydurma değer verilmedi.

Ölçülebilen UI→Kodi handoff floor'u, UI/probe tamamen çıktıktan sonraki Kodi
start→JSON-RPC-ready kısmı için en iyi cycle'da `7.446 s`'dir. Gerçek
`Player.Open` ve ilk video-frame zamanı start ile bitişik ölçülmedi; bu yüzden
tam UI→playback değeri hesaplanmadı.

Repeatability: `3/3 PASS`. `HANDOFF_NOT_REPEATABLE` oluşmadı.

## 13. Gate-window error audit

Audit aralığı: `2026-09-11 19:23:23 +03:00` –
`2026-09-11 19:43:49 +03:00`.

Journal ve kernel journal şu pattern'lerle tarandı:

```text
drm, master, atomic, vop, vop2, commit fail, timeout, iommu,
segfault, fatal, permission, failed mode, rkmpp, mpp
```

Bulunan DRM/VOP2 kayıtları yalnız beklenen CRTC disable/enable ve ölçülen
1920x1080p60/120 ile 3840x2160p24 mode geçişleridir. Yeni commit fail, timeout,
underflow, IOMMU fault, segfault veya fatal kayıt yoktur. Mevcut Kodi ve
`kodi.old.log` içinde DRM/RKMPP failure pattern'i yoktur.

Journald warning seviyesinde her production Kodi başlangıcında iki kez görülen
`pw.conf: can't load config client.conf` satırı vardır. Build ALSA'ya pinlidir,
PulseAudio/PipeWire playback yolu kullanılmaz; bu daha önce var olan ve Gate
değişkeniyle ilişkili olmayan uyarıdır. HDMI PHY `pll/lane locked!` satırları
başarılı link lock bildirimidir, hata değildir.

## 14. Target final state

`2026-09-11T19:43:49.921610633+03:00` itibarıyla:

- `/tmp/kms-handoff-probe` kaldırıldı;
- `/tmp/kms-handoff-probe-build` kaldırıldı;
- geçici watcher, monitor ve checksum dosyaları kaldırıldı;
- probe process yok;
- Kodi PID `99128`, expected production executable/args ile çalışıyor;
- Kodi `/dev/dri/card0` ve `/dev/dri/renderD128` fd'lerini taşıyor;
- debugfs clients içinde yalnız Kodi `card0 master=y`;
- JSON-RPC Ping sonucu `pong`;
- aktif player listesi boş;
- HDMI-A-1 idle state 1920x1080p60, RGB888, SDR;
- kernel aynı accepted build;
- ALSA card dosyası hash'i değişmedi:
  `492b921e8b3bba6c472b981d8f40cc920e412e7a48237d371020a24303a48e`;
- service unit, kernel, DT, boot, CEC, Bluetooth ve network ayarı değiştirilmedi.

Kodi launcher kendi tanımlı production davranışı gereği her start'ta canonical
`guisettings-appliance.xml` dosyasını yeniden seed etti ve bağlı Sony TV'nin
32 uygun mode'unu runtime whitelist'e yazdı. Bu nedenle mutable
`guisettings.xml` byte hash'i ekran değişimi öncesindeki 1080p sink hash'iyle
aynı değildir; kritik semantic değerler finalde `adjustrefreshrate=2`,
`useprimedecoder=true`, `useprimerenderer=0`, `usedisplayasclock=false` olarak
korundu. Elle veya probe tarafından persistent display config yazılmadı.

`TARGET_RESTORE_FAIL` oluşmadı.

## 15. Raw evidence ve checksum referansları

Raw accepted capture'lar:

```text
results/mediabox-platform/evidence/s1a-direct-kms-probe-handoff-2026-09-11/
  PRE_HANDOFF_GOLDEN/
  PRE_HANDOFF_GOLDEN_OSD/
  POST_HANDOFF_GOLDEN/
  POST_HANDOFF_GOLDEN_OSD/
  FINAL_CYCLE3_GOLDEN/
  FINAL_CYCLE3_GOLDEN_OSD/
  lifecycle-events.tsv
  probe-events.txt
  geometry-watch.txt
```

Her golden dizini `gui-render-path.txt`, `plane-state.txt`,
`sdr2hdr-registers.txt`, `vop-summary.txt` ve `hdmi-state.txt` içerir. Yanlış
ekrana bağlı ilk gözlem audit bütünlüğü için `PRELIMINARY_1080_*` dizinlerinde
tutuldu; PASS değerlendirmesinde kullanılmadı.

Öne çıkan checksum eşleşmeleri:

- PRE/POST/final hidden HDMI state:
  `d4adef54ac4bfc918066985fe577c7ca20f96e8ca65ecfad5292db458cc3bcda`
- PRE/POST/final hidden SDR2HDR register capture:
  `5444b319d52970a61d2c037c94233a3c4fc9db5a4d607b711b7e346d748743a6`
- PRE ve POST OSD SDR2HDR register capture:
  `d9aef8a6e85ccb60c03dc80ceceb8ad4ae40da19c7347ebafafb451b72fa2842`
- Canonical asset:
  `f4e32b8d3feb7efdebaaefaf7338a25bcc86ee144bf82bd4b93da6b80906f96a`

## 16. Sonraki Gate önerisi

Gate S1A process-lifecycle display ownership primitive'ini kapatır. Sonraki
Gate, bu probe'u ürün UI'ına büyütmek olmamalıdır. En dar sonraki adım,
önceden tariflenen S2 kapsamında `mediaboxd` v0'ın yalnız Kodi JSON-RPC köprüsü
ve Stremio `Play on TV` intercept akışını doğrulamaktır.

S2, bu Gate'te kanıtlanan tek-master invariant'ını tüketmeli; Kodi, kernel,
DT, HDR/color pipeline, CEC, Bluetooth ve persistent service mimarisini aynı
Gate'e dahil etmemelidir.

## DIRECT_KMS_LIFECYCLE_HANDOFF_PASS
