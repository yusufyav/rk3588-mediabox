# M1 Entegre MediaBox Platformu — 2026-09-11

## Sonuç

`agent/codex-m1a-mediaboxd` ve `agent/claude-m1b-webui`, ayrı `--no-ff`
merge commitleriyle `main` üzerine alındı. Backend/frontend contract farkları
giderildi, Web UI aynı `mediaboxd` process'i tarafından `/ui/` altında sunuldu
ve sistem Orange Pi 5 Ultra'ya production servis olarak kuruldu.

## Entegrasyon

1. **M1A:** Python kontrol daemon'u, API, SSE, Kodi lifecycle, telemetry,
   systemd packaging ve testler main'e alındı.
2. **M1B:** React/TypeScript/Vite Web UI, remote navigation, polling/SSE
   fallback ve testler main'e alındı.
3. **Contract:** health capability map eklendi; system/network/Kodi/display
   yanıtları ortak camelCase v1 wire schema'ya uyarlandı. Seek body
   `{ "seconds": n }`, open body `{ "url": "…", "resume_seconds": n }`
   olarak gerçek M1A sözleşmesiyle eşlendi. SSE `type/payload` envelope'u
   frontend ile birleştirildi.
4. **Static serving:** `/ui/` yalnız configured `webui_root` altını sunar;
   traversal ve symlink escape reddedilir, `/api` SPA fallback'e düşmez.
5. **Production unit:** `PrivateTmp=true` korunurken yalnız paylaşılan
   `/var/tmp/kodi-home` exact yolu `BindPaths` ile açıldı. AF_NETLINK izni
   genişletilmeden `/proc/net/route` + IPv4 ioctl fallback'i eklendi.

## Doğrulamalar

- Host Python compile: PASS
- Backend: 21/21 PASS
- Frontend: TypeScript PASS, 56/56 PASS
- Frontend production build: PASS; relative JS/CSS asset yolları PASS
- Host same-origin UI/API/SSE smoke: PASS
- Target staging `0.0.0.0:18787`: UI, JS, CSS, health, system, network,
  display, Kodi ve SSE PASS
- Target Kodi staging control: safe media open, pause, resume, absolute seek,
  stop PASS
- Production: `mediaboxd.service` active/enabled, `Restart=on-failure`,
  `NRestarts=0`, `PrivateTmp=yes`, `KillMode=process`
- Production restart: Kodi PID `123512` öncesi/sonrası aynı; JSON-RPC `pong`
- Production card0: `card0-HDMI-A-1`, connected, `1920x1080p60`, SDR/8-bit
- Geçici staging listener/process ve `/tmp/rk3588-mediabox-m1-20260911`
  temizlendi.

## Production

- Web UI: `http://10.27.27.25:8787/ui/`
- API: `http://10.27.27.25:8787/api/v1/`
- Install: `/opt/rk3588-mediabox/mediaboxd` ve
  `/opt/rk3588-mediabox/webui/dist`
- Config: `/etc/mediaboxd.toml`
- Unit: `/etc/systemd/system/mediaboxd.service`
- System reboot/shutdown action capability'leri default kapalıdır.

## Korunan kanıtlar ve cleanup

Main'e patch-equivalent cherry-pick ile korunan accepted kanıtlar:

- S0-A architecture report (`fc0cfa2`)
- S0-B runtime/CEC report (`c589d3b`)
- S1 lifecycle limitation report (`0668bbb`)
- direct-KMS lifecycle probe source (`dcb9df3`)
- S1A successful handoff report/evidence (`68209e9`)

Temizlenen yedi worktree: Claude S0A/M1B, Codex S0B/S1/S1A/M1A ve eski ACP
worktree. Temizlenen yedi branch: ilgili altı `agent/*` branch'i ile
`acp/implement-mp1b-csc-single-variable-plane-color_e-01e93c19`. ACP branch'i
tamamen main tarafından kapsandığı için bırakılan branch yoktur.

Rapor öncesi entegre code/evidence HEAD: `ea2bd50`.

## Bilinen borçlar ve M2 başlangıcı

**HIGH:** Kodi HTTP/JSON-RPC halen LAN'da unauthenticated `:8080` dinliyor.
Sonraki hardening'de tek kontrol yolu `Browser -> mediaboxd -> Kodi` olmalı;
doğrudan LAN Kodi JSON-RPC erişimi kapatılmalı veya kısıtlanmalıdır.

Web UI teknik olarak çalışır ve responsive/remote-navigation testleri geçer;
ancak kullanıcı görsel tasarımı kabul etmemiştir. Bu nedenle UI görsel yeniden
tasarımı ayrı ve açık bir ürün tasarım işi olarak ele alınmalıdır.

M2'nin kesin başlangıç noktası: main üzerindeki bu rapor commit'i; önce Kodi
`:8080` hardening ve kabul edilmiş yeni UI tasarımı, sonra Stremio streaming
server entegrasyonu ve `Player.Open` handoff.
