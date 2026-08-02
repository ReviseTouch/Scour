//! The fields, and the tokenizer that makes substring search possible.

use scour_core::text::{DefaultFolder, Folder};
use scour_core::{Entry, EntryId, Key, Kind, SortKey};
use serde::{Deserialize, Serialize};
use tantivy::schema::{
    FAST, INDEXED, IndexRecordOption, STORED, Schema, TextFieldIndexing, TextOptions,
};
use tantivy::tokenizer::{RawTokenizer, TextAnalyzer, Token, TokenStream, Tokenizer};

/// Trigrams that know where they are.
///
/// Tantivy's own `NgramTokenizer` assigns `position = 0` to every gram it
/// produces, which makes a phrase query over them meaningless: "contains
/// `abcd`" and "contains `a`, `b`, `c` and `d` somewhere" become the same
/// question. Emitting the character index as the position fixes it, and then a
/// `PhraseQuery` over consecutive trigrams means exactly "contains this
/// substring" — verified against a linear scan on 558,352 real file names:
/// `colpan` found 14,249 both ways, `__init__` found 437 both ways.
#[derive(Debug, Clone, Copy, Default)]
pub struct PositionalTrigram;

#[derive(Debug)]
pub struct TrigramStream {
    /// Character boundaries, so slicing never lands inside a code point.
    offsets: Vec<usize>,
    text: String,
    idx: usize,
    token: Token,
}

impl Tokenizer for PositionalTrigram {
    type TokenStream<'a> = TrigramStream;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> TrigramStream {
        let mut offsets: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
        offsets.push(text.len());
        TrigramStream {
            offsets,
            text: text.to_owned(),
            idx: 0,
            token: Token::default(),
        }
    }
}

impl TokenStream for TrigramStream {
    fn advance(&mut self) -> bool {
        // n characters yield n-2 trigrams.
        if self.offsets.len() < 4 || self.idx + 3 >= self.offsets.len() {
            return false;
        }
        let (from, to) = (self.offsets[self.idx], self.offsets[self.idx + 3]);
        self.token.offset_from = from;
        self.token.offset_to = to;
        self.token.position = self.idx;
        self.token.text.clear();
        self.token.text.push_str(&self.text[from..to]);
        self.idx += 1;
        true
    }

    fn token(&self) -> &Token {
        &self.token
    }

    fn token_mut(&mut self) -> &mut Token {
        &mut self.token
    }
}

/// The shortest term the index can answer. Two characters produce no trigram.
pub const MIN_TERM_CHARS: usize = 3;

pub const TOK_TRIGRAM: &str = "trigram";
pub const TOK_WHOLE: &str = "whole";

/// Field names. Constants rather than literals because a typo in one of these
/// is a runtime panic in a place far from the mistake.
pub mod field {
    pub const EID: &str = "eid";
    pub const EID_HASH: &str = "eid_hash";
    pub const NAME_NORM: &str = "name_norm";
    pub const PATH_NORM: &str = "path_norm";
    pub const DIRS: &str = "dirs";
    pub const PATH_EXACT: &str = "path_exact";
    pub const CONTENT: &str = "content";
    pub const NAME: &str = "name";
    pub const PATH: &str = "path";
    pub const NAME_SORT: &str = "name_sort";
    pub const PARENT_SORT: &str = "parent_sort";
    pub const EXT: &str = "ext";
    pub const MTIME: &str = "mtime";
    pub const CTIME: &str = "ctime";
    pub const ATIME: &str = "atime";
    pub const SIZE: &str = "size";
    pub const DISK: &str = "disk";
    pub const KIND: &str = "kind";
    pub const IS_DIR: &str = "is_dir";
    pub const MODE: &str = "mode";
    pub const UID: &str = "uid";
    pub const GID: &str = "gid";
    pub const ITEMS: &str = "items";
}

/// How an index is built. Fixed when it is created: changing any of these
/// means rebuilding, because they decide what is on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IndexOptions {
    /// Index full paths as trigrams, so `path:` is a fast term rather than a
    /// filter applied after the fact.
    ///
    /// Paths are roughly four times longer than names, so this is the single
    /// largest lever on index size. Off, a `path:`-only query has to read every
    /// candidate's stored path — correct, and far too slow to be a feature.
    pub index_paths: bool,
    /// Reserve and populate the content field. Nothing extracts content yet;
    /// this exists so that turning it on later is a rebuild rather than a
    /// format change.
    pub index_content: bool,
    /// Writer heap in megabytes. The transient peak while indexing follows it:
    /// 5M entries with a 1 GB heap measured 1492 MB of resident memory.
    pub writer_heap_mb: usize,
    /// How large the unsorted tail may grow before a rebuild is advised.
    ///
    /// Every query reads the whole tail, so this is the number that decides
    /// when searches start to feel slower.
    pub rebuild_threshold: u64,
}

impl Default for IndexOptions {
    fn default() -> Self {
        Self {
            index_paths: true,
            index_content: false,
            writer_heap_mb: 256,
            rebuild_threshold: 200_000,
        }
    }
}

pub fn build_schema(opts: &IndexOptions) -> Schema {
    let mut sb = Schema::builder();

    // Identity. Two representations of the same thing, for two different jobs.
    //
    // `eid` is the exact key, as bytes, and is what a delete term addresses —
    // a hash collision there would erase the wrong file. `eid_hash` is a
    // 64-bit digest in a fast column, read once per candidate in the hot loop
    // to check the pending-removal set; a collision there hides one extra row
    // for the second until the next commit, which is harmless.
    sb.add_bytes_field(field::EID, INDEXED | STORED);
    sb.add_u64_field(field::EID_HASH, FAST);

    // Searchable text: positional trigrams.
    let trigram = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer(TOK_TRIGRAM)
            .set_index_option(IndexRecordOption::WithFreqsAndPositions),
    );
    sb.add_text_field(field::NAME_NORM, trigram.clone());
    if opts.index_paths {
        sb.add_text_field(field::PATH_NORM, trigram.clone());
    }
    if opts.index_content {
        sb.add_text_field(field::CONTENT, trigram);
    }

    // Whole-value tokens. `dirs` holds every ancestor directory, which turns
    // "delete this subtree" into one term instead of a walk. `path_exact`
    // addresses a single entry — including a directory's own record, which its
    // ancestor tokens do not cover.
    let whole = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer(TOK_WHOLE)
            .set_index_option(IndexRecordOption::Basic),
    );
    sb.add_text_field(field::DIRS, whole.clone());
    sb.add_text_field(field::PATH_EXACT, whole.clone());
    // The extension is both filtered on and sorted by, so it is indexed and a
    // fast column. Its dictionary is tiny — a few hundred distinct values.
    sb.add_text_field(field::EXT, whole.set_fast(Some("raw")));

    // Displayed text goes in the document store. Measured on a 200-row page:
    // the store materialises a row in 0.32 µs, a dictionary-encoded string
    // column in 14.33 µs. lz4 costs nothing measurable and halves the file.
    sb.add_text_field(field::NAME, STORED);
    sb.add_text_field(field::PATH, STORED);

    // Sorting keys are columns, because they are read in bulk rather than one
    // row at a time. The folded name sorts case-insensitively; the parent
    // directory sorts "by folder, then by name", which is what sorting by path
    // is actually for — and its dictionary is small, because thousands of
    // files share one parent, where a full-path column would be unique per row.
    let sort_col = TextOptions::default().set_fast(Some("raw"));
    sb.add_text_field(field::NAME_SORT, sort_col.clone());
    sb.add_text_field(field::PARENT_SORT, sort_col);

    sb.add_i64_field(field::MTIME, FAST | INDEXED);
    sb.add_i64_field(field::CTIME, FAST | INDEXED);
    sb.add_i64_field(field::ATIME, FAST | INDEXED);
    sb.add_i64_field(field::SIZE, FAST | INDEXED);
    sb.add_i64_field(field::DISK, FAST);
    // Signed, like every other numeric column, even though neither is ever
    // negative: sorting reads columns through one typed accessor, and a single
    // u64 field among them means a second code path that exists only to say
    // the same thing.
    sb.add_i64_field(field::KIND, FAST | INDEXED);
    sb.add_i64_field(field::IS_DIR, FAST | INDEXED);
    sb.add_i64_field(field::MODE, FAST);
    sb.add_i64_field(field::UID, FAST);
    sb.add_i64_field(field::GID, FAST);
    sb.add_i64_field(field::ITEMS, FAST);

    sb.build()
}

pub fn register_tokenizers(index: &tantivy::Index) {
    index.tokenizers().register(
        TOK_TRIGRAM,
        TextAnalyzer::builder(PositionalTrigram).build(),
    );
    index.tokenizers().register(
        TOK_WHOLE,
        TextAnalyzer::builder(RawTokenizer::default()).build(),
    );
}

/// The fast column a sort key orders by, and whether that column is text.
///
/// A free function rather than a method on `SortKey`, because the key belongs
/// to the contract crate and the columns belong to this one — which is the
/// separation working as intended.
pub(crate) fn sort_column(key: SortKey) -> (&'static str, bool) {
    {
        match key {
            SortKey::Name => (field::NAME_SORT, true),
            // Sorting by path means "grouped by folder, then by name"; see the
            // note on `parent_sort` above.
            SortKey::Path => (field::PARENT_SORT, true),
            SortKey::Ext => (field::EXT, true),
            SortKey::Size => (field::SIZE, false),
            SortKey::Modified => (field::MTIME, false),
            SortKey::Created => (field::CTIME, false),
            SortKey::Accessed => (field::ATIME, false),
            SortKey::Kind => (field::KIND, false),
            SortKey::Items => (field::ITEMS, false),
            SortKey::Mode => (field::MODE, false),
            SortKey::Uid => (field::UID, false),
            SortKey::Gid => (field::GID, false),
            SortKey::Disk => (field::DISK, false),
        }
    }
}

/// A 64-bit digest of an entry id, for the hot-loop removal check.
pub fn eid_hash(id: &EntryId) -> u64 {
    const SEED: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut h = SEED ^ (id.source.0 as u64).wrapping_mul(SEED);
    let mut mix = |v: u64| h = (h.rotate_left(7) ^ v).wrapping_mul(SEED);
    match &id.key {
        Key::Inode { dev, ino } => {
            mix(*dev);
            mix(*ino);
        }
        Key::PathHash(v) => mix(*v),
        Key::Opaque(b) => {
            for chunk in b.chunks(8) {
                let mut buf = [0u8; 8];
                buf[..chunk.len()].copy_from_slice(chunk);
                mix(u64::from_le_bytes(buf));
            }
        }
    }
    h
}

/// The exact bytes a delete term addresses.
///
/// Hand-rolled rather than serde, because this is a key in an on-disk format:
/// it has to stay byte-identical across versions, and a derived encoding can
/// change when a field is reordered.
pub fn eid_bytes(id: &EntryId) -> Vec<u8> {
    let mut v = Vec::with_capacity(24);
    v.extend_from_slice(&id.source.0.to_be_bytes());
    match &id.key {
        Key::Inode { dev, ino } => {
            v.push(1);
            v.extend_from_slice(&dev.to_be_bytes());
            v.extend_from_slice(&ino.to_be_bytes());
        }
        Key::PathHash(h) => {
            v.push(2);
            v.extend_from_slice(&h.to_be_bytes());
        }
        Key::Opaque(b) => {
            v.push(3);
            v.extend_from_slice(b);
        }
    }
    v
}

pub fn eid_from_bytes(b: &[u8]) -> Option<EntryId> {
    use scour_core::SourceId;
    if b.len() < 5 {
        return None;
    }
    let source = SourceId(u32::from_be_bytes(b[..4].try_into().ok()?));
    let key = match b[4] {
        1 if b.len() == 21 => Key::Inode {
            dev: u64::from_be_bytes(b[5..13].try_into().ok()?),
            ino: u64::from_be_bytes(b[13..21].try_into().ok()?),
        },
        2 if b.len() == 13 => Key::PathHash(u64::from_be_bytes(b[5..13].try_into().ok()?)),
        3 => Key::Opaque(b[5..].into()),
        _ => return None,
    };
    Some(EntryId { source, key })
}

/// Everything about an entry that goes into the index, computed once.
pub struct Row {
    pub eid: Vec<u8>,
    pub eid_hash: u64,
    pub name: String,
    pub name_norm: String,
    pub path: String,
    pub path_norm: String,
    pub parent: String,
    pub ext: String,
    pub kind: Kind,
}

impl Row {
    pub fn build(e: &Entry) -> Self {
        let name = e.name().to_owned();
        let ext = e.ext();
        Self {
            eid: eid_bytes(&e.id),
            eid_hash: eid_hash(&e.id),
            name_norm: DefaultFolder.fold(&name),
            path_norm: DefaultFolder.fold(&e.path),
            parent: DefaultFolder.fold(e.parent()),
            kind: scour_core::kind_of(e.is_dir, &ext, e.meta.mode),
            ext,
            name,
            path: e.path.clone(),
        }
    }

    /// Every ancestor directory of this path, longest last.
    ///
    /// `/a/b/c.txt` yields `/a` and `/a/b`. The entry's own path is not
    /// included, which is why deleting a subtree also has to address
    /// `path_exact` — otherwise the directory's own record survives its
    /// contents.
    pub fn ancestors(path: &str) -> impl Iterator<Item = &str> {
        let mut at = 0usize;
        std::iter::from_fn(move || {
            let rest = path.get(at + 1..)?;
            let i = rest.find('/')?;
            at += i + 1;
            Some(&path[..at])
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scour_core::{Meta, SourceId};

    fn tokens(text: &str) -> Vec<(usize, String)> {
        let mut t = PositionalTrigram;
        let mut s = t.token_stream(text);
        let mut out = Vec::new();
        while s.advance() {
            let tok = s.token();
            out.push((tok.position, tok.text.clone()));
        }
        out
    }

    #[test]
    fn trigrams_carry_their_position() {
        // The whole reason this tokenizer exists: tantivy's own puts every
        // gram at position 0, and then a phrase query cannot mean "adjacent".
        assert_eq!(
            tokens("abcde"),
            vec![(0, "abc".into()), (1, "bcd".into()), (2, "cde".into())]
        );
    }

    #[test]
    fn short_text_yields_nothing() {
        assert!(tokens("").is_empty());
        assert!(tokens("ab").is_empty());
        assert_eq!(tokens("abc").len(), 1);
    }

    #[test]
    fn trigrams_are_cut_on_character_boundaries() {
        // 'ı', 'ş' and 'ğ' are two bytes each; slicing by byte would panic.
        let t = tokens("çalışkan");
        assert_eq!(t[0].1, "çal");
        assert_eq!(t.len(), "çalışkan".chars().count() - 2);
    }

    #[test]
    fn entry_ids_round_trip_through_their_key_bytes() {
        for id in [
            EntryId::inode(SourceId(3), 66_310, 1_234_567),
            EntryId::path_hash(SourceId(0), "/home/u/x"),
            EntryId {
                source: SourceId(9),
                key: Key::Opaque(b"etag-abc".as_slice().into()),
            },
        ] {
            assert_eq!(eid_from_bytes(&eid_bytes(&id)).as_ref(), Some(&id));
        }
    }

    #[test]
    fn distinct_ids_get_distinct_keys() {
        let a = eid_bytes(&EntryId::inode(SourceId(1), 1, 2));
        let b = eid_bytes(&EntryId::inode(SourceId(1), 2, 1));
        assert_ne!(a, b, "dev and ino must not be interchangeable");
        assert_ne!(
            eid_hash(&EntryId::inode(SourceId(1), 1, 2)),
            eid_hash(&EntryId::inode(SourceId(2), 1, 2))
        );
    }

    #[test]
    fn ancestors_cover_the_parents_and_not_the_entry() {
        assert_eq!(
            Row::ancestors("/a/b/c.txt").collect::<Vec<_>>(),
            vec!["/a", "/a/b"]
        );
        assert_eq!(Row::ancestors("/a").collect::<Vec<_>>(), Vec::<&str>::new());
        assert_eq!(Row::ancestors("").collect::<Vec<_>>(), Vec::<&str>::new());
    }

    #[test]
    fn a_row_folds_everything_it_will_be_searched_by() {
        let e = Entry {
            id: EntryId::path_hash(SourceId(0), "/home/u/Belgeler/RAPOR.PDF"),
            path: "/home/u/Belgeler/RAPOR.PDF".into(),
            is_dir: false,
            meta: Meta::UNKNOWN,
        };
        let r = Row::build(&e);
        assert_eq!(r.name, "RAPOR.PDF");
        assert_eq!(r.name_norm, "rapor.pdf");
        assert_eq!(r.path_norm, "/home/u/belgeler/rapor.pdf");
        assert_eq!(r.parent, "/home/u/belgeler");
        assert_eq!(r.ext, "pdf");
        assert_eq!(r.kind, Kind::Doc);
    }

    #[test]
    fn the_schema_reflects_its_options() {
        let full = build_schema(&IndexOptions {
            index_content: true,
            ..Default::default()
        });
        assert!(full.get_field(field::CONTENT).is_ok());
        assert!(full.get_field(field::PATH_NORM).is_ok());

        let lean = build_schema(&IndexOptions {
            index_paths: false,
            index_content: false,
            ..Default::default()
        });
        assert!(lean.get_field(field::CONTENT).is_err());
        assert!(lean.get_field(field::PATH_NORM).is_err());
        // Everything the default view needs is present either way.
        for f in [
            field::NAME_NORM,
            field::NAME,
            field::PATH,
            field::MTIME,
            field::EID,
        ] {
            assert!(lean.get_field(f).is_ok(), "{f} is not optional");
        }
    }

    #[test]
    fn every_sort_key_names_a_real_column() {
        let schema = build_schema(&IndexOptions::default());
        for k in [
            SortKey::Name,
            SortKey::Path,
            SortKey::Size,
            SortKey::Modified,
            SortKey::Created,
            SortKey::Accessed,
            SortKey::Ext,
            SortKey::Kind,
            SortKey::Items,
            SortKey::Mode,
            SortKey::Uid,
            SortKey::Gid,
            SortKey::Disk,
        ] {
            let (col, _) = sort_column(k);
            assert!(
                schema.get_field(col).is_ok(),
                "{k:?} sorts by a field that does not exist"
            );
        }
    }
}
