# rk3588-mediabox — agent notes

## Kalıcı log ve araç yolları

`/tmp` geliştirme bilgisayarında ve cihazda tmpfs'tir; yeniden başlatmada
silinir. Bu projede log, kanıt ve yardımcı araç `/tmp`'ye konmaz.

- Geliştirme bilgisayarı: `/var/tmp/rk3588-mediabox/` (diskte)
  - `tools/` — derlenmiş yardımcı araçlar. Pinli wasm-bindgen:
    `tools/wbg-0.2.128/bin/wasm-bindgen`; deploy'da
    `MEDIABOX_WASM_BINDGEN` ile verilir. Yoksa:
    `cargo install wasm-bindgen-cli --version 0.2.128 --locked --root /var/tmp/rk3588-mediabox/tools/wbg-0.2.128`
  - `logs/<iş>/` — deploy logları, ekran görüntüleri, ölçüm çıktıları.
- Cihaz (Plus): `/root/mediabox-verify/<iş>/` — journal dökümleri, status
  örnekleri, ölçüm kanıtı. Ürün öneki `/opt/rk3588-mediabox` değildir.

## Git disiplini

Bu bölüm `AGENTS.md` ve `CLAUDE.md` içinde aynıdır; biri değişirse diğeri de
aynı şekilde değişir.

1. Git geçmişi ajan çalışma günlüğü değildir.
2. Deneme-yanılma çalışmaları working tree üzerinde yürütülür.
3. Başarısız prototipler için commit ve tag oluşturulmaz.
4. Görsel tasarımlar kullanıcı onayından önce commit edilmez.
5. Commitler tamamlanmış, doğrulanmış ve anlamlı değişiklikleri temsil eder.
6. Kullanıcı açıkça yetkilendirmeden commit, push, tag veya remote geçmiş
   değişikliği yapılmaz.
