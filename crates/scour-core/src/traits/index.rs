//! Anything that can store entries and answer questions about them.

use std::fmt::Debug;

use crate::types::{
    ApplyReport, Change, Error, FacetRequest, FacetResponse, Hit, IndexStats, MaintReport,
    Maintenance, Result, ScanRequest, SearchRequest, SearchResponse, SourceId, UsageRequest,
    UsageResponse,
};

/// An index over entries.
///
/// Everything an implementation is asked for arrives as a typed request built
/// from an [`Ast`], never as a string: lowering that tree into whatever the
/// engine actually speaks is the implementation's job, and it is the only
/// place that knows the engine exists.
///
/// Implementations must be usable from several threads at once. In practice
/// searches are concurrent and writes are not, which is why `apply` takes
/// `&self` and is expected to serialise internally rather than force every
/// caller to hold a lock. The requirement is not theoretical: an index that
/// lets two commits overlap corrupts itself, and on Windows the failure is
/// noisier — an open handle makes a concurrent replace fail outright.
///
/// [`Ast`]: crate::types::Ast
pub trait Index: Send + Sync + Debug {
    /// Apply a stream of changes. Not durable until [`Index::commit`].
    ///
    /// Removals are expected to take effect for searches *immediately*, before
    /// the commit that erases them, so that deleting a directory does not leave
    /// its contents visible for the second or two until the next flush.
    fn apply(&self, changes: &mut dyn Iterator<Item = Change>) -> Result<ApplyReport>;

    /// Start a reconciliation pass, and return its number.
    ///
    /// Every entry upserted afterwards is stamped with it. This exists because
    /// a full rescan can only report what it *found*, and the interesting part
    /// of reconciling is what it did not: files deleted while nothing was
    /// watching. Remembering every id seen would cost hundreds of megabytes on
    /// a large tree; a stamp costs one column.
    fn begin_generation(&self) -> Result<u64>;

    /// Remove everything **this source** has under `under` that is not stamped
    /// with `generation`.
    ///
    /// Called once a scan has finished, with **every root that scan vouched
    /// for** — not once per root. Returns how many entries went.
    ///
    /// **One call, because one walk is one piece of evidence.** A pass notes
    /// the rows it found exactly as they already were, so that an untouched
    /// filesystem does not have to be rewritten to prove it is still there,
    /// and those notes belong to the pass rather than to any one of its roots.
    /// Called once per root, the first call consumed them and every root after
    /// it was reconciled against nothing: on a live index, a source rooted at
    /// `/usr /etc /opt /var` deleted the last three every other walk and put
    /// them back on the one between — `/opt` alternating between 5,477 rows
    /// and none, about once a minute. The signature is what makes that
    /// impossible to write again.
    ///
    /// The source is not decoration either. A sweep says "I walked these
    /// subtrees and did not find these rows", and that is a statement one
    /// source can only make about its own: where two sources' roots overlap,
    /// sweeping by path alone deletes the other's rows on the strength of a
    /// walk that never looked at them. Reproduced before this argument existed
    /// — a rescan of source 0 took source 1's row.
    ///
    /// `spare` names subtrees the walk could not look inside. Rows under them
    /// are left alone: the walk has no evidence about them either way, and
    /// deleting on no evidence is how a directory that lost its read
    /// permission loses its files from the index as well.
    fn sweep(
        &self,
        source: SourceId,
        under: &[String],
        generation: u64,
        spare: &crate::types::PrefixSet,
    ) -> Result<u64>;

    /// End a pass that will not be swept, and throw away what it noted.
    ///
    /// **A generation exists for the sweep that consumes it.** A walk that
    /// could not look — an unmounted volume, a root that lost its read
    /// permission, a cancelled pass — must not sweep, because sweeping on no
    /// evidence deletes what is merely out of reach. But it must still *end*,
    /// and until this existed only the sweep ended one.
    ///
    /// What that cost: whatever the pass noted about rows it found unchanged
    /// stayed noted, and the index keeps those notes per segment. So a
    /// compaction could not touch a noted segment — rightly, since folding
    /// renumbers what the notes point at — and the notes never went away. On a
    /// machine whose watcher walks a subtree every few seconds, one walk of a
    /// directory that had just been deleted was enough to stop compaction for
    /// good: the segment count then only rises, and every search reads all of
    /// them. Measured at 241.
    ///
    /// Doing nothing is a valid implementation for an index that keeps no such
    /// state, which is why this has a default.
    fn abandon_generation(&self, _generation: u64) -> Result<()> {
        Ok(())
    }

    /// Make everything applied so far durable and visible to new readers.
    ///
    /// Expensive — tens of milliseconds — which is why the engine batches
    /// rather than committing per change.
    fn commit(&self) -> Result<()>;

    /// Remove everything one source ever put here. Returns how many rows went.
    ///
    /// For a source that is no longer configured. Nothing else can do it: a
    /// sweep needs a generation and a walk, and a source that is gone will
    /// never walk again — so without this its rows stay in every search
    /// result, for ever, describing files nobody asked to be told about.
    ///
    /// Defaulted to nothing removed, because an index that cannot separate its
    /// sources should say so by not pretending to have done it.
    fn forget(&self, _source: SourceId) -> Result<u64> {
        Ok(0)
    }

    fn search(&self, req: &SearchRequest) -> Result<SearchResponse>;

    /// Every matching row, one at a time, for a caller that will not keep them.
    ///
    /// **The whole set, and never more than one row of it in hand.** This is
    /// what [`Index::search`] cannot be asked for: a page is bounded and this
    /// is not, so the answer cannot be a `Vec` at any size — 2.24 M rows is a
    /// couple of hundred megabytes of built paths, and the caller is writing
    /// them to a socket as they arrive.
    ///
    /// Nor can it be *assembled* from pages. A page costs what it takes to
    /// walk to its offset, so paging the whole set is linear per page and
    /// quadratic in total: measured on this index at 2.1 ms for the first page
    /// and 117.6 ms at offset one million, which is ten minutes to reach 1.4 M
    /// rows and still counting. One walk is 2.24 M rows and no offsets.
    ///
    /// `f` returns false to stop, and the count returned is what it was given.
    /// Stopping is ordinary: it is what a cancelled download looks like from
    /// down here, and an implementation must leave nothing behind when it
    /// happens.
    ///
    /// **The order is the implementation's own** and this says nothing about
    /// it. There is no `sort` on [`ScanRequest`] because ordering the whole
    /// set is a different problem from streaming it — one needs a key per
    /// match held until the last match is seen. A caller that needs an order
    /// sorts what it receives, which is what the spreadsheets and scripts on
    /// the other end of this were always going to do anyway.
    ///
    /// Defaulted to a refusal rather than to a paged loop: an index that
    /// cannot walk its matching set once should say so, instead of silently
    /// costing a caller the quadratic that this exists to remove.
    fn scan(&self, _req: &ScanRequest, _f: &mut dyn FnMut(&Hit) -> bool) -> Result<u64> {
        Err(Error::unsupported("streaming the whole matching set"))
    }

    fn facets(&self, req: &FacetRequest) -> Result<FacetResponse>;

    /// What each of these folders weighs: bytes on disk, and how many files.
    ///
    /// **Batched, because the caller is a page and a page has many folders on
    /// it.** An implementation that has to build something to answer builds it
    /// once for the whole list, which is the difference between a column and a
    /// wait.
    ///
    /// Hard links are counted once — `disk / links` a row — because this is
    /// printed beside [`Index::usage`] and the two must not disagree.
    ///
    /// Defaulted to *nothing known* rather than to zero: zero is a claim, and
    /// an index whose layout cannot answer this cheaply should say it has no
    /// answer instead of one that reads as an empty folder. Callers see
    /// `None`, and a frontend shows the same thing it shows for a file.
    fn subtree_sizes(&self, paths: &[String]) -> Result<Vec<Option<(u64, u64)>>> {
        Ok(vec![None; paths.len()])
    }

    fn stats(&self) -> Result<IndexStats>;

    fn maintain(&self, level: Maintenance) -> Result<MaintReport>;

    /// What a subtree weighs, and what is inside it.
    ///
    /// Defaulted, and the default is a refusal rather than a walk: this is an
    /// aggregation over a layout, not a query, and an index whose layout does
    /// not support it should say so instead of quietly taking a minute to
    /// answer what another one answers in milliseconds. [`Caps`] is how a
    /// caller finds out before asking.
    ///
    /// [`Caps`]: crate::types::Caps
    fn usage(&self, _req: &UsageRequest) -> Result<UsageResponse> {
        Err(Error::unsupported("disk usage"))
    }
}
