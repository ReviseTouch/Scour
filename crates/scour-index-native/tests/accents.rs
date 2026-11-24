//! Accents, typing mistakes and letters in order, checked against the truth:
//! every answer here is compared with [`scour_mock::brute_force`], which folds
//! each name both ways and fills a whole edit table a name.

use scour_core::{
    Ast, Change, Entry, EntryId, Index, Maintenance, Meta, Page, SearchRequest, SortKey, SourceId,
};
use scour_index_native::NativeIndex;
use scour_mock::brute_force;
use scour_query::parse_at;

const NOW: i64 = 1_785_000_000;

/// Words with and without marks, in pairs a search has to tell apart or not.
const WORDS: &[&str] = &[
    "sirket",
    "şirket",
    "ŞİRKET",
    "Sirket",
    "cafe",
    "café",
    "CAFÉ",
    "cafe\u{301}",
    "rapor",
    "raporu",
    "rapot",
    "raopr",
    "rappor",
    "rpor",
    "vapor",
    "Straße",
    "strasse",
    "STRASSE",
    "Ёлка",
    "елка",
    "ЁЛКА",
    "Öğüt",
    "ogut",
    "öğüt",
    "ışık",
    "isik",
    "IŞIK",
    "muhasebe",
    "muhsaebe",
    "muhasebeci",
    "muhasebici",
    "scour-gui",
    "scour-tui",
    "screen-guide",
    "Ａｂｃ",
    "abc",
    "Việt",
    "viet",
    "Łódź",
    "lodz",
    "señor",
    "senor",
    "報告",
    "Æther",
    "aether",
    "noël",
    "résumé",
    "resume",
    "ΑΘΉΝΑ",
    "αθηνα",
    "שָׁלוֹם",
    "שלום",
    "main",
    "README",
    "x",
];

const EXTS: &[&str] = &[
    "pdf", "PDF", "txt", "rs", "pdé", "PDÉ", "docx", "", "ı", "tar.gz",
];

const DIRS: &[&str] = &[
    "/home/u",
    "/home/u/Belgeler",
    "/home/u/Belgeler/Şirket",
    "/home/u/Belgeler/sirket",
    "/home/u/Café",
    "/home/u/cafe/İş",
    "/mnt/depo/Straße",
    "/mnt/depo/strasse/Ёлка",
];

fn corpus(files: usize) -> Vec<Entry> {
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = |n: usize| -> usize {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        ((state >> 33) as usize) % n
    };
    let mut entries: Vec<Entry> = Vec::new();
    for (i, dir) in DIRS.iter().enumerate() {
        entries.push(Entry {
            id: EntryId::inode(SourceId(0), 1, i as u64 + 1),
            path: (*dir).into(),
            is_dir: true,
            meta: Meta {
                mtime: NOW - 1_000_000 - i as i64,
                ..Meta::UNKNOWN
            },
        });
    }
    for i in 0..files {
        let mut name = String::new();
        for part in 0..next(3) + 1 {
            if part > 0 {
                name.push_str(["-", "_", " ", ""][next(4)]);
            }
            name.push_str(WORDS[next(WORDS.len())]);
        }
        let ext = EXTS[next(EXTS.len())];
        if !ext.is_empty() {
            name.push('.');
            name.push_str(ext);
        }
        entries.push(Entry {
            id: EntryId::inode(SourceId(0), 1, 100 + i as u64),
            // The number keeps every path its own.
            path: format!("{}/{i}-{name}", DIRS[next(DIRS.len())]),
            is_dir: false,
            meta: Meta {
                // Few distinct dates, so ties are common and ordered by path.
                mtime: NOW - next(400) as i64 * 3_600,
                size: next(1 << 20) as i64,
                ..Meta::UNKNOWN
            },
        });
    }
    entries
}

struct Fixture {
    _tmp: tempfile::TempDir,
    index: NativeIndex,
    entries: Vec<Entry>,
}

impl Fixture {
    fn new(files: usize, chunk: usize) -> Fixture {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let index = NativeIndex::open_or_create(tmp.path()).expect("create");
        let entries = corpus(files);
        for part in entries.chunks(chunk) {
            let mut it = part.iter().cloned().map(Change::Upsert);
            index.apply(&mut it).expect("apply");
            index.commit().expect("commit");
        }
        Fixture {
            _tmp: tmp,
            index,
            entries,
        }
    }

    fn found(&self, query: &Ast, sort: SortKey, desc: bool) -> (Vec<String>, u64) {
        let got = self
            .index
            .search(&SearchRequest {
                query: query.clone(),
                sort,
                descending: desc,
                page: Page {
                    offset: 0,
                    limit: 60,
                    count_cap: 10_000_000,
                },
            })
            .expect("search");
        (got.hits.into_iter().map(|h| h.path).collect(), got.total)
    }

    /// The page and the total, in both readings of an accent and the strict
    /// reading of a mistake.
    fn check(&self, q: &str, sort: SortKey, desc: bool) -> u64 {
        let mut totals = 0;
        for (exact, strict) in [(false, false), (true, false), (false, true)] {
            let query = Ast {
                exact_accents: exact,
                strict_typos: strict,
                ..parse_at(q, NOW)
            };
            let all = brute_force(&self.entries, &query, sort, desc, usize::MAX);
            let want: Vec<String> = all.iter().take(60).map(|h| h.path.clone()).collect();
            let (got, total) = self.found(&query, sort, desc);
            assert_eq!(
                got, want,
                "{q:?} by {sort:?} desc={desc} exact={exact} strict={strict}"
            );
            assert_eq!(
                total,
                all.len() as u64,
                "{q:?} counted, exact={exact} strict={strict}"
            );
            if !strict {
                totals += total;
            }
        }
        totals
    }
}

const QUERIES: &[&str] = &[
    "sirket",
    "şirket",
    "ŞİRKET",
    "cafe",
    "café",
    "cafe\u{301}",
    "strasse",
    "straße",
    "елка",
    "ёлка",
    "ogut",
    "öğüt",
    "isik",
    "ışık",
    "abc",
    "ａｂｃ",
    "viet",
    "việt",
    "lodz",
    "senor rapor",
    "señor|noël",
    "!şirket rapor",
    "!sirket rapor",
    "αθηνα",
    "αθήνα",
    "שלום",
    "aether",
    "æther",
    "sir*t",
    "şir*t*",
    "*caf?.pdf",
    "*.pdé",
    "ext:pde",
    "ext:pdé;rs",
    "ext:ı",
    "path:sirket",
    "path:şirket",
    "Belgeler/sirket",
    "belgeler/şirket",
    "cafe/is",
    "café/",
    "path:straße",
    "depo/strasse/елка",
    "regex:^[0-9]+-caf.\\.",
    "regex:café$",
    "regex:şirket",
    "case:Sirket",
    "case:ŞİRKET",
    "len:<12",
    "len:>30",
    "fuzzy:rapor",
    "fuzzy:raporr",
    "fuzzy:rapxor",
    "fuzzy:şirket",
    "fuzzy:sirkte",
    "fuzzy:muhasebe",
    "fuzzy:muhasebeci",
    "fuzzy:muhasebeci ext:pdf",
    "fuzzy:strasse !fuzzy:resume",
    "fuzzy:ёлкаа|fuzzy:αθηναα",
    "fuzzy:main",
    "fuzzy:zzzzzzzz",
    "letters:scrgui",
    "letters:şkt",
    "letters:skt ext:pdf",
    "!letters:a",
    "letters:報告",
];

#[test]
fn every_reading_of_an_accent_is_what_brute_force_gives() {
    let f = Fixture::new(6_000, 1_500);
    assert!(f.index.stats().expect("stats").segments >= 4);
    let mut found = 0;
    for q in QUERIES {
        found += f.check(q, SortKey::Modified, true);
    }
    // The fixture finds things: a test that compares nothing with nothing passes.
    assert!(found > 20_000, "{found}");
    for q in [
        "sirket",
        "şirket",
        "fuzzy:raporr",
        "fuzzy:rapxor",
        "letters:scrgui",
        "café/",
        "*.pdé",
    ] {
        for sort in [SortKey::Name, SortKey::Path, SortKey::Ext, SortKey::Size] {
            f.check(q, sort, false);
            f.check(q, sort, true);
        }
        f.check(q, SortKey::Modified, false);
    }
}

#[test]
fn one_segment_and_a_walk_in_parts_answer_the_same() {
    let f = Fixture::new(6_000, 6_100);
    f.index.maintain(Maintenance::Rebuild).expect("rebuild");
    assert_eq!(f.index.stats().expect("stats").segments, 1);
    for q in QUERIES {
        f.check(q, SortKey::Modified, true);
        f.check(q, SortKey::Name, false);
    }
}

#[test]
fn a_typo_finds_what_the_right_spelling_would_and_only_when_asked() {
    let f = Fixture::new(3_000, 3_100);
    let count = |q: &str| f.found(&parse_at(q, NOW), SortKey::Modified, true).1;
    assert_eq!(count("rapxor"), 0);
    assert!(count("fuzzy:rapxor") >= count("rapor"));
    assert!(count("rapor") > 0);
    // Forgiven as the service forgives it, the plain word says the same.
    let forgiven = parse_at("rapxor", NOW).forgiving().expect("long enough");
    assert_eq!(
        f.found(&forgiven, SortKey::Modified, true),
        f.found(&parse_at("fuzzy:rapxor", NOW), SortKey::Modified, true)
    );
    assert!(count("letters:scrgui") >= count("scour-gui") && count("scour-gui") > 0);
}
