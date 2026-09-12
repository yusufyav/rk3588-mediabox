# Linux-native HDMI-CEC

MediaBox CEC desteği libCEC kullanmaz. `mediabox-cec`, `linux/cec.h` ABI'sini
`repr(C)` yapılar ve ioctl'lerle doğrudan kullanır.

Başlangıç sırası:

1. Tercih edilen `/dev/cecX` açılır veya `/dev/cec*` deterministik sıralanır.
2. `CEC_ADAP_G_CAPS` ile logical-address/transmit yetenekleri doğrulanır.
3. `CEC_MODE_INITIATOR | CEC_MODE_EXCL_FOLLOWER` alınır. `EBUSY`, başka owner'ı
   açıkça raporlayan hata olur.
4. `CEC_ADAP_G_PHYS_ADDR` okunur; `0xffff` bağlantısız kabul edilir. Fiziksel
   adres sürücü/EDID'ye aittir ve userspace tarafından yazılmaz.
5. `CEC_ADAP_S_LOG_ADDRS`, CEC 2.0 Playback Device / `MediaBox` kimliği ve
   `CEC_LOG_ADDRS_FL_ALLOW_RC_PASSTHRU` ile çağrılır.
6. Receive loop mesajları ve User Control Pressed/Released çiftlerini işler.
7. Kapanışta logical address listesi temizlenir.

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
