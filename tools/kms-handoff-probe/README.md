# kms-handoff-probe

`kms-handoff-probe`, Kodi ile başka bir direct-KMS süreç arasında display
sahipliği devrini ölçmek için sonlu süre çalışan tanı aracıdır. GUI framework,
GBM veya EGL kullanmaz; doğrudan libdrm ile XRGB8888 dumb framebuffer oluşturur.

Modlar:

```text
kms-handoff-probe --inspect
kms-handoff-probe --acquire-only [--hold-seconds N]
kms-handoff-probe --scanout-test [--hold-seconds N]
```

Araç DRM card, bağlı HDMI connector, CRTC ve plane kimliklerini runtime'da
bulur. `--inspect` modeset ve master isteği yapmaz. Diğer iki mod yalnızca Kodi
tamamen durduktan sonra kullanılmalıdır. Scanout modu aktif HDMI CRTC'sindeki
mevcut modu aynen yeniden kullanır; aktif mod yoksa keyfi bir mod seçmez.

Build:

```sh
make -C tools/kms-handoff-probe
```

Yalnızca C11 derleyicisi, `pkg-config` ve libdrm development dosyaları gerekir.
Hedefte yeni paket kurulmaz.
