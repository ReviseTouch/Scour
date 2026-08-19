# TUI — planı

Üçüncü yüz: uçbirimde çalışan Scour. Diğer ikisi gibi **aynı servise** bağlanır,
aynı ayarları okur, aynı sözlükten konuşur. Bu belge ne yapılacağını ve hangi
kararın neye dayandığını tutar; bağlama ayrıntıları (her özelliğin hangi ortak
crate'e taşınacağı) ayrı bir turda işlenecek.

Ratatui **0.30.2** (19 Haziran 2026) üzerinden planlandı.

---

## 0. Neden uçbirim

Web ve pencere ikisi de bir masaüstü ister. Uçbirim istemez:

- **ssh üzerinden** — uzaktaki makinenin indeksini oradan aramak, port açmadan.
- **Port yok.** Web sürümü 127.0.0.1'de dinliyor; herkes bunu istemiyor.
- **Klavye.** Arama kutusundan sonuca kadar elin klavyeden çıkmaması.
- **Açılış.** Pencere ~55 ms'de kuruluyor; uçbirimin hedefi bunun altı.

---

## 1. ratatui 0.30.2 — elimizde ne var

Bu bölüm envanter: planın geri kalanı yalnız buradakilere dayanır.

### Çizim modeli
- **Anlık kip (immediate mode).** Her karede bütün ekran bir `Buffer`'a çiziliyor,
  ratatui yalnız değişen hücreleri uçbirime yazıyor. Kalıcı bileşen ağacı yok —
  yani Slint'te başımıza gelen "model değişti, satır yıkıldı, tıklama düştü"
  sınıfı hatalar **burada yapısal olarak yok**. (bkz. [Tuzaklar](#7-tuzaklar))
- `Buffer`/`Cell`: hücre başına karakter, ön/arka renk, değiştirici.

### Yerleşim
- `Layout` + `Constraint`: `Length` · `Min` · `Max` · `Percentage` · `Ratio` ·
  `Fill` (oransal dağıtım). İç içe bölmeler, `Flex` ile hizalama.
- `layout-cache` özelliği açık geliyor: aynı kısıt kümesi yeniden hesaplanmıyor.

### Hazır bileşenler
`Block` (kenarlık, başlık) · `Paragraph` (sarma, kaydırma) · `List` +
`ListState` · `Table` + `TableState` (sütun kısıtları, seçili satır) ·
`Tabs` · `Gauge` · `LineGauge` · `Sparkline` · `BarChart` · `Chart`
(eksenli, veri kümeli) · `Scrollbar` + `ScrollbarState` · `Canvas`
(nokta/çizgi/dikdörtgen ızgarası) · `Clear` (üstüne çizmek için) · `Calendar`.

Bizim işimize yarayanların eşlemesi [§4](#4-ekran-düzeni)'te.

### Metin ve renk
- `Text` → `Line` → `Span`: her parçanın kendi rengi. **Sorgu satırının renkli
  açıklaması bunun üstüne birebir oturuyor** — servisin `explain` cevabı zaten
  parça parça geliyor.
- `Stylize` kısayolları (`"x".bold().fg(...)`), 24-bit renk, `underline-color`
  (varsayılan açık), `palette` entegrasyonu.
- **Renk paletimiz hazır:** `scour-ui::{DARK, LIGHT, Rgba}` — pencerenin ve
  sayfanın kullandığı aynı palet. Uçbirim `Rgba` → `Color::Rgb` çevirir.

### Arka uçlar ve uçbirim yönetimi
- Arka uç: **crossterm** (varsayılan, çapraz platform), termion, termwiz, termina.
- `ratatui::run()` — kurulum, döngü, geri alma tek çağrıda; ya da
  `init()`/`restore()` elle; `init_with_options()` ile **satır içi kip**
  (uçbirimin bir bölgesinde çalışma) ve seçilebilir viewport.
- `DefaultTerminal` takma adı.

### Olaylar
- Ratatui'nin kendi olay okuması **yok**; crossterm'ün `event` modülü:
  klavye (değiştiricilerle, `KeyEventKind` — Windows'ta tekrar), **fare**
  (tıklama, tekerlek, sürükleme), yeniden boyutlanma, odak, yapıştırma
  (bracketed paste).
- **Async yok, olması da gerekmiyor.** Bizim modelimiz zaten "servisten cevap
  gelir, ekran yeniden çizilir". İki kanal: `crossterm::event::poll` ve IPC
  cevapları. Bir iş parçacığı olayları, biri soketi okur, ikisi de aynı
  `mpsc` kanalına yazar; döngü kanaldan okur ve çizer.

### Çizmediği şeyler (dürüstlük payı)
- **Ağaç bileşeni ratatui'de yok** (`tui-tree-widget` ayrı crate). Yinelenenler
  ve klasör boyutu için ya o crate ya kendi katlanır listemiz.
- **Metin girişi bileşeni yok.** Sorgu satırını kendimiz yazacağız — imleç,
  seçim, tarihçe. Küçük bir iş ve zaten kendi kurallarımız var.
- **Resim yok.** Küçük resimler ancak `ratatui-image` + kitty/sixel ile; ayrı
  ve isteğe bağlı bir aşama ([§6, Aşama 6](#6-aşamalar)).

---

## 2. Eşgüdüm — üç yüz, tek çekirdek

Kural: **bir özellik bir kez yazılır, üç yüz onu gösterir.** Bir yüzde
çalışıp diğerinde olmayan şey, ya henüz taşınmamıştır ya da o yüzün doğası
gereği yoktur — üçüncü bir ihtimal kabul edilmiyor.

### Zaten ortak olanlar
`scour-proto` (soru/cevap) · `scour-ipc` (taşıma) · `scour-engine` +
`scour-index-native` (arama) · `scour-settings` (ayarlar ve kurallar) ·
`scour-i18n` (sözlük) · `scour-places` (kapsamlar) · `scour-ui`
(palet, sütun tanımları, zaman bantları, tür renkleri).

### TUI'den önce ortaklaşması gerekenler
Bugün her yüz kendi kopyasını taşıyor; üçüncü kopyayı yazmadan önce taşınacak:

| ne | bugün nerede | nereye |
|---|---|---|
| sayı/boyut/tarih biçimi (`grouped`, "3,4 GB", "2 gün önce") | pencerede ve sayfada ayrı ayrı | `scour-ui` |
| yol parçalama (kırıntı, yaprak, klasör) | `crumb_of`, `leaf_of` | `scour-ui` |
| sorgu birleştirme (facet + kapsam + `dm:` → sorgu) | her yüzde | `scour-ui` |
| sayfa önbelleği kuralı (200'lük hizalı sayfa, LRU, bir önden çekme) | pencerede `rows.rs`, sayfada ayrı | ortak bir `scour-page` |
| rapor hesapları (en büyükler, yinelenenler, kullanım) | çoğu serviste, sunum her yüzde | sunum da `scour-ui`'ye |
| CSV üretimi | serviste | olduğu yerde kalır |

### Özellik çizelgesi
● var · ◐ kısmi · ○ yok · — o yüzde anlamsız

| özellik | web | pencere | TUI hedefi |
|---|:--:|:--:|:--:|
| sorgu satırı + renkli açıklama | ● | ● | ● |
| sorgu dili (Everything uyumu) | ● | ● | ● |
| sonuç listesi, beş sütun, sıralama | ● | ● | ● |
| sütun genişliği ayarı | ● | ● | ◐ (kısıtlarla) |
| sanal sayfalama + derin erişim | ● | ● | ● |
| tür rayı (çubuklu) | ● | ● | ● |
| kapsam rayı | ● | ● | ● |
| boyut rayı | ● | ● | ● |
| zaman şeridi (24 bar, tıkla-süz) | ● | ● | ● (`BarChart`) |
| görünümler: liste/ızgara/büyük | ● | ● | ◐ (liste + sıkışık ızgara) |
| çoklu seçim + seçim şeridi | ● | ● | ● |
| dosya/klasör açma | ● | ● | ● |
| rapor sekmesi | ● | ● | ● |
| kural paneli | ● | ● | ● |
| dil değiştirme | ● | ● | ● |
| CSV | ● | ● | ● |
| yardım | ● | ● | ● |
| canlı yenileme + sayaç şeridi | ● | ● | ● |
| açık/koyu tema | ● | ● | ◐ (uçbirim paletine uyum) |
| arayüz geçişi (⇄) | ● | ● | ● |
| küçük resimler | ● | ○ | ○ (Aşama 6) |
| önizleme | ● | ○ | — |
| sürükle-bırak | ○ | ○ | — |
| sütun seçici | ○ | ○ | ○ |
| klavye gezinme (PgUp/PgDn/Home/End) | ◐ | ○ | ● |

**Kural:** bu çizelgeye yeni satır eklemeden yeni özellik yazılmaz. Bir yüz
geri kalacaksa çizelgede ○ olarak durur — sessizce eksik kalmaz.

---

## 3. Mimari

```
apps/scour-tui/
  src/
    main.rs      — bayraklar, uçbirim kurulumu, döngü
    app.rs       — bütün durum tek yapı (App), saf geçişler
    link.rs      — servise bağlantı, cevaplar kanala
    keys.rs      — tuş → eylem eşlemesi (tek tablo, yardım bunu basar)
    draw/        — her bölge kendi dosyası: query, list, rail, strip,
                   report, rules, faces, help, meter
    theme.rs     — scour-ui paleti → ratatui Style
```

### Döngü
```
                 ┌───────────── crossterm olayları (iş parçacığı 1)
   mpsc kanal ◄──┤
                 └───────────── IPC cevapları (iş parçacığı 2)
        │
        ▼
   App::step(olay) → durum değişir → yalnız gerekiyorsa çiz
```

- **Boşta çizim yok.** Kanaldan bir şey gelmedikçe kare çizilmez; imleç yanıp
  sönmesi bile 500 ms'lik bir zamanlayıcıya bağlı. Pencerede boşta CPU'yu
  yakan tam da bu hatanın tersiydi.
- `poll` süresi: bir sonraki zamanlayıcıya kadar. Beklerken iş parçacığı uyur.

### Durum
Tek `App` yapısı; her alanın tek sahibi var. Sayfa önbelleği ortak
`scour-page`'ten gelir — pencerede öğrenilen kurallarla aynı: hizalı 200'lük
sayfalar, LRU, bir sayfa önden çekme, uçuşta tek istek, cevabın taşıdığı ofsete
yazma.

### Bağlantı
`scour-ipc` doğrudan. Web'in aksine köprü yok — ölçülmüştü: soketin taban
maliyeti yarım milisaniyenin altında.

---

## 4. Ekran düzeni

Web ve pencerenin düzeni bire bir taşınıyor; uçbirimde de aynı şeyi aynı yerde
bulmak esas.

```
┌──────────────────────────────────────────────────────────────┐
│ SCOUR  dosya adı · ext:pdf · kind:image dm:7d                │ 1 satır  sorgu
│ 200 / 2.696.724 · 1,39 ms · 2.070 satır · 291,7 MB · 3 kaynak│ 1 satır  sayaç
├────────────┬─────────────────────────────────────────────────┤
│ TÜR        │ Ad              Tür    Konum    Değişme   Boyut │ 1 satır  başlık
│  Klasör ▇▇ │ ────────────────────────────────────────────── │
│  Kod    ▇  │ log.txt         Belge  /home/…  2 sa      12 KB │ Fill     liste
│  Belge  ▇  │ …                                               │
│ KAPSAM     │                                                 │
│ BOYUT      │                                                 │
├────────────┴─────────────────────────────────────────────────┤
│ ▁▂▃▅▂▁▃▇▅▃▂▁▂▃▅▇▅▃▂▁▂▃▅▇   2 yıl önce ──────────── bugün    │ 2 satır  şerit
│ ARA  RAPOR                            1 seçili · 0,6 MB      │ 1 satır  alt
└──────────────────────────────────────────────────────────────┘
```

Eşleme:

| bölge | ratatui |
|---|---|
| sorgu satırı | `Paragraph` + elle imleç; renkli parçalar `Span` |
| sayaç şeridi | `Line` içinde renkli `Span`'ler |
| ray | `List`; çubuklar `LineGauge` ya da blok karakterle `Span` |
| sonuç listesi | `Table` + `TableState`, sütunlar `Constraint` |
| kaydırma çubuğu | `Scrollbar` + `ScrollbarState` |
| zaman şeridi | `BarChart` (24 çubuk); tıklama fare olayından |
| sekmeler | `Tabs` |
| rapor | `Paragraph` + `Table` + `Gauge` panoları |
| paneller (kural, dil, yardım, arayüz) | `Clear` + `Block` ile üstüne çizim |
| yinelenenler / klasör ağacı | katlanır liste (kendi), gerekirse `tui-tree-widget` |

**Dar uçbirim:** 80 sütunun altında ray kapanır (tuşla açılır), sütunlar
`Min` kısıtlarıyla erir: önce Konum, sonra Tür.

---

## 5. Klavye ve fare

Tek tablo (`keys.rs`), yardım paneli o tablodan basılır — belge ile davranışın
ayrışması böyle engelleniyor.

| tuş | ne |
|---|---|
| yazmak | sorguya gider (kip yok; arama kutusu her zaman canlı) |
| `↑ ↓` `PgUp PgDn` `Home End` | listede gezinme |
| `Enter` | aç · `Shift+Enter` klasörünü aç |
| `Tab` | ray ↔ liste ↔ sorgu |
| `Space` | seç/bırak · `Shift+↑↓` aralık |
| `F2`/`Ctrl+R` | rapor sekmesi |
| `Ctrl+E` | CSV |
| `Ctrl+K` | kurallar · `Ctrl+L` dil · `F1` yardım · `Ctrl+U` arayüz geçişi |
| `Ctrl+↑↓` | sıralama sütunu · `Ctrl+←→` yön |
| `Esc` | paneli kapat, sonra sorguyu temizle |
| `Ctrl+C` / `Ctrl+Q` | çık |

Fare: tekerlek kaydırır, tıklama satır seçer, başlığa tıklama sıralar,
şeride tıklama `dm:` süzgeci koyar — yani sayfadakiyle aynı.

---

## 6. Aşamalar

Her aşama **kendi başına kullanılabilir** bir şey bırakır; yarım özellik
bırakmaz.

**Aşama 0 — ortaklaşma.** [§2](#2-eşgüdüm--üç-yüz-tek-çekirdek)'deki taşıma:
biçimleyiciler, yol parçalama, sayfa önbelleği. Üç yüzün testleri yeşil kalır.
*Kabul:* pencere ve sayfa davranışı değişmez, kopyalar silinir.

**Aşama 1 — iskelet ve arama. ✔ 2026-08-19.** Uçbirim kurulumu, döngü, sorgu
satırı, sonuç listesi, sayaç şeridi, sayfalama, imleç ve kaydırma, fare
tekerleği, iki kip, `Enter` ile açma. *Ölçüldü:* ilk kare **5–9 ms** (hedef
< 50), boşta CPU **sıfır tik**, RSS **8,7 MB**. Karenin metin dökümü
`--once 120x30` ile alınıyor — penceredeki `SCOUR_GUI_SNAP`'in karşılığı.

**Aşama 2 — gezinme ve açma. ✔ 2026-08-19.** Tek tuş tablosu (`keys::MAP`) ve
yardım paneli **o tablodan** basılıyor — belge ile davranış ayrışamaz. Seçim
(`Space`, `Shift+↑↓`, `Ctrl+A`) yola göre tutuluyor, alt şeritte "2 picked ·
13,3 KiB". Sıralama `Ctrl+←→` sütun, `Ctrl+↑↓` yön; alt şerit hangisi olduğunu
söylüyor. `Enter` açar, `Shift+Enter` klasörünü. *Doğrulama:* `--press` ile
sentetik tuşlar — seçim, yardım ve boyuta göre sıralama ekran dökümüyle
görüldü.

**Aşama 3 — ray ve şerit. ✔ 2026-08-19.** Tür rayı (kendi renginde çubuklar ve
sayılar), kapsam rayı (`places`), üç boyut aralığı, 24 çubuklu zaman şeridi
(bir bandın tek dosyası bile en az bir tik — sıfırla karışmasın diye). `Tab`
okları raya alır, `Enter` basar, aynı satıra tekrar basmak temizler. Süzgeç
birleştirme artık `scour-ui::query`'de — üçüncü kopya yazılmadı. Ray, yeni
sayılar gelene kadar eskisini gösteriyor: boşalan bir ray imlecin altındaki
satırı kaydırır. *Kabul karşılandı:* `kind:folder rapor` uçbirimde 347,
komut satırında 347.

**Aşama 4 — paneller. ✔ 2026-08-19.** `Ctrl+K` kurallar (üç grup, tikler,
listeden uzun olsa da imleçle kaydırılıyor), `Ctrl+L` dil, `Ctrl+U` arayüz
geçişi (`scour-open` üzerinden), `Ctrl+E` CSV — indirilenler klasörüne yazıyor
ve nereye yazdığını söylüyor. `F1` yardım zaten vardı. *Kabul karşılandı:*
uçbirimden bir kural kapatıldı, `settings.json` 8'den 9'a çıktı, geri açıldı,
8'e döndü. Dışa aktarmanın hatası artık aramanın kuşağına bağlı değil —
öyleyken sessizce yutuluyordu (`Got::Failed`).

**Aşama 5 — rapor.** Panolar, en büyükler, yinelenenler, klasör boyutu.
*Kabul:* rakamlar sayfadakiyle birebir.

**Aşama 6 — isteğe bağlı.** Küçük resimler (kitty/sixel), satır içi kip,
sütun seçici.

---

## 7. Tuzaklar

Ödenmiş bedeller; tekrar ödenmeyecek.

- **Sayfa, cevabın taşıdığı ofsete yazılır.** Pencerede sayfa istekle
  eşleşmediği için liste bozuluyordu.
- **Sayım tavanı uzunluk değildir.** Etkileşimli arama 1000'e kadar sayar;
  kesin sayım gelince liste uzar. Uçbirimde de aynı: `total` ile `counted`
  ayrı tutulacak.
- **Kısa gelen son sayfa sonuçtur** — istenenden *ve* servisin verdiği en büyük
  sayfadan kısa geldiyse. Tek koşul yetmiyor.
- **Sıralama maliyeti.** Değişme dışı sıralamalar bütün indeksi yürüyor;
  uçbirimde de aynı sınır var, sıralama değiştirmek ucuz değil.
- **Bayat cevap.** Servis `Rules` cevaplarını birleştirip geriden veriyor:
  gönderilenle uyuşmayan cevap geçmişe aittir, ekranı ezmemeli. Bu ders
  uçbirime de taşınacak (pencerede `State::sent_off`).
- **Anlık kipin hediyesi:** Slint'te tıklamayı öldüren "model değişti, satır
  yıkıldı" sınıfı hata burada olamaz — kalıcı bileşen yok. Ama karşılığında
  **her kare her şeyi çizmek zorunda**: çizim maliyeti satır sayısıyla değil,
  ekran hücresiyle sınırlı kalmalı; liste asla tümüyle gezilmemeli.
- **Boşta çizme.** Zamanlayıcıyla dönen bir döngü, hiçbir şey değişmese de
  CPU yakar. Kanal beklemeli.

---

## 8. Ölçüm

[MEASUREMENTS.md](MEASUREMENTS.md) kuralı burada da geçerli: sayısı olmayan
iddia görüş sayılır. Ölçülecekler:

| ne | nasıl | hedef |
|---|---|---|
| ilk kare | `SCOUR_TUI_TRACE=1`, açılıştan ilk çizime | < 50 ms |
| kare süresi (kaydırırken) | çizim başına µs, medyan ve p99 | < 2 ms |
| boşta CPU | 60 sn boşta `pidstat` | ~%0 |
| bellek | RSS | < 30 MB |
| derin sayfa | 2,6 M'inci satıra atlama | motorla aynı: < 1 ms |

Karşılaştırma dönüşümlü (ABAB) yapılır; tek koşu karar vermez.

---

## 9. Kararlar (Hasan, 2026-08-19)

### 9.1 Açılışta hangi yüz — hatırlanan yüz

**Pencere (Slint) varsayılan. Kullanıcı bir kez değiştirince artık o açılır.**

Yani `⇄` panelinden bir yüz seçmek yalnız onu başlatmaz, **tercihi de yazar**.
Bu üç yüze birden dokunan bir iş:

| parça | ne olacak |
|---|---|
| `scour-settings` | `face: String` alanı (`"window"` · `"browser"` · `"tui"`), varsayılan `"window"` |
| pencere · sayfa · TUI | `⇄`'den seçilen yüz ayarlara yazılır (`Ask::Remember`) |
| başlatıcı | masaüstü girişi ve `Ctrl+"` kısayolu **hatırlanan yüzü** açar |

Bugün masaüstü girişi `scripts/scour-app`'i (tarayıcı kipi) çalıştırıyor,
kısayol `scripts/scour-show`'u. İkisi de tek bir `scripts/scour-open`'a
bağlanacak: ayarı okur, karşılığını çalıştırır, ayar yoksa pencereyi açar.

**Neden ayarda, ortamda değil:** ayar dosyası zaten üç yüzün paylaştığı yer
(sütun genişlikleri, dil, kurallar oradan geliyor). Ayrı bir yere yazmak
dördüncü bir doğruluk kaynağı olurdu.

### 9.2 Kip — var, ama arama kipi varsayılan

Hasan: "bilmiyorum kip iyi mi, ekle."

**İki kip, ve açılış arama kipinde.** Everything'in davranışı korunuyor: pencere
açılır, yazmaya başlarsın, sorguya gider. Kip isteyene de kapı açık:

| kip | nasıl girilir | ne yapar |
|---|---|---|
| **arama** (varsayılan) | açılışta · `/` · `i` | yazdığın sorguya gider; `↑↓` listede gezinir |
| **gezinme** | `Esc` | `j k g G` `d u` gezinme, `n N` sonraki/önceki, `Space` seçim, `q` çıkış |

Kural: **kip, bir tuşun anlamını hiçbir zaman sessizce değiştirmez.** `Enter`,
`Tab`, `Ctrl+…` ve fare her iki kipte de aynı. Değişen yalnız çıplak harflerin
sorguya mı gideceği yoksa hareket mi olacağı. Kipi sayaç şeridinin sağ ucundaki
tek kelime söyler; kipsiz çalışmak isteyen ayarla gezinme kipini kapatabilir.

### 9.3 Renkler — kendi paletimiz dayatılır

`scour-ui::{DARK, LIGHT}` doğrudan `Color::Rgb` olarak kullanılır. Sebep:
üç yüzün aynı görünmesi bu projenin baştan beri kuralı, ve uçbirimin 16 rengine
düşmek "Belge mavisi" gibi anlam taşıyan renkleri kaybettirir — tür rayı ve
yaş şeridi bunun üstüne kurulu.

İki kaçış:
- **Uçbirim 24-bit renk desteklemiyorsa** (`COLORTERM` boşsa) en yakın 256/16
  renge düşülür; bu bir yedek, tercih değil.
- `SCOUR_TUI_COLORS=terminal` diyen, uçbirimin kendi paletini alır. Belgelenir,
  varsayılan değildir.

Açık/koyu seçimi ayardan gelir (üç yüzde ortak); `SCOUR_GUI_SCHEME`'in karşılığı
`SCOUR_TUI_SCHEME`.

### 9.4 Ağaç — kendimiz yazıyoruz

`tui-tree-widget` bakımlı ve uyumlu (0.24.1, 9 Ağustos 2026, `ratatui-core 0.1`
üstünde) — ama bizim ihtiyacımız **tek seviyeli katlanır liste**: yinelenen
grubu → yolları, klasör → alt klasörleri. Bu, girintili bir `List` demek; iki
crate bağımlılığı etmez. Projenin kuralı da bu ([[bağımsız modüller]]: arama
dışı her şey kendi crate'i, bağımlılıksız).

**Geri dönüş kapısı:** gerçek bir ağaç gezintisi (klasör hiyerarşisinde
dolaşma) istenirse `tui-tree-widget` uyumlu ve hazır; o gün eklenir.

### 9.5 Uçbirimi ne açacak

`⇄` panelinden "Uçbirim" seçilince sırayla denenir — ilk çalışan kazanır:

1. **`$TERMINAL`** — kullanıcı söylediyse tartışma yok.
2. **`xdg-terminal-exec`** — freedesktop'un önerilen "varsayılan uçbirim"
   belirtiminin başvuru uygulaması; kurulu olduğunda doğru cevap odur.
3. **`x-terminal-emulator`** — Debian/Ubuntu'nun alternatifler sistemi.
4. **Bilinenler**, komut bayrağıyla birlikte: `ptyxis -x` · `kgx --` ·
   `gnome-terminal --` · `konsole -e` · `foot` · `kitty` · `wezterm start --` ·
   `alacritty -e` · `xterm -e`.

Bayrak listesi elle tutulur çünkü **hepsi farklı**: bu makinede `ptyxis -x`,
`alacritty -e`. Hiçbiri bulunamazsa panel "uçbirim bulunamadı" der ve
`$TERMINAL`'i nasıl ayarlayacağını yazar — sessizce hiçbir şey yapmaz.

---

## 10. Sıradaki adım

Aşama 0 (ortaklaşma) ve 9.1'in ayar alanı, ilk `scour-tui` satırından önce
gelir: üçüncü kopya yazılmadan kopyalar birleşir.
