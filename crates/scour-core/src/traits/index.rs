//! Anything that can store entries and answer questions about them.

use std::fmt::Debug;

use crate::types::{
    ApplyReport, Change, Error, FacetRequest, FacetResponse, IndexStats, MaintReport, Maintenance,
    Result, SearchRequest, SearchResponse, UsageRequest, UsageResponse,
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

    /// Remove everything under `under` that is not stamped with `generation`.
    ///
    /// Called once a scan of that subtree has finished. Returns how many
    /// entries went.
    fn sweep(&self, under: &str, generation: u64) -> Result<u64>;

    /// Make everything applied so far durable and visible to new readers.
    ///
    /// Expensive — tens of milliseconds — which is why the engine batches
    /// rather than committing per change.
    fn commit(&self) -> Result<()>;

    fn search(&self, req: &SearchRequest) -> Result<SearchResponse>;

    fn facets(&self, req: &FacetRequest) -> Result<FacetResponse>;

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
