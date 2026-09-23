# mediaboxctl

CLI iş mantığı çalıştırmaz; `/run/mediabox/mediaboxd.sock` üzerindeki typed
daemon protokolünü kullanır. Farklı test socket'i `--socket PATH` ile seçilir.
Daemon yoksa exit code `2` ve socket yolunu içeren açık hata verir. API hataları
exit code `1` ile raporlanır.

```sh
mediaboxctl status
mediaboxctl system
mediaboxctl kodi status
mediaboxctl kodi play-pause
mediaboxctl kodi stop
mediaboxctl kodi seek +30
mediaboxctl kodi open https://example.invalid/video.mp4 --resume 120
mediaboxctl kodi restart
mediaboxctl cec status
mediaboxctl cec devices
mediaboxctl cec active-source
mediaboxctl cec wake-tv
mediaboxctl cec standby-tv
mediaboxctl input monitor
```

`--json` global seçeneği her daemon yanıtını veya input olayını tek satırlık
machine-readable JSON olarak basar:

```sh
mediaboxctl --json status
mediaboxctl --json input monitor
```

`wake-tv` ve `standby-tv` TV'nin güç durumunu gerçekten değiştirir; yalnız
operatörün açık isteğiyle kullanılmalıdır. Otomatik startup politikası değildir.

Ekran ayarı TV'deki Ayarlar > Ekran ile aynı işi yapar ve aynı kuralları
gösterir:

```sh
mediaboxctl display status                     # hatta giden, seçim, bekleyen deneme
mediaboxctl display modes                      # her mod ve gönderilebilen renkler
mediaboxctl display modes 3840x2160p30         # o moddaki her hücre, gerekçesiyle
mediaboxctl display set 3840x2160@30 --color ycbcr422 --bits 10
mediaboxctl display set auto
mediaboxctl display keep                       # denemeyi kalıcı yap
mediaboxctl display revert                     # denemeyi şimdi geri al
```

`set` bir denemedir: 15 saniye içinde `keep` gelmezse önceki ayara dönülür. Mod
`auto`, `GxY@Hz` ya da listedeki etiketle (`3840x2160p29.97`, `1920x1080i60`)
verilir. Deneme yalnız televizyon arayüzü ekranı tutarken kabul edilir.
