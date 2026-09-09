//! What a search asks for, and what it answers with.

use serde::{Deserialize, Serialize};

use super::{Ast, Entry, EntryId, Kind, Meta};

/// How results are ordered. One order is privileged by the index rather than by
/// preference: documents are stored newest-first, so `Modified` descending stops at
/// the first page while every other order has to visit every match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortKey {
    /// How well the name answers the query. Only meaningful with text in the query:
    /// with none, every row scores alike and the stored order shows through.
    Relevance,
    Name,
    Path,
    Size,
    #[default]
    Modified,
    Created,
    Accessed,
    Ext,
    Kind,
    Mode,
    Uid,
    Gid,
    /// Space allocated on disk, as opposed to the logical size.
    Disk,
}

/// Which slice of the results, and how much counting to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page {
    pub offset: u32,
    pub limit: u32,
    /// Stop counting matches at this many. An exact total is the last remaining cost
    /// proportional to hits: 18.1 ms of a 37 ms keystroke on a one-letter query,
    /// 0.88 ms capped. A caller needing an exact number raises the cap.
    pub count_cap: u32,
}

/// Rows in a page, and the floor `scourd` puts under a configured `result_limit`:
/// a client asking for exactly this many is answered in full, or a paging window's
/// last page never reaches the end of the list.
pub const PAGE_ROWS: u32 = 200;

impl Default for Page {
    fn default() -> Self {
        Self {
            offset: 0,
            limit: PAGE_ROWS,
            count_cap: 10_000,
        }
    }
}

impl Page {
    pub fn new(offset: u32, limit: u32) -> Self {
        Self {
            offset,
            limit,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchRequest {
    pub query: Ast,
    pub sort: SortKey,
    pub descending: bool,
    pub page: Page,
}

impl Default for SearchRequest {
    fn default() -> Self {
        Self {
            query: Ast::default(),
            sort: SortKey::Modified,
            descending: true,
            page: Page::default(),
        }
    }
}

/// Every row a query matches, asked for once. No page — bounding is what
/// [`SearchRequest`] is for — and no sort; see [`Index::scan`](crate::Index::scan).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanRequest {
    pub query: Ast,
}

/// One result row, complete: nothing is projected away. Materialising a full row
/// costs 0.32 µs, so withholding fields would only make every caller ask twice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hit {
    pub id: EntryId,
    pub path: String,
    pub is_dir: bool,
    pub kind: Kind,
    pub meta: Meta,
    /// For a directory: what everything under it comes to. Not folded into
    /// `meta.size`, which is the directory's own entry table and what `sort:size`
    /// orders by. `None` for files and where the layout cannot answer it cheaply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub under: Option<Subtree>,
}

/// What a folder holds, totalled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subtree {
    /// Space on disk, hard links counted once, as the disk-usage report counts them.
    pub disk: u64,
    pub files: u64,
}

impl Hit {
    pub fn name(&self) -> &str {
        match self.path.rfind('/') {
            Some(i) => &self.path[i + 1..],
            None => &self.path,
        }
    }

    /// The folder it sits in — the same answer [`Entry::parent`](crate::Entry::parent)
    /// gives, so no frontend cuts the path itself and disagrees about `/x`.
    pub fn parent(&self) -> &str {
        match self.path.rfind('/') {
            Some(0) => "/",
            Some(i) => &self.path[..i],
            None => "",
        }
    }
}

impl From<&Entry> for Hit {
    fn from(e: &Entry) -> Self {
        Self {
            id: e.id.clone(),
            path: e.path.clone(),
            is_dir: e.is_dir,
            kind: e.kind(),
            meta: e.meta,
            // What is under an entry is a question about the index, and this has none.
            under: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SearchResponse {
    pub hits: Vec<Hit>,
    /// Matches found, counted up to [`Page::count_cap`].
    pub total: u64,
    /// True when counting stopped at the cap, so `total` is a floor.
    pub capped: bool,
    pub took_us: u64,
    /// Whether the index answered by early termination or visited every match.
    /// Reported, because a fast path silently not taken looks like one that is.
    pub fast_path: bool,
    /// Rows the index had to look at, when it can say; zero when it cannot. An index
    /// that has quietly stopped skipping looks like one that never could.
    #[serde(default)]
    pub rows_visited: u64,
    /// Rows whose path was reconstructed, including those skipped to reach `offset`.
    /// Offset 200,000 builds 200,200 paths — 225 ms against 0.54 ms for the first
    /// page — which is how deep paging is told apart from an expensive query.
    #[serde(default)]
    pub rows_built: u64,
    /// Terms the parser could not read as written, as offsets into the query sent.
    /// The parser never fails, so `dm:yarin` becomes a name search answering `0 of 0`,
    /// which reads as a query that matched nothing. Warning roles only; empty is usual.
    #[serde(default)]
    pub misread: Vec<crate::Span>,
}

/// What to group a facet count by.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FacetBy {
    #[default]
    Kind,
    Ext {
        top: u32,
    },
    /// Immediate children of this directory, with a count under each.
    Dir {
        path: String,
        top: u32,
    },
    /// How old the matching files are, in caller-chosen bands. `edges` are ages in
    /// **days**, ascending; a file lands in the first band whose edge it is not older
    /// than, anything past the last in one keyed `older`. Keys are the edges as text.
    Age {
        edges: Vec<u32>,
    },
}

/// One question's answer, beside the question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FacetGroup {
    /// What the keys mean, echoed back so a renderer need not remember what it asked.
    pub by: FacetBy,
    pub facets: Vec<Facet>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FacetRequest {
    pub query: Ast,
    /// The questions, all about the same rows and answered in one walk.
    pub by: Vec<FacetBy>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Facet {
    pub key: String,
    pub count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FacetResponse {
    /// One per question asked, in the order asked. Several, because they are answers
    /// about the same rows: asked separately, three of them walk the matching set
    /// three times, which on 2.1 M entries ran 100–200 ms behind a keystroke.
    #[serde(default)]
    pub groups: Vec<FacetGroup>,
    /// How many rows matched, from the same walk. Exact unless `capped`, and free:
    /// the walk visits every matching row anyway.
    #[serde(default)]
    pub total: u64,
    pub facets: Vec<Facet>,
    /// What the keys mean, echoed back: whether a key is a `kind:` token to translate
    /// or an extension to print as it is.
    #[serde(default)]
    pub by: FacetBy,
    /// The scan stopped at its cap, so the counts are a lower bound.
    #[serde(default)]
    pub capped: bool,
    pub took_us: u64,
    /// Terms the parser could not read as written. See [`SearchResponse::misread`].
    #[serde(default)]
    pub misread: Vec<crate::Span>,
}

/// What one `apply` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ApplyReport {
    pub upserted: u64,
    pub removed: u64,
    pub subtrees_removed: u64,
    /// Entries the index already held exactly as they arrived. A walk reports what it
    /// saw rather than what changed, so on an untouched filesystem this is nearly all
    /// of them; suddenly zero means the "unchanged" test is broken.
    pub unchanged: u64,
}

impl ApplyReport {
    pub fn total(&self) -> u64 {
        self.upserted + self.removed + self.subtrees_removed
    }

    /// What the walk handed over, including what it did not have to write.
    pub fn seen(&self) -> u64 {
        self.total() + self.unchanged
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct IndexStats {
    pub entries: u64,
    pub dirs: u64,
    /// Sum of file lengths in the index directory, in-flight and orphaned files
    /// included. Logical bytes, not allocated blocks, so `du` can differ.
    pub bytes_on_disk: u64,
    pub segments: u32,
    /// Entries indexed since the last rebuild. They live outside the ordered part and
    /// are scanned in full on every query, which is what a rebuild folds them into.
    pub unsorted_entries: u64,
    /// Entries hidden but not yet erased, waiting for the next commit.
    pub pending_removals: u64,
    /// Whether the index was built with document content.
    pub has_content: bool,
}

/// How much work a maintenance pass may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Maintenance {
    /// Flush pending changes. Milliseconds.
    #[default]
    Flush,
    /// No writes are expected soon: give back whatever was being held for them.
    /// Separate from `Flush`, which a burst needs every second, where this is what an
    /// idle machine needs once.
    Idle,
    /// Reclaim space from deleted entries. Seconds.
    Compact,
    /// Rebuild the ordered body from scratch, folding in everything indexed
    /// since. Restores the fast path. Minutes on a large index.
    Rebuild,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MaintReport {
    pub level: Maintenance,
    pub bytes_before: u64,
    pub bytes_after: u64,
    pub took_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_the_interactive_case() {
        let r = SearchRequest::default();
        assert_eq!(r.sort, SortKey::Modified);
        assert!(r.descending, "newest first is what a search box shows");
        assert_eq!(r.page.limit, 200);
        assert!(
            r.page.count_cap > r.page.limit,
            "counting past the page is the point of a cap"
        );
    }

    /// A row and an entry are the same file: a list showing one location while `stat`
    /// reports another is a list nobody can check.
    #[test]
    fn a_hit_and_an_entry_cut_a_path_the_same_way() {
        for path in ["/a/b/c.txt", "/x", "bare", "/deep/er/still/f", ""] {
            let entry = Entry {
                id: EntryId::path_hash(crate::SourceId(0), path),
                path: path.to_owned(),
                is_dir: false,
                meta: Meta::UNKNOWN,
            };
            let hit = Hit::from(&entry);
            assert_eq!(hit.parent(), entry.parent(), "parent of {path:?}");
            assert_eq!(hit.name(), entry.name(), "name of {path:?}");
        }
    }

    #[test]
    fn hit_name_is_the_last_component() {
        let h = Hit {
            id: EntryId::path_hash(super::super::SourceId(0), "/a/b/c.txt"),
            path: "/a/b/c.txt".into(),
            is_dir: false,
            kind: Kind::Doc,
            meta: Meta::UNKNOWN,
            under: None,
        };
        assert_eq!(h.name(), "c.txt");
    }
}

/// How old the bytes in a directory are: today, this week, this month, six months,
/// this year, older.
pub const AGE_BANDS: usize = 6;

/// What one directory weighs, including everything below it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DirUsage {
    pub path: String,
    /// Logical size, in bytes.
    pub bytes: u64,
    /// Space actually allocated: smaller for a sparse file, larger for a tiny one.
    /// Reported beside `bytes`, not instead of it.
    pub disk: u64,
    pub files: u64,
    /// `bytes` split by [`AGE_BANDS`].
    pub age: [u64; AGE_BANDS],
}

/// What a subtree weighs. A hard-linked file counts once, as `du` counts it: every
/// name of one inode has the same [`EntryId`](crate::types::EntryId), so the index
/// holds one row, credited to whichever of its names was written last.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageRequest {
    /// The subtree to weigh. Empty means every root the index holds.
    pub path: String,
    /// How many children to name.
    pub top: u32,
    /// Weigh only the files this matches; empty — the default — weighs all of them.
    /// A filtered total is not a disk-usage figure and must not be shown as one, and
    /// it saves little: the folder tree costs two thirds of the report either way.
    #[serde(default)]
    pub query: Ast,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct UsageResponse {
    /// The scope itself.
    pub root: DirUsage,
    /// Its immediate children, heaviest first, at most `top` of them.
    pub children: Vec<DirUsage>,
    /// How many children there were before the list was cut.
    pub child_count: u32,
    pub took_us: u64,
}
