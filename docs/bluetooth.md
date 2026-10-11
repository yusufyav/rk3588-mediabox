# Bluetooth

How the appliance's Bluetooth is driven: the controller, bluez, the daemon's
verbs and the television's settings screen. The code is
`rust/crates/mediaboxd-rs/src/wireless.rs` (daemon) and
`rust/crates/mediabox-tv/src/screens/wireless.rs` (screen model).

## Hardware

| Board | Part | Bus | Firmware | Recovery |
| --- | --- | --- | --- | --- |
| Orange Pi 5 Plus | RTL8852BU (Bluetooth half of the RTL8852BE combo) | USB (`0bda:b85b`, `btusb`) | `rtl_bt/rtl8852bu_fw.bin` by the kernel | none needed: power off/on 85/197 ms |
| Orange Pi 5 Ultra | UART part | UART, attached by a vendor unit | pushed by that unit | re-attach unit, then `bluetooth.service` |

The Plus shares one chip between Wi-Fi (PCIe) and Bluetooth (USB);
`mediabox-wireless-up` unblocks Wi-Fi first for that reason. bluez is 5.82,
`main.conf` is stock (`TemporaryTimeout` 30 s).

## bluetoothctl, not D-Bus

Measured on the Plus: `show` + `devices` + `info` × 3 answer in 83 ms;
a search is running 94 ms after it is asked for and stops 199 ms after the
bluetoothctl that owns it exits (bluez ties a discovery to its client). A
resident D-Bus client would save tens of milliseconds and cost a dependency,
so each verb stays one `bluetoothctl` command — with a time limit, killed on
expiry:

| Verb | Limit | On expiry |
| --- | --- | --- |
| show / devices / info | 5 s | error |
| power | 10 s | recovery (on) |
| pair | 35 s | `cancel-pairing`, half-made bond removed |
| connect | 20 s | `disconnect` (bluez keeps trying otherwise — measured) |
| disconnect / remove / trust | 10 s | error |

A status is three listings (`devices`, `devices Paired`, `devices Connected`)
and an `info` only for a connected device (battery) or a device seen for the
first time (icon, cached).

## Coordination

- **Controller lock** (`RwLock`): power and recovery are exclusive; device
  verbs and searches share it. A recovery can therefore never run twice or
  under a pairing.
- **Device claim**: one verb per address. A second one is refused
  ("Bu cihazda bir işlem zaten sürüyor"); other devices are not held up.
- **Pairing window** (`Mutex`): `pairable on` → `pair` → `pairable off`, one
  pairing at a time. Discoverable is never switched on.
- **Search**: one at a time, 12 s, started by the daemon and answered as soon
  as bluez reports `Discovering: yes` (or with why not — a search on a radio
  that is off used to exit 0 and look like a search that found nothing).
  Pairing and connecting end it first, as `CachedBluetoothDevice.startPairing`
  does. `bluetooth_scan {stop: true}` ends it; the screen sends that when it is
  left.
- **Found devices** are kept 120 s after bluez forgets them, so one search
  that misses a device does not move the list. Pairing one bluez has dropped
  listens for it again (10 s) before it pairs.

## Pairing, trust, connect

```
pair ── fail ──> cancel-pairing; partial bond removed (if it was not paired before) → error
  │
  ok ── trust (failure reported, not fatal)
  │
  connected already? ── yes ──> done
  │ no
  connect ── fail ──> paired, not connected: outcome.connected=false + reason
```

The answer carries `outcome {paired, trusted, connected, error}`; the screen
says "Eşleşti, ancak bağlanamadı: …" instead of claiming success.

The agent (`mediabox-bt-agent`) is `NoInputNoOutput`: every request is Just
Works, accepted without a question. The protection is the window, not the
answer — the board is pairable only while a pairing the viewer started runs.
A keyboard that would show a passkey pairs without one; showing the passkey
on the television would need an agent the interface can talk to.

## Screen

As the Wi-Fi list (`RadioList` in `wireless.slint`): the switch, "Eşleşmiş
cihazlar" (connected first, then by name), "Kullanılabilir cihazlar" with
"Yenile" — the spinner while a search runs — and the named devices found, by
name. Nameless found devices are not listed, as Android's default. Rows carry
the device's glyph and, under the name, "Eşleşiyor…", "Bağlanıyor…",
"Bağlı, pil %80" and so on.

Ok follows `BluetoothDevicePreference.onClicked`: found → pair, paired →
connect, connected → its menu (Bağlantıyı kes, Eşleşmeyi kaldır, the latter
confirmed). Long Ok or Menu opens the same menu on any paired device.

While the screen is open it looks every 2 s; only an action's own answer ends
that action's "…" — a look never does, and a look taken while the radio is
being switched is ignored.

## Ses

Bluetooth kulaklık ve hoparlör sesi **bluez-alsa** ile çalışır; PipeWire veya
PulseAudio yoktur, Kodi ve oynatıcı doğrudan ALSA'ya çalar.

| Parça | Ne yapar |
| --- | --- |
| `bluez-alsa-utils` | `bluealsa` daemon'u: bluez'e A2DP uç noktası kaydeder. Onsuz kulaklık eşleşir ama her bağlantı `a2dp-sink profile connect failed: Protocol not available` ile düşer (Plus'ta ölçüldü) |
| `libasound2-plugin-bluez` | ALSA eklentisi: `bluealsa:DEV=<adres>,PROFILE=a2dp` PCM'i |
| `packaging/systemd/bluealsa.service.d/10-mediabox.conf` | Yalnız `-p a2dp-source` (kutu → kulaklık). Debian ayrıca `a2dp-sink` açar; o, odadaki her telefonun kutuya ses çalabilmesi demektir |
| `bluealsa-aplay.service` | Kapalı (gelen sesi çalan servis) |
| `config/alsa/62-mediabox-bluetooth.conf` | `mediabox_bluetooth` PCM'i, ipucu açık. bluez-alsa'nın kendi ipucu `namehint.extended` olmadan gizli kalır ve Kodi onu listelemez |

Kurulum: `install-mediabox.sh` ve `deploy-mediabox-v3.sh` paketleri yoksa kurar,
drop-in'i ve ALSA tanımını yerleştirir, `bluealsa-aplay`'i kapatır;
`create-mediabox-release.sh` paketleri golden karttan `runtime-packages.txt`'ye
yazar.

**Yönlendirme.** "Otomatik" seçiliyken bağlı A2DP cihazı ekranın sesinden önce
gelir; elle seçilmiş çıkış korunur (seçili çıkış takılı değilse kulaklık
kullanılır). mediaboxd-rs `bluealsa-cli monitor --properties`'i izler;
`PCMAdded`/`PCMRemoved` gelince plan yeniden hesaplanır ve çalan mpv (kesintisiz)
ile çalışan Kodi (`audiooutput.audiodevice`, kulaklıkta passthrough kapalı)
yeni cihaza geçirilir. Kodi kulaklık bağlıyken açılırsa `mediabox-hdmi-prepare`
aynısını `guisettings.xml`'e yazar.

**Ses seviyesi (Android modeli).** Kulaklıkta tek seviye MediaBox'unkidir ve
AVRCP mutlak ses (`bluealsa --a2dp-volume`) ile kulaklığa uygulanır; mpv kazancı
100'de kalır. Kurallar AOSP'den: kulaklıktan gelen değer kaydedilir ama geri
yazılmaz (`AudioService`, `FLAG_BLUETOOTH_ABS_VOLUME`); gönderdiğimiz
`SetAbsoluteVolume`'un cevapları basış sayılmaz (`avrcp/device.cc`). BlueZ
cevabı ve kulaklığın kendi olayını aynı `Volume` özelliğinde verir
(`profiles/audio/avrcp.c`) ve her gönderim üç kez bildirilir (ölçüldü), bu
yüzden son 2 s içinde gönderilmiş bir değere eşit her bildirim cevaptır. Ses
değişiklikleri daemon'da tek kilit altında; arayüz ses isteklerini tek
kuyrukta sırayla gönderir ve göstergeye yalnızca son cevabı çizer.

**Kodi.** Kodi'nin kendi seviyesine dokunulmaz (Kodi Android'de de ses
tuşlarını sisteme bırakır: `AndroidKey.cpp`, `XBMCApp.cpp onVolumeChanged`).
Ses kulaklıktayken `keymaps/mediabox-bluetooth.xml` (→
`/run/mediabox/kodi-bluetooth-keymap.xml`) ses tuşlarını
`NotifyAll(mediabox,…)`'a verir; daemon MediaBox seviyesini değiştirir ve
Kodi'de "Ses %X" bildirimi gösterir (en fazla 1,6 s'de bir, son değerle; Kodi
bildirimleri kuyruğa alır). Ses HDMI'da ya da başka karttayken keymap boştur ve
Kodi tuşları eskisi gibi kendisi işler (`reloadkeymaps` ile çalışırken
değişir; ölçüldü).

**Medya tuşları.** BlueZ her kulaklığın AVRCP tuşları için `<ad> (AVRCP)` giriş
aygıtı açar. Ses o kulaklıkta değilken aygıt çekirdeğin
`/sys/class/input/inputN/inhibited` anahtarıyla susturulur (Android aktif
olmayan cihazın tuşlarını yok sayar); bunun için mediaboxd-rs.service'e
`ReadWritePaths=-/sys/devices/virtual/input` eklendi.

**Kulaklık ve klavye birlikte: ses kesintisi.** Kulaklık ile bir BLE klavye
aynı anda bağlıyken ses gidip geliyordu. Plus'ta btsnoop ile ölçüldü (aynı
film, Wi-Fi, kulaklık): klavye yokken denetleyicinin kulaklığa ayrılmış ACL
tamponu zamanın %0-19'unda doluydu, ROG AZOTH 7,5 ms aralıkla bağlıyken %98;
akış saniyelerce gerçek zamanın (~42 kB/s) altına düştü. Wi-Fi yükü, AVRCP
ses komutları ve BLE arka plan taraması ayrı ayrı denenip elendi. İki neden
vardı, ikisi de Android'in davranışıyla düzeltildi:

*bluez-alsa yarım paket gönderiyordu.* ALSA eklentisi PCM'i 4096 baytlık bir
pipe ile (1024 kare) verir; bluealsa her uyanışta eldekini kodladığı için her
dolu SBC paketini (MTU 672 baytta 6 çerçeve) 2 çerçevelik bir paket izliyordu:
paketlerin %38-50'si dolu, saniyede ~104 paket. Android kodlayıcısı MTU'ya göre
sabit sayıda çerçeve gönderir (`system/stack/a2dp/a2dp_sbc_encoder.cc`).
`patches/bluez-alsa/0001-a2dp-sbc-send-full-packets.patch` aynısını yapar:
PCM tamponu bir MTU'luk çerçeveyle dolmadan kodlamaz. Sonuç (ölçüldü): tüm
paketler dolu (671 B), saniyede 62,6 paket, 42,0 kB/s. bluez-alsa 5.0.0'da
aynı kod duruyor; yama yukarıya gönderilmedi.

Paket golden kartta Debian kaynağından derlenir:

```sh
MEDIABOX_HOST=<kart> scripts/build-bluez-alsa.sh
```

Betik Debian'ın `4.3.1-3` kaynak paketini indirir (kartta `deb-src` yoksa
yalnız derleme için ekler), yamayı uygular, `4.3.1-3+mediabox1` olarak
derler (`nocheck noddebs`), iki paketi kurup `apt-mark hold` ile sabitler ve
`.deb`'leri `/opt/rk3588-mediabox/packages/bluez-alsa/` altına koyar. Derleme
bağımlılıklarını kaldırır ve paket listesinin derleme öncesiyle aynı olduğunu
denetler; değilse hata verir. Plus'ta 1 dk 21 sn sürdü; üretilen `bluealsa` ve
ALSA eklentisi daha önce ölçülen yamalı dosyalarla birebir aynı (sha256).

Dağıtım: release önekle birlikte `.deb`'leri taşır; golden kartta yamalı
sürüm ya da onun `.deb`'leri yoksa capture kapıda durur. `install-mediabox.sh`
paket adlarını apt'den kurduktan sonra arşivdeki `.deb`'leri kurup hold eder;
`deploy-mediabox-v3.sh` önekte `.deb` varsa onları kurar, yoksa Debian'ınkini
kurup uyarır. Denendi: Debian'ın `4.3.1-3`'ü kuruluyken kurucunun ve
deploy'un bloğu `4.3.1-3+mediabox1 hi`'ye geçirdi; ikinci çalıştırma bir şey
değiştirmedi. Hold, Debian'dan gelecek bir `4.3.1-4`'ün yamayı ezmesini
engeller; Debian sürümü yükseltilecekse yama o sürüme karşı yeniden denenir
(`MEDIABOX_BLUEZ_ALSA_DEBIAN_VERSION`).

*Klavye 7,5 ms istiyordu.* ROG AZOTH bağlantı aralığını en az ve en çok 7,5 ms
ister (bluez kaydında `MinInterval=MaxInterval=6`). Android çevre biriminin
isteğini en az 11,25 ms'ye yükseltir (`system/stack/l2cap/l2c_ble.cc`,
`L2CA_AdjustConnectionIntervals`, `BTM_BLE_CONN_INT_MIN_LIMIT` 0x0009); Linux
yükseltmez (2019'da eklenen alt sınır farelerin gecikmesi yüzünden geri
alındı, 68d19d7d9957). mediaboxd-rs (`src/bt_le.rs`) hci0'ı ham HCI soketiyle
izler: merkezi olduğu bir LE bağlantısı 11,25 ms'nin altında kurulur ya da
güncellenirse `HCI_LE_Connection_Update` ile 11,25 ms'ye çeker; gecikme
(latency) ve denetim süresi korunur, süre kuralı bozulacaksa gecikme düşürülür
(Core Vol 6 Part B 4.5.2). Klavye her ~7 s'de yine 7,5 ms ister ve her
seferinde yeniden çekilir (ölçüldü: 5 dakikada 43 istek, 43 yükseltme). Bunun
için mediaboxd-rs.service'e `RestrictAddressFamilies=… AF_BLUETOOTH`
eklendi. İkisi birlikte (ölçüldü, 5 dakika, film + kulaklık + klavye): tampon
doluluğu %0, 42 kB/s altı saniye yalnız sarma anlarında; kullanıcı kesinti
duymadı.

**Ölçülenler (Plus, Arctis GameBuds):** codec yalnız SBC (kulaklığın
sunduğu); bluez-alsa'nın kaynak codec'leri SBC, MP3, aptX, aptX-HD,
FastStream, LDAC, Opus — AAC yok. bluez-alsa'nın bildirdiği gecikme 200 ms;
dudak senkronu için ayrıca ayar yapılmadı.
