# Scour

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
tar xzf scour-0.2.0-alpha.1-linux-x86_64.tar.gz
cd scour-0.2.0-alpha.1-linux-x86_64
./install.sh
```

Betik parola istemez. Yedi ikiliyi `~/.local/bin` altına, menü girdisini ve
simgeyi `~/.local/share` altına kurar; GNOME ve KDE'de **Super+F**'yi
pencereye bağlar (`SCOUR_KEY=ctrl+alt+s` başka bir tuş seçer,
`SCOUR_KEY=none` bağlamayı atlar). Bu dosyaları silmek kurulumu geri alır.

İkililer glibc 2.39'a göre derlenmiştir: Ubuntu 24.04 ve üstü, Debian 13 ve
üstü, Fedora 40 ve üstü, yuvarlanan dağıtımlar. Daha eski sürümler kaynaktan
derlemeyi gerektirir. Paket sıfırdan kurulmuş Ubuntu 24.04, Fedora, Debian 13
ve Arch konteynerlerinde kurulup çalıştırılmıştır.

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

### Klavye kısayolu

Kurucu GNOME ve KDE'de bir tuş bağlar. Diğer masaüstlerinde herhangi bir tuşu
`scour-gui`'ye bağlayın: ilk basış pencereyi açar, sonraki her basış aynı
pencereyi öne getirir; ikinci bir kopya hiç başlatılmaz.

| masaüstü | yer |
|---|---|
| GNOME | Ayarlar → Klavye → Klavye Kısayolları → Özel Kısayollar, komut `scour-gui` |
| KDE Plasma | Sistem Ayarları → Kısayollar → Komut Ekle, `scour-gui` |
| diğer | compositor'ın tuş bağlama ayarı, komut `scour-gui` |

Wayland üzerinde GNOME bir programın kendi penceresini öne almasına izin
vermez; pencere öne gelmek yerine görev çubuğunda işaretlenebilir.
`platform/gnome` altındaki Shell eklentisi bunu sağlar; `scripts/install-desktop`
eklentiyi bir bağlamayla birlikte kurar.

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
systemctl start scour.service
```

Hesap, `sudo`'yu çalıştıran kullanıcıya; kökler — işaretlenecek dosya
sistemleri — `/home`'a varsayılır. Kurucu yardımcıyı root'a ait
`/usr/local/libexec/scour` altına koyar, bir sistem birimi ve yalnız o hesabın
yalnız bu servisi parolasız başlatıp durdurmasına izin veren bir polkit kuralı
kurar. Kurmadan önce iki dosyayı da okuyun. Kullanıcı birimi etkinse önce
kapatın: `systemctl --user disable --now scourd.service`.

### Ayarlar

Ayarlar `~/.config/scour/config.toml` dosyasındadır. Atlama listeleri indekse
neyin girmediğini belirler; beklenen bir dosya çıkmıyorsa önce oraya bakın.

## Sorgu dili

Boşluk VE, `|` VEYA, `!` DEĞİL; tırnak tam ifade, `*` ve `?` joker,
`alan:değer` alan süzgeci. Alan adları Türkçe de yazılabilir (`tür:kod`).

```
rapor ext:pdf dm:30d          adında "rapor" geçen, son 30 günde değişmiş PDF'ler
*.log size:>100mb             100 MB'den büyük günlük dosyaları
under:/home/u/Projeler *.rs   bir dizinin altındaki bütün Rust dosyaları
path:src ext:rs !test         src altındaki Rust dosyaları, testler hariç
kind:image dm:today           bugün değişen görseller
```

`scour syntax` başvuruyu basar. Arama büyük/küçük harfe duyarsızdır; `i`, `ı`,
`I` ve `İ` tek harf sayılır. Ayrıştırma hata vermez: tanınmayan bir alan metin
olarak aranır, `scour explain "<sorgu>"` sorgunun nasıl okunduğunu gösterir.

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
