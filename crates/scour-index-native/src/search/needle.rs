//! Finding a term in a name or a path: the bytes, their folded twin, and a set
//! of folders.

use super::*;
use crate::names::Folded;
use memchr::memmem::Finder;
use scour_core::text::{DefaultFolder, Folder};

/// A term as it was typed, and the same without its marks.
#[derive(Debug)]
pub(super) struct Marked {
    pub(super) want: Needle,
    pub(super) folded: Needle,
}

/// A needle the query owns, prepared once: `str::contains` constructs a Two-Way
/// searcher on every call, and folding the row into a buffer before searching
/// keeps SIMD on both the fold and the search.
#[derive(Debug)]
pub struct Needle {
    finder: Finder<'static>,
}

impl Needle {
    pub(super) fn new(needle: &str) -> Needle {
        Needle {
            finder: Finder::new(needle.as_bytes()).into_owned(),
        }
    }

    /// Is the needle in this **already folded** text?
    pub(super) fn found_in(&self, folded: &[u8]) -> bool {
        self.finder.find(folded).is_some()
    }

    /// The needle as the parser folded it — what the trigram index is keyed on.
    pub(super) fn folded(&self) -> &[u8] {
        self.finder.needle()
    }
}

/// Which directories a path term can be answered from. **A path is a directory,
/// a separator and a name**, so "the path holds `t`" is the directory holding
/// `t`, or `t` straddling the separator. Read once a query: 313,641 directories
/// against 3,019,671 rows.
#[derive(Debug)]
pub struct PathSet {
    /// Directories holding the term anywhere in them.
    pub(super) whole: Vec<bool>,
    /// Directories ending with the part of the term before its last slash.
    pub(super) ending: Vec<bool>,
    /// What a name must start with, for those.
    pub(super) tail: Vec<u8>,
    /// The term again, when it could be in the **name** — which it can be
    /// whenever it holds no separator. `path:rapor` matches `/home/u/x/rapor.pdf`
    /// on the name alone, and a set of directories cannot see that.
    pub(super) in_name: Option<Needle>,
    /// How many directories up to each number can hold a match, for a term with
    /// a separator — where only `whole` and `ending` can answer. A block whose
    /// directory numbers hold none of them is not read: the zone map `under:`
    /// already skips on. Empty when the name could answer instead and nothing
    /// says which blocks' names can.
    reach: Vec<u32>,
    /// The blocks whose names can hold the term, by the trigram index, when the
    /// name could answer: with `reach`, what lets `path:rapor` skip the blocks
    /// neither a directory nor a name can match in. Sorted.
    named: Option<Vec<u32>>,
    /// The term is asked of paths with their marks, so of a name with its own.
    pub(super) marked: bool,
}

impl PathSet {
    /// `term` is as the parser folded it and `plain` the same without marks;
    /// `marked` says which of the two a path is asked for.
    pub(super) fn build(term: &str, plain: &str, marked: bool, seg: &Segment<'_>) -> PathSet {
        let term = if marked { term } else { plain };
        let (head, tail) = match term.rsplit_once('/') {
            Some((head, tail)) => (head, tail),
            // No separator at all: nothing straddles, and `whole` answers it.
            None => (term, ""),
        };
        let count = seg.dirs.len();
        let mut set = PathSet {
            whole: vec![false; count],
            ending: vec![false; count],
            tail: tail.as_bytes().to_vec(),
            in_name: None,
            reach: Vec::new(),
            named: None,
            marked,
        };
        let needle = Needle::new(term);
        let mut fold = Folded::default();
        let mut cased = String::new();
        set.in_name = (!term.contains('/')).then(|| Needle::new(term));
        seg.dirs.each_bytes(|id, dir| {
            // Folded once and asked twice — the fold a name of this segment
            // got, or the one that keeps marks when the term is asked of them.
            let text = (marked && !dir.is_ascii()).then(|| std::str::from_utf8(dir).ok());
            let folded = match text.flatten() {
                Some(dir) => {
                    cased.clear();
                    DefaultFolder.fold_into(dir, &mut cased);
                    cased.as_bytes()
                }
                None => fold.fold_bytes(dir),
            };
            set.whole[id] = needle.found_in(folded);
            set.ending[id] = folded.ends_with(head.as_bytes());
        });
        if set.in_name.is_some() && std::env::var_os("SCOUR_NO_TRIGRAM").is_none() {
            set.named = seg.tri.candidates(plain.as_bytes());
        }
        if set.in_name.is_none() || set.named.is_some() {
            set.reach.reserve_exact(count + 1);
            let mut n = 0u32;
            set.reach.push(0);
            for (w, e) in set.whole.iter().zip(&set.ending) {
                n += u32::from(*w || *e);
                set.reach.push(n);
            }
        }
        set
    }

    /// Can `block`, whose directory numbers run from `lo` to `hi`, hold a match?
    pub(super) fn admits(&self, block: usize, lo: u32, hi: u32) -> bool {
        if self.reach.is_empty() {
            return true;
        }
        if let Some(named) = &self.named
            && named.binary_search(&(block as u32)).is_ok()
        {
            return true;
        }
        let at = |i: u32| self.reach.get(i as usize).copied();
        match (at(lo), at(hi.saturating_add(1))) {
            (Some(below), Some(through)) => through > below,
            // A number past the table: say yes rather than guess.
            _ => true,
        }
    }
}

/// What one step away from home costs. Eight, so that sixty steps — the most the
/// table records — stay under the gap between two rungs even after a long name
/// has taken its 255.
pub(super) const STEP: i64 = 8;

/// Where `needle` first appears in `haystack`.
pub(super) fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    memchr::memmem::find(haystack, needle)
}
