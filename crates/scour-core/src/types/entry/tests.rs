use super::*;

fn entry(path: &str, is_dir: bool, mode: i64) -> Entry {
    Entry {
        id: EntryId::path_hash(SourceId(0), path),
        path: path.into(),
        is_dir,
        meta: Meta {
            mode,
            ..Meta::UNKNOWN
        },
    }
}

#[test]
fn path_components() {
    let e = entry("/home/u/Projeler/main.rs", false, 0o100644);
    assert_eq!(e.name(), "main.rs");
    assert_eq!(e.parent(), "/home/u/Projeler");
    assert_eq!(e.ext(), "rs");
    assert_eq!(e.kind(), Kind::Code);
    assert_eq!(entry("/etc", true, 0).parent(), "/");
    assert_eq!(entry("relative", false, 0).parent(), "");
}

#[test]
fn extension_rules() {
    assert_eq!(ext_of("main.RS"), "rs");
    assert_eq!(ext_of("arşiv.TAR.GZ"), "gz");
    assert_eq!(
        ext_of(".bashrc"),
        "",
        "a name starting with a dot has no extension"
    );
    assert_eq!(ext_of("LICENSE"), "");
    assert_eq!(ext_of("file."), "");
}

#[test]
fn a_directory_has_no_extension() {
    // Real names, off the Windows volume. A dot in a folder name is
    // ordinary there, and the last one is not a type.
    for name in ["TRABZON.MÜZEKKERE.CEVABI", "Trabzon 2. Grup", "mod.rs"] {
        let dir = entry(&format!("/mnt/depo/{name}"), true, 0o40755);
        assert_eq!(dir.ext(), "", "a directory named {name} has no extension");
        let file = entry(&format!("/mnt/depo/{name}"), false, 0o100644);
        assert_ne!(file.ext(), "", "a file named {name} still has one");
    }
}

#[test]
fn kinds() {
    assert_eq!(kind_of(true, "src", 0o40755), Kind::Dir);
    assert_eq!(kind_of(false, "photo.png", 0o100644), Kind::Image);
    // The permission bit is how an extensionless binary is recognised.
    assert_eq!(kind_of(false, "scourd", 0o100755), Kind::Exec);
    assert_eq!(kind_of(false, "stderr", 0o100644), Kind::File);
    // And it never overrides an extension: 3,408 executable `.png` files counted.
    assert_eq!(kind_of(false, "photo.png", 0o100755), Kind::Image);
    // With an unknown mode — Windows — the extension still decides.
    assert_eq!(kind_of(false, "setup.exe", 0), Kind::Exec);
    assert_eq!(Kind::from_name("klasör"), Some(&[Kind::Dir][..]));
    assert_eq!(Kind::from_name("folder"), Some(&[Kind::Dir][..]));
    assert_eq!(Kind::from_name("nope"), None);
    assert_eq!(Kind::from_u8(Kind::Media.as_u8()), Some(Kind::Media));
}

#[test]
fn the_tables_are_sorted_and_hold_each_extension_once() {
    // Binary-searched: an unsorted row is an extension that stops being found.
    for (what, table) in [("ext", EXT_TABLE), ("name", NAME_TABLE)] {
        for pair in table.windows(2) {
            assert!(
                pair[0].0 < pair[1].0,
                "{what} table: {:?} is not before {:?}",
                pair[0].0,
                pair[1].0
            );
        }
    }
    for pair in GENERATED_PAGES.windows(2) {
        assert!(pair[0] < pair[1], "pages: {:?} then {:?}", pair[0], pair[1]);
    }
    // The documented exception must not also be in the table, or which one
    // wins would depend on the order of two `if`s.
    assert!(table_lookup(EXT_TABLE, BIN_IS_BUILD.0).is_none());
}

/// The execute bit answers only where the name does not: an `ntfs3` mount with
/// `fmask=0022` gives every file on it `0755`, PDFs included.
#[test]
fn an_execute_bit_on_a_document_does_not_make_it_a_program() {
    for name in [
        "License.pdf",
        "notlar.docx",
        "yedek.zip",
        "resim.png",
        "a.txt",
    ] {
        assert!(
            !runs_when_opened(name, 0o755),
            "{name} at 0755 must still be something to open, not to run"
        );
    }
    // Where the name says nothing, the bit is all there is — as in `kind_of`.
    assert!(runs_when_opened("scourd", 0o755));
    assert!(!runs_when_opened("LICENSE", 0o644));
}

/// The type column and the double-click are decided by different functions, and
/// they must not drift.
#[test]
fn every_executable_kind_runs_when_opened() {
    for name in [
        "a.exe",
        "a.msi",
        "a.appimage",
        "a.run",
        "a.bat",
        "a.cmd",
        "a.com",
    ] {
        assert_eq!(kind_of(false, name, 0o644), Kind::Exec, "{name}");
        assert!(runs_when_opened(name, 0o644), "{name}");
    }
    // And the ones that are not `Exec` and run anyway, without the bit: a script
    // is `Code` and a launcher is a text file.
    for name in ["setup.sh", "start.desktop", "build.ps1", "thing.lnk"] {
        assert_ne!(kind_of(false, name, 0o644), Kind::Exec, "{name}");
        assert!(runs_when_opened(name, 0o644), "{name}");
    }
    // Run by Windows on a double-click, read on Linux.
    for name in ["fatura.js", "a.hta", "a.vbe", "ayar.reg", "kisayol.url"] {
        assert_eq!(runs_when_opened(name, 0o644), cfg!(windows), "{name}");
    }
}

#[test]
fn every_kind_can_be_asked_for_by_its_own_token() {
    // A query built out of `msgid()` searches for literal text: `Executable`
    // folds to a word the parser does not take.
    for k in Kind::ALL {
        let token = crate::text::DefaultFolder::of(k.token());
        let got =
            Kind::from_name(&token).unwrap_or_else(|| panic!("kind:{} does not parse", k.token()));
        assert!(got.contains(&k), "kind:{} does not mean {k:?}", k.token());
    }
    // A retired kind is still decodable and still searchable, so an index
    // written before the split answers `kind:media` the way it always did.
    assert!(
        Kind::from_name("media")
            .expect("media")
            .contains(&Kind::Media)
    );
    assert!(
        !Kind::OFFERED.contains(&Kind::Media),
        "retired, not offered"
    );
}

#[test]
fn generated_documentation_is_build_output_and_a_written_page_is_not() {
    // Worth 9.8 percentage points on the measured corpus: 83.7% of the
    // HTML there is rustdoc or javadoc.
    assert_eq!(kind_of(false, "struct.Segment.html", 0), Kind::Build);
    assert_eq!(kind_of(false, "mod.rs.html", 0), Kind::Build);
    assert_eq!(kind_of(false, "package-summary.html", 0), Kind::Build);
    assert_eq!(kind_of(false, "toString.html", 0), Kind::Doc);
    assert_eq!(kind_of(false, "index.html", 0), Kind::Doc);
    assert_eq!(kind_of(false, "about.htm", 0), Kind::Doc);
}

#[test]
fn an_extensionless_name_is_still_worth_asking_about() {
    assert_eq!(kind_of(false, "LICENSE-MIT", 0), Kind::Doc);
    assert_eq!(kind_of(false, "README", 0), Kind::Doc);
    assert_eq!(kind_of(false, "Makefile", 0), Kind::Code);
    assert_eq!(kind_of(false, ".gitignore", 0), Kind::Config);
    assert_eq!(kind_of(false, ".cargo-ok", 0), Kind::Build);
    // `LİCENSE` has to reach `license`. Lowercasing under `tr_TR` gives
    // `lıcense` and misses, which is the bug the folder exists to avoid.
    assert_eq!(kind_of(false, "LİCENSE", 0), Kind::Doc);
    // And the fingerprints deliberately left unrecognised stay unknown:
    // every prefix rule that would catch them is a plausible real name.
    assert_eq!(kind_of(false, "build-script-build", 0), Kind::File);
    assert_eq!(kind_of(false, "lib-hashbrown", 0), Kind::File);
}

#[test]
fn a_version_number_is_not_an_extension_even_though_it_looks_like_one() {
    // `ext_str` takes the rightmost dot, so `license-apache-2.0` has extension
    // `0`. Purely numeric extensions are 0.458% of this corpus, all versions, and
    // they land in `File`, which is right by accident.
    assert_eq!(ext_str("license-apache-2.0"), "0");
    assert_eq!(kind_of(false, "license-apache-2.0", 0), Kind::File);
    assert_eq!(kind_of(false, "2.2.20", 0), Kind::File);
}

#[test]
fn mode_strings() {
    assert_eq!(mode_string(0o100644), "-rw-r--r--");
    assert_eq!(mode_string(0o40755), "drwxr-xr-x");
    assert_eq!(mode_string(0o120777), "lrwxrwxrwx");
    assert_eq!(mode_string(0o104755), "-rwsr-xr-x");
    assert_eq!(mode_string(0), "");
}

#[test]
fn path_hash_is_stable_and_spread() {
    let a = EntryId::path_hash(SourceId(1), "/home/u/a.txt");
    assert_eq!(a, EntryId::path_hash(SourceId(1), "/home/u/a.txt"));
    assert_ne!(a, EntryId::path_hash(SourceId(1), "/home/u/b.txt"));
    assert_ne!(a, EntryId::path_hash(SourceId(2), "/home/u/a.txt"));
}
