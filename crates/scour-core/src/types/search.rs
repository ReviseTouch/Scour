//! What a search asks for, and what it answers with.

use serde::{Deserialize, Serialize};

use super::{Ast, Entry, EntryId, Kind, Meta};

/// How results are ordered.
///
/// One order is privileged and the rest are not, which is a property of the
/// index rather than a preference: documents are stored newest-first, so
/// `Modified` descending can be answered by walking the postings and stopping
/// at the first page, while every other order has to visit every match. An
/// index is free to accelerate more of these; none may return the wrong order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortKey {
    /// How well the name answers the query, rather than any property of the
    /// file. Only meaningful for a query with text in it; with none, every row
    /// scores the same and the order falls back to the stored one.
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
    /// Stop counting matches at this many.
    ///
    /// With early termination, an exact total is the only remaining piece of
    /// work proportional to the number of hits — on a one-letter query that was
    /// measured at 18.1 ms of a 37 ms keystroke, against 0.88 ms once capped.
    /// A caller that genuinely needs an exact number asks for one by raising
    /// the cap, and pays for it knowingly.
    pub count_cap: u32,
}

impl Default for Page {
    fn default() -> Self {
        Self {
            offset: 0,
            limit: 200,
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

/// Every row a query matches, asked for once.
///
/// **A query and nothing else, and the two missing fields are the point.**
/// There is no page, because the whole set is what this asks for and bounding
/// it is what [`SearchRequest`] is for. And there is no sort: see
/// [`Index::scan`], which explains why an order is a different problem from a
/// stream and who is expected to solve it.
///
/// [`Index::scan`]: crate::Index::scan
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanRequest {
    pub query: Ast,
}

/// One result row, complete.
///
/// Nothing is projected away. Materialising a full row from the index's
/// document store was measured at 0.32 µs, so withholding fields would save
/// nothing and would force every caller to ask twice. Frontends that care
/// about payload size — the MCP server, mainly — trim on the way out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hit {
    pub id: EntryId,
    pub path: String,
    pub is_dir: bool,
    pub kind: Kind,
    pub meta: Meta,
    /// For a directory: what everything under it comes to.
    ///
    /// **Not folded into `meta.size`, deliberately.** A directory's `size` is
    /// its own entry table, it is what the `Size` column holds, and it is what
    /// `sort:size` orders by — so overwriting it here would put a number on
    /// screen that the ordering beside it disagrees with, which is the sort of
    /// wrongness that looks like a sorting bug for weeks.
    ///
    /// `None` for files, and for an index whose layout cannot answer it
    /// cheaply. See [`Index::subtree_sizes`].
    ///
    /// [`Index::subtree_sizes`]: crate::Index::subtree_sizes
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub under: Option<Subtree>,
}

/// What a folder holds, totalled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subtree {
    /// Space on disk, hard links counted once — the same arithmetic the
    /// disk-usage report does, because the two are printed beside each other.
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

    /// The folder it sits in — the same answer [`Entry::parent`] gives.
    ///
    /// **Here because every frontend was deriving it.** A row is shown as a
    /// name and a location, so a list without this needs each frontend to cut
    /// the path itself; the browser bridge had its own `parent_of`, four lines
    /// long and a duplicate of the one on `Entry`. One missing method, one
    /// copy per frontend, and three chances to disagree about what the parent
    /// of `/x` is.
    ///
    /// [`Entry::parent`]: crate::Entry::parent
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
            // An entry is one row. What is under it is a question about the
            // index, and this conversion has no index.
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
    /// Whether the index answered by early termination or had to visit every
    /// match. Reported rather than hidden, because a design whose fast path is
    /// silently not being taken looks exactly like one that is.
    pub fast_path: bool,
    /// Rows the index had to look at, when it can say. Zero when it cannot.
    ///
    /// Reported for the same reason as `fast_path`: an index that has quietly
    /// stopped skipping looks exactly like one that never could, and the number
    /// is what tells them apart. It is also what turns "why is this query slow"
    /// from a guess into a subtraction.
    #[serde(default)]
    pub rows_visited: u64,
    /// Rows whose path was reconstructed, including the ones then skipped to
    /// reach `offset`.
    ///
    /// The number that makes deep paging diagnosable instead of merely slow.
    /// Reaching offset 200,000 means building 200,200 paths and discarding all
    /// but two hundred of them — measured at 225 ms against 0.54 ms for the
    /// first page, and multiplied again by the number of segments, because each
    /// one is asked for the whole prefix. A client that can see this can tell
    /// "the query is expensive" from "you asked for page a thousand".
    #[serde(default)]
    pub rows_built: u64,
    /// Terms the parser could not read as written, as offsets into the query
    /// that was sent.
    ///
    /// **The parser never fails, and that is what makes this necessary.** A
    /// term it cannot read is searched for as its own text, so `dm:yarin`
    /// quietly becomes a name search and `ext:` becomes an extension filter
    /// nothing can satisfy. Both answer `0 of 0`, which is indistinguishable
    /// from a query that was understood and matched nothing — and of those two
    /// readings, the wrong one is the one anybody draws.
    ///
    /// A search box shows this while it is being typed, out of `explain`. The
    /// surfaces with no search box — a command line, a model — get one answer
    /// and do not ask a second question, so the warning has to travel with it.
    /// Only [`Role::is_warning`] roles appear here; empty is the ordinary case.
    ///
    /// [`Role::is_warning`]: crate::Role::is_warning
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
    /// How old the matching files are, counted into caller-chosen bands.
    ///
    /// `edges` are ages in **days**, ascending; a file lands in the first band
    /// whose edge it is not older than, and anything older than the last edge
    /// lands in an overflow band keyed `older`. The bands are the caller's
    /// because the shape of a histogram is a presentation choice — a chart of
    /// twenty-four logarithmic bars and a list of six named periods want
    /// different edges out of the same rows, and neither belongs in here.
    ///
    /// The keys are the edges as text, so a reply is self-describing.
    Age {
        edges: Vec<u32>,
    },
}

/// One question's answer, beside the question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FacetGroup {
    /// What the keys mean. Echoed back for the same reason `FacetResponse::by`
    /// is: a renderer that has to remember what it asked in order to draw the
    /// answer is a renderer that will get the two out of step.
    pub by: FacetBy,
    pub facets: Vec<Facet>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FacetRequest {
    pub query: Ast,
    /// The questions, all of them about the same rows. See
    /// [`FacetResponse::groups`] for why this is a list.
    pub by: Vec<FacetBy>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Facet {
    pub key: String,
    pub count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FacetResponse {
    /// One per question asked, in the order they were asked.
    ///
    /// **Several, because they are all answers about the same rows.** The
    /// sidebar wants a count, a breakdown by kind and a distribution by age,
    /// and asking for them one at a time walks the matching set three times to
    /// produce three views of it. Measured on 2.1 M entries, the three
    /// together were 100–200 ms behind a keystroke; one walk is one third of
    /// that by construction.
    #[serde(default)]
    pub groups: Vec<FacetGroup>,
    /// How many rows matched, from the same walk.
    ///
    /// Exact unless `capped`. The count is free here — the walk has to visit
    /// every matching row anyway — where asking for it separately is a second
    /// pass over the whole set.
    #[serde(default)]
    pub total: u64,
    pub facets: Vec<Facet>,
    /// What the keys mean, echoed back.
    ///
    /// Without it a client has to remember what it asked in order to render
    /// the answer, and the two things that need to know — is this key a
    /// `kind:` token to be translated, or an extension to be printed as it is
    /// — are exactly the ones that get out of step.
    #[serde(default)]
    pub by: FacetBy,
    /// The scan stopped at its cap, so the counts are a lower bound.
    ///
    /// `SearchResponse` has said this from the start and a facet could not,
    /// which meant the rail understated at scale with no way to tell. A count
    /// that is quietly wrong is worse than one that says it is incomplete.
    #[serde(default)]
    pub capped: bool,
    pub took_us: u64,
    /// Terms the parser could not read as written. See
    /// [`SearchResponse::misread`].
    ///
    /// A grouping of the wrong set of rows is the same lie as a count of it,
    /// told one level up: every bar has a plausible height and the whole
    /// picture is of files nobody asked about.
    #[serde(default)]
    pub misread: Vec<crate::Span>,
}

/// What one `apply` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ApplyReport {
    pub upserted: u64,
    pub removed: u64,
    pub subtrees_removed: u64,
    /// Entries the index already held exactly as they arrived.
    ///
    /// A walk reports what it saw rather than what changed, so on an untouched
    /// filesystem this is nearly all of them. Counted because it is the
    /// difference between a rescan that costs nothing and one that rewrites the
    /// index — and because a number that is suddenly zero is how a bug in
    /// deciding "unchanged" would announce itself.
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
    /// Sum of file lengths in the index directory, including an in-flight or
    /// orphaned file that no published segment names yet.
    ///
    /// This is the index's logical byte footprint, not allocated filesystem
    /// blocks: sparse files and transparent compression can make `du` differ.
    pub bytes_on_disk: u64,
    pub segments: u32,
    /// Entries indexed since the last rebuild.
    ///
    /// These live outside the ordered part of the index and have to be scanned
    /// in full on every query, so this number is the reason a rebuild exists.
    /// When it grows past the configured threshold, searches slow down
    /// measurably and a rebuild is due.
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
    /// No writes are expected soon: give back whatever was being held for
    /// them.
    ///
    /// Separate from `Flush` because they happen at different rates. Flushing
    /// is what a burst of changes needs every second; this is what a machine
    /// sitting idle overnight needs once. An index that holds a large write
    /// buffer — which is most of them — is otherwise a process that costs
    /// hundreds of megabytes to leave running.
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

    /// The two must agree: a row and an entry are the same file, and a list
    /// that shows one location while `stat` reports another is a list nobody
    /// can check.
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

/// How old the bytes in a directory are.
///
/// Six bands: today, this week, this month, six months, this year, older. The
/// thing no disk-usage tool shows and the one that decides what to delete —
/// twenty-five gigabytes matters less than twenty-five gigabytes nothing has
/// touched in a year.
pub const AGE_BANDS: usize = 6;

/// What one directory weighs, including everything below it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DirUsage {
    pub path: String,
    /// Logical size, in bytes.
    pub bytes: u64,
    /// Space actually allocated. Smaller for a sparse file, larger for a tiny
    /// one. Reported beside `bytes` rather than instead of it, because showing
    /// the logical size and calling it disk usage is the standard lie.
    pub disk: u64,
    pub files: u64,
    /// `bytes` split by [`AGE_BANDS`].
    pub age: [u64; AGE_BANDS],
}

/// What a subtree weighs.
///
/// **A hard-linked file is counted once**, the way `du` counts it and `du -l`
/// does not — and that is not a choice made here, it is what the index holds.
/// A source with stable identities gives every name of one inode the same
/// [`EntryId`], so the index has one row for it however many names it has.
/// 489,373 files on the corpus this was measured against have more than one.
///
/// The corollary is worth knowing: those bytes are attributed to *one* of the
/// directories the file appears in, whichever name was written last. `du` is
/// arbitrary here too — it credits whichever it reaches first — but the two
/// can disagree about where the weight sits while agreeing about the total.
///
/// [`EntryId`]: crate::types::EntryId
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageRequest {
    /// The subtree to weigh. Empty means every root the index holds.
    pub path: String,
    /// How many children to name.
    pub top: u32,
    /// Weigh only the files this matches. Empty — the default — weighs all of
    /// them, which is the `du` question and the one this answered first.
    ///
    /// **A filtered total is not a disk-usage figure and must not be shown as
    /// one.** It is what the matching files come to, arranged by folder: an
    /// answer to "where do my photos sit", not to "what is on this disk". A
    /// frontend that applies this owes its reader a word saying so.
    ///
    /// Asking is cheaper than not asking, but by less than it looks like it
    /// should be. The rollup walks the rows a search walks rather than all of
    /// them — and that is only the first of its two passes. The second one
    /// builds the folder tree, which is the same tree whatever was asked, so
    /// a query matching *nothing* still costs two thirds of the whole report.
    /// Measured over 2.2 M rows: 376 ms unfiltered, 243 ms for a query nothing
    /// answers, 224–332 ms for the rest. Scoping is the lever that works —
    /// the same numbers under one folder are 57 ms and 13.6 ms.
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
