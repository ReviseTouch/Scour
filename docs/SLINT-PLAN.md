# Slint — planı

İki arayüz kalıcı: tarayıcı penceresi **hafif ve basit** olduğu için kalıyor,
Slint onu olabildiğince yakın taklit ediyor. İkisi de aynı servise IPC ile
bağlanıyor ve birlikte gelişiyor.

Bu belge kararların ve ölçümlerin kaydı; sıradaki adımlar en sonda.

---

## 0. Kararlar (Hasan, 2026-08-18)

| karar | ne demek |
|---|---|
| **Web kalıcı** | Slint ana ürün olsa da tarayıcı penceresi silinmiyor. İkisi eşit. |
| **Taklit** | Slint web'e olabildiğince benzeyecek; piksel eşitlik değil, ayırt edilemezlik. |
| **Arama kutusu aynı** | İkisinde aynı tasarım. |
| **Gevşek bağlama** | Slint kodu karıştırmayacak; her özellik kendi modülü, kendi `.slint` dosyası. |
| **IPC** | İletişim sokettten. Ölçüldü, aşağıda. |
| **Skia** | Erken denenecek, karar ölçümden sonra. |

---

## 1. IPC — ölçüldü, sorun değil

Endişe yerindeydi ve cevabı sayı: canlı servise 200 istek, medyan.

| sayfa | tur | motorun kendi işi | IPC + JSON | cevap |
|---|---|---|---|---|
| 20 satır | 7,26 ms | 7,11 ms | **0,15 ms** (%2) | 8 KB |
| 50 satır | 7,63 | 7,37 | 0,26 (%3,4) | 20 KB |
| 200 satır | 8,25 | 7,57 | **0,68 ms** (%8,2) | 71 KB |
| boş istek (`status`) | 0,45 | ~0 | **0,45 ms** | — |

Yani soketin taban maliyeti **yarım milisaniyenin altında**, 200 satırlık bir
sayfada serileştirmeyle birlikte 0,7 ms. Bir aramanın 8 ms'sinin 7,4'ü zaten
motorun sıralama ve sayfa kurma işi ve doğrudan linklense de duracaktı.

**Slint web'den hızlı olacak.** Web'in yolu tarayıcı → HTTP → köprü → IPC;
Slint doğrudan sokete bağlanıyor, bir durak eksik.

Aynı süreçte linkleme seçenek değil, ölçümden bağımsız olarak: indeks tek
yazıcı kabul ediyor ve o yazıcı `scourd`.

---

## 2. Ne paylaşılıyor, ne paylaşılmıyor

**Zaten ortak** (~31.000 satır): motor, indeks, tarama, kurallar, sorgu
ayrıştırıcı, protokol, ayarlar (`state/settings.json` — biri değiştirince öteki
görüyor), çeviri kataloğu.

**Bugün iki kopya:**

| karar | web | Slint |
|---|---|---|
| tema | CSS `:root`, 26 değişken | `theme.slint`, 30 property |
| sütunlar | JS `COLUMNS`, 12 sütun | 4 sabit alan |
| tür ikonları | CSS `k-*`, 14 SVG | yok |
| klavye | 32 dağınık kontrol | birkaçı |

Tema **şu an aynı hex kodlarını taşıyor** — `#0d1117`, `q-key: #7fa9e0`,
`unit: 4px`, `row: 30px`. Ayrışma henüz olmadı; onu tutan tek şey dikkat.

---

## 3. `scour-ui` — veri, kod üretimi değil

Ortak arayüz kararları bir crate'te **veri olarak** durur: tema paleti,
sütun tanımları (id · çeviri anahtarı · sıralama anahtarı · varsayılan genişlik
· hizalama), tür renkleri, sorgu rol renkleri, klavye haritası.

**`build.rs` ile `.slint` üretimi yok.** Denendi ve reddedildi *önce*, çünkü
üretilen dosyadaki satır numarası kaynağa denk gelmez ve hata mesajlarını
okunmaz hâle getirir — "kodlara karışıklık katmasın" kuralının ilk kurbanı bu
olurdu. Bunun yerine:

* **Slint** `.slint` dosyalarını elle tutar; `Theme` globalinin alanları
  `in property` olur ve `main.rs` açılışta `scour-ui`'den doldurur.
* **Web** sayfayı sunarken `:root{…}` bloğunu ve `COLUMNS` dizisini aynı
  crate'ten üretip enjekte eder.

Bağ tek yönlü ve gevşek: iki taraf da bir veri crate'ini *okur*, kimse kimseyi
tanımaz. Bir test iki tarafın aynı kaynaktan beslendiğini bağlar.

---

## 4. Arama kutusu — önce gölge, olmazsa çip

Web'in çözümü **gölge katmanı**: şeffaf bir `<input>` ve altına birebir oturan,
`pointer-events: none` olan renkli bir kopya (`.qshadow`). Kullanıcı düz metin
yazar, gördüğü renkli kopyadır. Rolleri motor verir (`explain` → spans);
arayüzün tek işi rolü renge çevirmektir.

Slint'te sıra:

1. **Aynı gölge katmanını dene.** Şeffaf `TextInput` + altında renkli `Text`
   parçaları. Tutarsa web'e hiç dokunmadan **birebir aynı** olur. Risk:
   Slint'in metin ölçümü tarayıcınınkiyle aynı olmayabilir; tek boşluklu yazı
   tipiyle hizalama tutmalı, imleç ve seçim görünürlüğü sınanmalı.
2. **Tutmazsa çip.** Tamamlanan terim renkli bir kutucuğa dönüşür, yazılan
   kuyruk düz kalır. Çalışan örnek: `RustWailsChat/Hukuk/.../ui/app.slint`.
   Slint'in `TextInput`'unda aralık-bazlı renklendirme yok (upstream #9560).
   Bu yola girilirse **web de çipe çevrilir** — ikisi aynı kalmalı.

Karar ölçümle verilir, tahminle değil.

---

## 5. Aşamalar

Her aşamanın bitiş ölçütü var; ölçüt sağlanmadan sıradakine geçilmez.

**0 · sözleşme.** `scour-ui` kurulur, tema ve sütunlar oraya taşınır, iki taraf
da ondan beslenir. *Ölçüt:* web ekran görüntüsü öncesiyle aynı, Slint derlenir,
testler geçer. Görsel çıktı değişmez — kazanç bundan sonrasında.

**1 · Slint kullanılabilir olur.** `settings` (sıralama, dil, sütun hafızası),
`wait` (canlı yenileme), `places` (kapsam rayı). *Ölçüt:* pencere kapat-aç aynı
yerden devam eder; bir dosya değişince liste tazelenir.

**2 · sütunlar ve görünüm.** 12 sütun, seçilebilir, genişlik sürüklenebilir; üç
görünüm modu. *Ölçüt:* web ile aynı sütunlar ve aynı varsayılan genişlikler.
En riskli aşama: sanal listenin aritmetiği satır yerine *çizgi* cinsinden
yeniden yazılmak zorunda — web'de bu bir tur sürdü.

**3 · paneller.** Kural paneli, dil menüsü, yardım, zaman şeridi. Hepsi
katalogdan beslenir; yeni metin yazılmaz.

**4 · zengin kısımlar.** Önizleme, rapor sekmesi (klasör boyutu, yinelenenler),
küçük resimler, ve arama kutusu (bkz. §4).

**Skia** aşama 0'dan hemen sonra denenir: `SLINT_BACKEND=winit-skia`, aynı
pencere, aynı liste, kare süresi ve boşta CPU ölçülür. Karar sayıya bakılarak
verilir.

---

## 6. Dosya düzeni — her özellik bağımsız

    apps/scour-gui/
      src/
        main.rs        pencere, durum, olay döngüsü
        link.rs        IPC (var)
        rows.rs        satır modeli (var)
        settings.rs    ayarların okunması/yazılması        ← aşama 1
        places.rs      kapsam rayı                          ← aşama 1
        columns.rs     sütun seçimi ve genişlikler          ← aşama 2
        rules.rs       atlama kuralları paneli              ← aşama 3
        preview.rs     önizleme                             ← aşama 4
      ui/
        theme.slint    globaller (değerler Rust'tan gelir)
        main.slint     pencere iskeleti
        rows.slint     liste
        rail.slint     kenar çubuğu                         ← aşama 1
        rules.slint    kural paneli                         ← aşama 3
        peek.slint     önizleme                             ← aşama 4

Kural: bir özellik bir modül ve bir `.slint` dosyası. `main.slint` iskeleti
tutar, içeriği tutmaz.
