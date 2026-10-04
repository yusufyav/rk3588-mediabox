# AR-GE: NPU ile AI upscale (1080p → 4K) — rafa kaldırıldı

**Durum (2026-10-04): rafta.** Ürün değişmedi; MediaBox 1080p içeriği bugün
olduğu gibi VOP2'nin donanım ölçekleyicisiyle 4K'ya çıkarıyor. Bu not yalnızca
AR-GE kaydıdır: ne denendi, ne ölçüldü, neden bırakıldı. Konu kendiliğinden
yeniden açılmaz. Açılırsa aşağıdaki tek kural geçerlidir: **karar yalnız gerçek
bir 1080p yayın kopyası ile aynı filmin 2160p kopyası karşılaştırılarak verilir.**

## Soru

RK3588 NPU'su (3 çekirdek, 6 TOPS INT8), 1080p film karelerini gerçek zamanlı
(23.976 fps, uçtan uca ≤ 41.7 ms) 4K'ya çıkarıp VOP2'den daha iyi bir görüntü
verebilir mi?

## Cevap

**Hız: evet. Gerçek filmde kalite: hayır.**

- NPU'nun kurallarına göre tasarlanıp eğitilen bir ağ (MBSR) 1080p NV12'yi
  3840×2160 NV12'ye uçtan uca 29 ms medyan, 39 ms p95 ile çeviriyor. Ölçüm
  300 s ve 9 801 kare sürdü; 3 çekirdek kullanıldı, CPU'ya düşen işlem yok,
  termal sorun yok.
- 4K karenin küçültülüp tekrar büyütüldüğü yapay testlerde VOP2'yi her karede
  geçti; eğitimde görmediği filmlerde de geçti.
- **Gerçek bir yayın çiftinde geçemedi.** Test: *In the Mood for Love*, UHD
  kaynaklı 1080p ve 2160p sürümleri, kareler birebir eşlenmiş, aynı writeback
  yolu. Doğal sahnelerin 7/9'unda VOP2'nin 0.25–0.79 dB gerisinde, 2'sinde
  eşit. Yalnız jenerik yazısında önde (+4.9 dB).
- **Neden:** Gerçek 1080p kopyada film greni ve ince detay sıkıştırmayla
  kaybolmuş. VOP2 o kaybı yumuşatıyor; model ise kalan sıkıştırma izlerini
  keskinleştiriyor. Kaybolan detayı hiçbir büyütücü geri getiremez. Bu NPU'ya
  sığan model boyutunda doğal sahnelerdeki gerçekçi tavan, VOP2 ile eşitlik ya
  da çok küçük bir kazanç.
- Piyasadaki "AI upscale" ürünleri (NVIDIA Shield, Amlogic) doğruluğu değil
  algısal keskinliği hedefler. Bu çalışmanın ölçütü doğruluktu.

## Denenenler

| yol | sonuç | neden |
|---|---|---|
| RGA3 donanım ölçekleyici | ret | çıktı bir kaynak pikseli kayık, düzeltilemiyor (`tools/rga-upscale/`) |
| FSRCNN, FSRCNN-small, ESPCN (FP16/INT8) | ret | ya yavaş (45–640 ms) ya kalitesi VOP2'nin altında; INT8 desen artefaktı |
| RT4KSR x2 | ret | kalite iyi; NPU'da 333 ms. LayerNorm 5×56 ms, GELU 4×10 ms, ikisi de tek çekirdekte |
| SPAN x2 | ret | tam 1080p'de 850 GMAC/kare; derleyici tahmini ≥ 0.5 s |
| QuickSRNet Small 2x W8A8 | ret | tam çözünürlükte her conv 18.7 M döngü; NPU'da tek başına 38–54 ms |
| MBSR (bu çalışmada tasarlanıp eğitildi) | hızlı; yapay testte iyi; **gerçek sürümde VOP2'yi geçmedi** | bkz. "Cevap" |

## NPU hakkında öğrenilenler (başka işler için de geçerli)

RKNN Toolkit/runtime 2.3.2, sürücü 0.9.8, NPU 1000 MHz:

- **Tam 1080p'de conv pahalı.** 3×3 conv, kanal sayısından bağımsız olarak
  piksel ve filtre noktası başına 1 döngü harcıyor (≈ 18.7 M döngü). Yarım
  çözünürlükte (540×960) 32 kanala kadar her conv 3 çekirdekte ≈ 2.3 ms; 48
  kanalda maliyet 3.3 kat artıyor.
- **1–3 kanallı tam çözünürlüklü tensörler** NC1HWC2 düzeninde 16–32 kat
  dolguyla şişiyor; örneğin 530 MB'lık bir çıkış tensörü.
- **Kaçınılacak op'lar:** LayerNorm (`exNorm`) ve GELU tek çekirdekte ve
  yavaş çalışıyor. INT8'de Sqrt/Div ve explicit Pad CPU'ya düşüyor. Conv,
  ReLU/LeakyReLU/Clip, Concat ve Add hızlı ve 3 çekirdeğe yayılıyor.
- **INT8 tuzakları:** Kanal başına ölçekte, büyük bir merkez ağırlığıyla aynı
  kanaldaki küçük ağırlıklar kabalaşıyor (öğrenilmiş anchor her pikselde ~1 kod
  hata verdi). Sınırlı aktivasyonlar (Clip 0..1) ve özdeşlik dallarıyla patlayan
  aktivasyonlar ağın katmanlarını öldürdü. Teşhiste RKNN `accuracy_analysis` ve
  `RKNN_QUERY_PERF_DETAIL` işe yaradı.
- **Kart ve ekran:** VP0'a yalnız Cluster0 ve Esmart0 bağlanabiliyor ve
  Cluster0 NV12 kabul etmiyor; iki katmanlı bölünmüş ekran yapılamıyor. Kartta
  Vulkan ve OpenCL yok; HDR→SDR tonemap için libplacebo ya da tonemap_opencl
  çalışmıyor. Media-runtime ffmpeg'inin nv15→p010le dönüşümü bozuk;
  yuv420p10le doğru.
- **Test yöntemi:** Yapay küçültme testleri yanıltıcı. Gerçek sürüm çifti
  şart: kareler zamanda ±3 kare aranarak eşlenmeli, VOP2 ile aday aynı
  writeback yolundan okunmalı.

## Malzeme nerede

Kod, ağırlıklar ve ayrıntılı raporlar repodan kaldırıldı; git geçmişinde duruyor:

```sh
git show d013f74 --stat                                   # son hali
git checkout d013f74 -- tools/ai-upscale                  # geri getirmek için
```

| commit | içerik |
|---|---|
| `6f8fde4` | FSRCNN / ESPCN, ölçüm araçları (`aisr-bench.c`, kalite/zamansal metrikler) |
| `61cc629` | RT4KSR |
| `c80114a` | SPAN |
| `5ee4cd2` | QuickSRNet |
| `d1c657e` | NPU şekil taraması (hangi ağ boyutu bütçeye sığar) |
| `1f2dc01` | MBSR eğitimi ve ağırlıkları (`weights/mbsr-x2-c32d9-qat.pt`) |
| `55a3d41` | Görülmemiş filmlerde doğrulama |
| `284f1ef` | TV'de canlı A/B demosu (`aisr-demo.c`) |
| `d013f74` | Gerçek sürüm çifti testi (`hdr_pairs.py`) — belirleyici sonuç |

RGA araştırmasının araçları ayrı bir karar olarak `tools/rga-upscale/`
altında kalıyor.

## Açık kalanlar (yalnız konu yeniden açılırsa)

- Gerçek bozulmalarla (yayın bit hızında x264/x265, gren kaybı) yeniden
  eğitim. Beklenti gerçek sürüm testinde eşitlik ile +0.3 dB arası; garanti
  yok.
- HDR10 / Dolby Vision: MBSR yalnız SDR. HDR için PQ/BT.2020 alanında 10-bit
  bir model ve HDR çıkış yolu gerekir.
- NPU'nun bu ürüne daha olası katkıları başka alanlarda: çevrimdışı sesli
  arama (Whisper/Zipformer), diyalog netleştirme, intro/jenerik tespiti.
  Hiçbiri ölçülmedi.
