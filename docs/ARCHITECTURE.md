# Scour — nasıl kurulu

Bu belge, depoyu ilk kez açan birinin **nereye bakacağını** ve bir şey
eklerken **nereye koyacağını** bilmesi için. Kararların gerekçesi kodun
kendisinde; burada olan şey haritanın kendisi.

Kural tek cümleyle: **içeride sıkı, aralarında gevşek.** Her crate tek bir işi
bütün olarak yapar; aralarındaki bağ veri ve sözleşmedir, çağrı zinciri değil.

---

## 1. Bir bakışta

```
        ┌──────────┐   ┌──────────┐   ┌──────────┐   ┌──────────┐
        │  sayfa   │   │ pencere  │   │ uçbirim  │   │ komut s. │   ← dört yüz
        │scour-web │   │scour-gui │   │scour-tui │   │  scour   │
        └────┬─────┘   └────┬─────┘   └────┬─────┘   └────┬─────┘
             └──────────────┴───────┬──────┴──────────────┘
                                    │  tek soket, tek dil
                              ┌─────▼──────┐
                              │  scourd    │   ← tek yazan, tek karar veren
                              └─────┬──────┘
                    ┌───────────────┼───────────────┐
             ┌──────▼─────┐  ┌──────▼──────┐  ┌─────▼──────┐
             │scour-engine│  │scour-index- │  │scour-source│
             │  (arama)   │  │   native    │  │   -fs      │
             └────────────┘  └─────────────┘  └────────────┘
```

Dört yüz, **dört program değil**. Hepsi aynı servise aynı soketten bağlanır,
aynı cevabı alır. Aralarındaki fark yalnızca çizimdir.

---

## 2. Katmanlar ve kural

Bağımlılık aşağı doğru akar; yukarı doğru **hiç akmaz**.

| katman | crate | ne bilir | ne bilmez |
|---|---|---|---|
| **veri** | `scour-core` | bir satır nedir, bir sorgu nedir, bir hata nedir | dosya sistemi, soket, arayüz |
| **dil** | `scour-query` | yazılanı nasıl okuyacağı | indeksin nasıl saklandığı |
| **saklama** | `scour-index-native` | sütunlar, trigramlar, klasör tablosu | sorgunun nereden geldiği |
| **arama** | `scour-engine` | hangi satır cevaba girer, hangi sırayla | kimin sorduğu |
| **kaynak** | `scour-source-fs`, `scour-watch` | dosya sisteminin yürünmesi ve izlenmesi | indeksin içi |
| **taşıma** | `scour-proto`, `scour-ipc` | soru ve cevabın şekli, çerçeveleme | ikisinin de anlamı |
| **paylaşılan sunum** | `scour-ui`, `scour-page`, `scour-i18n`, `scour-settings`, `scour-places` | her yüzün aynı cevabı vermesi gereken sorular | çizim araçları |
| **yüzler** | `scour-web`, `scour-gui`, `scour-tui`, `scour` | çizim, tuş, fare | indeks, dosya sistemi, motor |

**Bir yüz motoru linklemez.** Dördü de bildiği her şeyi soketten öğrenir. Bu,
gevşek bağın nerede olduğunu söyler: yüzler ile servis arasında.

---

## 3. Paylaşılan sunum — asıl "merkezden yönetim" burası

Aynı cevabı dört ayrı yerde vermek, dördünün sessizce ayrışması demektir.
Bunlar bir kez yazılır:

| crate | ne karara bağlar | neden orada |
|---|---|---|
| `scour-ui::format` | `5.356.281`, `1,44 MiB`, `636,4 MB`, `2026-08-13 00:49`, altı yaş bandı | noktalama **dilin**, platformun değil: Türkçe masaüstünde İngilizce pencere `5,356,281` yazar |
| `scour-ui::path` | yaprak, klasör, kırıntı adımları | üç yüz üç ayrı yerde kesiyordu |
| `scour-ui::query` | süzgeç ile yazılanın birleşmesi, basılana tekrar basınca temizlenmesi | "tek süzgeç, sona eklenir" bir dil kuralı, bir çizim kuralı değil |
| `scour-ui` (palet, sütunlar, bantlar, tür renkleri) | `#0d1117`, sütun kimlikleri, 24 zaman bandı | CSS ile `.slint` iki kopya tutuyordu ve odak rengi çoktan ayrışmıştı |
| `scour-ui::faces` | hangi yüzde ne var | aşağıda, §5 |
| `scour-page` | sayfa 200 satır, LRU 32, cevabın taşıdığı ofsete yazılır, kısa sayfa iki ölçüye göre sonuçtur | altı hata pahasına öğrenildi; ikinci kez öğrenilmesin |
| `scour-settings` | sütun genişliği, dil, düzen, **hangi yüz açılır**, atlama kuralları | dört yüzün ortak hafızası; `config.toml` elle yazılan dosya olarak kalır |
| `scour-i18n` | katalog, **dil sırası**: seçilen → `config.toml` → masaüstü → İngilizce | `.po` dosyaları; kodda İngilizce msgid. Sıra `choose()` içinde bir kez yazılıdır; pencere kendi kopyasını tutuyordu ve `SCOUR_LANG`'i görmüyordu |
| `scour-places` | masaüstünün kendi klasörleri, hangi bölüm okuma zamanı tutar | bir makine sorusu, bir indeks sorusu değil |
| `scour-thumbs` | küçük resim önbelleği nerede, hangi türe hiç bakılmaz, kim üretebilir | dört `stat`'ı hangi satırın hak ettiği tek kural; sayfa ve pencere aynı soruyu soruyor |

Bir şey **iki yüzde birden** gerekiyorsa yeri buradadır. Üçüncü kopya yazılıyorsa
bir şey yanlış gidiyordur.

Aynı kural yollar için de geçerli: ayarların nerede durduğunu `Config::state_dir()`
söyler. Üç yerde ayrı ayrı yazılıydı — servis, pencere ve uçbirim — ve biri
ayrıştığı gün bir yüzde seçilen dil öteki yüzde görünmeden kaybolurdu.

Metin de öyle: bir yüzün gösterdiği her kelime katalogdan gelir. Uçbirimde bunu
denetleyen bir test var — kendi kaynağını okuyup `say(...)`'a verilen her msgid'i
Türkçe katalogda arar, çünkü elle tutulan bir msgid listesi yazıldığı gün doğrudur.

---

## 4. Servis: tek yazan

`scourd` indeksi yazan tek süreçtir; yüzler yalnız okur ve sorar.

- **Üç şerit.** `scour-ipc` tek seferde tek çağrı taşır ve `scourd` bağlantı
  başına bir iş parçacığı açar. Bu yüzden her yüz birden çok bağlantı tutar:
  tuş vuruşunun beklediği (arama), bekleyebileceği (facet, kural, CSV) ve
  **uzun yoklama** (indeks değişti mi) — sonuncusu bağlantısını otuz saniye
  tuttuğu için kendi şeridinde olmak zorunda.
- **Kuşak (generation).** Her arama bir sayı taşır; eski bir cevap çizilmez.
  Yavaş gelen `re` cevabının hızlı gelen `rapor` cevabının üstüne yazması, bir
  arama kutusunun yapabileceği en kötü şeydir.
- **Cevap ofseti taşır.** Sayfa, istenen ofsete değil, **cevabın söylediği**
  ofsete yazılır.

---

## 5. Yüzler arasında bağ: çizelge kodda

`scour-ui::faces` her özelliği ve dört yüzdeki durumunu tutar; `scour features`
onu basar:

```
feature         page          window        terminal      command line
search          yes           yes           yes           yes
language        yes           yes           yes           yes
thumbnails      yes           no            no            —
```

`no` ile `—` farklıdır: biri "henüz yok", öteki "orada olamaz" (komut satırında
küçük resim). Her ikisi de **neden** olduğunu yazmak zorundadır — testi bunu
denetler.

**Yeni bir özellik çizelgeye satır eklemekle başlar.** Bir yüz geride
kalacaksa orada `no` olarak durur; sessizce eksik kalmaz. Belge eskiyebilir,
kod eskimez.

---

## 6. Yüzler: her biri neyi kendi yapar

| yüz | çizim | kendine ait olan |
|---|---|---|
| **sayfa** (`scour-web`) | HTML/CSS/JS tek dosyada (`page.html`), köprü onu servis eder | tarayıcıda çalışır; `POST` yolları jeton + origin + `--no-launch` ile çevrili |
| **pencere** (`scour-gui`) | Slint, yazılım çizici | model bir kez kurulur, yerinde güncellenir (bkz. `docs/SLINT-PLAN.md`) |
| **uçbirim** (`scour-tui`) | ratatui, anlık kip | `--once`, `--press`, `--click`: ekranı ve tıklamayı denetlemenin tek yolu |
| **komut satırı** (`scour`) | metin ve `--json` | tek soru, tek cevap; sayfalama yok |

Ortak kural: **çizen yer ile vuran yer aynı aritmetiği kullanır.** Pencerede bu
kural iki gün yedi (panel bir yerde çiziliyor, başka yerde test ediliyordu);
uçbirimde `spot_at` ve `draw` sabitleri paylaşır.

---

## 7. Doğrulama araçları

Ekran görüntüsü bir arayüzü doğrulamaz; iz kaydı ve sentetik olay doğrular.

| araç | ne yapar |
|---|---|
| `SCOUR_GUI_SNAP=/x.ppm` | pencere kendi fotoğrafını çeker |
| `SCOUR_GUI_QUERY/PANEL/SCROLL/CLICK/HOVER` | pencereyi bir duruma sokar, sentetik olay gönderir |
| `scour-tui --once WxH` | kareyi **metin olarak** basar |
| `scour-tui --press`, `--click` | tuşa ve noktaya basar |
| `SCOUR_TUI_TRACE=/tmp/log` | işleyişi dosyaya yazar (ekrana değil) |
| `scripts/bench` | dokuz sorgu, servisin CPU/RSS'i, uçbirimin ilk karesi |
| `examples/*.rs` (indeks) | `rankcheck`, `reachcost`, `pathcost` — iddiadan önce ölçüm |
| `tests/smoke.rs` (indeks) | her sorgu şeklini **kaba kuvvetle** karşılaştırır |

Ölçüm kuralı `docs/MEASUREMENTS.md`'nin başında: iki ikiliyi **dönüşümlü**
koştur, sabahı öğleden sonrayla karşılaştırma.

---

## 8. Bir şey eklerken

1. `scour-ui::faces`'e satırı ekle — dört yüzün durumuyla.
2. Anlamı nereye ait? İki yüzde birden gerekiyorsa paylaşılan crate'e; bir
   yüze özgü çizimse o yüze.
3. Servis tarafı gerekiyorsa `scour-proto`'ya soru/cevap ekle — eski istemcinin
   yeni cevabı okuyabilmesi için alanlar `#[serde(default)]`.
4. Ölçülebilir bir iddia varsa önce ölç (`examples/`, `scripts/bench`) ve
   `docs/MEASUREMENTS.md`'ye komutuyla yaz.
5. Doğruluk iddiası varsa kaba kuvvetle karşılaştır.

---

## 9. Nereden okumaya başlamalı

- **Ne yapıyor:** `README.md`, sonra `scour features`.
- **Dil:** `crates/scour-query/src/syntax.rs` (kılavuzun kendisi).
- **Sorgunun yolu:** `apps/scourd/src/handle.rs` → `scour-engine` →
  `scour-index-native/src/search.rs`.
- **Bir yüzün yolu:** `apps/scour-tui/src/main.rs` en kısası ve döngüsü
  başında anlatılmış.
- **Neden böyle:** `docs/MEASUREMENTS.md`, `docs/TUI-PLAN.md`,
  `docs/SLINT-PLAN.md`.

---

## 8. Önizleme paneli — üçüncü sütun

Listenin yanında, seçili satırı gösteren bir panel. **Bir kip, bir bakış
değil**: açık kalır ve oklar nereye giderse oraya uyar — Everything'in preview
pane'i budur. Pencere ile sayfa aynı paneli çiziyor:

| | nereden |
|---|---|
| hangi olgular, hangi sırayla | `scour_ui::preview::FACTS` — etiketler sütun başlıklarının kendi msgid'leri |
| genişlik ve sınırları | `scour_ui::preview::PANEL_WIDE/MIN/MAX` |
| dosyanın *ne olduğu* | servis (`Request::Preview`) — karar dosyanın ilk sekiz kilobaytını ister |
| olgular | servis (`Request::Stat`) — dört tanesi hiçbir sütunda yok |
| açık mı | `Settings::preview` — bir yüzde iğnelenen panel ötekinde de açılır |

Resim için **küçük resim öncelikli**: 380 piksellik bir panel için kırk
megapiksellik bir çözme yapılmaz. Yoksa servisten istenir (ızgarayla aynı
kapı, aynı dörtlü sınır); dosya 512 KB'den küçükse doğrudan çizilir — simgeler,
ekran görüntüleri ve küçük resim önbelleğinin kendi dosyaları bu sınıfta.

**Slint'te iki tuzak, ikisi de ölçülerek bulundu.** Bir çocuğun
`preferred-height`'ini ebeveyninin `height`'ine bağlamak çemberdir ve Slint'in
cevabı hiç çizmemektir. Ve beş sabit genişlikli sütunu olan liste, kendi
asgarisi pencereden geniş olduğu için kardeşini elli piksele sıkıştırır:
listeye `min-width: 0px` demek, "sağdan kesilebilir" demektir ve panelin
genişliğini alabilmesinin tek yolu odur.
