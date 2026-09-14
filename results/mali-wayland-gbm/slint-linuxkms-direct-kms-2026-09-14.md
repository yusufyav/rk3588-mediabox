# Slint LinuxKMS direct-KMS sonucu — 2026-09-14

## Sonuç

`mediabox-tv`, Slint 1.17.1'in `backend-linuxkms-noseat` ve
`renderer-femtovg` özellikleriyle AArch64 için başarıyla derlendi. Wayland ve
software renderer derlemeye dahil edilmedi. Ancak uygulama hedefte ilk kareden
ve EGL başlatmasından önce şu kesin hatayla durdu:

```text
Error: Error creating gbm device: No such file or directory (os error 2)
```

Bu nedenle istenen durma kuralı uygulandı; başka backend, renderer, Mali/Mesa
varyantı veya software rasterizer denenmedi.

## Kök neden

RK3588 hedefinde görüntü ve render aygıtları ayrık:

- KMS/HDMI sahibi: `/dev/dri/card0`
- ARM Mali GBM'nin çalışan render yolu: `/dev/dri/renderD128` ve `/dev/mali0`

Mevcut Slint LinuxKMS backend'i bağlı çıkışı açtığı aynı DRM fd'sini GBM aygıtı
oluşturmak için de kullanıyor. HDMI için seçilen `card0` fd'si bu pinli Mali
runtime'ında GBM aygıtına dönüşemiyor ve `ENOENT` veriyor. Önceki minimal GBM
probe'un `renderD128` üzerinde ARM EGL 1.5 ve 27 config ile çalışması bu ayrımı
doğruluyor. Hata EGL'den önce olduğu için bu turda EGL vendor/renderer satırı,
Home+Detail karesi ve performans ölçümleri üretilemedi.

## Rollback ve regresyon

- Başarısız native servis kaldırıldı; eski üretim servisi ve yardımcıları
  yedekten geri getirildi.
- `:8788` ve TV endpoint'i HTTP 200; MediaBox penceresi HDMI-A-1 üzerinde
  1920x1080 tam ekran ve AB24 olarak doğrulandı.
- Pinli `libmali.so.1.9.0` SHA256 değeri değişmedi:
  `fc17c1c2b4a2dea84df0811ae574e288bf10ae97dc4ef8911d0c75d335339a57`.
- Tek kısa Kodi regresyonunda yerel 4K23.976 HEVC Main10 HDR10 örneği açıldı.
  Kodi `3840x2160`, `NV15`, BT.2020 limited ve `SetHDR:exit:hdr ... eotf=2`
  kaydetti; ardından DRM'yi bıraktı ve SurfaceManager MediaBox UI'yi yeniden
  başlattı.
- `/dev/cec0`, kernel, paketler, Mesa/Mali varyantları ve üretim Web UI kodu
  değiştirilmedi.

Çalışmayan uygulama değişiklikleri commit edilmedi ve cihazda bu denemeden
geçici dosya bırakılmadı.
