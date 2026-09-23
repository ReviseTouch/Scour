# Scour

<img src="assets/scour.svg" width="72" alt="">

*English: [README.md](README.md)*

Scour, Linux için bir dosya indeksleyici ve arama aracıdır. Yapılandırılmış
birimlerdeki her dosya ve klasörü kendi indeksinde tutar, dosya sistemi
değiştikçe indeksi günceller ve aramaları milisaniyeler içinde bu indeksten
cevaplar. Aynı indeks, bir MCP sunucusu üzerinden dil modellerine de açılır.
Rust ile yazılmıştır; dışarıdan bir arama motoruna ya da veritabanına bağımlı
değildir.

**[revisetouch.com/scour](https://revisetouch.com/scour)** ·
**[Belgeler](https://revisetouch.com/docs/scour/giris)** ·
**[Sürümler](https://github.com/ReviseTouch/Scour/releases)**

> **Alfa sürüm.** Scour bir Linux makinesinde, 4,8 milyon kayıtlık bir
> indeksle günlük kullanımdadır; ölçümler oradan alınmıştır. Windows derlemesi
> tek bir makinede başlatılmıştır, bunun dışında test edilmemiştir. macOS
> derlenir, çalıştırılmamıştır. İndeks biçimi sürümler arasında değişebilir.

```
$ scour "ext:rs size:>10kb dm:7d"
      Kod     5,18 MiB  /home/u/Projeler/Scour/target/release/build/scour-gui/out/main.rs
      Kod     20,1 KiB  /home/u/Projeler/Scour/apps/scour/src/main.rs
      Kod     30,2 KiB  /home/u/Projeler/Scour/apps/scour/src/render.rs
      Kod     24,9 KiB  /home/u/Projeler/ColpanRust/crates/repo_latches/tests/surface_line_budget.rs
      Kod     13,8 KiB  /home/u/Projeler/ColpanRust/crates/command_catalog/tests/panels.rs
1705 içinden 5, 3,27 ms (28000 satır) · daha fazlası için -n 40
```

Uçbirimde komut beş satır basar; boruya yazarken kırk. Sayıyı `-n` belirler.

## Arayüzler

Tek bir servis (`scourd`) indeksi tutar; dört arayüz ona yerel bir soketten
bağlanır ve ayarlarını paylaşır. Aşağıdaki görüntüler aynı anda, aynı sorguyla
(`kind:code dm:7d size:>10kb`), 4,7 milyon kayıtlık bir indekste alınmıştır.

**Komut satırı** — `scour`

![Komut satırında Scour](docs/img/command-line.webp)

**Pencere** — `scour-gui`, yerel bir Slint uygulaması

![Scour penceresi](docs/img/window.webp)

**Tarayıcı** — `scour-web`, tek bir sayfa sunan yerel bir köprü

![Tarayıcıda Scour](docs/img/browser.webp)

**Uçbirim** — `scour-tui`, tam ekran uçbirim arayüzü

![Uçbirimde Scour](docs/img/terminal.webp)

## Performans

`find` her sorguda dosya sistemini baştan yürür. Scour bir kez yürür, indeks
tutar ve değişiklikleri izler. Canlı indekste (4,8 milyon kayıt, diskte
493 MiB), tam gidiş-dönüş olarak — soket, ayrıştırma, arama, sıralama, sayım
ve kırk satır:

| sorgu | ms |
|---|---:|
| `rapor` | 7,9 |
| `size:>10mb` | 7,5 |
| `kind:code dm:7d` | 16,7 |
| `kind:image` (1,6 milyon eşleşme) | 37,3 |

İndeks bellek eşlemelidir: sayfa önbelleğinde yaşar ve çekirdek baskı
altında geri alabilir; arama, indeksin tamamı kadar değil, onlarca megabayt
yerleşik bellek ister. Değişiklik bildirimleri eksiksiz sayılmaz: her kaynak
ayrıca tam bir eşitleme geçişi alır — varsayılan olarak 30 dakikada bir, ne
izleyicisi ne yazma sayacı olan kaynaklarda dakikada bir. Ölçümler ve onları
üreten komutlar [docs/MEASUREMENTS.md](docs/MEASUREMENTS.md) dosyasındadır.

## Kurulum

### Hazır sürümle

```bash
tar xzf scour-0.2.0-alpha.2-linux-x86_64.tar.gz
cd scour-0.2.0-alpha.2-linux-x86_64
./install.sh
```

Betik parola istemez. Yedi ikiliyi `~/.local/bin` altına, menü girdisini ve
simgeyi — SVG çizemeyen paneller için dokuz boyda PNG ile birlikte —
`~/.local/share` altına kurar; GNOME ve KDE'de **Super+F**'yi Scour'a bağlar
(`SCOUR_KEY=ctrl+alt+s` başka bir tuş seçer, `SCOUR_KEY=none` bağlamayı atlar).
Uçbirimde ardından Scour'u oturumunuzla başlatmayı (systemd kullanıcı birimi)
teklif eder ve indeks cevap verene kadar bekler; `--yes` kabul eder,
`--no-service` teklifi atlar, uçbirimsiz bir çalıştırma hiç sormaz. Bu
dosyaları silmek kurulumu geri alır.

Servis çalışmıyorsa arayüz onu kendisi başlatır. Pencere, uçbirim arayüzü ve
tarayıcı köprüsü `scourd`'u önce kendi ikilisinin yanında, sonra `PATH`'te
arar, çıktısı durum dizinindeki `scourd.log`'a gidecek şekilde ayrı bir
oturumda başlatır ve sokete en çok on saniye bekler. systemd gerekmez.
`SCOUR_NO_AUTOSTART=1` bunu kapatır; komut satırı hiçbir şey başlatmaz.

Sürüm ikilileri bir Debian 11 konteynerinde glibc 2.31'e göre derlenir:
Debian 11, Ubuntu 22.04, RHEL 9 ve sonrası. Pencere ayrıca her masaüstünde
bulunan `libfontconfig1`'i ister. Paket sıfırdan kurulmuş Ubuntu 22.04,
Ubuntu 24.04 ve Debian 11 konteynerlerinde kurulup çalıştırılmıştır.

### Debian, Ubuntu ve Fedora paketleri

```bash
sudo apt install ./scour_0.2.0~alpha.2-1_amd64.deb      # Debian 11 ve üstü, Ubuntu 22.04 ve üstü
sudo dnf install ./scour-0.2.0~alpha.2-1.x86_64.rpm     # Fedora
```

İkisi de yedi ikiliyi taşır: altısı `/usr/bin`'de, ayrıcalıklı `scour-watch`
`/usr/libexec/scour/` altında; başlatıcılar, menü girdisi, dört boyda simge ve
`/usr/lib/systemd/user/` altında kullanıcı birimi. Kurmak hiçbir şeyi
başlatmaz: `systemctl --user enable --now scourd.service` ayrıcalıksız servisi
başlatır; `/usr/share/doc/scour/` altında sistem birimi ve polkit kuralı örnek
olarak, paketin neyi nereye koyduğunu anlatan bir notla durur. Pencerenin
çalışırken yüklediği X11 ve Wayland kütüphaneleri zorunlu değil önerilidir;
`--no-install-recommends` bir sunucuda yalnız komut satırını kurar. Ubuntu
22.04, Ubuntu 24.04, Debian 12 ve Fedora 40 konteynerlerinde denenmiştir.
`scripts/package` ikisini derlenmiş ikililerden üretir; `cargo-deb`,
`cargo-generate-rpm` ve `rsvg-convert` ister.

### Flatpak

```bash
flatpak install --user flathub org.flatpak.Builder
flatpak run org.flatpak.Builder --user --install --force-clean \
    build-dir packaging/flatpak/com.revisetouch.Scour.yml
flatpak run com.revisetouch.Scour
```

Manifest `packaging/flatpak/` altındadır; Scour henüz Flathub'da değildir.
Kum havuzundaki Scour bütün dosya sistemini indeksler ve arar; pencere,
tarayıcı sayfası ve uçbirim arayüzü çalışır, ancak `fanotify` işareti
koyamadığı için canlı izleme yoktur: servis değişikliği birimin yazma sayacı
oynayınca ve dönemsel eşitleme geçişinde bulur. Küçük resimler çalışma
ortamının taşımadığı bir programı ister. Soket kum havuzunun içindedir; komut
satırına ve MCP sunucusuna `flatpak run --command=scour com.revisetouch.Scour`
ve `flatpak run --command=scour-mcp com.revisetouch.Scour` ile ulaşılır.
Ayarlar ve indeks `~/.var/app/com.revisetouch.Scour/` altında, tarball
kurulumundan ayrı durur. Gerisi `packaging/flatpak/README.md` dosyasındadır.

**Windows.** Çalışma alanı `x86_64-pc-windows-msvc` için derlenir ve sürümde
bir zip vardır. İkililer bir Windows makinesinde bir kez başlatılmıştır:
servis indeksledi, komut satırı aradı. Pencere, canlı izleme, ağ ve FAT32
birimleri orada test edilmemiştir; USN günlüğü okuyucusu yoktur, ilk tarama
diski yürür. **macOS** derlenir, çalıştırılmamıştır. Bir CI işi üç hedefin de
derlendiğini denetler.

### Kaynaktan

Gerekenler: Rust 1.88 ya da üstü ([rustup](https://rustup.rs)), `pkg-config`
ve yalnız pencerenin istediği fontconfig geliştirme başlıkları:

| dağıtım | paketler |
|---|---|
| Ubuntu, Debian | `sudo apt install pkg-config libfontconfig1-dev` |
| Fedora | `sudo dnf install pkgconf fontconfig-devel` |
| Arch | `sudo pacman -S pkgconf fontconfig` |

```bash
git clone https://github.com/ReviseTouch/Scour.git
cd Scour
cargo build --release
scripts/release                      # dist/scour-<sürüm>-linux-x86_64.tar.gz üretir
cd dist && tar xzf scour-*-linux-x86_64.tar.gz && cd scour-*-linux-x86_64 && ./install.sh
```

Temiz derleme birkaç dakika sürer. Bir C derleyicisi varsa, pencerenin daha
eski glibc sürümlerinde de çalışması için iki libm simgesini sabitlemekte
kullanılır; yoksa derleme yine tamamlanır. `install.sh` yerine elle kurmak
için: `install -m755 target/release/scour{,d,-gui,-tui,-web,-watch,-mcp}
~/.local/bin/` (bu durumda menü girdisi ve kısayol olmaz).

Bir ikilinin glibc tabanını onu derleyen makine belirler; yuvarlanan bir
dağıtımda derlenen tarball Ubuntu 22.04'te açılmaz. `scripts/release-build`
aynı yedi ikiliyi bir Debian 11 konteynerinde (podman ya da docker)
`target/container/release` altına derler ve her birinin istediği tabanı basar;
`scripts/release --container` onları paketler.

### Klavye kısayolu

Scour'u her kombinasyon açabilir ve bu Scour'un içinden ayarlanır: pencerede
ve tarayıcı sayfasında bir "Klavye kısayolu" satırı var; uçbirimde `scour
hotkey set super+f` aynı işi yapar (`scour hotkey` durumu gösterir, `scour
hotkey clear` kaldırır). Kombinasyon tek bir yazımla yazılır: `super+f`,
`ctrl+alt+s`, `super+F2`. GNOME'da tuş hemen çalışır, KDE'de bir sonraki
oturum açılışında. Diğer masaüstlerinde ve Flatpak içinde Scour bağı
yazamaz: masaüstünün kendi klavye ayarlarından elle bağlanacak komutu
gösterir, `scour-open` ya da `flatpak run com.revisetouch.Scour`. Kurucu
Super+F'yi teklif eder (`SCOUR_KEY=ctrl+alt+s` başka bir tuş seçer,
`SCOUR_KEY=none` atlar); pencere ilk açılışta bir kez teklif eder. Tuş
başlatıcıyı çalıştırır, yani en son geçtiğin yüzü açar; Scour açıkken tekrar
basınca ikinci bir kopya başlatmaz, açık pencereyi öne getirir.

Wayland üzerinde GNOME'da bir pencereyi yalnız kabuk öne getirebilir; bu
yüzden Scour tek işi bu olan küçük bir Shell eklentisiyle gelir
(`platform/gnome`). Kurucu ve paketler eklentiyi yerleştirir, tuşu ayarlamak
onu açar; GNOME yeni kurulan bir eklentiyi bir sonraki oturum açılışında
yükler. Eklenti yoksa ya da başka bir masaüstündeyse ikinci basış yine
Scour'u açar; pencere öne gelmek yerine görev çubuğunda işaretlenebilir.

### Dosya sistemini izleme

Linux'ta Scour birim başına bir `fanotify` işaretiyle izler. Maliyet dizin
sayısına bağlı değildir. İşaret koymak `CAP_SYS_ADMIN` ister; `scourd` bu
yetkiyi taşımaz. Ayrı bir yardımcı, `scour-watch`, işaretleri koyar,
tanıtıcıyı devreder, yetkiyi bırakır ve `scourd`'u çalıştırır.

inotify kullanılmaz ve inotify'a düşen bir yol yoktur. inotify, oturumdaki her
programın paylaştığı bir bütçeden dizin başına bir izin ister; bütçe tükenince
ilgisiz programlar hata verir. İşaret yoksa `scourd`, birimin yazma sayacı
değişince o birimi yürüyerek eşitler; hiçbir değişiklik kaybolmaz, ancak geç
görünür. Ağ ve FUSE bağlarının yazma sayacı yoktur; onlar işaret ister.

```bash
sudo bash packaging/install-service.sh [--user AD] [KÖK...]   # root isteyen tek adım
systemctl start scour@<kullanıcı>.service
```

Sistem birimi bir şablondur, hesap başına bir örnek çalışır.
`scour@hasan.service`, `/etc/scour/hasan.conf` dosyasındaki dosya sistemlerini
(kurucuya verilen kökler, varsayılan `/home`) işaretler, o hesaba düşer ve
`scourd`'u onun adına çalıştırır; yani bir örnek yalnız hesabının okuyabildiğini
indeksler ve soketi o hesabın kendi çalışma dizinindedir. Hesap `sudo`'yu
çalıştıran kullanıcıya varsayılır. Kurucu yardımcıyı root'a ait
`/usr/local/libexec/scour` altına koyar, şablonu ve her hesabın yalnız kendi
örneğini parolasız başlatıp durdurmasına izin veren, başka hiçbir şeye izin
vermeyen bir polkit kuralı kurar. İkinci bir kişi için `--user` ile yeniden
çalıştırın; eski tek kullanıcılı `scour.service` kendiliğinden taşınır.
Kurmadan önce iki dosyayı da okuyun. O hesabın kullanıcı birimi etkinse kurucu
kapatılana kadar reddeder: `systemctl --user disable --now scourd.service`.

### Ayarlar

Ayarlar `~/.config/scour/config.toml` dosyasındadır. Atlama listeleri indekse
neyin girmediğini belirler; beklenen bir dosya çıkmıyorsa önce oraya bakın.

## Sorgu dili

Boşluk VE, `|` VEYA, `!` DEĞİL; tırnak tam ifade, `*` ve `?` joker,
`alan:değer` alan süzgeci. Alan adları Türkçe de yazılabilir (`tür:kod`).

```
rapor ext:pdf dm:30d          adında "rapor" geçen, son 30 günde değişmiş PDF'ler
*.log size:>100mb             100 MB'den büyük günlük dosyaları
under:~/Projeler *.rs         bir dizinin altındaki bütün Rust dosyaları; ~ ev dizinin
path:src ext:rs !test         src altındaki Rust dosyaları, testler hariç
kind:image dm:today           bugün değişen görseller
```

`scour syntax` başvuruyu basar. Arama büyük/küçük harfe duyarsızdır; `i`, `ı`,
`I` ve `İ` tek harf sayılır. Ayrıştırma hata vermez: tanınmayan bir alan metin
olarak aranır, `scour explain "<sorgu>"` sorgunun nasıl okunduğunu gösterir.

## Rapor

Rapor bir klasör için "baytlar nerede" sorusunu cevaplar ve her yüz aynı resmi
aynı sayılardan çizer: kapsamın baytlarını yaşa göre gösteren bir şerit ve
efsanesi; baytların nerede olduğunu gösteren bir şerit, en ağır klasörler ve
kalanı; sonra klasör başına bir satır, adın arkasında payı, kendi yaş şeridi,
boyutu, payı ve dosya sayısı; türler için altı dilim ve kalanı gösteren, efsanesi
aramayı `kind:` ile açan bir halka; ve çubuk olarak en büyük dosyalar. Pencere ve
tarayıcı sayfası bunları piksel, uçbirim yüzü blok hücreleriyle çizer; uçbirimde
`scour du` ve `scour facets` de şeridi ve çubukları çizer, boruya yazarken her
zamanki tabloyu basar. Paylar tek bir crate'ten gelir, `scour-chart`; efsane
yüze tamamlanacak şekilde yuvarlar ve sayfanın JavaScript kopyası her test
koşusunda onunla karşılaştırılır.

## Yinelenen dosyalar

Tarayıcı yüzünün rapor sekmesi aynı boyuttaki dosyaları, en çok yer
kazandıracak grup başta olmak üzere listeler: her grup, biri dışında hepsini
silmenin ne boşaltacağını söyler; başlık grupları toplar ve okunarak aynı
oldukları kanıtlandı mı, yoksa yalnız boyutları mı aynı, onu belirtir. Boyut
eşleşmesiyle hiçbir şey silinmez. Her grupta bir kopya "tut" işaretini taşır,
varsayılan olarak en yenisi; "Diğer N tanesini çöp kutusuna taşı" yalnız
"Okuyarak doğrula" o grubu kanıtladıktan sonra açılır, bir kez sorar ve
masaüstünün çöp kutusuna gönderir, asla doğrudan silmez. Her yolun arama
listesindeki menüsü vardır. Sabit bağlantılar kopya sayılır; birini çöpe
taşımak yer açmaz ve sayfa bunu henüz söylemiyor.

## MCP sunucusu

`scour-mcp`, on bir salt-okunur araçlı bir Model Context Protocol sunucusudur.
Her cevap sınırlıdır ve kesildiğinde bunu söyler: `scour_tree` bir milyon
girdili dizini on girdili kadar hızlı listeler ve kaç girdiyi dışarıda
bıraktığını bildirir; `scour_count` listelemeden sayar; `scour_facets` bir
dosya kümesini türe, uzantıya ya da dizine göre özetler; `scour_sources` hangi
yolların indekste olduğunu söyler. Yeniden tarama, bakım ve kapatma protokolde
vardır, modele açılmaz.

`scour mcp-config` istemci için yapılandırmayı ve gideceği dosyayı basar:

| istemci | komut |
|---|---|
| Claude Desktop | `scour mcp-config` → `claude_desktop_config.json` |
| Claude Code | `scour mcp-config --for claude-code` → bir `claude mcp add` komutu |
| Codex | `scour mcp-config --for codex` → `~/.codex/config.toml` |
| Cursor | `scour mcp-config --for cursor` → `~/.cursor/mcp.json` |
| Gemini CLI | `scour mcp-config --for gemini` → `~/.gemini/settings.json` |
| VS Code | `scour mcp-config --for vscode` → `.vscode/mcp.json` |

`scourd` çalışıyor olmalıdır; MCP sunucusu da diğer arayüzler gibi onun bir
istemcisidir.

## Mimari

Birden fazla katmanın bağımlı olduğu tek crate `scour-core`'dur — tipler ve
trait'ler.

```
   scour (CLI)   scour-mcp   scour-web   scour-gui   scour-tui
        └─────────────── scour-proto ───────────────┘
                              │
                          scour-ipc          yerel soket, NDJSON
                              │
                           scourd            somut tiplerin adlandırıldığı tek yer
                              │
                        scour-engine         Box<dyn Source>, Arc<dyn Index>
                              │
         ┌─────────────── scour-core ───────────────┐
         │             tipler + trait'ler           │
  scour-index-native   scour-source-fs   scour-query   scour-config   scour-i18n
```

Motor, kaynağı ve indeksi trait'ler üzerinden alır ve somut tiplerini
bilmez; indeks gerçeklemesini değiştirmek `scourd`'da tek satırlık bir
değişikliktir. İndeks satırları en yeniden başlayarak saklar, gösterilen
metni sıralı sütunların dışında tutar ve her üst dizini bir terim olarak
indeksler; bir alt ağacı silmek tek işlemdir. Test paketi her sorgu şeklini
kaba kuvvetle çalışan bir başvuru gerçeklemesiyle karşılaştırır.

Ayrıntı: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md); dosya türü
sınıflandırması: [docs/TAXONOMY.md](docs/TAXONOMY.md). (Belgeler İngilizcedir.)

## Diller

Kaynak dili İngilizcedir. Çeviriler `lang/` altındaki gettext kataloglarıdır,
ikililere derlenir ve İngilizce metinle anahtarlanır. `SCOUR_LANG=tr scour
status` tek bir çalıştırma için dil seçer. Gelen diller: İngilizce, Türkçe.

## Lisans

MIT ya da Apache-2.0, tercihinize göre.
