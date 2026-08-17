# Scour — görev devri

Bir oturumun sonu. Buradan devam edecek olan için, eksiksiz.

---

## 0. Hemen bilmen gerekenler

**Depo:** `/home/hasan/Projeler/Scour` · dal `main` · HEAD **`ab62d7f`**
· GitHub'da (`hasantr/Scour`, private) · çalışma ağacı temiz.

**Bekleyen tek eylem, ve Hasan'a ait:**

```bash
sudo /home/hasan/.local/bin/scour-yeniden
```

Çalışan `scourd` **16 Ağustos'tan**. Kurulu ikililer bugünden. Yani şu an
devrede olmayanlar: fanotify kuyruk tavanları, paralel dizin yürüyüşü, küçük
resim üretimi, ve atlama kuralları paneli. Panel açılıyor ama boş görünüyor —
servis `Request::Rules`'u tanımıyor, köprü 502 dönüyor, panel uydurmuyor. **Bu
bir arıza değil.**

Sudo yalnız `/mnt/depo`'ya fanotify işareti koymak için gerekiyor.
`scour-watch` işareti koyup yetkiyi bırakıyor; servis Hasan'ın kullanıcısıyla
çalışıyor. **Sen sudo çalıştırma.**

**Geri dönüş noktaları:**

| etiket | ne |
|---|---|
| `perf-2026-08-15` | performans turundan sonra |
| `arayuz-oncesi-2026-08-16` | arayüz turundan önce |
| `backup-before-deep-performance-2026-08-15` | Codex'in turundan önce |

Canlı indeksin yedeği: `~/.local/share/scour/index-backup-before-perf-20260815`
(195 MB). **Silme.**

---

## 1. Hasan'la çalışma kuralları — bunlar pazarlık konusu değil

**Ajan turu yapma.** Açıkça istemedikçe. Kendi sözleriyle: *"bu inanılmaz uzun
sürüyor, ajanlar gerçekten çok fazla uzatıyorlar işi, bu şekilde yorucu
oluyor."* Bir tur 45–80 dakika sürüyor ve o sürede konuşma duruyor. Bağlamın
yetmiyorsa **söyle ve sor**, kendiliğinden devretme. ([[hasan-ajan-kullanimi]])

**`sudo` çalıştırma.** Hiçbir koşulda. Root gereken tek şey servis yeniden
başlatma ve o Hasan'ın parmak iziyle oluyor.

**`/mnt/depo`'ya yazma.** Windows'la paylaşılan NTFS birim.

**Canlı indekse hiçbir ikili yöneltme.** `~/.local/share/scour/index`. Uyumsuz
bir ikili onu `discard` edip siler. Ölçüm için `cp -a --reflink=auto` ile
kopyala.

**Çalışan `scourd`'a dokunma** — durdurma, sinyal gönderme.

**Görünür davranış Hasan'ın kararı.** Bir şeyin nasıl göründüğünü ya da ne
kadar sürede göründüğünü değiştiriyorsan **sor**. Commit sıklığı, sıralama
varsayılanı, dil, görünüm modu — hepsi bu sınıfta.
([[scour-varsayilan-kararlari]])

**Türkçe konuş.**

---

## 2. Bu depoda pahalıya öğrenilmiş dersler

Bunlar tavsiye değil; her biri saatlere mal oldu.

### Ölçüm

**Kopya indeks canlı gibi davranmıyor.** Ad sıralaması kopyada 49 ms, canlıda
1.352 ms kuyruk yapıyordu. Her sayının hangisi olduğunu söyle.

**`RssAnon` tek başına yanıltıcı** — makine takas kullanıyor ve o 6,6 kat
oynuyor. Değişmez olan `RssAnon + VmSwap`. İki farklı ölçüm ("78,5 MB" ve
"36 MB") aynı servisin iki hâliydi.

**Aletin hangi soruyu sorduğunu doğrula.** `scour search`'ün varsayılan
`count_cap`'i **100.000**, köprününki **1.000**. CLI'la ölçüp "her arama 46 ms"
demek, arayüzün hiç yapmadığı bir isteği ölçmektir. Saatler kaybettim.

**Tek koşu ölçüm değildir.** Dönüşümlü ikili, en az iki tur, her turu ayrı
bildir. Aynı ikili aynı turda %1,67 ve %4,64 ölçüldü.

**Makine yüklüyken ölçme.** `uptime`'a bak. Kendi derlemelerin yükü sayılır.

### Pencere

**Sayfa `scour-web` ikilisinin içinde gömülü.** Derlemek yetmiyor; **köprünün
yeniden başlaması** gerekiyor. Sekmeyi yenilemek eski sayfayı getirir. Bu iki
kez saatlere mal oldu.

**Görünen bir pencere kare almıyor olabilir.** `document.hidden === false`,
`visibilityState === "visible"`, ve `requestAnimationFrame` iki saniye sessiz.
`Page.startScreencast` **yetmiyor** — kareyi zorlayan `Page.captureScreenshot`.
Kareden yazılan her şey (yerleşim, `sizer`, satır konumları) onsuz okunamaz;
söz zincirinden senkron yazılanlar (ray sayıları, sayaç, basılı durum) güvenli.
([[scour-arayuz-dogrulama]])

**Rust testleri sayfanın çalıştığını söylemiyor.** `a7789d8`'de var olmayan bir
düğmeye dinleyici ekledim; sayfa tek betik olduğu için o satırdan aşağısı hiç
çalışmadı — 32 çevrili metnin 31'i boş kaldı, rapor sekmesi tire doldu. Bütün
Rust testleri geçiyordu. **Aç ve bak.**

**Listenin sıralamasını kontrol et.** Canlı yenilemeyi `konum` sıralı bir
listede sınayıp "bozuk" sonucuna vardım. Bozuk değildi.

### Kod

**`tests/whole.rs`'teki kaba-kuvvet karşılaştırmaları kapıdır.** Bu hafta
**dört** yanlış optimizasyonu reddettiler ve dördünde de haklıydılar.

**Sayfanın motor sözlüğünden kopması iki ayrı arıza çıkardı** (sayfa 8 tür
biliyordu, motor 13). Artık `the_page_takes_the_engines_kind_vocabulary` testi
bağlıyor. Yeni bir sözlük kopyası açma; testle bağla.

**Her görünür metin katalogdan geçer.** `T("…")`, `data-t*`,
`lang/tr/LC_MESSAGES/scour.po`. İki test zorluyor.

**`config.toml`'u yeniden yazma.** Elle yazılmış, ölçümlerle dolu. Arayüzün
yazdığı her şey indeksin yanındaki `state/`'e gider — sütun genişliği de,
atlama kuralı da.

---

## 3. Bugün ne yapıldı

### Sıralama — bitti

Aynı indeks, 200 satırlık sayfa, canlı servis:

    Değişme  1,2 ms   ·  Konum  2,8  ·  Ad  3,0
    Uzantı   3,1      ·  Tür    4,1  ·  Boyut 8,2

Dünkü hâli: Ad **1.421 ms**, Konum **1.184**, Uzantı **496**, Değişme ↑ **97,8**.

Nasıl: saklı sıra dosyaları (`seg-*.porder`, `.norder`, `.eorder`, satır başına
4 bayt), bölge haritasıyla top-k (sayısal sütunlar), `names::Reader` (ad
arenasını her satırda baştan taramayı bıraktı), ve sınırlı seçim (`narrow`'un
kendisi, sayfanın iki katını geçmeyen tampon üzerinde).

**Sebep tespiti önemliydi:** gerileme değildi. `845c979` sıralamayı *hatırlanır*
yaptı; Hasan bir kez `konum`a tıkladı ve o günden beri her açılış 2,24 milyon
satırlık tam yürüyüş oldu. Kod yavaşlamadı, yavaş olan kalıcı oldu.

### Bellek — bitti

Tepe **551 → 178 MB**. Sıralama artık satır başına anahtar üretmiyor. İzleyici
kuyruklarına tavan kondu (bir `rm -rf` ~9 milyon ad sahiplenebiliyordu ve
`clear` kapasiteyi tuttuğu için tepe kalıcı taban oluyordu).

**Heap'in yeri belli:** izleyici, 254 MB'ın ~230'u. Soğuk taban 10 MB.

### Boşta maliyet — bitti

`reviseRows` **kaldırıldı**; görünen pencereyi artık yalnız `fillWindow`
çekiyor. İki yol aynı işi yapıp biri ötekini iptal ediyordu.

    arama/sn   2,20-4,25 → 1,25-1,40   ·   iptal edilen  6-34 → 0
    99 pencereyi doldurmak: 282 istek → 144-154

Boşta servis: **0,002 çekirdek** (pencere kapalı).

### Açılış — bitti

`DirMap::build` paralelleştirildi: iki kaynak sıcak **2,4 → 0,7 sn**, soğuk
NTFS **6,71 → 0,38**. Çift yürüyüş *var* ama zorunlu (fanotify olayı ebeveynini
dosya tanıtıcısıyla adlandırıyor); birleştirme üç gerekçeyle reddedildi.

### Özellikler — bitti

- **CSV**, sınırsız, servis tarafında akışlı: **2.248.592 satır / 3,60 sn**.
  `scour export` komutu da var.
- **Dil menüsü** — 189 Türkçe metin katalogda, iki test zorluyor.
- **Üç görünüm modu** — detay, simge, büyük simge. Sanal listenin aritmetiği
  satır yerine *çizgi* cinsinden yeniden yazıldı.
- **Küçük resimler** — masaüstünün kendi üreticilerine ürettiriliyor
  (`/usr/share/thumbnailers/*.thumbnailer`), standart önbelleğe `Thumb::URI` ve
  `Thumb::MTime` ile. GNOME'un kendi `lookup()`'ı 90/90 kabul etti.
- **`revisetouch.com`** yardım panelinin altında.
- **Atlama kuralları paneli** — üç grup, tek düzenlenebilir.

### Düzeltilen arızalar

Tarih sıralamasının yarışı · kenar çubuğunun filtre seçince sıfırlanması (iki
turda: takma ad eşleştirmesi, sonra sayfanın sekiz türlük eski sözlüğü) · boş
sonuçta canlı yenilemenin 12,6 saniye geride kalması · `empty.hidden`'ın dört
yazarı · sayfayı üçte birinde durduran eksik düğme (**benim hatam**).

---

## 4. Açık işler

### Hemen

1. **Servisi yeniden başlat** (Hasan). Yukarıda.
2. **Ayar kuralının taramayı gerçekten etkilediği kanıtlanmadı.**
   `wire.rs::scan_options` üç satırla ayarları `ScanOptions`'a katıyor,
   derleniyor, doğru okunuyor — ama test servisim hiç indekslemedi ve sebebini
   bulamadım. `/tmp` gömülü atlama listesinde (iki denemem oraya gitti);
   üçüncüsü `/var/tmp`'de yine sıfır verdi. **Devam eden önce o taramayı
   çalıştırsın.**

### Kısa

3. **Diskte ~4 GB ajan artığı** — `/var/tmp/scour-*-wt`, `/var/tmp/scour-*-idx`,
   `~/.local/share/scour/index-bench-tail-20260815`. `scour-btrfs-wt` ve
   `scour-fanotify-wt` **Hasan'ın**, dokunma. Yedeğe de dokunma.
4. **Taşınabilirlik** — `scour-places`'in `/proc/self/mounts` okuması `cfg`siz
   (macOS'ta boş döner, çökmez ama birim bilgisi olmaz), ve `scan.rs`'in
   `SYS_ioprio_set` çağrısı `cfg`siz (macOS'ta **derlenmez**). Hasan: *"şimdilik
   geniş alalım, kapı açık olsun."*
5. **Izgarada `↑`/`↓`** satırda yana yürüyor, sütunda aşağı değil.

### Orta

6. **CSV'de sıralama yok** — akış indeksin kendi sırasında. Sebebi eşitlik
   grupları: `modified` artan, satır listesinin tersi değil; tersi ama eşit
   tarihli her öbek kendi içinde ters. Kendi turunu ister. Hesap
   `NativeIndex::scan`'de.
7. **Kenar çubuğunun 210 ms'lik ikinci sorusu** önbelleğe alınmadı, ve gerekçesi
   ölçülü: `sidebarCost` duvar saati tutuyor, canlı yenileme 40 ms'den pahalı
   olanı reddediyor — önbellek isabeti onu eşiğin altına düşürüp o eşiğin
   engellediği onda bir çekirdeği geri açardı. Doğru çözüm maliyeti duvar
   saatinden ayrı fiyatlandırmak.
8. **Commit sıklığı** (`commit_watched`, kodda sabit 1 sn) — açık pencere
   dakikada 12 parça ürettiriyor, diske yazma 12,6 → 50,8 MB/dk. **Hasan'ın
   kararı**, çünkü kaydettiği dosyanın görünme süresi.
9. **Ad sıralaması 3 ms ama gerileme riski**: `.norder` yalnız sıkışma/yeniden
   inşa ile yazılıyor. Yeni parçalarda yok, ta ki sıkışana kadar.
10. **B kademesi kural yönetimi** — kural değişince motorun ilgili ağacı kendi
    kendine tarayıp süpürmesi. Şu an "sonraki taramada geçerli" deniyor.

### Uzun

11. **TUI** — hiç yok. Hasan'ın aylardır süren asıl isteği.
12. **Slint'i tamamlamak** — 20–120 satır getiriyor, devam sayfalaması yok, üç
    sıralama başlığı. Ana ürün bu olmalı; Chromium penceresi maket.
13. **Sürükle-bırak (dışarı)** — `dragstart` kodda sıfır kez geçiyor.
14. **Kayıtlı aramalar** · **yeniden adlandır/sil/taşı** (Hasan'ın kararı).
15. **fsearch/plocate karşılaştırma tablosu** — HN'e gitmeden önce şart. İlk
    soru "plocate'e göre nasıl" olacak ve bugün cevabı yok.
16. **inotify tarafı** hâlâ dizin başına yol tutuyor (~230 MB). Hasan fanotify
    kullandığı için etkilenmiyor ama taşınabilir yol o.

---

## 5. Ölçüm araçları — yeniden icat etme

| araç | ne yapar |
|---|---|
| `crates/scour-index-native/examples/searchcost.rs` | altı sıralama × üç offset |
| `examples/scale.rs` | eğriyi yürür: boyut, bellek, inşa, gecikme |
| `examples/bench.rs` | mock korpus, sorgu maliyetleri |
| `examples/foldercost.rs` | `weigh_folders`'ın sayfa başına bedeli |
| `examples/compact_cost.rs <dir> rebuild` | kopyada yeniden inşa |
| `scripts/probe` | gerçek pencereye CDP; docstring'ini oku |
| `scripts/scour-app` | pencere; `SCOUR_APP_PORT`, `SCOUR_APP_DEBUG` |
| `docs/MEASUREMENTS.md` | her ölçümün kaydı, canlı/kopya ayrımıyla |

---

## 6. Ölçülmüş sayılar — tekrar ölçme

    kayıt                    2.240.000        indeks 211,9 MiB
    köklerdeki gerçek dosya  4.786.967        (/home 3.221.182 + /mnt/depo 1.565.785)
    fark                    ~2.500.000        atlama kurallarından

      target/            2.087.642     .rustup/       247.430
      .cargo/registry/     192.972     .cache/         83.945
      .git/                 44.709     node_modules/   39.238

    yapılandırılmamış: /usr 325.627 · /var 18.645 · /opt 5.478

    boşta servis (pencere kapalı)   0,002 çekirdek
    RssAnon 21 MB + VmSwap ~55     RssFile 128 MB
    7 M satırda anonim bellek      30 MB (sentetik, scale.rs)

---

## 7. Bellek dosyaları

`~/.claude/projects/-home-hasan-Projeler-RustEverything/memory/` — `MEMORY.md`
dizin. Bu devir için en önemlileri:

`hasan-ajan-kullanimi` · `scour-siralama-maliyeti` · `scour-kalan-plan` ·
`scour-arayuz-plani` · `scour-atlama-kurallari-plani` ·
`scour-arayuz-dogrulama` · `scour-olcum-yontemi` · `scour-kurulum` ·
`scour-varsayilan-kararlari`

---

## 8. Son söz

Hasan'ın izlenimi ölçümden önce gelir. Bu oturumda üç kez o haklı çıktı ve ben
yanıldım: "laglı" dediğinde, gerilemenin hangi turda olduğunu söylediğinde, ve
RAM'in şiştiğini söylediğinde. Kendisi kullanıyor.

Ve kanıtlayamadığın şeyi commit etme. Bu hafta dört yanlış optimizasyon
reddedildi, üçünü ajanlar buldu, biri benim yorumumdu. Yavaş ama doğru bir
motor, hızlı ama yanlış olandan iyidir — ve bu karar her seferinde doğru çıktı.
