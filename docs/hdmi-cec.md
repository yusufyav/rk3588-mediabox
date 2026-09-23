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
4. Adaptörde önceki bir yapılandırma varsa temizlenir, sonra
   `CEC_ADAP_S_LOG_ADDRS` CEC 2.0 Playback Device / `MediaBox` kimliği ve
   `CEC_LOG_ADDRS_FL_ALLOW_RC_PASSTHRU` ile **fiziksel adres olsun olmasın**
   çağrılır. Fiziksel adres sürücü/EDID'ye aittir ve userspace tarafından
   yazılmaz; `f.f.f.f` iken yapılandırma kabul edilir ve çekirdek, o sokete bir
   televizyon bağlandığı anda mantıksal adresi kendisi alır.
5. Tek bir alıcı iş parçacığı bütün adaptörleri ve bir `eventfd`'yi tek
   `poll()` ile, zaman aşımı olmadan bekler; kapanışta `eventfd`'ye yazılır.
   Ölçüm: boştayken 10 saniyede 4 uyanma (eski 250 ms'lik döngüde 40).
6. Receive loop mesajları ve User Control Pressed/Released çiftlerini işler.
7. Kapanışta logical address listesi temizlenir.

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

Outbound işlemler yalnız explicit API/CLI komutudur:

- `cec active-source`: broadcast `<Active Source>` ve EDID fiziksel adresi.
- `cec wake-tv`: TV'ye `<Image View On>`.
- `cec standby-tv`: TV'ye `<Standby>`.

Daemon başlangıcında wake/standby/active-source otomatik gönderilmez.
`cec devices` explicit çağrısı 0..14 logical address poll'ü yapar ve yanıt veren
cihazlardan `<Give Physical Address>` ister. Gözlenen `<Report Physical Address>`
mesajları topology kaydını zenginleştirir. Durum çıktısı adapter/driver,
capability, fiziksel/mantıksal adresler, bilinen cihazlar, son RX/TX ve hata
sayaçlarını içerir.

CEC RC passthrough kernelin `rc0 -> eventN` yolunu da etkin tutar. Daemon hiçbir
evdev aygıtını grab etmediği için Kodi'nin bugünkü doğrudan input davranışı
bozulmaz. Varsayılan `Ui` modunda raw CEC medya olayları da Kodi'ye tekrar
gönderilmez; böylece çift yürütme oluşmaz.
