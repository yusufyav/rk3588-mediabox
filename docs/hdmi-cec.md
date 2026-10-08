# Linux-native HDMI-CEC

MediaBox CEC desteği libCEC kullanmaz. `mediabox-cec`, `linux/cec.h` ABI'sini
`repr(C)` yapılar ve ioctl'lerle doğrudan kullanır.

Hangi soketin televizyona bağlı olduğu açılışta bilinmez ve sonradan değişir:
iki HDMI verici olan bir kartta her vericinin kendi CEC adaptörü vardır ve
televizyon hangisindeyse fiziksel adresi o alır. Bu yüzden daemon tek bir
adaptör seçmez, hepsini tutar.

Başlangıç sırası:

1. Her `/dev/cec*` açılır (`--cec-device` verilmişse yalnız o). Bir adaptörün
   açılamaması diğerlerini durdurmaz.
2. `CEC_ADAP_G_CAPS` ile logical-address/transmit yetenekleri doğrulanır.
3. `CEC_MODE_INITIATOR | CEC_MODE_EXCL_FOLLOWER` alınır. `EBUSY`, başka owner'ı
   açıkça raporlayan hata olur.
4. Adaptörde önceki bir yapılandırma varsa temizlenir, sonra HDMI-CEC
   ayarlarına göre ya hiç claim yapılmaz (CEC kapalı) ya da
   `CEC_ADAP_S_LOG_ADDRS` CEC 2.0 Playback Device / `MediaBox` kimliğiyle,
   TV kumandası açıksa `CEC_LOG_ADDRS_FL_ALLOW_RC_PASSTHRU` ile, **fiziksel
   adres olsun olmasın** çağrılır. Fiziksel adres sürücü/EDID'ye aittir ve userspace tarafından
   yazılmaz; `f.f.f.f` iken yapılandırma kabul edilir ve çekirdek, o sokete bir
   televizyon bağlandığı anda mantıksal adresi kendisi alır.
5. Tek bir alıcı iş parçacığı bütün adaptörleri ve bir `eventfd`'yi tek
   `poll()` ile, zaman aşımı olmadan bekler; kapanışta `eventfd`'ye yazılır.
   Ölçüm: boştayken 10 saniyede 4 uyanma (eski 250 ms'lik döngüde 40).
6. Receive loop mesajları ve User Control Pressed/Released çiftlerini işler.
7. Kapanışta önce güç politikası uygulanır (aşağıda), sonra logical address
   listesi temizlenir.

Komutlar, o anda mantıksal adresi olan adaptöre gider. Fiziksel ve mantıksal
adresler açılışta önbelleğe alınmaz, kullanıldığı anda çekirdekten
(`CEC_ADAP_G_PHYS_ADDR`, `CEC_ADAP_G_LOG_ADDRS`) okunur; böylece başka bir
girişe alınmış bir televizyon da doğru adreslenir.

Neden böyle: eski daemon açılışta seçili çıkışın tek adaptörünü alıyor, fiziksel
adres yoksa CEC'i kalıcı olarak "kullanılamıyor" sayıyordu. Plus'ta ölçüldü:
hiçbir şey takılı değilken açılan daemon boş soketin `/dev/cec0`'ını aldı,
`f.f.f.f` gördü ve çekirdek televizyonun `4.0.0.0` adresini çoktan `/dev/cec1`'e
vermişken ömrü boyunca CEC'siz kaldı. Şimdi aynı açılışta hiçbir şey yeniden
başlatılmadan: `/dev/cec1 (dw_hdmi_qp) physical 4.0.0.0 logical [4]`.

Normalize edilen minimum kumanda kümesi: Up, Down, Left, Right, Ok, Back, Home,
Play, Pause ve Stop. Basılan tuş initiator bazında tutulduğu için operandsız
`User Control Released` doğru aksiyonun release olayı olarak yayınlanır.

Elle gönderilen komutlar (API/CLI/TV/web):

- `cec active-source`: broadcast `<Active Source>` ve EDID fiziksel adresi.
- `cec wake-tv`: TV'ye `<Image View On>`.
- `cec standby-tv`: TV'ye `<Standby>`.

`cec devices` explicit çağrısı 0..14 logical address poll'ü yapar ve yanıt veren
cihazlardan `<Give Physical Address>` ister. Gözlenen `<Report Physical Address>`
mesajları topology kaydını zenginleştirir. Durum çıktısı adapter/driver,
capability, fiziksel/mantıksal adresler, bilinen cihazlar, son RX/TX, hata
sayaçları, **kayıtlı HDMI-CEC ayarları** (`settings`) ve **bu açılışın oturumu**
(`session`) içerir.

## HDMI-CEC ayar paneli ve güç politikası

Ayarlar → TV ve Kumanda → HDMI-CEC (TV), web arayüzünde "HDMI / CEC" sekmesi
ve `mediaboxctl cec set <ayar> <değer>` aynı isteği gönderir:
`{"command":"cec_settings_set","settings":{...}}`. Kural `mediabox-core`'da
(`CecSettings::validate`), karar mantığı `mediabox-cec::policy`'de (adaptöre
dokunmayan saf fonksiyonlar), uygulama `mediaboxd-rs`'te.

| Ayar | Alan | Varsayılan | AOSP karşılığı |
|---|---|---|---|
| HDMI-CEC | `enabled` | açık | `hdmi_cec_enabled` |
| TV kumandası | `remote_control` | açık | doğrudan yok (AOSP UCP'yi hep alır) |
| Açılışta TV'yi aç | `wake_tv_on_start` | kapalı | One Touch Play'in `<Image View On>` yarısı |
| Açılışta TV girişini seç | `active_source_on_start` | kapalı | One Touch Play'in `<Active Source>` yarısı |
| Kapanışta TV'yi kapat | `standby_tv_on_shutdown` | kapalı | `power_control_mode` ≠ `none` |
| Kapanışta önceki duruma dön | `restore_power_on_shutdown` | kapalı | yok; bize ait, `<Give Device Power Status>` ile |
| Kapatma hedefi | `power_target` | TV | `power_control_mode` (`to_tv`, `to_tv_and_audio_system`, `broadcast`) |
| Başka girişe geçilince | `on_active_source_lost` | açık kal | `power_state_change_on_active_source_lost` (`none`/`standby_now`) |

Varsayılanlar panelden önceki davranıştır: kullanıcı açıkça istemeden TV'ye,
AVR'ye veya başka cihaza güç ya da kaynak mesajı gönderilmez.

Kalıcılık: `/var/lib/mediabox/cec.json` (yaz + rename). Oturum:
`/var/lib/mediabox/cec-session.json`, `boot_id` ile etiketli. `/run/mediabox`
kullanılmaz çünkü unit'in `RuntimeDirectory`'si daemon durunca silinir; o
zaman her deploy bir açılış gibi görünüp TV'yi yeniden uyandırırdı.

### Ana anahtar

- Kapalı: her adaptörün mantıksal adresleri temizlenir (`Claim::Released`).
  Kutu bus'ta görünmez, çekirdek UCP'yi rc-core'a (Kodi'nin okuduğu evdev)
  iletmez, hotplug'da da kendiliğinden claim yapmaz. Daemon ayrıca
  `cec_wake_tv`, `cec_standby_tv`, `cec_active_source`, `cec_devices`
  isteklerini `CEC_DISABLED` ile reddeder, RX döngüsü tuş yayınlamaz ve
  policy hiçbir yanıt üretmez. IR, Bluetooth ve USB girişleri bu yoldan
  geçmez, etkilenmez.
- Açık: adaptörler yeniden claim edilir. Daemon/cihaz yeniden başlatılmaz.
  Açılış dizisi (One Touch Play) bir ayar değişikliğinde çalışmaz; AOSP'de de
  bu bir açılış/uyanma davranışıdır.
- Alt ayarlar ana anahtar kapalıyken saklanır; açılınca aynen döner.
- TV kumandası ayarı değişince adaptörler RC passthrough bayrağıyla/bayraksız
  yeniden claim edilir; daemon da UCP'yi giriş bus'ına yayınlamayı keser.
  Bu, güç ayarlarından bağımsızdır.

### Açılış (bir açılışta bir kez)

`start_done` oturumda tutulur; aynı açılışta daemon yeniden başlarsa dizi
tekrar çalışmaz.

1. Seçili çıkışın adaptörü adres alana kadar saniyede bir bakılır, en çok
   30 sn. 5. ve 15. saniyede adres alamamış adaptör bir kez yeniden claim
   edilir. Süre dolarsa dizi nedeni loglanarak atlanır. Ayrı bir iş
   parçacığında çalışır; açılışı bloke etmez. Ana anahtar bu sırada
   kapatılırsa bekleme biter.
2. Uyandırma ya da geri dönüş açıksa `<Give Device Power Status>` gönderilir,
   `cec_msg.reply = <Report Power Status>` ile çekirdek yanıtı bekler (1 sn).
   Yalnız 0 (açık) ve 1 (bekleme) kesin sayılır; 2/3, yanıt yok veya
   Feature Abort `Unknown`'dır.
3. Uyandırma açık ve TV "açık" demediyse `<Image View On>`.
4. Aktif kaynak açıksa broadcast `<Active Source>`.

Her mesaj en çok 3 kez denenir (250 ms ara).

### Kapanış

`main` SIGTERM'i aldığında, RX iş parçacığı durdurulmadan ve adaptörler
bırakılmadan önce `cec_shutdown` çalışır. Neden systemd'nin iş kuyruğundan
okunur (`systemctl list-jobs`): `poweroff.target`/`halt.target` → kapatma,
`reboot.target`/`kexec.target`/`soft-reboot.target` → yeniden başlatma, hiçbiri
→ yalnız daemon duruyor (deploy). Kuyruk okunamazsa daemon'un kendisinden
istenen `system_power` kullanılır.

- Yeniden başlatma ve daemon durması: hiçbir şey gönderilmez.
- Kapatma + "TV'yi kapat": `power_target`'a `<Standby>` (0; 0 ve 5; ya da 15).
- Kapatma + "önceki duruma dön": yalnız oturumda TV başlangıçta *bekleme*
  ve TV'yi MediaBox uyandırdıysa TV'ye (0) `<Standby>`. Yalnız TV'nin
  başlangıç durumu bilindiği için hedef ayarından bağımsız olarak yalnız TV.
- İkisi aynı anda açık olamaz: daemon `CEC_SETTINGS_INVALID` ile reddeder;
  arayüzler birini açınca diğerini kapatır (`CecSettings::with`).
- Başka bir cihaz aktif kaynak olduysa (`ActiveSource::Other`) hiçbir şey
  gönderilmez. AOSP `onStandby` yalnız `wasActiveSource` iken gönderir; burada
  bus hiçbir şey söylemediyse (`Unknown`) de gönderilir, yoksa açılış dizisi
  kapalı bir kutu TV'yi hiç kapatamazdı.
- Kapatmada kutu aktif kaynaksa (`ActiveSource::Us`) ve TV kapatılmıyorsa
  TV'ye `<Inactive Source>` gider (AOSP `onStandby`, `power_control_mode`
  `none`). TV'nin hangi girişe döneceğine TV karar verir; bir playback
  cihazının TV'nin girişini değiştirebileceği standart bir mesaj yoktur.
  Neden eklendi: 2026-10-08 Plus'ta "açılışta aktif kaynak" + "önceki duruma
  dön" ile TV başta açıktı; kapatmada hiçbir şey gitmedi
  (`standby -> []`) ve TV MediaBox girişinde kaldı.
- Toplam süre 3 sn ile sınırlı (unit `TimeoutStopSec=10s`).

### Bus'tan gelenler (yalnız seçili adaptör)

| Gelen | Tepki |
|---|---|
| TV'den `<Set Stream Path>` bizim fiziksel adrese | aktif kaynak = biz, broadcast `<Active Source>` |
| TV'den `<Routing Change>` yeni adres bizimki | aynı |
| `<Set Stream Path>`/`<Routing Change>` başka adrese, biz aktifken | kaynak kaybı |
| Başka cihazdan `<Active Source>` | aktif kaynak = başkası; biz aktifken kaynak kaybı |
| `<Request Active Source>` | yalnız aktif kaynaksak `<Active Source>` |
| `<Inactive Source>` | aktif başkasıysa → bilinmiyor |
| Bize `<Give Device Power Status>` | `<Report Power Status>` açık |
| TV'den `<Report Power Status>` / `<Standby>` | oturumdaki TV güç durumu |

Kaynak kaybı + "Beklemeye geç": MediaBox'ın kendi uyku durumu yok; güvenli
yaşam döngüsü kullanılır: kendi oynatıcımız `Intent::Stop` ile durur (yer
hesaba yazılır), Kodi oynatıyorsa durdurulur ve ekran arayüze döner. Hiçbir
şey kapatılmaz. "Açık kal" (varsayılan) iken başka girişe geçmek hiçbir şeyi
durdurmaz.

Diğer soketteki ikinci bir TV'nin adaptöründen gelen mesajlar tuş olarak
işlenir (eski davranış) ama policy'ye girmez; gönderim her zaman seçili
çıkışın adaptöründen yapılır (`crate::cec::choose`), belirsiz eşleşmede hiç
gönderilmez.

### Platforma özgü farklar

- AOSP One Touch Play `<Text View On>` gönderir; burada Sony'de ölçülmüş olan
  `<Image View On>` korunur. İkisi de TV için zorunlu mesajlardır.
- AOSP'de TV açıkken de One Touch Play gönderilir; burada TV "açık" diyorsa
  `<Image View On>` atlanır (yalnız aktif kaynak ayarı girişi değiştirir).
- `<Image View On>` yalnız TV'ye gider; ses sistemi standartta bu yolla
  açılmaz (TV/ARC üzerinden kendi açılır). Hedef ayarı yalnız kapanıştaki
  `<Standby>`'ı etkiler.
- Kernel çekirdek mesajlarını (Give Physical Address, OSD Name, CEC Version,
  Vendor ID) kendisi yanıtlar; daemon yalnız tabloda olanları ele alır.

CEC RC passthrough kernelin `rc0 -> eventN` yolunu da etkin tutar. Daemon hiçbir
evdev aygıtını grab etmediği için Kodi'nin bugünkü doğrudan input davranışı
bozulmaz. Varsayılan `Ui` modunda raw CEC medya olayları da Kodi'ye tekrar
gönderilmez; böylece çift yürütme oluşmaz.

## Açılışta mantıksal adres alınamazsa adaptör bir daha açılmıyordu

2026-09-25, Plus (6.1.115): Açılışta `/dev/cec1` için Playback adresi alınamadı
(`CEC /dev/cec1: … Playback mantıksal adresi alınamadı`). dmesg'de
`cec-dw_hdmi_qp: message 44 timed out` ve `message 88 timed out` görüldü;
bunlar adres 4 ve 8 için yapılan claim yoklamaları, sonuç NACK değil gönderim
zaman aşımı. Kernel adres 8'i sonradan kendisi aldı, ama daemon o adaptörü
listeye hiç almadı ve bir sonraki açılışa kadar her komutu
`seçili çıkışın CEC bağdaştırıcısı /dev/cec1 açık değil` diye reddetti.

Kod yolu:
- `mediabox-cec/src/lib.rs`: fiziksel adres geçerliyken mantıksal adres 15
  dönerse `CecError::Busy`.
- `mediaboxd-rs/src/main.rs`: adaptörler açılışta bir kez `Adapter::open_all()`
  ile açılıyor; açılamayan listeye girmiyor, sonra yeniden denenmiyor.

Bir sonraki açılışta CEC normal çalıştı. Yoklamaların neden zaman aşımına
uğradığı ölçülmedi.

Düzeltme (2026-10-08, cihazda henüz ölçülmedi): `Adapter::claim` adres 15
dönmesini hata saymaz; adaptör listede kalır, durum "mantıksal adres
alınamadı" der, çekirdek sonraki hotplug'da yeniden claim eder ve açılış
dizisi bekleme sırasında `Adapter::recover` ile iki kez yeniden ister.
